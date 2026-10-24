//! Versioned JSON capability ABI in a fresh fuel-metered Wasm instance.
//! No WASI, filesystem, clock, environment, network, subprocess or native imports.
//! Guests export `memory`, `rustpy_alloc(i32)->i32`, and
//! `rustpy_invoke(i32,i32)->i64`. The request is UTF-8 JSON at the allocated
//! pointer/length. The result packs its pointer in the high 32 bits and length
//! in the low 32 bits. Memory and pointers belong only to this fresh instance;
//! the IDE validates the complete JSON result before applying capabilities.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Read, path::Path};
use wasmi::{Config, Engine, Linker, Module, Store, StoreLimitsBuilder};

pub const MAX_MODULE: usize = 8 * 1024 * 1024;
pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const FUEL: u64 = 2_000_000;

fn leb(data: &[u8], offset: &mut usize) -> Result<usize, String> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *data.get(*offset).ok_or("Truncated Wasm integer")?;
        *offset += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err("Oversized Wasm integer".into());
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value as usize);
        }
    }
    Err("Oversized Wasm integer".into())
}
/// Bound compilation inputs as well as guest execution. In particular a tiny
/// malformed module cannot request billions of locals before validation.
fn preflight(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err("Expected a Wasm v1 module".into());
    }
    let mut offset = 8;
    let mut total_locals = 0usize;
    while offset < bytes.len() {
        let id = bytes[offset];
        offset += 1;
        let length = leb(bytes, &mut offset)?;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or("Truncated Wasm section")?;
        let data = &bytes[offset..end];
        offset = end;
        if id == 0 {
            if length > 1024 * 1024 {
                return Err("Wasm custom section exceeds 1 MiB".into());
            }
            continue;
        }
        if id > 12 {
            return Err("Unsupported Wasm section".into());
        }
        if id == 8 {
            continue;
        }
        let mut cursor = 0;
        let count = leb(data, &mut cursor)?;
        let maximum = match id {
            1 | 3 | 10 => 4096,
            2 => 0,
            4 | 5 => 1,
            6 => 1024,
            7 => 256,
            9 | 11 => 128,
            12 => 128,
            _ => 4096,
        };
        if count > maximum {
            return Err("Wasm declaration count exceeds plugin compilation budget".into());
        }
        if id == 1 {
            let mut signature_values = 0usize;
            for _ in 0..count {
                if data.get(cursor) != Some(&0x60) {
                    return Err("Only Wasm function types are supported".into());
                }
                cursor += 1;
                for _ in 0..2 {
                    let values = leb(data, &mut cursor)?;
                    signature_values = signature_values
                        .checked_add(values)
                        .ok_or("Too many Wasm signature values")?;
                    if values > 128 || signature_values > 16384 {
                        return Err("Wasm function signature budget exceeded".into());
                    }
                    cursor = cursor
                        .checked_add(values)
                        .filter(|end| *end <= data.len())
                        .ok_or("Truncated Wasm signature")?;
                }
            }
        }
        if id == 10 {
            for _ in 0..count {
                let size = leb(data, &mut cursor)?;
                if size > 512 * 1024 {
                    return Err("Wasm function exceeds 512 KiB".into());
                }
                let end = cursor
                    .checked_add(size)
                    .filter(|end| *end <= data.len())
                    .ok_or("Truncated Wasm function")?;
                let body = &data[cursor..end];
                cursor = end;
                let mut local = 0;
                let declarations = leb(body, &mut local)?;
                if declarations > 128 {
                    return Err("Too many Wasm local declarations".into());
                }
                let mut locals = 0usize;
                for _ in 0..declarations {
                    let added = leb(body, &mut local)?;
                    locals = locals.checked_add(added).ok_or("Too many Wasm locals")?;
                    total_locals = total_locals
                        .checked_add(added)
                        .ok_or("Too many Wasm locals")?;
                    if locals > 4096 || total_locals > 65536 || local >= body.len() {
                        return Err("Too many or invalid Wasm locals".into());
                    }
                    local += 1;
                }
            }
        }
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    DocumentRead,
    DocumentWrite,
    Actions,
    ToolWindows,
    Inspections,
    Completion,
    FileTypes,
    Themes,
    Keymaps,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifest {
    pub api_version: u32,
    pub module: String,
    pub capabilities: BTreeSet<Capability>,
}
impl RuntimeManifest {
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.api_version != 1 {
            problems.push("runtime.api_version: only version 1 is supported".into());
        }
        if crate::paths::safe_relative_path(&self.module).is_err()
            || !self.module.ends_with(".wasm")
        {
            problems.push("runtime.module: choose a relative .wasm path inside the plugin".into());
        }
        problems
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub path: String,
    pub source: String,
    pub version: u64,
    pub offset: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub api_version: u32,
    pub event: String,
    pub action: Option<String>,
    pub document: Option<Document>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Response {
    pub api_version: u32,
    pub actions: Vec<Action>,
    pub tool_windows: Vec<Window>,
    pub diagnostics: Vec<Diagnostic>,
    pub completions: Vec<Completion>,
    pub edits: Vec<Edit>,
    pub file_types: Vec<FileType>,
    pub themes: Vec<serde_json::Value>,
    pub keymaps: Vec<serde_json::Value>,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub id: String,
    pub title: String,
    pub text: String,
    pub actions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    pub severity: String,
    pub message: String,
    pub code: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub label: String,
    pub insert: String,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileType {
    pub extensions: Vec<String>,
    pub language: String,
}

fn bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Plugin file exceeds its byte budget".into());
    }
    Ok(bytes)
}

pub fn module_bytes(dir: &Path, manifest: &RuntimeManifest) -> Result<Vec<u8>, String> {
    if !manifest.problems().is_empty() {
        return Err(manifest.problems().join("; "));
    }
    bounded_file(
        &crate::paths::existing_file_inside(dir, &manifest.module)?,
        MAX_MODULE,
    )
}
fn manifest_bytes(dir: &Path, runtime: &RuntimeManifest) -> Result<Vec<u8>, String> {
    let bytes = bounded_file(
        &crate::paths::existing_file_inside(dir, crate::MANIFEST_FILE)?,
        1024 * 1024,
    )?;
    let current: crate::PluginManifest =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if current.runtime.as_ref() != Some(runtime) {
        return Err("Plugin manifest changed; reload it before granting permissions".into());
    }
    Ok(bytes)
}

/// Consent covers both manifest permissions and exact executable bytes. Recheck
/// on the worker immediately before instantiation, never cache a stale grant.
pub fn digest(dir: &Path, manifest: &RuntimeManifest) -> Result<String, String> {
    let mut bytes = manifest_bytes(dir, manifest)?;
    bytes.push(0);
    bytes.extend(module_bytes(dir, manifest)?);
    Ok(crate::sha256_hex(&bytes))
}

pub fn invoke(
    dir: &Path,
    manifest: &RuntimeManifest,
    granted_digest: &str,
    mut request: Request,
    policy: &rustpy_settings::plugins::Settings,
) -> Result<Response, String> {
    if digest(dir, manifest)? != granted_digest {
        return Err("Plugin changed; review its permissions and grant again".into());
    }
    let bytes = module_bytes(dir, manifest)?;
    // Bind the grant to the exact bytes being instantiated, including file-swap races.
    let loaded_manifest = manifest_bytes(dir, manifest)?;
    crate::signatures::verify_loaded_runtime(
        dir,
        &manifest.module,
        &bytes,
        &loaded_manifest,
        policy,
    )?;
    let mut grant_input = loaded_manifest;
    grant_input.push(0);
    grant_input.extend_from_slice(&bytes);
    if crate::sha256_hex(&grant_input) != granted_digest {
        return Err("Plugin changed while loading".into());
    }
    if request.api_version != 1 {
        return Err("Unsupported plugin request version".into());
    }
    if !manifest.capabilities.contains(&Capability::DocumentRead) {
        request.document = None;
    }
    let payload = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    if payload.len() > MAX_MESSAGE {
        return Err("Plugin request exceeds 1 MiB".into());
    }
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    preflight(&bytes)?;
    let module = Module::new(&engine, &bytes[..]).map_err(|e| e.to_string())?;
    if module.imports().next().is_some() {
        return Err("Plugin imports are forbidden; use the JSON capability API".into());
    }
    let limits = StoreLimitsBuilder::new()
        .memory_size(16 * 1024 * 1024)
        .table_elements(4096)
        .instances(1)
        .tables(1)
        .memories(1)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(&engine, limits);
    store.limiter(|limits| limits);
    store.set_fuel(FUEL).map_err(|e| e.to_string())?;
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| e.to_string())?
        .start(&mut store)
        .map_err(|e| e.to_string())?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or("Plugin must export memory")?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "rustpy_alloc")
        .map_err(|e| e.to_string())?;
    let call = instance
        .get_typed_func::<(i32, i32), i64>(&store, "rustpy_invoke")
        .map_err(|e| e.to_string())?;
    let pointer = alloc
        .call(&mut store, payload.len() as i32)
        .map_err(|e| e.to_string())?;
    if pointer < 0 {
        return Err("Plugin returned a negative allocation".into());
    }
    memory
        .write(&mut store, pointer as usize, &payload)
        .map_err(|e| e.to_string())?;
    let packed = call
        .call(&mut store, (pointer, payload.len() as i32))
        .map_err(|e| e.to_string())? as u64;
    let (pointer, length) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
    if length > MAX_MESSAGE {
        return Err("Plugin result exceeds 1 MiB".into());
    }
    let mut output = vec![0; length];
    memory
        .read(&store, pointer, &mut output)
        .map_err(|e| e.to_string())?;
    let response: Response =
        serde_json::from_slice(&output).map_err(|e| format!("Invalid plugin response: {e}"))?;
    validate_response(&response, manifest, &request)?;
    Ok(response)
}

pub fn validate_response(
    response: &Response,
    manifest: &RuntimeManifest,
    request: &Request,
) -> Result<(), String> {
    if response.api_version != 1 {
        return Err("Unsupported plugin response version".into());
    }
    if !response.edits.is_empty() && !matches!(request.event.as_str(), "action" | "script") {
        return Err("Document edits require an explicit plugin action or script invocation".into());
    }
    for (present, capability) in [
        (!response.actions.is_empty(), Capability::Actions),
        (!response.tool_windows.is_empty(), Capability::ToolWindows),
        (!response.diagnostics.is_empty(), Capability::Inspections),
        (!response.completions.is_empty(), Capability::Completion),
        (!response.edits.is_empty(), Capability::DocumentWrite),
        (!response.file_types.is_empty(), Capability::FileTypes),
        (!response.themes.is_empty(), Capability::Themes),
        (!response.keymaps.is_empty(), Capability::Keymaps),
    ] {
        if present && !manifest.capabilities.contains(&capability) {
            return Err(format!("Plugin did not request {capability:?}"));
        }
    }
    if response.actions.len() > 128
        || response.tool_windows.len() > 16
        || response.diagnostics.len() > 512
        || response.completions.len() > 512
        || response.edits.len() > 128
        || response.file_types.len() > 32
        || response.themes.len() > 8
        || response.keymaps.len() > 8
        || response.status.len() > 8192
    {
        return Err("Plugin contribution count exceeds its budget".into());
    }
    let valid_id = |id: &str| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    let mut ids = BTreeSet::new();
    for action in &response.actions {
        if !valid_id(&action.id)
            || !ids.insert(&action.id)
            || action.title.is_empty()
            || action.title.len() > 256
        {
            return Err("Invalid or duplicate plugin action".into());
        }
    }
    let mut window_ids = BTreeSet::new();
    for window in &response.tool_windows {
        if !valid_id(&window.id)
            || !window_ids.insert(&window.id)
            || window.actions.iter().any(|action| !valid_id(action))
            || window.title.len() > 256
            || window.text.len() > 65536
            || window.actions.len() > 128
        {
            return Err("Invalid plugin tool window".into());
        }
    }
    let source = request
        .document
        .as_ref()
        .map(|document| document.source.as_str())
        .unwrap_or("");
    let range = |start: usize, end: usize| {
        start <= end
            && end <= source.len()
            && source.is_char_boundary(start)
            && source.is_char_boundary(end)
    };
    for diagnostic in &response.diagnostics {
        if !range(diagnostic.start, diagnostic.end)
            || !["error", "warning", "info"].contains(&diagnostic.severity.as_str())
            || diagnostic.message.len() > 8192
            || !valid_id(&diagnostic.code)
        {
            return Err("Invalid plugin diagnostic".into());
        }
    }
    let mut edits = response.edits.iter().collect::<Vec<_>>();
    edits.sort_by_key(|edit| edit.start);
    let mut end = 0;
    for edit in edits {
        if !range(edit.start, edit.end) || edit.start < end || edit.text.len() > 65536 {
            return Err("Invalid or overlapping plugin edits".into());
        }
        end = edit.end;
    }
    for completion in &response.completions {
        if completion.label.is_empty()
            || completion.label.len() > 256
            || completion.insert.len() > 65536
            || completion.detail.len() > 8192
        {
            return Err("Invalid plugin completion".into());
        }
    }
    for kind in &response.file_types {
        if ![
            "python",
            "json",
            "yaml",
            "toml",
            "javascript",
            "html",
            "css",
            "markdown",
            "shell",
            "sql",
            "text",
        ]
        .contains(&kind.language.to_ascii_lowercase().as_str())
        {
            return Err("Unsupported plugin file-type language; contribute a TextMate grammar for a new lexer".into());
        }
        if kind.extensions.is_empty()
            || kind.extensions.len() > 32
            || kind.language.len() > 64
            || kind
                .extensions
                .iter()
                .any(|extension| crate::manifest::normalize_extension(extension).is_err())
        {
            return Err("Invalid plugin file type".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn admission_rejects_imports_and_oversized_local_counts_before_translation() {
        let mut imports = b"\0asm\x01\0\0\0".to_vec();
        imports.extend_from_slice(&[2, 1, 1]);
        assert!(preflight(&imports).is_err());
        let mut locals = b"\0asm\x01\0\0\0".to_vec();
        // One body with a single declaration of 4097 i32 locals.
        locals.extend_from_slice(&[10, 8, 1, 6, 1, 0x81, 0x20, 0x7f, 0x0b, 0x0b]);
        assert!(preflight(&locals).is_err());
    }
    #[test]
    fn background_document_events_cannot_write_even_with_write_permission() {
        let manifest = RuntimeManifest {
            api_version: 1,
            module: "plugin.wasm".into(),
            capabilities: [Capability::DocumentWrite].into_iter().collect(),
        };
        let request = Request {
            api_version: 1,
            event: "document".into(),
            action: None,
            document: Some(Document {
                path: "x.py".into(),
                source: "x".into(),
                version: 1,
                offset: 0,
            }),
        };
        let response = Response {
            api_version: 1,
            edits: vec![Edit {
                start: 0,
                end: 1,
                text: "y".into(),
            }],
            ..Default::default()
        };
        assert!(validate_response(&response, &manifest, &request).is_err());
        let explicit = Request {
            event: "action".into(),
            action: Some("replace".into()),
            ..request
        };
        assert!(validate_response(&response, &manifest, &explicit).is_ok());
    }
    #[test]
    fn actual_guest_loop_is_stopped_by_fuel_before_invocation() {
        // (module (memory (export "memory") 1)
        //   (func (export "rustpy_alloc") (param i32) (result i32)
        //     (loop (br 0)) (i32.const 0))
        //   (func (export "rustpy_invoke") (param i32 i32) (result i64) (i64.const 0)))
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend_from_slice(&[
            1, 12, 2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e,
        ]);
        wasm.extend_from_slice(&[3, 3, 2, 0, 1, 5, 3, 1, 0, 1]);
        let mut exports = vec![3, 12];
        exports.extend_from_slice(b"rustpy_alloc");
        exports.extend_from_slice(&[0, 0, 13]);
        exports.extend_from_slice(b"rustpy_invoke");
        exports.extend_from_slice(&[0, 1, 6]);
        exports.extend_from_slice(b"memory");
        exports.extend_from_slice(&[2, 0]);
        wasm.extend_from_slice(&[7, exports.len() as u8]);
        wasm.extend(exports);
        wasm.extend_from_slice(&[
            10, 16, 2, 9, 0, 3, 0x40, 0x0c, 0, 0x0b, 0x41, 0, 0x0b, 4, 0, 0x42, 0, 0x0b,
        ]);
        let directory = tempfile::tempdir().unwrap();
        let manifest = RuntimeManifest {
            api_version: 1,
            module: "plugin.wasm".into(),
            capabilities: BTreeSet::new(),
        };
        std::fs::write(
            directory.path().join("plugin.json"),
            serde_json::json!({"runtime":manifest}).to_string(),
        )
        .unwrap();
        std::fs::write(directory.path().join("plugin.wasm"), wasm).unwrap();
        let grant = digest(directory.path(), &manifest).unwrap();
        let request = Request {
            api_version: 1,
            event: "activate".into(),
            action: None,
            document: None,
        };
        assert!(invoke(
            directory.path(),
            &manifest,
            &grant,
            request,
            &Default::default()
        )
        .unwrap_err()
        .to_lowercase()
        .contains("fuel"));
    }
    #[test]
    fn undeclared_capabilities_and_non_utf8_or_overlapping_ranges_are_rejected() {
        let mut manifest = RuntimeManifest {
            api_version: 1,
            module: "plugin.wasm".into(),
            capabilities: BTreeSet::new(),
        };
        let request = Request {
            api_version: 1,
            event: "action".into(),
            action: Some("edit".into()),
            document: Some(Document {
                path: "a.py".into(),
                source: "é = 1".into(),
                version: 4,
                offset: 0,
            }),
        };
        let mut response = Response {
            api_version: 1,
            edits: vec![Edit {
                start: 0,
                end: 1,
                text: "x".into(),
            }],
            ..Default::default()
        };
        assert!(validate_response(&response, &manifest, &request).is_err());
        manifest.capabilities.insert(Capability::DocumentWrite);
        assert!(validate_response(&response, &manifest, &request).is_err());
        response.edits[0].end = 2;
        assert!(validate_response(&response, &manifest, &request).is_ok());
        response.edits.push(Edit {
            start: 0,
            end: 2,
            text: "bad overlap".into(),
        });
        assert!(validate_response(&response, &manifest, &request).is_err());
    }
    #[test]
    fn consent_detects_module_and_permission_manifest_changes_without_execution() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = RuntimeManifest {
            api_version: 1,
            module: "plugin.wasm".into(),
            capabilities: BTreeSet::new(),
        };
        std::fs::write(
            directory.path().join("plugin.json"),
            serde_json::json!({"runtime":manifest}).to_string(),
        )
        .unwrap();
        std::fs::write(directory.path().join("plugin.wasm"), b"first module").unwrap();
        let grant = digest(directory.path(), &manifest).unwrap();
        std::fs::write(directory.path().join("plugin.wasm"), b"second module").unwrap();
        assert_ne!(grant, digest(directory.path(), &manifest).unwrap());
        std::fs::write(directory.path().join("plugin.json"),serde_json::json!({"runtime":{"api_version":1,"module":"plugin.wasm","capabilities":["document_write"]}}).to_string()).unwrap();
        assert!(digest(directory.path(), &manifest).is_err());
    }
}

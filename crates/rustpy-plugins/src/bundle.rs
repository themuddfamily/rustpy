//! Plugin bundles (`.rpplugin`): a whole plugin directory in one JSON file,
//! the archive format of the Marketplace. Like the settings bundle of
//! `rustpy-settings` it maps relative paths to file contents:
//!
//! ```json
//! {"format": "rustpy-plugin", "version": 1,
//!  "files": {"plugin.json": "{…}", "colors/dark.json": "{…}", "icon.png": {"base64": "iVBOR…"},
//!            "bin/run.sh": {"text": "#!/bin/sh\n…", "executable": true}}}
//! ```
//!
//! UTF-8 files are stored as strings, anything else as `{"base64": …}`;
//! executable files (owner-execute bit set when packed on Unix) as
//! `{"text": …, "executable": true}` or `{"base64": …, "executable": true}`.
//! Unpacked files get mode `0644`, executable ones `0755` (Unix). Unpacking
//! validates every path like a manifest path (relative, no `..`, portable
//! names, at most 16 directories deep) and caps sizes, so a hostile bundle
//! cannot write outside the plugin directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::contrib::{is_executable, validate_plugin_dir};
use crate::error::{io_err, PluginError};
use crate::manifest::MANIFEST_FILE;
use crate::paths::safe_relative_path;
use crate::registry::{create_plugin_file, plugin_files};

/// The `format` value of a plugin bundle.
pub const BUNDLE_FORMAT: &str = "rustpy-plugin";
/// The usual file extension of a plugin bundle.
pub const BUNDLE_EXTENSION: &str = "rpplugin";
const BUNDLE_VERSION: u64 = 1;

/// Largest bundle accepted (download or file).
pub(crate) const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
/// Largest total size of the unpacked files.
const MAX_UNPACKED_BYTES: usize = 64 * 1024 * 1024;
const MAX_BUNDLE_FILES: usize = 5_000;

#[derive(Serialize)]
#[serde(untagged)]
enum BundleFile {
    Text(String),
    Binary { base64: String },
    ExecutableText { text: String, executable: bool },
    ExecutableBinary { base64: String, executable: bool },
}

#[derive(Serialize)]
struct BundleDoc {
    format: &'static str,
    version: u64,
    files: BTreeMap<String, BundleFile>,
}

/// Packs a plugin directory (validated first) into bundle bytes. The output
/// is deterministic, so its SHA-256 can go into a catalog.
pub fn pack_bundle(dir: &Path) -> Result<Vec<u8>, PluginError> {
    validate_plugin_dir(dir)?;
    let mut files = BTreeMap::new();
    for file in plugin_files(dir)? {
        let bytes = fs::read(&file.path).map_err(io_err(&file.path))?;
        let executable = cfg!(unix) && is_executable(&file.path);
        let entry = match (String::from_utf8(bytes), executable) {
            (Ok(text), false) => BundleFile::Text(text),
            (Ok(text), true) => BundleFile::ExecutableText {
                text,
                executable: true,
            },
            (Err(e), false) => BundleFile::Binary {
                base64: base64_encode(e.as_bytes()),
            },
            (Err(e), true) => BundleFile::ExecutableBinary {
                base64: base64_encode(e.as_bytes()),
                executable: true,
            },
        };
        files.insert(file.rel, entry);
    }
    let doc = BundleDoc {
        format: BUNDLE_FORMAT,
        version: BUNDLE_VERSION,
        files,
    };
    let mut out = serde_json::to_vec_pretty(&doc).map_err(|source| PluginError::Json {
        what: "plugin bundle".into(),
        source,
    })?;
    out.push(b'\n');
    Ok(out)
}

/// Writes the files of bundle `bytes` into the (empty) directory `dest`.
pub(crate) fn unpack_bundle(bytes: &[u8], dest: &Path) -> Result<(), PluginError> {
    let bad = |message: String| PluginError::Bundle(message);
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(bad(format!(
            "it is larger than {} MiB",
            MAX_BUNDLE_BYTES / (1024 * 1024)
        )));
    }
    let doc: Value =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("it is not valid JSON: {e}")))?;
    if doc.get("format").and_then(Value::as_str) != Some(BUNDLE_FORMAT) {
        return Err(bad(format!(
            "it is not a RustPy plugin bundle (no \"format\": \"{BUNDLE_FORMAT}\")"
        )));
    }
    match doc.get("version").and_then(Value::as_u64) {
        Some(BUNDLE_VERSION) => {}
        Some(v) => {
            return Err(bad(format!(
                "bundle format version {v} is not supported (expected {BUNDLE_VERSION})"
            )))
        }
        None => return Err(bad("the bundle format `version` is missing".into())),
    }
    let Some(Value::Object(files)) = doc.get("files") else {
        return Err(bad(
            "`files` must be an object mapping paths to contents".into()
        ));
    };
    if files.len() > MAX_BUNDLE_FILES {
        return Err(bad(format!("it has more than {MAX_BUNDLE_FILES} files")));
    }

    // Decode and check everything before writing anything.
    let mut decoded = Vec::with_capacity(files.len());
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for (rel, content) in files {
        let path = safe_relative_path(rel).map_err(bad)?;
        // Paths differing only in case (or in `./`, `//`) would collide on some file systems.
        let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        if !seen.insert(key) {
            return Err(bad(format!("`{rel}` appears more than once")));
        }
        let expected = || {
            bad(format!(
                "`{rel}`: expected a string or {{\"text\" or \"base64\": \"…\", \"executable\": true|false}}"
            ))
        };
        let (data, executable) = match content {
            Value::String(text) => (text.as_bytes().to_vec(), false),
            Value::Object(obj) => {
                let executable = match obj.get("executable") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => {
                        return Err(bad(format!("`{rel}`: `executable` must be true or false")))
                    }
                };
                let data = match (obj.get("text"), obj.get("base64")) {
                    (Some(Value::String(text)), None) => text.as_bytes().to_vec(),
                    (None, Some(Value::String(b64))) => {
                        base64_decode(b64).map_err(|e| bad(format!("`{rel}`: {e}")))?
                    }
                    _ => return Err(expected()),
                };
                (data, executable)
            }
            _ => return Err(expected()),
        };
        total += data.len();
        if total > MAX_UNPACKED_BYTES {
            return Err(bad(format!(
                "its files are larger than {} MiB in total",
                MAX_UNPACKED_BYTES / (1024 * 1024)
            )));
        }
        decoded.push((path, data, executable));
    }
    if !decoded
        .iter()
        .any(|(path, _, _)| path == Path::new(MANIFEST_FILE))
    {
        return Err(bad(format!("it has no {MANIFEST_FILE} at its root")));
    }

    for (rel, data, executable) in decoded {
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        // `create_new` never follows or replaces anything already there.
        let mut file = create_plugin_file(&target, executable)?;
        std::io::Write::write_all(&mut file, &data).map_err(io_err(&target))?;
    }
    Ok(())
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(BASE64_ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard base64; padding is optional and whitespace is ignored.
pub(crate) fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let unpadded = digits
        .strip_suffix(b"==")
        .or_else(|| digits.strip_suffix(b"="))
        .unwrap_or(&digits);
    if unpadded.len() % 4 == 1 {
        return Err("invalid base64 (wrong length)".into());
    }
    let mut out = Vec::with_capacity(unpadded.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in unpadded {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("invalid base64 (unexpected character)".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, encoded) in cases {
            assert_eq!(base64_encode(raw), encoded);
            assert_eq!(base64_decode(encoded).unwrap(), raw);
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)).unwrap(), all);
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm8").unwrap(), b"fo");
        assert!(base64_decode("Zm9v!").is_err());
        assert!(base64_decode("Z").is_err());
    }

    #[test]
    fn hostile_bundles_are_rejected_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":"{}","../evil":"x"}}"#,
                "outside",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":"{}","/etc/x":"x"}}"#,
                "absolute",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":"{}","A.json":"x","a.json":"y"}}"#,
                "more than once",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"x.json":"{}"}}"#,
                "no plugin.json",
            ),
            (
                r#"{"format":"rustpy-settings","version":1,"files":{}}"#,
                "not a RustPy plugin bundle",
            ),
            (
                r#"{"format":"rustpy-plugin","version":2,"files":{}}"#,
                "version 2",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":{"base64":"!!"}}}"#,
                "base64",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":7}}"#,
                "expected a string",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":{"text":"{}","base64":"e30="}}}"#,
                "expected a string",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":{"text":"{}","executable":"yes"}}}"#,
                "`executable` must be true or false",
            ),
            (
                r#"{"format":"rustpy-plugin","version":1,"files":{"plugin.json":"{}","con.txt":"x"}}"#,
                "reserves for a device",
            ),
            ("PK\u{3}\u{4}", "not valid JSON"),
        ];
        for (json, needle) in cases {
            let err = unpack_bundle(json.as_bytes(), dir.path())
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{json}: {err}");
        }
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "nothing may be written"
        );
    }
}

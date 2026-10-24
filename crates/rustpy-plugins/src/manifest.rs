//! `plugin.json`: the plugin manifest, its serde types and static validation
//! (checks that need no file system access).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::split_words;
use crate::error::PluginError;
use crate::paths::{is_windows_device_name, read_text_limited, safe_relative_path};
use crate::version::Version;

/// The manifest's file name inside a plugin directory.
pub const MANIFEST_FILE: &str = "plugin.json";

/// Manifests larger than this are rejected.
pub(crate) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ID_LEN: usize = 128;
const MAX_NAME_CHARS: usize = 100;

/// Ids that would clash with files the registry keeps next to the plugin directories.
const RESERVED_IDS: &[&str] = &["state.json"];

/// Grammar files the IDE understands: TextMate JSON or property-list grammars.
pub(crate) const GRAMMAR_SUFFIXES: &[&str] =
    &[".tmlanguage.json", ".json", ".tmlanguage", ".plist"];

/// A plugin manifest (`plugin.json`).
///
/// Unknown fields are ignored (reported as warnings by
/// [`validate_plugin_dir`](crate::validate_plugin_dir)), so newer manifests
/// still load in older IDEs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginManifest {
    /// Reverse-DNS identifier, e.g. `com.example.solarized-theme`; also the
    /// name of the plugin's directory under `<config_dir>/plugins/`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Semver-ish version (`1.2.3`, `1.0.0-beta.1`).
    pub version: String,
    pub description: String,
    pub vendor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Oldest IDE version the plugin works with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ide_version: Option<String>,
    pub contributes: Contributes,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::runtime::RuntimeManifest>,
}

/// What a plugin adds to the IDE. Every path is relative to the plugin directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Contributes {
    /// JSON files in the IDE's live-template format: an array of
    /// `{abbreviation, description, template, contexts, …}` objects
    /// (`text` / `context` are accepted as aliases).
    pub live_templates: Vec<String>,
    /// JSON color schemes: `{name, base, colors, editor}`.
    pub color_schemes: Vec<String>,
    /// JSON keymaps: `{name, bindings: {actionId: ["Ctrl+Shift+P", …]}}`.
    pub keymaps: Vec<String>,
    pub textmate_grammars: Vec<GrammarDecl>,
    pub external_tools: Vec<ExternalToolDecl>,
    pub file_templates: Vec<FileTemplateDecl>,
    /// Declarative host-provided viewers; plugins execute no editor code.
    pub table_editors: Vec<TableEditorDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableEditorKind {
    Csv,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEditorDecl {
    pub name: String,
    pub extensions: Vec<String>,
    pub kind: TableEditorKind,
}

/// A TextMate grammar (`.tmLanguage.json` or `.tmLanguage` property list).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrammarDecl {
    /// Language name shown in the IDE, e.g. `Nim`.
    pub language: String,
    /// File extensions without the dot (a leading dot is tolerated), e.g. `["nim", "nims"]`.
    pub extensions: Vec<String>,
    pub path: String,
}

/// An entry for Tools › External Tools. It runs only when the user invokes it.
///
/// `program` and `arguments` are written like the IDE's own External Tools:
/// whitespace separates arguments, `"…"` and `'…'` group them, and there are
/// no escapes. The IDE runs the program directly, without a shell. They and
/// `working_dir` may use the IDE's macros ([`TOOL_MACROS`]: `$FilePath$`,
/// `$ProjectFileDir$`, …) plus `$PluginDir$`, the plugin's installation
/// directory; any other `$` is rejected. Shell operators (`;`, `|`, `&`,
/// `>`, `<`, `` ` ``, `(`, `)`, `$(…)`) are rejected too, so the command line
/// the IDE shows is the one command that runs, even when the program is a
/// shell. That makes the command visible, not harmless: the program
/// itself can do anything the user can (`sh -c '…'` or `python script.py` are
/// single commands), like any tool the user adds by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalToolDecl {
    pub name: String,
    /// A command on the `PATH` (`ruff`) or a macro path (`$PluginDir$/bin/tool`).
    pub program: String,
    pub arguments: String,
    /// Empty (the project directory) or a macro path such as `$ProjectFileDir$`.
    pub working_dir: String,
    /// Menu group; defaults to the plugin name.
    pub group: String,
    pub show_console: bool,
}

impl Default for ExternalToolDecl {
    fn default() -> Self {
        Self {
            name: String::new(),
            program: String::new(),
            arguments: String::new(),
            working_dir: String::new(),
            group: String::new(),
            show_console: true,
        }
    }
}

/// A template offered by File › New.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileTemplateDecl {
    pub name: String,
    /// Extension of created files, without the dot (`py`).
    pub extension: String,
    /// Text file with the template body.
    pub path: String,
}

impl PluginManifest {
    /// Parses and statically validates manifest JSON (the contents of `plugin.json`).
    pub fn from_json(text: &str) -> Result<PluginManifest, PluginError> {
        parse_manifest(text).map(|(manifest, _warnings)| manifest)
    }

    /// Reads, parses and statically validates `<dir>/plugin.json`.
    pub fn load(dir: &Path) -> Result<PluginManifest, PluginError> {
        let path = dir.join(MANIFEST_FILE);
        let text = read_text_limited(&path, MAX_MANIFEST_BYTES, MANIFEST_FILE).map_err(|e| {
            if !path.exists() {
                PluginError::invalid(format!("no {MANIFEST_FILE} in {}", dir.display()))
            } else {
                PluginError::invalid(e)
            }
        })?;
        Self::from_json(&text)
    }

    /// Every static validation problem, each prefixed with the offending
    /// field (`id: …`, `contributes.keymaps[0]: …`). Empty when valid.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.screenshots.len() > 8 {
            out.push("At most 8 plugin screenshots are supported".into());
        }
        for screenshot in &self.screenshots {
            if safe_relative_path(screenshot).is_err()
                || ![".png", ".jpg", ".jpeg", ".webp"]
                    .iter()
                    .any(|extension| screenshot.to_ascii_lowercase().ends_with(extension))
            {
                out.push("Plugin screenshots must be relative PNG/JPEG/WebP files".into());
            }
        }
        if let Some(runtime) = &self.runtime {
            out.extend(runtime.problems());
        }
        if let Err(e) = validate_plugin_id(&self.id) {
            out.push(format!("id: {e}"));
        }
        if self.name.trim().is_empty() {
            out.push("name: is required".into());
        } else if self.name.chars().count() > MAX_NAME_CHARS {
            out.push(format!("name: is longer than {MAX_NAME_CHARS} characters"));
        } else if self.name.chars().any(char::is_control) {
            out.push("name: contains control characters".into());
        }
        if self.version.is_empty() {
            out.push("version: is required".into());
        } else if let Err(e) = Version::parse(&self.version) {
            out.push(format!("version: {e}"));
        }
        if let Some(url) = &self.homepage {
            if let Err(e) = check_http_url(url) {
                out.push(format!("homepage: {e}"));
            }
        }
        if let Some(v) = &self.min_ide_version {
            if let Err(e) = Version::parse(v) {
                out.push(format!("min_ide_version: {e}"));
            }
        }
        let c = &self.contributes;
        for (field, paths) in [
            ("live_templates", &c.live_templates),
            ("color_schemes", &c.color_schemes),
            ("keymaps", &c.keymaps),
        ] {
            for (i, path) in paths.iter().enumerate() {
                if let Err(e) = safe_relative_path(path) {
                    out.push(format!("contributes.{field}[{i}]: {e}"));
                }
            }
        }
        for (i, editor) in c.table_editors.iter().enumerate() {
            let at = format!("contributes.table_editors[{i}]");
            check_label(&mut out, &at, "name", &editor.name);
            if editor.extensions.is_empty() || editor.extensions.len() > 16 {
                out.push(format!("{at}.extensions: expected 1..16 extensions"));
            }
            for extension in &editor.extensions {
                if let Err(error) = normalize_extension(extension) {
                    out.push(format!("{at}.extensions: {error}"));
                }
            }
        }
        for (i, g) in c.textmate_grammars.iter().enumerate() {
            let at = format!("contributes.textmate_grammars[{i}]");
            check_label(&mut out, &at, "language", &g.language);
            if g.extensions.is_empty() {
                out.push(format!(
                    "{at}.extensions: at least one file extension is required"
                ));
            }
            for ext in &g.extensions {
                if let Err(e) = normalize_extension(ext) {
                    out.push(format!("{at}.extensions: {e}"));
                }
            }
            match safe_relative_path(&g.path) {
                Err(e) => out.push(format!("{at}.path: {e}")),
                Ok(_) => {
                    let lower = g.path.to_ascii_lowercase();
                    if !GRAMMAR_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
                        out.push(format!(
                            "{at}.path: `{}` is not a TextMate grammar (use .tmLanguage.json or .tmLanguage)",
                            g.path
                        ));
                    }
                }
            }
        }
        for (i, t) in c.external_tools.iter().enumerate() {
            let at = format!("contributes.external_tools[{i}]");
            check_label(&mut out, &at, "name", &t.name);
            check_tool(&mut out, &at, t);
        }
        for (i, f) in c.file_templates.iter().enumerate() {
            let at = format!("contributes.file_templates[{i}]");
            check_label(&mut out, &at, "name", &f.name);
            if let Err(e) = normalize_extension(&f.extension) {
                out.push(format!("{at}.extension: {e}"));
            }
            if let Err(e) = safe_relative_path(&f.path) {
                out.push(format!("{at}.path: {e}"));
            }
        }
        out
    }

    /// The parsed [`version`](Self::version), if valid.
    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }

    /// Whether `ide_version` satisfies `min_ide_version` (true when unset;
    /// false when either version does not parse).
    pub fn is_compatible_with(&self, ide_version: &str) -> bool {
        is_compatible(self.min_ide_version.as_deref(), ide_version)
    }
}

pub(crate) fn is_compatible(min_ide_version: Option<&str>, ide_version: &str) -> bool {
    match min_ide_version {
        None => true,
        Some(min) => match (Version::parse(min), Version::parse(ide_version)) {
            (Ok(min), Ok(ide)) => ide >= min,
            _ => false,
        },
    }
}

/// Parses manifest JSON and validates it; also returns warnings for unknown fields.
pub(crate) fn parse_manifest(text: &str) -> Result<(PluginManifest, Vec<String>), PluginError> {
    let json_err = |source| PluginError::Json {
        what: MANIFEST_FILE.into(),
        source,
    };
    let value: Value = serde_json::from_str(text).map_err(json_err)?;
    if !value.is_object() {
        return Err(PluginError::invalid(format!(
            "{MANIFEST_FILE} must contain a JSON object"
        )));
    }
    // Deserializing from the text (not the `Value`) keeps line/column positions in errors.
    let manifest: PluginManifest = serde_json::from_str(text).map_err(json_err)?;
    let problems = manifest.problems();
    if !problems.is_empty() {
        return Err(PluginError::Invalid { problems });
    }
    Ok((manifest, unknown_manifest_fields(&value)))
}

/// Reports keys the manifest schema does not know (likely typos).
fn unknown_manifest_fields(value: &Value) -> Vec<String> {
    const TOP: &[&str] = &[
        "id",
        "name",
        "version",
        "description",
        "vendor",
        "homepage",
        "min_ide_version",
        "contributes",
        "runtime",
        "screenshots",
        "$schema",
    ];
    const CONTRIBUTES: &[&str] = &[
        "live_templates",
        "color_schemes",
        "keymaps",
        "textmate_grammars",
        "external_tools",
        "file_templates",
        "table_editors",
    ];
    const TABLE_EDITOR: &[&str] = &["name", "extensions", "kind"];
    const GRAMMAR: &[&str] = &["language", "extensions", "path"];
    const TOOL: &[&str] = &[
        "name",
        "program",
        "arguments",
        "working_dir",
        "group",
        "show_console",
    ];
    const FILE_TEMPLATE: &[&str] = &["name", "extension", "path"];

    let mut out = Vec::new();
    unknown_keys(value, TOP, MANIFEST_FILE, &mut out);
    if let Some(contributes) = value.get("contributes") {
        unknown_keys(contributes, CONTRIBUTES, "contributes", &mut out);
        for (field, known) in [
            ("table_editors", TABLE_EDITOR),
            ("textmate_grammars", GRAMMAR),
            ("external_tools", TOOL),
            ("file_templates", FILE_TEMPLATE),
        ] {
            if let Some(Value::Array(items)) = contributes.get(field) {
                for (i, item) in items.iter().enumerate() {
                    unknown_keys(item, known, &format!("contributes.{field}[{i}]"), &mut out);
                }
            }
        }
    }
    out
}

/// Pushes a warning for every key of `value` (an object) not in `known`.
pub(crate) fn unknown_keys(value: &Value, known: &[&str], at: &str, out: &mut Vec<String>) {
    let Value::Object(map) = value else { return };
    for key in map.keys() {
        if known.contains(&key.as_str()) {
            continue;
        }
        let hint = known
            .iter()
            .filter(|k| !k.starts_with('$'))
            .map(|k| (edit_distance(key, k), *k))
            .filter(|(d, _)| *d <= 2)
            .min()
            .map(|(_, k)| format!(" (did you mean `{k}`?)"))
            .unwrap_or_default();
        let prefix = if at.is_empty() {
            String::new()
        } else {
            format!("{at}: ")
        };
        out.push(format!("{prefix}unknown field `{key}` is ignored{hint}"));
    }
}

/// Levenshtein distance, for "did you mean" hints.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Checks a plugin id: lowercase reverse-DNS (`com.example.my-plugin`) that is
/// also safe as a directory name on every OS.
pub fn validate_plugin_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("is required".into());
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!("`{id}` is longer than {MAX_ID_LEN} characters"));
    }
    let example = "a reverse-DNS identifier such as `com.example.my-plugin`";
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return Err(format!(
            "`{id}` is not {example} (at least two dot-separated parts)"
        ));
    }
    if id.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("`{id}` must be lowercase"));
    }
    for segment in &segments {
        if segment.is_empty() {
            return Err(format!("`{id}` has an empty part; expected {example}"));
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err(format!(
                "`{id}` may only contain lowercase letters, digits, `-`, `_` and dots"
            ));
        }
        if segment.starts_with(['-', '_']) || segment.ends_with(['-', '_']) {
            return Err(format!(
                "`{id}`: parts must start and end with a letter or digit"
            ));
        }
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!("`{id}` must start with a letter"));
    }
    if RESERVED_IDS.contains(&id) || is_windows_device_name(id) {
        return Err(format!("`{id}` is reserved"));
    }
    Ok(())
}

/// Validates a file extension and returns it without a leading dot.
pub(crate) fn normalize_extension(raw: &str) -> Result<String, String> {
    let ext = raw.strip_prefix('.').unwrap_or(raw);
    if ext.is_empty() {
        return Err("the extension is empty".into());
    }
    if ext.len() > 32
        || !ext
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.'))
        || ext.starts_with('.')
        || ext.ends_with('.')
        || ext.contains("..")
    {
        return Err(format!(
            "`{raw}` is not a file extension (letters, digits, `_`, `-`, `+`, `.`)"
        ));
    }
    Ok(ext.to_string())
}

/// `http://` or `https://` URL with a host and no spaces.
pub(crate) fn check_http_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("`{url}` is not an http(s) URL"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("`{url}` is not a valid URL"));
    }
    Ok(())
}

fn check_label(out: &mut Vec<String>, at: &str, field: &str, value: &str) {
    if value.trim().is_empty() {
        out.push(format!("{at}.{field}: is required"));
    } else if value.chars().any(char::is_control) {
        out.push(format!("{at}.{field}: contains control characters"));
    } else if value.chars().count() > MAX_NAME_CHARS {
        out.push(format!(
            "{at}.{field}: is longer than {MAX_NAME_CHARS} characters"
        ));
    }
}

/// The macro that expands to the plugin's installation directory.
pub(crate) const PLUGIN_DIR_MACRO: &str = "$PluginDir$";

/// The macros a plugin tool may use (written `$Name$`): macros the IDE
/// expands in External Tools, plus `PluginDir`, which is replaced when the
/// plugin loads. Anything else between `$` signs is rejected: the IDE would
/// pass it on as written (`$HOME$`), and a shell, as a program, would read
/// `$NAME` as an environment variable.
pub const TOOL_MACROS: &[&str] = &[
    "FilePath",
    "FileDir",
    "FileName",
    "FileNameWithoutExtension",
    "ProjectFileDir",
    "LineNumber",
    "PyInterpreterDirectory",
    "PluginDir",
];

/// Shell operators a plugin tool must not use. The IDE runs a tool's program
/// directly, so for most programs they are just text, but a tool whose
/// program is a shell would run more (or something else) than it shows.
const SHELL_OPERATORS: &[char] = &[';', '&', '|', '`', '<', '>', '(', ')'];

/// Rejects shell operators, line breaks, `$` outside the [`TOOL_MACROS`] and
/// escaped macros (`\$FilePath$`).
fn check_command_text(text: &str) -> Result<(), String> {
    if text.chars().any(char::is_control) {
        return Err("must be a single line without control characters".into());
    }
    if let Some(c) = text.chars().find(|c| SHELL_OPERATORS.contains(c)) {
        return Err(format!(
            "`{c}` is not allowed: plugin tools run a single command without shell operators"
        ));
    }
    if text.contains("\\$") {
        return Err("`\\$` is not allowed: macros cannot be escaped".into());
    }
    let mut rest = text;
    while let Some(start) = rest.find('$') {
        let after = &rest[start + 1..];
        match after.find('$').map(|end| &after[..end]) {
            Some(name) if TOOL_MACROS.contains(&name) => rest = &after[name.len() + 1..],
            Some(name)
                if !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                let known: Vec<String> = TOOL_MACROS.iter().map(|m| format!("`${m}$`")).collect();
                return Err(format!(
                    "`${name}$` is not a macro; use {}",
                    known.join(", ")
                ));
            }
            _ => return Err("`$` may only appear in macros such as `$FilePath$`".into()),
        }
    }
    Ok(())
}

/// Splits `$Macro$rest` into (`$Macro$`, `rest`) when `text` starts with one of the [`TOOL_MACROS`].
fn leading_macro(text: &str) -> Option<(&str, &str)> {
    let after = text.strip_prefix('$')?;
    let end = after.find('$')?;
    TOOL_MACROS
        .contains(&&after[..end])
        .then(|| text.split_at(end + 2))
}

/// Checks the path after a macro (`$PluginDir$/bin/tool`): empty or `/relative/path`.
fn check_macro_path(text: &str) -> Result<(), String> {
    let Some((_, rest)) = leading_macro(text) else {
        return Err(format!(
            "`{text}` must start with a macro such as `$ProjectFileDir$`"
        ));
    };
    if rest.is_empty() {
        return Ok(());
    }
    match rest.strip_prefix('/') {
        Some(rel) => safe_relative_path(rel).map(|_| ()),
        None => Err(format!(
            "`{text}`: separate the macro from the path with `/`"
        )),
    }
}

/// Checks a tool's program the way the IDE reads it, after quote removal
/// (`$PluginDir$/'..'/x` is `$PluginDir$/../x`, and a backslash stays, which
/// Windows reads as a separator): one word, either a command name on the
/// `PATH` or a macro path that stays inside its directory.
fn check_program(program: &str) -> Result<(), String> {
    check_command_text(program)?;
    let words = split_words(program)?;
    let [word] = words.as_slice() else {
        return Err(format!(
            "`{program}` must be a single command without spaces; put options in `arguments`"
        ));
    };
    if word.is_empty() {
        Err("is required".into())
    } else if word.starts_with('$') {
        check_macro_path(word)
    } else if word.contains('$') {
        Err(format!(
            "`{program}`: a macro can only start the program (`$PluginDir$/<path>`)"
        ))
    } else if word.contains(['/', '\\']) {
        Err(format!(
            "`{program}` must be a command name on the PATH (e.g. `ruff`) or `$PluginDir$/<path>`"
        ))
    } else {
        Ok(())
    }
}

fn check_tool(out: &mut Vec<String>, at: &str, t: &ExternalToolDecl) {
    let program = t.program.trim();
    if program.is_empty() {
        out.push(format!("{at}.program: is required"));
    } else if let Err(e) = check_program(program) {
        out.push(format!("{at}.program: {e}"));
    }
    match check_command_text(&t.arguments).and_then(|_| split_words(&t.arguments)) {
        Err(e) => out.push(format!("{at}.arguments: {e}")),
        Ok(words) => {
            // Paths below the plugin directory must stay inside it. The words are
            // checked as the IDE reads them, after quote removal, so
            // `"$PluginDir$"/..` is caught like `$PluginDir$/..`.
            for word in words {
                let mut rest = word.as_str();
                while let Some(pos) = rest.find(PLUGIN_DIR_MACRO) {
                    let tail = &rest[pos..];
                    if let Err(e) = check_macro_path(tail) {
                        out.push(format!("{at}.arguments: {e}"));
                    }
                    rest = &tail[PLUGIN_DIR_MACRO.len()..];
                }
            }
        }
    }
    let wd = t.working_dir.trim();
    if !wd.is_empty() {
        if let Err(e) = check_command_text(wd).and_then(|_| check_macro_path(wd)) {
            out.push(format!("{at}.working_dir: {e}"));
        }
    }
    if t.group.chars().any(char::is_control) {
        out.push(format!("{at}.group: contains control characters"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids() {
        for ok in [
            "com.example.plugin",
            "org.rustpy.solarized-theme",
            "io.github.user_1.x2",
            "a.b",
        ] {
            assert!(validate_plugin_id(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "plugin",
            "Com.Example.X",
            "com..x",
            ".com.x",
            "com.x.",
            "com.-x",
            "com.x-",
            "1com.x",
            "com.ex ample",
            "com/x.y",
            "com.x\\y",
            "state.json",
            "con.example",
            "com.é.x",
        ] {
            assert!(
                validate_plugin_id(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(validate_plugin_id(&format!("com.{}", "x".repeat(130))).is_err());
    }

    #[test]
    fn command_text() {
        assert!(check_command_text("--fix $FilePath$ --line $LineNumber$").is_ok());
        assert!(check_command_text("'quoted arg' \"other\"").is_ok());
        let every_macro: Vec<String> = TOOL_MACROS.iter().map(|m| format!("${m}$")).collect();
        assert!(check_command_text(&every_macro.join(" ")).is_ok());
        assert!(check_command_text(&every_macro.concat()).is_ok());
        for bad in [
            "a; rm -rf ~",
            "a && b",
            "a | b",
            "`id`",
            "$(id)",
            "> out",
            "a\nb",
            "$HOME",
            "$$",
            "x $ y",
            // Unknown `$NAME$` tokens would reach the program as written.
            "$REVIEW_SECRET$ $HOME$",
            "$HOME$",
            "$filepath$",
            "$FilePath$$PATH$",
            // Macros cannot be escaped: the IDE would expand it after the `\`.
            "\\$FilePath$",
        ] {
            assert!(
                check_command_text(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(check_command_text("$HOME$")
            .unwrap_err()
            .contains("`$HOME$` is not a macro; use `$FilePath$`"));
    }

    #[test]
    fn macro_paths() {
        assert!(check_macro_path("$PluginDir$").is_ok());
        assert!(check_macro_path("$PluginDir$/bin/tool.sh").is_ok());
        assert!(check_macro_path("$ProjectFileDir$/src").is_ok());
        assert!(check_macro_path("$PluginDir$/../x").is_err());
        assert!(check_macro_path("$PluginDir$x").is_err());
        assert!(check_macro_path("/usr/bin").is_err());
        assert!(check_macro_path("$HOME$/bin").is_err());
    }

    #[test]
    fn plugin_dir_paths_in_arguments_are_checked_after_quote_removal() {
        let tool = |arguments: &str| ExternalToolDecl {
            name: "T".into(),
            program: "echo".into(),
            arguments: arguments.into(),
            ..ExternalToolDecl::default()
        };
        let problems = |arguments: &str| {
            let mut out = Vec::new();
            check_tool(&mut out, "t", &tool(arguments));
            out
        };
        for bad in [
            "\"$PluginDir$\"/../../..",
            "'$PluginDir$'/../x",
            "--config=\"$PluginDir$/..\"/x",
            "$PluginDir$/a\"/../..\"",
            "x \"$PluginDir$/ok\" '$PluginDir$'/..",
        ] {
            let found = problems(bad);
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("points outside the plugin directory")),
                "{bad:?}: {found:?}"
            );
        }
        for ok in [
            "\"$PluginDir$/ruff.toml\"",
            "--config=$PluginDir$/conf/ruff.toml $FilePath$",
            "'$PluginDir$/a b.toml'",
            "$PluginDir$",
        ] {
            assert_eq!(problems(ok), Vec::<String>::new(), "{ok:?}");
        }
        assert!(problems("'unclosed $FilePath$")[0].contains("not closed"));
    }

    /// The program is read as the IDE reads it: `'..'` is `..` once the
    /// quotes are gone, so it cannot name a file outside the plugin.
    #[test]
    fn programs_are_checked_after_quote_removal() {
        let problems = |program: &str| {
            let mut out = Vec::new();
            let tool = ExternalToolDecl {
                name: "T".into(),
                program: program.into(),
                ..ExternalToolDecl::default()
            };
            check_tool(&mut out, "t", &tool);
            out
        };
        for bad in [
            "$PluginDir$/'..'/'..'/esc.sh",
            "\"$PluginDir$/..\"/x",
            "'$PluginDir$'/../x",
            "$PluginDir$/bin/\"..\"/../x",
        ] {
            let found = problems(bad);
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("points outside the plugin directory")),
                "{bad:?}: {found:?}"
            );
        }
        for (bad, why) in [
            ("ruff check", "single command"),
            ("'ruff' check", "single command"),
            ("''", "is required"),
            ("'/usr/bin/ruff'", "command name on the PATH"),
            ("'$PluginDir$", "not closed"),
            // `x<plugin dir>` would be a path, not a command on the PATH.
            ("x$PluginDir$", "can only start the program"),
            ("ruff'$FileName$'", "can only start the program"),
        ] {
            let found = problems(bad);
            assert!(found.iter().any(|p| p.contains(why)), "{bad:?}: {found:?}");
        }
        for ok in [
            "ruff",
            "'ruff'",
            "$PluginDir$/bin/run.sh",
            "\"$PluginDir$\"/bin/'run'.sh",
            "\"$PluginDir$/my tool.sh\"",
        ] {
            assert_eq!(problems(ok), Vec::<String>::new(), "{ok:?}");
        }
    }

    /// The IDE reads a tool's command without escapes (Windows paths keep
    /// their backslashes), and Windows reads `\` as a directory separator. A
    /// check that removed backslashes like `sh` passed
    /// `$PluginDir$/x\..\..\esc.bat` as the file `x....esc.bat`, while Windows
    /// ran `<plugins>/../esc.bat`, outside the plugin.
    #[test]
    fn backslashes_in_plugin_paths_are_not_escapes() {
        let problems = |program: &str, arguments: &str| {
            let mut out = Vec::new();
            let tool = ExternalToolDecl {
                name: "T".into(),
                program: program.into(),
                arguments: arguments.into(),
                ..ExternalToolDecl::default()
            };
            check_tool(&mut out, "t", &tool);
            out
        };
        for (program, arguments) in [
            ("$PluginDir$/x\\..\\..\\esc.bat", ""),
            ("$PluginDir$/bin\\run.bat", ""),
            ("echo", "--config=$PluginDir$/x\\..\\..\\c.toml"),
            ("echo", "\"$PluginDir$/x\\..\\..\\c.toml\""),
        ] {
            let found = problems(program, arguments);
            assert!(
                found.iter().any(|p| p.contains("uses `\\`")),
                "{program:?} {arguments:?}: {found:?}"
            );
        }
        // Elsewhere a backslash is just a character (`\d+` for a regex).
        assert_eq!(problems("rg", "\\d+ $FilePath$"), Vec::<String>::new());
    }

    #[test]
    fn extensions_and_urls() {
        assert_eq!(normalize_extension(".py").unwrap(), "py");
        assert_eq!(normalize_extension("tar.gz").unwrap(), "tar.gz");
        for bad in ["", ".", "a/b", "a b", "..x", "x.", "a..b"] {
            assert!(normalize_extension(bad).is_err(), "{bad:?}");
        }
        assert!(check_http_url("https://example.com/x").is_ok());
        assert!(check_http_url("http://127.0.0.1:8080").is_ok());
        for bad in ["ftp://x", "https://", "https:// x", "example.com"] {
            assert!(check_http_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn did_you_mean() {
        assert_eq!(edit_distance("contribute", "contributes"), 1);
        let mut out = Vec::new();
        unknown_keys(
            &serde_json::json!({"contribute": {}, "id": "x"}),
            &["id", "contributes"],
            "plugin.json",
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                "plugin.json: unknown field `contribute` is ignored (did you mean `contributes`?)"
            ]
        );
    }
}

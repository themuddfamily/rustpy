//! Contribution files: reading, validating and resolving what a plugin adds
//! to the IDE into values the editor can use directly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustpy_highlight::{ColorScheme, HighlightTag, Rgb, TextStyle};
use rustpy_settings::ExternalTool;
use rustpy_templates::{LiveTemplate, TemplateContext, TemplateVariable};
use serde_json::{Map, Value};

use crate::command;
use crate::error::PluginError;
use crate::manifest::{
    normalize_extension, parse_manifest, unknown_keys, ExternalToolDecl, PluginManifest,
    MANIFEST_FILE, MAX_MANIFEST_BYTES, PLUGIN_DIR_MACRO,
};
use crate::paths::{absolute, existing_file_inside, read_text_limited};
use crate::sha256::sha256_hex;

/// Contribution files larger than this are rejected.
const MAX_CONTRIBUTION_BYTES: u64 = 8 * 1024 * 1024;
/// File templates larger than this are rejected.
const MAX_FILE_TEMPLATE_BYTES: u64 = 1024 * 1024;

/// What plugins add to the IDE, resolved and ready to use. Every item carries
/// the id of the plugin it comes from; paths are absolute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contributions {
    pub live_templates: Vec<LiveTemplateContribution>,
    pub color_schemes: Vec<ColorSchemeContribution>,
    pub keymaps: Vec<KeymapContribution>,
    pub textmate_grammars: Vec<GrammarContribution>,
    pub external_tools: Vec<ExternalToolContribution>,
    pub file_templates: Vec<FileTemplateContribution>,
    pub table_editors: Vec<TableEditorContribution>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableEditorContribution {
    pub plugin_id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub kind: crate::manifest::TableEditorKind,
}

/// One live-template file.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveTemplateContribution {
    pub plugin_id: String,
    /// The JSON file the templates come from.
    pub path: PathBuf,
    /// The templates; `group` defaults to the plugin name.
    pub templates: Vec<LiveTemplate>,
}

/// One color scheme.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSchemeContribution {
    pub plugin_id: String,
    pub path: PathBuf,
    /// The base scheme (`Dark` / `Light`) with the plugin's colors applied.
    pub scheme: ColorScheme,
}

/// One keymap, in the shape of `rustpy-ui/assets/keymaps/*.json`: layered over
/// the Default keymap, each listed action gets exactly the given shortcuts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapContribution {
    pub plugin_id: String,
    pub path: PathBuf,
    pub name: String,
    /// Action id → shortcuts such as `"Ctrl+Shift+P"`; an empty list unbinds the action.
    pub bindings: BTreeMap<String, Vec<String>>,
}

/// One TextMate grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrammarContribution {
    pub plugin_id: String,
    /// The grammar file (`.tmLanguage.json` or `.tmLanguage`).
    pub path: PathBuf,
    pub language: String,
    /// File extensions without the dot.
    pub extensions: Vec<String>,
    /// The grammar's `scopeName`, e.g. `source.nim`.
    pub scope_name: String,
    /// SHA-256 (hex) of the grammar file when the plugin was loaded. An
    /// update keeps the file's path, so this is what tells an editor that
    /// loaded the grammar to load it again.
    pub sha256: String,
}

/// One entry for Tools › External Tools.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalToolContribution {
    pub plugin_id: String,
    /// `$PluginDir$` is already replaced by the plugin directory — in
    /// `program` and `arguments` written in the IDE's External Tools syntax
    /// (quoted where the directory needs it, to fit where the macro stands:
    /// bare, inside `"…"` or inside `'…'`; `program` is one word), verbatim
    /// in `working_dir`. The IDE's own macros (`$FilePath$`, …) are left for it.
    pub tool: ExternalTool,
}

/// One File › New template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTemplateContribution {
    pub plugin_id: String,
    pub path: PathBuf,
    pub name: String,
    /// Extension of created files, without the dot.
    pub extension: String,
    /// The template body.
    pub text: String,
}

impl FileTemplateContribution {
    /// The body with `${KEY}` placeholders replaced (`[("NAME", "module")]`);
    /// unknown placeholders are kept as written.
    pub fn render(&self, vars: &[(&str, &str)]) -> String {
        let mut out = self.text.clone();
        for (key, value) in vars {
            out = out.replace(&format!("${{{key}}}"), value);
        }
        out
    }
}

impl Contributions {
    /// True when nothing is contributed.
    pub fn is_empty(&self) -> bool {
        self.live_templates.is_empty()
            && self.color_schemes.is_empty()
            && self.keymaps.is_empty()
            && self.textmate_grammars.is_empty()
            && self.external_tools.is_empty()
            && self.file_templates.is_empty()
            && self.table_editors.is_empty()
    }

    /// Every contributed live template, in plugin order.
    pub fn all_live_templates(&self) -> Vec<LiveTemplate> {
        self.live_templates
            .iter()
            .flat_map(|f| f.templates.iter().cloned())
            .collect()
    }

    /// Every contributed external tool, in plugin order.
    pub fn all_external_tools(&self) -> Vec<ExternalTool> {
        self.external_tools.iter().map(|t| t.tool.clone()).collect()
    }

    /// The first contributed color scheme called `name`.
    pub fn color_scheme(&self, name: &str) -> Option<&ColorScheme> {
        self.color_schemes
            .iter()
            .map(|c| &c.scheme)
            .find(|s| s.name == name)
    }

    /// The first contributed keymap called `name`.
    pub fn keymap(&self, name: &str) -> Option<&KeymapContribution> {
        self.keymaps.iter().find(|k| k.name == name)
    }

    /// The first grammar registered for `extension` (case-insensitive, leading dot optional).
    pub fn grammar_for_extension(&self, extension: &str) -> Option<&GrammarContribution> {
        let ext = extension.strip_prefix('.').unwrap_or(extension);
        self.textmate_grammars
            .iter()
            .find(|g| g.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }

    pub fn append(&mut self, other: &Contributions) {
        self.table_editors
            .extend(other.table_editors.iter().cloned());
        self.live_templates
            .extend(other.live_templates.iter().cloned());
        self.color_schemes
            .extend(other.color_schemes.iter().cloned());
        self.keymaps.extend(other.keymaps.iter().cloned());
        self.textmate_grammars
            .extend(other.textmate_grammars.iter().cloned());
        self.external_tools
            .extend(other.external_tools.iter().cloned());
        self.file_templates
            .extend(other.file_templates.iter().cloned());
    }
}

/// A plugin directory that passed every check.
#[derive(Clone, Debug)]
pub struct ValidatedPlugin {
    pub manifest: PluginManifest,
    /// The plugin directory (absolute).
    pub dir: PathBuf,
    /// What the plugin contributes, resolved against `dir`.
    pub contributions: Contributions,
    /// Non-fatal findings: unknown fields, unknown highlight tags, …
    pub warnings: Vec<String>,
}

/// Fully validates a plugin directory: the manifest, then every contribution
/// file (exists inside the directory, parses, has valid values).
pub fn validate_plugin_dir(dir: &Path) -> Result<ValidatedPlugin, PluginError> {
    let check = check_plugin_dir(dir);
    match check.manifest {
        Some(manifest) if check.errors.is_empty() => Ok(ValidatedPlugin {
            manifest,
            dir: check.dir,
            contributions: check.contributions,
            warnings: check.warnings,
        }),
        _ => Err(PluginError::Invalid {
            problems: check.errors,
        }),
    }
}

/// The outcome of checking a plugin directory, failures included.
pub(crate) struct DirCheck {
    /// The manifest as far as it could be read; possibly invalid when `errors` is not empty.
    pub manifest: Option<PluginManifest>,
    pub dir: PathBuf,
    pub contributions: Contributions,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn check_plugin_dir(dir: &Path) -> DirCheck {
    let dir = absolute(dir);
    let mut check = DirCheck {
        manifest: None,
        dir: dir.clone(),
        contributions: Contributions::default(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    let text = match existing_file_inside(&dir, MANIFEST_FILE)
        .and_then(|path| read_text_limited(&path, MAX_MANIFEST_BYTES, MANIFEST_FILE))
    {
        Ok(text) => text,
        Err(e) => {
            check.errors.push(e);
            return check;
        }
    };
    match parse_manifest(&text) {
        Ok((manifest, warnings)) => {
            check.warnings = warnings;
            check.contributions = resolve(&manifest, &dir, &mut check.errors, &mut check.warnings);
            check.manifest = Some(manifest);
        }
        Err(PluginError::Invalid { problems }) => {
            // Keep what is readable (name, version) for display.
            check.manifest = serde_json::from_str::<PluginManifest>(&text).ok();
            check.errors = problems
                .into_iter()
                .map(|p| format!("{MANIFEST_FILE}: {p}"))
                .collect();
        }
        Err(other) => check.errors.push(other.to_string()),
    }
    check
}

/// Errors and warnings found in one contribution, before prefixing.
#[derive(Default)]
struct Notes {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Notes {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Moves the notes into the plugin's lists under `at`; true when there were no errors.
    fn finish(self, at: &str, errors: &mut Vec<String>, warnings: &mut Vec<String>) -> bool {
        let ok = self.errors.is_empty();
        errors.extend(self.errors.into_iter().map(|e| format!("{at}: {e}")));
        warnings.extend(self.warnings.into_iter().map(|w| format!("{at}: {w}")));
        ok
    }
}

fn load_json(dir: &Path, rel: &str) -> Result<(PathBuf, Value), String> {
    let path = existing_file_inside(dir, rel)?;
    let text = read_text_limited(&path, MAX_CONTRIBUTION_BYTES, rel)?;
    let value =
        serde_json::from_str(&text).map_err(|e| format!("`{rel}` is not valid JSON: {e}"))?;
    Ok((path, value))
}

/// Resolves every contribution of a statically valid manifest.
fn resolve(
    manifest: &PluginManifest,
    dir: &Path,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Contributions {
    let mut out = Contributions::default();
    for screenshot in &manifest.screenshots {
        if let Err(error) = existing_file_inside(dir, screenshot).and_then(|path| {
            std::fs::metadata(path)
                .map_err(|error| error.to_string())
                .and_then(|metadata| {
                    if metadata.len() > 8 * 1024 * 1024 {
                        Err("Screenshot exceeds 8 MiB".into())
                    } else {
                        Ok(())
                    }
                })
        }) {
            errors.push(format!("screenshots: {error}"));
        }
    }
    if let Some(runtime) = &manifest.runtime {
        if let Err(error) = crate::runtime::module_bytes(dir, runtime) {
            errors.push(format!("runtime.module: {error}"));
        }
    }
    let plugin_id = &manifest.id;
    let c = &manifest.contributes;

    for decl in &c.table_editors {
        out.table_editors.push(TableEditorContribution {
            plugin_id: plugin_id.clone(),
            name: decl.name.clone(),
            kind: decl.kind.clone(),
            extensions: decl
                .extensions
                .iter()
                .map(|e| normalize_extension(e).expect("validated manifest extension"))
                .collect(),
        });
    }

    for (i, rel) in c.live_templates.iter().enumerate() {
        let at = format!("contributes.live_templates[{i}]");
        let mut notes = Notes::default();
        let loaded = load_json(dir, rel).map_err(|e| notes.error(e)).ok();
        let templates = loaded
            .as_ref()
            .map(|(_, value)| parse_live_templates(value, manifest.name.trim(), &mut notes));
        if notes.finish(&at, errors, warnings) {
            if let (Some((path, _)), Some(templates)) = (loaded, templates) {
                out.live_templates.push(LiveTemplateContribution {
                    plugin_id: plugin_id.clone(),
                    path,
                    templates,
                });
            }
        }
    }

    for (i, rel) in c.color_schemes.iter().enumerate() {
        let at = format!("contributes.color_schemes[{i}]");
        let mut notes = Notes::default();
        let loaded = load_json(dir, rel).map_err(|e| notes.error(e)).ok();
        let scheme = loaded
            .as_ref()
            .and_then(|(_, value)| parse_color_scheme(value, &mut notes));
        if notes.finish(&at, errors, warnings) {
            if let (Some((path, _)), Some(scheme)) = (loaded, scheme) {
                out.color_schemes.push(ColorSchemeContribution {
                    plugin_id: plugin_id.clone(),
                    path,
                    scheme,
                });
            }
        }
    }

    for (i, rel) in c.keymaps.iter().enumerate() {
        let at = format!("contributes.keymaps[{i}]");
        let mut notes = Notes::default();
        let loaded = load_json(dir, rel).map_err(|e| notes.error(e)).ok();
        let keymap = loaded
            .as_ref()
            .and_then(|(_, value)| parse_keymap(value, &mut notes));
        if notes.finish(&at, errors, warnings) {
            if let (Some((path, _)), Some((name, bindings))) = (loaded, keymap) {
                out.keymaps.push(KeymapContribution {
                    plugin_id: plugin_id.clone(),
                    path,
                    name,
                    bindings,
                });
            }
        }
    }

    for (i, decl) in c.textmate_grammars.iter().enumerate() {
        let at = format!("contributes.textmate_grammars[{i}]");
        let resolved = existing_file_inside(dir, &decl.path).and_then(|path| {
            let text = read_text_limited(&path, MAX_CONTRIBUTION_BYTES, &decl.path)?;
            let scope = grammar_scope(&text, &decl.path)?;
            Ok((path, scope, sha256_hex(text.as_bytes())))
        });
        match resolved {
            Ok((path, scope_name, sha256)) => out.textmate_grammars.push(GrammarContribution {
                plugin_id: plugin_id.clone(),
                path,
                language: decl.language.trim().to_string(),
                extensions: decl
                    .extensions
                    .iter()
                    .filter_map(|e| normalize_extension(e).ok())
                    .collect(),
                scope_name,
                sha256,
            }),
            Err(e) => errors.push(format!("{at}: {e}")),
        }
    }

    for (i, decl) in c.external_tools.iter().enumerate() {
        match resolve_tool(decl, dir, manifest.name.trim()) {
            Ok((tool, program_file)) => {
                if let Some(file) = program_file.filter(|f| !is_executable(f)) {
                    let rel = file.strip_prefix(dir).unwrap_or(&file).to_string_lossy();
                    warnings.push(format!(
                        "contributes.external_tools[{i}]: `{rel}` is not executable (installing the plugin makes it executable)"
                    ));
                }
                out.external_tools.push(ExternalToolContribution {
                    plugin_id: plugin_id.clone(),
                    tool,
                });
            }
            Err(e) => errors.push(format!("contributes.external_tools[{i}]: {e}")),
        }
    }

    for (i, decl) in c.file_templates.iter().enumerate() {
        let resolved = existing_file_inside(dir, &decl.path).and_then(|path| {
            let text = read_text_limited(&path, MAX_FILE_TEMPLATE_BYTES, &decl.path)?;
            Ok(FileTemplateContribution {
                plugin_id: plugin_id.clone(),
                path,
                name: decl.name.trim().to_string(),
                extension: normalize_extension(&decl.extension)?,
                text,
            })
        });
        match resolved {
            Ok(template) => out.file_templates.push(template),
            Err(e) => errors.push(format!("contributes.file_templates[{i}]: {e}")),
        }
    }

    warn_duplicates(
        "color scheme",
        out.color_schemes.iter().map(|c| c.scheme.name.as_str()),
        warnings,
    );
    warn_duplicates(
        "keymap",
        out.keymaps.iter().map(|k| k.name.as_str()),
        warnings,
    );
    out
}

fn warn_duplicates<'a>(
    what: &str,
    names: impl Iterator<Item = &'a str>,
    warnings: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            warnings.push(format!("the {what} `{name}` is contributed more than once"));
        }
    }
}

// ----- field helpers ----------------------------------------------------------

/// A string field: `Ok(None)` when absent or `null`.
fn opt_str<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key}: must be a string")),
    }
}

/// A boolean field: `Ok(None)` when absent or `null`.
fn opt_bool(obj: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{key}: must be true or false")),
    }
}

/// A required, non-blank string field (trimmed).
fn required_str(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    match opt_str(obj, key)? {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(format!("{key}: is required")),
    }
}

// ----- live templates -----------------------------------------------------------

const TEMPLATE_KEYS: &[&str] = &[
    "abbreviation",
    "description",
    "template",
    "text",
    "contexts",
    "context",
    "group",
    "variables",
    "reformat",
];

/// Template contexts by (case-insensitive) name.
fn parse_context(name: &str) -> Option<TemplateContext> {
    let squashed: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    Some(match squashed.as_str() {
        "any" | "everywhere" => TemplateContext::Any,
        "statement" => TemplateContext::Statement,
        "expression" => TemplateContext::Expression,
        "classbody" | "class" => TemplateContext::ClassBody,
        "module" | "toplevel" => TemplateContext::Module,
        "comment" => TemplateContext::Comment,
        _ => return None,
    })
}

/// Parses a live-template file: a JSON array in the IDE's format
/// (`template` / `contexts`), also accepting `text` / `context` (a string or
/// an array of context names).
fn parse_live_templates(
    value: &Value,
    default_group: &str,
    notes: &mut Notes,
) -> Vec<LiveTemplate> {
    let Value::Array(items) = value else {
        notes.error("expected a JSON array of live templates");
        return Vec::new();
    };
    let mut out: Vec<LiveTemplate> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let at = format!("[{i}]");
        let Value::Object(obj) = item else {
            notes.error(format!("{at}: expected an object"));
            continue;
        };
        unknown_keys(item, TEMPLATE_KEYS, &at, &mut notes.warnings);
        match parse_live_template(obj, default_group) {
            Ok((template, warnings)) => {
                notes
                    .warnings
                    .extend(warnings.into_iter().map(|w| format!("{at}: {w}")));
                if out.iter().any(|t| t.abbreviation == template.abbreviation) {
                    notes.warn(format!(
                        "{at}: the abbreviation `{}` is defined more than once",
                        template.abbreviation
                    ));
                }
                out.push(template);
            }
            Err(problems) => notes
                .errors
                .extend(problems.into_iter().map(|p| format!("{at}.{p}"))),
        }
    }
    out
}

fn parse_live_template(
    obj: &Map<String, Value>,
    default_group: &str,
) -> Result<(LiveTemplate, Vec<String>), Vec<String>> {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();

    let abbreviation = required_str(obj, "abbreviation").unwrap_or_else(|e| {
        problems.push(e);
        String::new()
    });
    if abbreviation.contains(char::is_whitespace) {
        problems.push(format!(
            "abbreviation: `{abbreviation}` must not contain spaces"
        ));
    }
    let description = opt_str(obj, "description").unwrap_or_else(|e| {
        problems.push(e);
        None
    });

    let body_key = if obj.contains_key("template") {
        "template"
    } else {
        "text"
    };
    if obj.contains_key("template") && obj.contains_key("text") {
        warnings.push("both `template` and `text` are set; `text` is ignored".to_string());
    }
    let body = match opt_str(obj, body_key) {
        Ok(Some(s)) if !s.is_empty() => s.to_string(),
        Ok(_) => {
            problems.push("template: is required (the text to insert)".into());
            String::new()
        }
        Err(e) => {
            problems.push(e);
            String::new()
        }
    };

    let contexts_key = if obj.contains_key("contexts") {
        "contexts"
    } else {
        "context"
    };
    let names: Vec<&str> = match obj.get(contexts_key) {
        None | Some(Value::Null) => vec!["Any"],
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => {
            let names: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            if names.len() != items.len() {
                problems.push(format!("{contexts_key}: must be a list of context names"));
            } else if names.is_empty() {
                problems.push(format!("{contexts_key}: must not be empty"));
            }
            names
        }
        Some(_) => {
            problems.push(format!(
                "{contexts_key}: must be a context name or a list of them"
            ));
            Vec::new()
        }
    };
    let mut contexts = Vec::new();
    for name in names {
        match parse_context(name) {
            Some(c) if !contexts.contains(&c) => contexts.push(c),
            Some(_) => {}
            None => problems.push(format!(
                "{contexts_key}: `{name}` is not a template context (use Any, Statement, Expression, ClassBody, Module or Comment)"
            )),
        }
    }

    let group = match opt_str(obj, "group") {
        Ok(Some(g)) if !g.trim().is_empty() => g.trim().to_string(),
        Ok(_) => default_group.to_string(),
        Err(e) => {
            problems.push(e);
            String::new()
        }
    };
    let variables: Vec<TemplateVariable> = match obj.get("variables") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => match serde_json::from_value::<Vec<TemplateVariable>>(v.clone()) {
            Ok(vars) => {
                if vars.iter().any(|var| var.name.trim().is_empty()) {
                    problems.push("variables: every variable needs a `name`".into());
                }
                vars
            }
            Err(e) => {
                problems.push(format!("variables: {e}"));
                Vec::new()
            }
        },
    };
    let reformat = opt_bool(obj, "reformat").unwrap_or_else(|e| {
        problems.push(e);
        None
    });

    if !problems.is_empty() {
        return Err(problems);
    }
    let mut template = LiveTemplate::new(
        &abbreviation,
        description.unwrap_or(""),
        &body,
        &contexts,
        &group,
    )
    .with_variables(variables);
    template.reformat = reformat.unwrap_or(false);
    Ok((template, warnings))
}

// ----- color schemes ------------------------------------------------------------

const SCHEME_KEYS: &[&str] = &["name", "base", "dark", "colors", "editor", "$schema"];

/// Every highlight tag the built-in schemes style.
fn known_tags() -> &'static [HighlightTag] {
    static TAGS: OnceLock<Vec<HighlightTag>> = OnceLock::new();
    TAGS.get_or_init(|| {
        let mut tags: Vec<HighlightTag> = Vec::new();
        for scheme in [
            ColorScheme::dark(),
            ColorScheme::light(),
            ColorScheme::high_contrast(),
        ] {
            tags.extend(scheme.styles.into_keys());
        }
        tags.sort();
        tags.dedup();
        tags
    })
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A highlight tag by name: `FunctionDefinition`, `function_definition`, `function-definition`, …
fn lookup_tag(name: &str) -> Option<HighlightTag> {
    if let Ok(tag) = serde_json::from_value::<HighlightTag>(Value::String(name.to_string())) {
        return Some(tag);
    }
    let wanted = squash(name);
    known_tags()
        .iter()
        .copied()
        .find(|tag| squash(&format!("{tag:?}")) == wanted)
}

/// `#rrggbb` or `#rgb`.
fn parse_hex_color(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(Rgb::hex),
        3 => {
            let d: Vec<u8> = hex
                .bytes()
                .map(|b| (b as char).to_digit(16).unwrap_or(0) as u8)
                .collect();
            Some(Rgb(d[0] * 17, d[1] * 17, d[2] * 17))
        }
        _ => None,
    }
}

fn parse_color(value: &Value) -> Result<Rgb, String> {
    let s = value
        .as_str()
        .ok_or("expected a color such as \"#268bd2\"")?;
    parse_hex_color(s).ok_or_else(|| format!("`{s}` is not a #rrggbb color"))
}

/// A tag's style: `"#rrggbb"` (foreground) or `{fg, bg, bold, italic, underline}`.
/// Unspecified attributes keep the base scheme's value; `null` clears a color.
fn parse_style(value: &Value, base: TextStyle, at: &str, notes: &mut Notes) -> Option<TextStyle> {
    match value {
        Value::String(_) => match parse_color(value) {
            Ok(fg) => Some(TextStyle {
                fg: Some(fg),
                ..base
            }),
            Err(e) => {
                notes.error(format!("{at}: {e}"));
                None
            }
        },
        Value::Object(obj) => {
            let mut style = base;
            let mut ok = true;
            for (key, v) in obj {
                match key.as_str() {
                    "fg" | "bg" => {
                        let color = if v.is_null() {
                            Ok(None)
                        } else {
                            parse_color(v).map(Some)
                        };
                        match color {
                            Ok(c) if key == "fg" => style.fg = c,
                            Ok(c) => style.bg = c,
                            Err(e) => {
                                notes.error(format!("{at}.{key}: {e}"));
                                ok = false;
                            }
                        }
                    }
                    "bold" | "italic" | "underline" => match v.as_bool() {
                        Some(b) => match key.as_str() {
                            "bold" => style.bold = b,
                            "italic" => style.italic = b,
                            _ => style.underline = b,
                        },
                        None => {
                            notes.error(format!("{at}.{key}: must be true or false"));
                            ok = false;
                        }
                    },
                    _ => notes.warn(format!("{at}: unknown style attribute `{key}` is ignored")),
                }
            }
            ok.then_some(style)
        }
        _ => {
            notes.error(format!(
                "{at}: expected \"#rrggbb\" or an object with fg, bg, bold, italic, underline"
            ));
            None
        }
    }
}

/// Parses `{name, base, dark?, colors?, editor?}` into a full scheme.
fn parse_color_scheme(value: &Value, notes: &mut Notes) -> Option<ColorScheme> {
    let Value::Object(obj) = value else {
        notes.error("expected a JSON object");
        return None;
    };
    unknown_keys(value, SCHEME_KEYS, "", &mut notes.warnings);
    let name = required_str(obj, "name").unwrap_or_else(|e| {
        notes.error(e);
        String::new()
    });
    if !name.is_empty()
        && (ColorScheme::by_name(&name).is_some()
            || ColorScheme::builtin_names()
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&name)))
    {
        notes.error(format!(
            "name: `{name}` is a built-in scheme; choose another name"
        ));
    }
    let base = match opt_str(obj, "base") {
        Ok(Some(b)) => ColorScheme::by_name(b).or_else(|| {
            notes.error(format!(
                "base: `{b}` is not a built-in scheme (use \"Dark\" or \"Light\")"
            ));
            None
        }),
        Ok(None) => {
            notes.error("base: is required (\"Dark\" or \"Light\")");
            None
        }
        Err(e) => {
            notes.error(e);
            None
        }
    };
    // Keep validating against Dark when the base is unusable, to report every problem at once.
    let mut scheme = base.unwrap_or_else(ColorScheme::dark);
    scheme.name = name;
    match opt_bool(obj, "dark") {
        Ok(Some(dark)) => scheme.dark = dark,
        Ok(None) => {}
        Err(e) => notes.error(e),
    }

    match obj.get("colors") {
        None | Some(Value::Null) => {}
        Some(Value::Object(colors)) => {
            for (key, v) in colors {
                match lookup_tag(key) {
                    Some(tag) => {
                        if let Some(style) =
                            parse_style(v, scheme.style(tag), &format!("colors.{key}"), notes)
                        {
                            scheme.styles.insert(tag, style);
                        }
                    }
                    None => notes.warn(format!("colors: unknown highlight tag `{key}` is ignored")),
                }
            }
        }
        Some(_) => notes.error("colors: must be an object mapping highlight tags to colors"),
    }

    match obj.get("editor") {
        None | Some(Value::Null) => {}
        Some(Value::Object(editor)) => {
            // Overlay onto the base's editor colors through serde, so every
            // `EditorColors` field is addressable by its name.
            let mut fields = match serde_json::to_value(&scheme.editor) {
                Ok(Value::Object(fields)) => fields,
                _ => Map::new(),
            };
            for (key, v) in editor {
                if !fields.contains_key(key) {
                    notes.warn(format!("editor: unknown color `{key}` is ignored"));
                    continue;
                }
                match parse_color(v)
                    .and_then(|c| serde_json::to_value(c).map_err(|e| e.to_string()))
                {
                    Ok(color) => {
                        fields.insert(key.clone(), color);
                    }
                    Err(e) => notes.error(format!("editor.{key}: {e}")),
                }
            }
            match serde_json::from_value(Value::Object(fields)) {
                Ok(colors) => scheme.editor = colors,
                Err(e) => notes.error(format!("editor: {e}")),
            }
        }
        Some(_) => notes.error("editor: must be an object mapping editor color names to colors"),
    }
    notes.errors.is_empty().then_some(scheme)
}

// ----- keymaps --------------------------------------------------------------------

const KEYMAP_KEYS: &[&str] = &["name", "bindings", "$schema"];

fn is_modifier(part: &str) -> bool {
    matches!(
        part.to_ascii_lowercase().as_str(),
        "ctrl" | "control" | "cmd" | "meta" | "alt" | "option" | "shift"
    )
}

/// Checks a shortcut such as `Ctrl+Shift+P` (modifiers plus exactly one key).
fn check_shortcut(shortcut: &str) -> Result<(), String> {
    if shortcut.trim().is_empty() {
        return Err("empty shortcut".into());
    }
    if shortcut.contains(char::is_whitespace) {
        return Err(format!(
            "`{shortcut}`: shortcuts are single chords such as `Ctrl+Shift+P` (no spaces or key sequences)"
        ));
    }
    let parts: Vec<&str> = shortcut.split('+').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!(
            "`{shortcut}` is not a shortcut such as `Ctrl+Shift+P` (write `Plus` for the + key)"
        ));
    }
    match parts.iter().filter(|p| !is_modifier(p)).count() {
        1 => Ok(()),
        0 => Err(format!("`{shortcut}` has modifiers but no key")),
        _ => Err(format!("`{shortcut}` names more than one key")),
    }
}

/// An action id such as `GotoAction`, `Vcs.Push` or `$Copy` (the clipboard and
/// undo actions are written with a leading `$`).
fn is_action_id(id: &str) -> bool {
    let name = id.strip_prefix('$').unwrap_or(id);
    !name.is_empty()
        && id.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Parses `{name, bindings: {actionId: ["Ctrl+Shift+P", …]}}` (a single string is accepted for one shortcut).
fn parse_keymap(
    value: &Value,
    notes: &mut Notes,
) -> Option<(String, BTreeMap<String, Vec<String>>)> {
    let Value::Object(obj) = value else {
        notes.error("expected a JSON object");
        return None;
    };
    unknown_keys(value, KEYMAP_KEYS, "", &mut notes.warnings);
    let name = required_str(obj, "name").unwrap_or_else(|e| {
        notes.error(e);
        String::new()
    });
    let mut bindings = BTreeMap::new();
    match obj.get("bindings") {
        Some(Value::Object(map)) => {
            for (action, v) in map {
                if !is_action_id(action) {
                    notes.error(format!("bindings: `{action}` is not an action id"));
                    continue;
                }
                let shortcuts: Option<Vec<&str>> = match v {
                    Value::String(s) => Some(vec![s.as_str()]),
                    Value::Array(items) => items.iter().map(Value::as_str).collect(),
                    _ => None,
                };
                let Some(shortcuts) = shortcuts else {
                    notes.error(format!("bindings.{action}: expected a list of shortcuts such as [\"Ctrl+Shift+P\"]"));
                    continue;
                };
                for s in &shortcuts {
                    if let Err(e) = check_shortcut(s) {
                        notes.error(format!("bindings.{action}: {e}"));
                    }
                }
                bindings.insert(
                    action.clone(),
                    shortcuts.into_iter().map(str::to_string).collect(),
                );
            }
        }
        None | Some(Value::Null) => {
            notes.error("bindings: is required (an object mapping action ids to shortcuts)")
        }
        Some(_) => notes.error("bindings: must be an object mapping action ids to shortcuts"),
    }
    notes.errors.is_empty().then_some((name, bindings))
}

// ----- TextMate grammars --------------------------------------------------------------

/// A grammar's `scopeName`, checking the text of file `rel` is a TextMate grammar.
fn grammar_scope(text: &str, rel: &str) -> Result<String, String> {
    let not_grammar = |what: &str| format!("`{rel}` is not a TextMate grammar ({what})");
    if rel.to_ascii_lowercase().ends_with(".json") {
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("`{rel}` is not valid JSON: {e}"))?;
        let scope = value
            .get("scopeName")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| not_grammar("no `scopeName`"))?;
        if !value.get("patterns").is_some_and(Value::is_array) {
            return Err(not_grammar("no `patterns` array"));
        }
        Ok(scope.trim().to_string())
    } else {
        if !text.contains("<plist") {
            return Err(not_grammar("not a property list"));
        }
        plist_string(text, "scopeName").ok_or_else(|| not_grammar("no `scopeName`"))
    }
}

/// The `<string>` value following `<key>{key}</key>` in a property list.
fn plist_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("<key>{key}</key>"))?;
    let rest = &text[at..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    let value = rest[start..start + end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

// ----- external tools -------------------------------------------------------------------

/// Whether `path` can be run as a program (on Unix: its owner-execute bit).
pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o100 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// The program of a tool, read the way the IDE reads it: one word, after
/// quote removal (`"$PluginDir$"/bin/'run'.sh` is `$PluginDir$/bin/run.sh`).
fn program_word(decl: &ExternalToolDecl) -> Option<String> {
    let words = command::split_words(decl.program.trim()).ok()?;
    let [word] = <[String; 1]>::try_from(words).ok()?;
    Some(word)
}

/// The file a `$PluginDir$/…` program names (relative to the plugin
/// directory), read the way the IDE reads the program: after quote removal,
/// so `"$PluginDir$"/bin/'run'.sh` names `bin/run.sh`.
pub(crate) fn plugin_program(decl: &ExternalToolDecl) -> Option<String> {
    let word = program_word(decl)?;
    let rel = word.strip_prefix(PLUGIN_DIR_MACRO)?.trim_start_matches('/');
    (!rel.is_empty()).then(|| rel.to_string())
}

/// The tool with `$PluginDir$` replaced, and the plugin file it runs (for
/// `$PluginDir$/…` programs, which must exist). The IDE reads the tool's
/// command like any External Tool (see [`command`]), so the plugin directory
/// is written in that syntax, quoted where it needs to be: the IDE reads back
/// exactly the program word that was checked and the plugin's paths, whatever
/// characters the configuration directory has.
fn resolve_tool(
    decl: &ExternalToolDecl,
    dir: &Path,
    plugin_name: &str,
) -> Result<(ExternalTool, Option<PathBuf>), String> {
    let program_file = plugin_program(decl)
        .map(|rel| existing_file_inside(dir, &rel))
        .transpose()?;
    // Runtime tool macros must never interpret characters from this path.
    // External Tools decodes `$$` to a literal dollar after splitting.
    let dir_text = dir.to_string_lossy().replace('$', "$$");
    let group = decl.group.trim();
    let tool = ExternalTool {
        name: decl.name.trim().to_string(),
        group: if group.is_empty() {
            plugin_name.to_string()
        } else {
            group.to_string()
        },
        // One word, written so that the IDE reads it whole: as it is, or
        // starting with a quote (a program with quotes inside is otherwise
        // taken literally). Only a leading `$PluginDir$` is a macro there.
        program: program_word(decl)
            .map(|word| match word.strip_prefix(PLUGIN_DIR_MACRO) {
                Some(rest) => command::quote(&format!("{dir_text}{rest}")),
                None => command::quote(&word),
            })
            .ok_or_else(|| format!("`{}` must be a single command", decl.program.trim()))?,
        arguments: command::substitute(decl.arguments.trim(), PLUGIN_DIR_MACRO, &dir_text),
        working_dir: decl.working_dir.trim().replace(PLUGIN_DIR_MACRO, &dir_text),
        show_console: decl.show_console,
        // Plugin tools have no shortcut of their own (the keymap can bind one).
        shortcut: String::new(),
        options: Default::default(),
    };
    Ok((tool, program_file))
}

/// Runtime styles use the same validators and representations as disk plugins.
pub fn runtime_styles(
    plugin_id: &str,
    response: &crate::runtime::Response,
) -> Result<Contributions, String> {
    let mut out = Contributions::default();
    let mut notes = Notes::default();
    for value in &response.themes {
        if let Some(scheme) = parse_color_scheme(value, &mut notes) {
            out.color_schemes.push(ColorSchemeContribution {
                plugin_id: plugin_id.into(),
                path: PathBuf::new(),
                scheme,
            });
        }
    }
    for value in &response.keymaps {
        if let Some((name, bindings)) = parse_keymap(value, &mut notes) {
            out.keymaps.push(KeymapContribution {
                plugin_id: plugin_id.into(),
                path: PathBuf::new(),
                name,
                bindings,
            });
        }
    }
    if !notes.errors.is_empty() {
        return Err(notes.errors.join("; "));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colors_and_tags() {
        assert_eq!(parse_hex_color("#268bd2"), Some(Rgb(0x26, 0x8b, 0xd2)));
        assert_eq!(parse_hex_color("#FfF"), Some(Rgb(255, 255, 255)));
        for bad in ["268bd2", "#26 8bd", "#+12345", "#1234", "#ggg", ""] {
            assert_eq!(parse_hex_color(bad), None, "{bad:?}");
        }
        assert_eq!(lookup_tag("Keyword"), Some(HighlightTag::Keyword));
        assert_eq!(
            lookup_tag("function_definition"),
            Some(HighlightTag::FunctionDefinition)
        );
        assert_eq!(
            lookup_tag("fstring-brace"),
            Some(HighlightTag::FStringBrace)
        );
        assert_eq!(lookup_tag("NoSuchTag"), None);
    }

    #[test]
    fn color_scheme_overlays_the_base() {
        let mut notes = Notes::default();
        let scheme = parse_color_scheme(
            &json!({
                "name": "Paper",
                "base": "Light",
                "colors": {
                    "Keyword": "#112233",
                    "comment": {"fg": "#445566", "italic": true, "bg": null},
                    "ClassDefinition": {"bold": false},
                    "Sparkles": "#000000"
                },
                "editor": {"background": "#fdf6e3", "nonsense": "#000000"}
            }),
            &mut notes,
        )
        .expect("valid scheme");
        assert!(notes.errors.is_empty(), "{:?}", notes.errors);
        let light = ColorScheme::light();
        assert_eq!(scheme.name, "Paper");
        assert!(!scheme.dark);
        assert_eq!(
            scheme.style(HighlightTag::Keyword).fg,
            Some(Rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(
            scheme.style(HighlightTag::Keyword).bold,
            light.style(HighlightTag::Keyword).bold
        );
        assert!(scheme.style(HighlightTag::Comment).italic);
        assert!(!scheme.style(HighlightTag::ClassDefinition).bold);
        assert_eq!(
            scheme.style(HighlightTag::ClassDefinition).fg,
            light.style(HighlightTag::ClassDefinition).fg
        );
        assert_eq!(scheme.editor.background, Rgb(0xfd, 0xf6, 0xe3));
        assert_eq!(scheme.editor.foreground, light.editor.foreground);
        assert_eq!(
            scheme.style(HighlightTag::String),
            light.style(HighlightTag::String)
        );
        assert_eq!(notes.warnings.len(), 2, "{:?}", notes.warnings);
    }

    #[test]
    fn color_scheme_errors_are_collected() {
        let mut notes = Notes::default();
        let scheme = parse_color_scheme(
            &json!({"name": "Dark", "base": "Neon", "colors": {"Keyword": "red", "String": {"bold": "yes"}}, "editor": {"caret": 7}}),
            &mut notes,
        );
        assert!(scheme.is_none());
        let all = notes.errors.join("\n");
        for needle in [
            "built-in scheme",
            "`Neon`",
            "colors.Keyword",
            "colors.String.bold",
            "editor.caret",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in\n{all}");
        }
    }

    #[test]
    fn color_scheme_base_accepts_legacy_names() {
        for (base, dark) in [
            ("Dark", true),
            ("Light", false),
            ("Darcula", true),
            ("IntelliJ Light", false),
        ] {
            let mut notes = Notes::default();
            let scheme = parse_color_scheme(&json!({"name": "Mine", "base": base}), &mut notes)
                .expect("valid scheme");
            assert!(notes.errors.is_empty(), "{base}: {:?}", notes.errors);
            assert_eq!(scheme.dark, dark, "{base}");
        }
        let mut notes = Notes::default();
        parse_color_scheme(&json!({"name": "Mine", "base": "Neon"}), &mut notes);
        let all = notes.errors.join("\n");
        assert!(all.contains("use \"Dark\" or \"Light\""), "{all}");
    }

    #[test]
    fn live_templates_accept_both_spellings() {
        let mut notes = Notes::default();
        let templates = parse_live_templates(
            &json!([
                {"abbreviation": "bp", "description": "breakpoint", "template": "breakpoint()$END$", "contexts": ["Statement"]},
                {"abbreviation": "todo", "text": "# TODO: $END$", "context": "comment", "variables": [{"name": "X", "expression": "user()"}]},
                {"abbreviation": "any", "text": "x"}
            ]),
            "My Plugin",
            &mut notes,
        );
        assert!(notes.errors.is_empty(), "{:?}", notes.errors);
        assert_eq!(templates.len(), 3);
        assert_eq!(templates[0].template, "breakpoint()$END$");
        assert_eq!(templates[0].contexts, vec![TemplateContext::Statement]);
        assert_eq!(templates[0].group, "My Plugin");
        assert_eq!(templates[1].template, "# TODO: $END$");
        assert_eq!(templates[1].contexts, vec![TemplateContext::Comment]);
        assert_eq!(templates[1].variables[0].expression, "user()");
        assert_eq!(templates[2].contexts, vec![TemplateContext::Any]);
    }

    #[test]
    fn live_template_errors() {
        let mut notes = Notes::default();
        parse_live_templates(
            &json!([{"abbreviation": "a b", "template": "x"}, {"template": "x"}, {"abbreviation": "c"}, {"abbreviation": "d", "text": "x", "context": "Nowhere"}, 3]),
            "P",
            &mut notes,
        );
        let all = notes.errors.join("\n");
        for needle in [
            "[0].abbreviation",
            "[1].abbreviation: is required",
            "[2].template: is required",
            "[3].context: `Nowhere`",
            "[4]: expected an object",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in\n{all}");
        }
        let mut notes = Notes::default();
        parse_live_templates(&json!({"abbreviation": "x"}), "P", &mut notes);
        assert_eq!(
            notes.errors,
            vec!["expected a JSON array of live templates"]
        );
    }

    #[test]
    fn keymaps_and_shortcuts() {
        for ok in [
            "Ctrl+Shift+P",
            "F12",
            "Alt+ArrowDown",
            "Cmd+Plus",
            "shift+F6",
        ] {
            assert!(check_shortcut(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Ctrl+",
            "Ctrl++",
            "Ctrl+Shift",
            "Ctrl+K Ctrl+C",
            "Ctrl+A+B",
        ] {
            assert!(check_shortcut(bad).is_err(), "{bad:?}");
        }
        let mut notes = Notes::default();
        let (name, bindings) = parse_keymap(
            &json!({"name": "Mine", "bindings": {"GotoFile": ["Ctrl+P"], "Run": "Ctrl+F5", "Debug": [], "$Copy": ["Alt+W"]}}),
            &mut notes,
        )
        .unwrap();
        assert_eq!(name, "Mine");
        assert_eq!(bindings["$Copy"], vec!["Alt+W"]);
        for bad in ["$", "$$Copy", "a$b", "Copy$", "a b", ""] {
            assert!(!is_action_id(bad), "{bad:?}");
        }
        assert_eq!(bindings["Run"], vec!["Ctrl+F5"]);
        assert!(bindings["Debug"].is_empty());
        let mut notes = Notes::default();
        assert!(parse_keymap(
            &json!({"name": "", "bindings": {"bad id!": ["F1"], "Run": [1]}}),
            &mut notes
        )
        .is_none());
        assert_eq!(notes.errors.len(), 3, "{:?}", notes.errors);
    }

    #[test]
    fn plist_scope() {
        let plist = "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>name</key><string>Nim</string><key>scopeName</key><string> source.nim </string></dict></plist>";
        assert_eq!(
            plist_string(plist, "scopeName").as_deref(),
            Some("source.nim")
        );
        assert_eq!(plist_string(plist, "fileTypes"), None);
    }

    /// The plugin directory is written in the IDE's command syntax, quoted
    /// for its place: the IDE reads back the checked program and the plugin's
    /// paths whatever the directory is called (`Application Support` on
    /// macOS, a user named O'Brien, a `"`, `$` or `\` in a path).
    #[test]
    fn plugin_dir_is_quoted_for_its_place_in_the_command() {
        let tmp = tempfile::tempdir().unwrap();
        let mut names = vec!["plain", "Application Support", "o'brien $y"];
        if cfg!(unix) {
            names.push("o'brien \"x\" $y \\ `z`");
        }
        for name in names {
            let dir = tmp.path().join(name).join("com.x");
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("bin/run.sh"), "").unwrap();
            let decl = ExternalToolDecl {
                name: "Run".into(),
                program: "$PluginDir$/bin/run.sh".into(),
                arguments:
                    "--a $PluginDir$/a.toml --b=\"$PluginDir$/b.toml\" '$PluginDir$/c' $FilePath$"
                        .into(),
                working_dir: "$PluginDir$/bin".into(),
                ..ExternalToolDecl::default()
            };
            let d = dir.to_string_lossy().replace('$', "$$");
            let (tool, file) = resolve_tool(&decl, &dir, "X").unwrap();
            assert_eq!(file, Some(dir.join("bin/run.sh")));
            assert_eq!(
                command::split_words(&tool.program).unwrap(),
                [format!("{d}/bin/run.sh")]
            );
            assert_eq!(
                command::split_words(&tool.arguments).unwrap(),
                [
                    "--a".to_string(),
                    format!("{d}/a.toml"),
                    format!("--b={d}/b.toml"),
                    format!("{d}/c"),
                    "$FilePath$".to_string(),
                ],
                "{}",
                tool.arguments
            );
            assert_eq!(tool.working_dir, format!("{d}/bin"));
            // The IDE takes a program that does not start with a quote as it
            // is, quotes included, so quotes in the manifest are resolved here:
            // the program is the path itself, or starts with a quote.
            let quoted = ExternalToolDecl {
                program: "\"$PluginDir$\"/bin/'run'.sh".into(),
                ..decl.clone()
            };
            let (tool, _) = resolve_tool(&quoted, &dir, "X").unwrap();
            assert_eq!(
                command::split_words(&tool.program).unwrap(),
                [format!("{d}/bin/run.sh")]
            );
            assert!(
                tool.program == format!("{d}/bin/run.sh") || tool.program.starts_with('"'),
                "{}",
                tool.program
            );
            if name != "plain" {
                assert!(tool.program.starts_with('"'), "{}", tool.program);
            }
        }
        let decl = ExternalToolDecl {
            program: "$PluginDir$/bin/run.sh".into(),
            ..ExternalToolDecl::default()
        };
        assert_eq!(plugin_program(&decl).as_deref(), Some("bin/run.sh"));
        // The program file is the one the IDE runs: quotes are removed first.
        let quoted = ExternalToolDecl {
            program: "\"$PluginDir$\"/bin/'run'.sh".into(),
            ..decl.clone()
        };
        assert_eq!(plugin_program(&quoted).as_deref(), Some("bin/run.sh"));
        let on_path = ExternalToolDecl {
            program: "ruff".into(),
            ..decl
        };
        assert_eq!(plugin_program(&on_path), None);
    }
}

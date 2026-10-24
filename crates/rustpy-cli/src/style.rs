//! The code style `rustpy format` applies to a file. Without `-s` it is what
//! Code › Reformat Code uses in the IDE: Settings › Editor › Code Style, the
//! file's detected indentation and `.editorconfig`; when the code style selects
//! Black or Ruff as the formatter, Black's defaults with the `[tool.black]` or
//! `[tool.ruff]` settings of the nearest `pyproject.toml` (or `ruff.toml`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustpy_format::{FormatOptions, QuoteStyle};
use rustpy_settings::Settings;

use crate::mini_xml::{self, Tag};

/// Files or folders that make a folder a project root for `rustpy format`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".rustpy",
    ".idea",
    ".git",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
];

/// The nearest folder at or above `path` with a [`PROJECT_MARKERS`] entry.
pub fn project_root_of(path: &Path) -> Option<PathBuf> {
    let mut dir = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(d) = dir {
        if PROJECT_MARKERS.iter().any(|m| d.join(m).exists()) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Reads RustPy settings from `settings.json` or from a bundle written by
/// File › Manage IDE Settings › Export.
pub fn load_settings_file(path: &Path) -> Result<Settings, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?;
    let value = if value.get("format").and_then(|f| f.as_str()) == Some("rustpy-settings") {
        let inner = value
            .get("files")
            .and_then(|f| f.get("settings.json"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                format!(
                    "{}: the settings bundle has no settings.json",
                    path.display()
                )
            })?;
        serde_json::from_str(inner)
            .map_err(|e| format!("{}: settings.json in the bundle: {e}", path.display()))?
    } else {
        value
    };
    serde_json::from_value(value)
        .map_err(|e| format!("{}: not RustPy settings: {e}", path.display()))
}

fn quote_style(name: &str) -> QuoteStyle {
    match name {
        "double" => QuoteStyle::Double,
        "single" => QuoteStyle::Single,
        _ => QuoteStyle::Preserve,
    }
}

/// The options of the built-in formatter for `file`, layered like the IDE's
/// Reformat Code: code style settings, then the detected indentation (when
/// "Detect and use existing file indents" is on), then `.editorconfig`.
pub fn builtin_options(
    settings: &Settings,
    file: &Path,
    text: &str,
    use_editorconfig: bool,
) -> FormatOptions {
    let mut editor = settings.editor.clone();
    settings.code_style.apply_language(file, &mut editor);
    let e = &editor;
    let mut tab_size = e.tab_size.max(1);
    let mut indent = e.indent_size;
    let mut use_tabs = e.use_tab_character;
    let mut margin = e.right_margin;
    let ec = if use_editorconfig && e.editorconfig_enabled {
        rustpy_settings::editorconfig::resolve(file)
    } else {
        Default::default()
    };
    if let Some(n) = ec.tab_width {
        tab_size = n.clamp(1, 16);
    }
    if e.detect_indents {
        match rustpy_text::util::detect_indent(text, true) {
            Some(rustpy_text::util::IndentStyle::Tabs) => {
                use_tabs = true;
                indent = tab_size;
            }
            Some(rustpy_text::util::IndentStyle::Spaces(n)) => {
                use_tabs = false;
                indent = n.clamp(1, 16);
            }
            None => {}
        }
    }
    if let Some(style) = &ec.indent_style {
        use_tabs = style.eq_ignore_ascii_case("tab");
    }
    match ec.indent_size {
        Some(n) => indent = n.clamp(1, 16),
        None if ec.indent_style.is_some() && use_tabs => indent = tab_size,
        None => {}
    }
    if let Some(n) = ec.max_line_length {
        margin = n;
    }
    let cs = &settings.code_style;
    let mut o = FormatOptions::pep8();
    o.indent = indent.max(1);
    o.tabs = use_tabs.then_some(rustpy_format::TabIndent {
        width: tab_size,
        smart: ec.ide.smart_tabs.unwrap_or(false),
    });
    if let Some(width) = ec.ide.continuation_indent_size {
        o.continuation_indent = Some(rustpy_format::ContinuationIndent {
            width,
            parameters: ec.ide.continuation_parameters.unwrap_or(true),
            arguments: ec.ide.continuation_arguments.unwrap_or(true),
            collections: ec.ide.continuation_collections.unwrap_or(true),
        })
    }
    o.max_line_length = if ec.max_line_length_off {
        16384
    } else {
        margin.max(40)
    };
    o.quote = quote_style(&cs.quote_style);
    o.magic_trailing_comma = cs.magic_trailing_comma;
    o.blank_lines_top_level = cs.blank_lines_top_level;
    o.blank_lines_in_class = cs.blank_lines_in_class;
    rustpy_format::style_policy::apply(&mut o, cs, &ec);
    o
}

fn toml_int(t: &toml::Table, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|k| t.get(*k))
        .and_then(|v| v.as_integer())
        .and_then(|n| usize::try_from(n).ok())
}

fn toml_bool(t: &toml::Table, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|k| t.get(*k))
        .and_then(|v| v.as_bool())
}

fn toml_str<'a>(t: &'a toml::Table, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| t.get(*k)).and_then(|v| v.as_str())
}

/// `[tool.<name>]` of a parsed pyproject.toml.
pub fn tool_table<'a>(pyproject: &'a toml::Table, name: &str) -> Option<&'a toml::Table> {
    pyproject.get("tool")?.as_table()?.get(name)?.as_table()
}

/// Black's options from `[tool.black]` (`line-length`, `skip-string-normalization`,
/// `skip-magic-trailing-comma`; Black accepts `_` for `-` too).
pub fn black_options(black: Option<&toml::Table>) -> FormatOptions {
    let mut o = FormatOptions::black();
    if let Some(t) = black {
        if let Some(n) = toml_int(t, &["line-length", "line_length"]) {
            o.max_line_length = n.max(1);
        }
        if toml_bool(
            t,
            &["skip-string-normalization", "skip_string_normalization"],
        ) == Some(true)
        {
            o.quote = QuoteStyle::Preserve;
        }
        if toml_bool(
            t,
            &["skip-magic-trailing-comma", "skip_magic_trailing_comma"],
        ) == Some(true)
        {
            o.magic_trailing_comma = false;
        }
    }
    o
}

/// Ruff's formatter options: `line-length`, `indent-width` and `[format]`
/// `quote-style` and `skip-magic-trailing-comma` (`ruff` is the `[tool.ruff]`
/// table, or the whole `ruff.toml`).
pub fn ruff_options(ruff: Option<&toml::Table>) -> FormatOptions {
    let mut o = FormatOptions::black();
    if let Some(t) = ruff {
        if let Some(n) = toml_int(t, &["line-length", "line_length"]) {
            o.max_line_length = n.max(1);
        }
        if let Some(n) = toml_int(t, &["indent-width", "indent_width"]) {
            o.indent = n.clamp(1, 16);
        }
        if let Some(f) = t.get("format").and_then(|f| f.as_table()) {
            match toml_str(f, &["quote-style", "quote_style"]) {
                Some("single") => o.quote = QuoteStyle::Single,
                Some("preserve") => o.quote = QuoteStyle::Preserve,
                Some("double") => o.quote = QuoteStyle::Double,
                _ => {}
            }
            if toml_bool(
                f,
                &["skip-magic-trailing-comma", "skip_magic_trailing_comma"],
            ) == Some(true)
            {
                o.magic_trailing_comma = false;
            }
        }
    }
    o
}

/// Options from an XML code style scheme (`<code_scheme>`, also
/// inside `.idea/codeStyles/Project.xml`): the right margin, the Python indent
/// size and the Python blank-line settings.
pub fn code_style_scheme_options(xml: &str) -> Result<FormatOptions, String> {
    let tags = mini_xml::tags(xml)?;
    let mut o = FormatOptions::pep8();
    let mut scheme = false;
    let mut general_margin: Option<usize> = None;
    let mut python_margin: Option<usize> = None;
    // Open elements, `codeStyleSettings` recorded with its language.
    let mut stack: Vec<String> = Vec::new();
    for tag in &tags {
        match tag {
            Tag::Start { name, empty, .. } => {
                if name == "code_scheme" {
                    scheme = true;
                }
                if name == "option" {
                    let (Some(key), Some(value)) = (tag.attr("name"), tag.attr("value")) else {
                        continue;
                    };
                    let n = value.trim().parse::<usize>().ok();
                    let parent = stack.last().map(String::as_str).unwrap_or("");
                    let in_python = stack.iter().any(|s| s == "codeStyleSettings:Python");
                    match (key, parent) {
                        ("RIGHT_MARGIN", "code_scheme") => general_margin = n,
                        ("RIGHT_MARGIN", "codeStyleSettings:Python") => python_margin = n,
                        ("INDENT_SIZE", "indentOptions") if in_python => {
                            if let Some(n) = n {
                                o.indent = n.clamp(1, 16);
                            }
                        }
                        ("BLANK_LINES_AROUND_METHOD", "codeStyleSettings:Python") => {
                            if let Some(n) = n {
                                o.blank_lines_in_class = n;
                            }
                        }
                        (
                            "BLANK_LINES_AROUND_TOP_LEVEL_CLASSES_FUNCTIONS",
                            "Python" | "PyCodeStyleSettings",
                        ) => {
                            if let Some(n) = n {
                                o.blank_lines_top_level = n;
                            }
                        }
                        _ => {}
                    }
                }
                if !empty {
                    let entry = if name == "codeStyleSettings" {
                        format!("codeStyleSettings:{}", tag.attr("language").unwrap_or(""))
                    } else {
                        name.clone()
                    };
                    stack.push(entry);
                }
            }
            Tag::End { .. } => {
                stack.pop();
            }
        }
    }
    if !scheme {
        return Err("not a code style scheme (no <code_scheme> element)".into());
    }
    if let Some(m) = python_margin.or(general_margin) {
        o.max_line_length = m.max(40);
    }
    Ok(o)
}

/// A code style given with `-s`.
#[derive(Clone, Debug)]
pub enum StyleFile {
    /// RustPy settings: their code style applies (without `.editorconfig`).
    Settings(Box<Settings>),
    /// Fixed options from an XML code style scheme or a pyproject.toml / ruff.toml.
    Options(Box<FormatOptions>),
}

/// Reads the `-s` file: `.xml` (code style scheme), `.toml` (pyproject.toml with
/// `[tool.black]` or `[tool.ruff]`, or a ruff.toml), anything else RustPy settings.
pub fn load_style_file(path: &Path) -> Result<StyleFile, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "xml" => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            code_style_scheme_options(&text)
                .map(|options| StyleFile::Options(Box::new(options)))
                .map_err(|e| format!("{}: {e}", path.display()))
        }
        "toml" => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let table: toml::Table = text
                .parse()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "ruff.toml" || name == ".ruff.toml" {
                return Ok(StyleFile::Options(Box::new(ruff_options(Some(&table)))));
            }
            if let Some(black) = tool_table(&table, "black") {
                return Ok(StyleFile::Options(Box::new(black_options(Some(black)))));
            }
            if let Some(ruff) = tool_table(&table, "ruff") {
                return Ok(StyleFile::Options(Box::new(ruff_options(Some(ruff)))));
            }
            Err(format!(
                "{}: no [tool.black] or [tool.ruff] table",
                path.display()
            ))
        }
        _ => load_settings_file(path).map(|s| StyleFile::Settings(Box::new(s))),
    }
}

/// What to do with one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Format(Box<FormatOptions>),
    /// Not formatted, with the reason.
    Skip(String),
}

/// Resolves the style per file, caching parsed pyproject files.
pub struct StyleResolver {
    settings: Settings,
    style_file: Option<StyleFile>,
    allow_defaults: bool,
    /// Parsed formatter configuration per (directory, tool) (`None`: none there).
    tool_configs: HashMap<(PathBuf, String), Option<(PathBuf, toml::Table)>>,
}

impl StyleResolver {
    pub fn new(settings: Settings, style_file: Option<StyleFile>, allow_defaults: bool) -> Self {
        Self {
            settings,
            style_file,
            allow_defaults,
            tool_configs: HashMap::new(),
        }
    }

    /// The formatter options for `file` with contents `text`.
    pub fn decide(&mut self, file: &Path, text: &str) -> Decision {
        let mut effective = self.settings.clone();
        if self.style_file.is_none() {
            if let Some(root) = project_root_of(file) {
                if let Err(error) = effective.load_project_code_style(&root) {
                    return Decision::Skip(error);
                }
            }
        }
        let policy_settings = match &self.style_file {
            Some(StyleFile::Settings(settings)) => settings.as_ref().clone(),
            _ => effective.clone(),
        };
        let root =
            project_root_of(file).unwrap_or_else(|| file.parent().unwrap_or(file).to_path_buf());
        let context = rustpy_settings::scopes::ScopeContext {
            root: root.clone(),
            ..Default::default()
        };
        match policy_settings.formatting_excluded(file, &context) {
            Ok(true) => return Decision::Skip("Excluded by Code Style".into()),
            Err(error) => return Decision::Skip(format!("Invalid do-not-format scope: {error}")),
            _ => {}
        }
        let lines = match policy_settings.code_style.reformat_scope.as_str() {
            "vcs" => {
                let Some(git) = rustpy_vcs::GitRepo::discover(file) else {
                    return Decision::Skip("VCS changed text requires Git".into());
                };
                Some(
                    git.line_changes(file, text)
                        .iter()
                        .map(|change| (change.start, change.count))
                        .collect::<Vec<_>>(),
                )
            }
            "changed" => Some(Vec::new()),
            _ => None,
        };
        let ec = if self.style_file.is_none() {
            policy_settings.editor.editorconfig_for(file)
        } else {
            Default::default()
        };
        if ec.ide.formatter_enabled == Some(false) {
            return Decision::Skip("Formatting disabled by EditorConfig".into());
        }
        let restrict = |mut options: FormatOptions| {
            rustpy_format::style_policy::apply(&mut options, &policy_settings.code_style, &ec);
            options.changed_lines = lines.clone();
            Decision::Format(Box::new(options))
        };
        match &self.style_file {
            Some(StyleFile::Options(o)) => return restrict(o.as_ref().clone()),
            Some(StyleFile::Settings(s)) => {
                return restrict(match s.code_style.formatter.as_str() {
                    "black" | "ruff" => FormatOptions::black(),
                    _ => builtin_options(s, file, text, false),
                })
            }
            None => {}
        }
        let in_project = project_root_of(file).is_some();
        if !in_project && !self.allow_defaults {
            return Decision::Skip("not in a project (no .rustpy, .idea, .git, pyproject.toml, setup.py or setup.cfg above it); use -allowDefaults".into());
        }
        let defaults;
        let settings = if in_project {
            &effective
        } else {
            defaults = Settings::default();
            &defaults
        };
        match settings.code_style.formatter.clone().as_str() {
            "black" => {
                let config = self.tool_config(file, "black");
                restrict(black_options(
                    config.as_ref().and_then(|(_, t)| tool_table(t, "black")),
                ))
            }
            "ruff" => {
                let config = self.tool_config(file, "ruff");
                let table = config.as_ref().map(|(path, t)| {
                    if path.file_name().is_some_and(|n| n != "pyproject.toml") {
                        Some(t)
                    } else {
                        tool_table(t, "ruff")
                    }
                });
                restrict(ruff_options(table.flatten()))
            }
            _ => restrict(builtin_options(settings, file, text, true)),
        }
    }

    /// The configuration file that applies to `file` for `tool`: the nearest
    /// `pyproject.toml` (Black stops at the first one), or for Ruff also a
    /// `ruff.toml` / `.ruff.toml`.
    pub fn tool_config(&mut self, file: &Path, tool: &str) -> Option<(PathBuf, toml::Table)> {
        let mut dir = if file.is_dir() {
            Some(file.to_path_buf())
        } else {
            file.parent().map(Path::to_path_buf)
        };
        while let Some(d) = dir {
            let key = (d.clone(), tool.to_string());
            if let Some(hit) = self.tool_configs.get(&key) {
                if hit.is_some() {
                    return hit.clone();
                }
            } else {
                let mut names = vec![];
                if tool == "ruff" {
                    names.extend([".ruff.toml", "ruff.toml"]);
                }
                names.push("pyproject.toml");
                let mut found = None;
                for name in names {
                    let path = d.join(name);
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    match text.parse::<toml::Table>() {
                        Ok(t) => {
                            // A pyproject.toml without [tool.ruff] does not stop Ruff's search.
                            if tool == "ruff"
                                && name == "pyproject.toml"
                                && tool_table(&t, "ruff").is_none()
                            {
                                continue;
                            }
                            found = Some((path, t));
                            break;
                        }
                        Err(e) => log::warn!("{}: {e}", path.display()),
                    }
                }
                self.tool_configs.insert(key, found.clone());
                if found.is_some() {
                    return found;
                }
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        None
    }

    /// Black's `exclude` / `extend-exclude` / `force-exclude` regular expressions
    /// for files under `root`'s pyproject.toml (only when Black is the formatter).
    pub fn black_excludes(&mut self, path: &Path) -> Vec<(PathBuf, regex::Regex)> {
        if self.style_file.is_some() || self.settings.code_style.formatter != "black" {
            return Vec::new();
        }
        let Some((config, table)) = self.tool_config(path, "black") else {
            return Vec::new();
        };
        let Some(black) = tool_table(&table, "black") else {
            return Vec::new();
        };
        let root = config.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut out = Vec::new();
        for key in [
            "exclude",
            "extend-exclude",
            "extend_exclude",
            "force-exclude",
            "force_exclude",
        ] {
            let Some(pattern) = black.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            // Black compiles multi-line patterns in verbose mode.
            let source = if pattern.contains('\n') {
                format!("(?x){pattern}")
            } else {
                pattern.to_string()
            };
            match regex::Regex::new(&source) {
                Ok(re) => out.push((root.clone(), re)),
                Err(e) => log::warn!("{}: [tool.black] {key}: {e}", config.display()),
            }
        }
        out
    }
}

/// Whether Black's exclude patterns match `path` (relative to the pattern's
/// root, with a leading `/` and a trailing `/` for folders, like Black).
pub fn black_excluded(excludes: &[(PathBuf, regex::Regex)], path: &Path) -> bool {
    excludes.iter().any(|(root, re)| {
        let Ok(rel) = path.strip_prefix(root) else {
            return false;
        };
        let mut s = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
        if path.is_dir() {
            s.push('/');
        }
        re.is_match(&s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_options_follow_settings_detection_and_editorconfig() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.py");
        let mut s = Settings::default();
        s.editor.right_margin = 100;
        s.code_style.quote_style = "single".into();
        s.code_style.blank_lines_top_level = 3;
        let o = builtin_options(&s, &file, "def f():\n    pass\n", true);
        assert_eq!(
            (
                o.indent,
                o.max_line_length,
                o.quote,
                o.blank_lines_top_level
            ),
            (4, 100, QuoteStyle::Single, 3)
        );
        // A file indented with two spaces keeps them (detect_indents is on by default).
        let o = builtin_options(&s, &file, "def f():\n  if x:\n    pass\n", true);
        assert_eq!(o.indent, 2);
        s.editor.detect_indents = false;
        assert_eq!(
            builtin_options(&s, &file, "def f():\n  if x:\n    pass\n", true).indent,
            4
        );
        // .editorconfig wins over both.
        std::fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.py]\nindent_size = 3\nmax_line_length = 79\n",
        )
        .unwrap();
        let o = builtin_options(&s, &file, "", true);
        assert_eq!((o.indent, o.max_line_length), (3, 79));
        let o = builtin_options(&s, &file, "", false);
        assert_eq!(
            (o.indent, o.max_line_length),
            (4, 100),
            "-s settings ignore .editorconfig"
        );
    }

    #[test]
    fn black_and_ruff_configuration() {
        let t: toml::Table = "[tool.black]\nline-length = 100\nskip-string-normalization = true\nskip-magic-trailing-comma = true\n".parse().unwrap();
        let o = black_options(tool_table(&t, "black"));
        assert_eq!(
            (o.max_line_length, o.quote, o.magic_trailing_comma),
            (100, QuoteStyle::Preserve, false)
        );
        let o = black_options(None);
        assert_eq!(
            (o.max_line_length, o.quote, o.magic_trailing_comma),
            (88, QuoteStyle::Double, true)
        );
        let t: toml::Table = "[tool.ruff]\nline-length = 110\nindent-width = 2\n[tool.ruff.format]\nquote-style = \"single\"\n".parse().unwrap();
        let o = ruff_options(tool_table(&t, "ruff"));
        assert_eq!(
            (o.max_line_length, o.indent, o.quote),
            (110, 2, QuoteStyle::Single)
        );
    }

    #[test]
    fn xml_code_style_scheme() {
        let xml = r#"<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <option name="RIGHT_MARGIN" value="140" />
    <Python>
      <option name="BLANK_LINES_AROUND_TOP_LEVEL_CLASSES_FUNCTIONS" value="3" />
    </Python>
    <codeStyleSettings language="JSON">
      <option name="RIGHT_MARGIN" value="60" />
      <indentOptions><option name="INDENT_SIZE" value="8" /></indentOptions>
    </codeStyleSettings>
    <codeStyleSettings language="Python">
      <option name="RIGHT_MARGIN" value="99" />
      <option name="BLANK_LINES_AROUND_METHOD" value="2" />
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>"#;
        let o = code_style_scheme_options(xml).unwrap();
        assert_eq!(
            (
                o.max_line_length,
                o.indent,
                o.blank_lines_top_level,
                o.blank_lines_in_class
            ),
            (99, 2, 3, 2)
        );
        let o = code_style_scheme_options(
            r#"<code_scheme name="x"><option name="RIGHT_MARGIN" value="100"/></code_scheme>"#,
        )
        .unwrap();
        assert_eq!((o.max_line_length, o.indent), (100, 4));
        assert!(code_style_scheme_options("<profile/>").is_err());
    }

    #[test]
    fn settings_files_and_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("settings.json");
        std::fs::write(
            &plain,
            r#"{"editor": {"right_margin": 90}, "code_style": {"quote_style": "double"}}"#,
        )
        .unwrap();
        let s = load_settings_file(&plain).unwrap();
        assert_eq!(
            (s.editor.right_margin, s.code_style.quote_style.as_str()),
            (90, "double")
        );
        let bundle = dir.path().join("export.json");
        let inner =
            serde_json::to_string(&serde_json::json!({"editor": {"right_margin": 77}})).unwrap();
        std::fs::write(&bundle, serde_json::to_string(&serde_json::json!({"format": "rustpy-settings", "version": 1, "files": {"settings.json": inner}})).unwrap()).unwrap();
        assert_eq!(load_settings_file(&bundle).unwrap().editor.right_margin, 77);
        std::fs::write(&plain, "{ not json").unwrap();
        assert!(load_settings_file(&plain)
            .unwrap_err()
            .contains("not valid JSON"));
        let toml_file = dir.path().join("pyproject.toml");
        std::fs::write(&toml_file, "[project]\nname = 'x'\n").unwrap();
        assert!(load_style_file(&toml_file)
            .unwrap_err()
            .contains("no [tool.black]"));
        std::fs::write(&toml_file, "[tool.black]\nline-length = 70\n").unwrap();
        assert!(
            matches!(load_style_file(&toml_file).unwrap(), StyleFile::Options(o) if o.max_line_length == 70)
        );
    }

    #[test]
    fn decisions_for_projects_and_loose_files() {
        let dir = tempfile::tempdir().unwrap();
        let loose = dir.path().join("loose.py");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(proj.join("pkg")).unwrap();
        std::fs::write(
            proj.join("pyproject.toml"),
            "[tool.black]\nline-length = 72\n",
        )
        .unwrap();
        let in_proj = proj.join("pkg/m.py");
        let mut s = Settings::default();
        s.editor.right_margin = 111;
        // The temporary directory may itself be inside a project (a checkout in /tmp).
        let loose_outside = project_root_of(&loose).is_none();
        let mut r = StyleResolver::new(s.clone(), None, false);
        if loose_outside {
            assert!(
                matches!(r.decide(&loose, ""), Decision::Skip(m) if m.contains("-allowDefaults"))
            );
        }
        assert!(
            matches!(r.decide(&in_proj, ""), Decision::Format(o) if o.max_line_length == 111),
            "the IDE's style, pyproject ignored by the built-in formatter"
        );
        let mut r = StyleResolver::new(s.clone(), None, true);
        if loose_outside {
            assert!(
                matches!(r.decide(&loose, ""), Decision::Format(o) if o.max_line_length == 120),
                "factory defaults outside projects"
            );
        }
        s.code_style.formatter = "black".into();
        let mut r = StyleResolver::new(s.clone(), None, false);
        assert!(
            matches!(r.decide(&in_proj, ""), Decision::Format(o) if o.max_line_length == 72 && o.quote == QuoteStyle::Double)
        );
        let mut r = StyleResolver::new(
            s,
            Some(StyleFile::Options(Box::new(FormatOptions::pep8()))),
            false,
        );
        assert!(
            matches!(r.decide(&loose, ""), Decision::Format(o) if *o == FormatOptions::pep8()),
            "-s applies everywhere"
        );
    }

    #[test]
    fn black_exclude_patterns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("migrations")).unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.black]\nextend-exclude = '''\n(\n  ^/migrations/\n  | generated_.*\\.py\n)\n'''\n").unwrap();
        let mut s = Settings::default();
        s.code_style.formatter = "black".into();
        let mut r = StyleResolver::new(s, None, false);
        let ex = r.black_excludes(dir.path());
        assert_eq!(ex.len(), 1);
        assert!(black_excluded(&ex, &dir.path().join("migrations")));
        assert!(black_excluded(
            &ex,
            &dir.path().join("pkg/generated_api.py")
        ));
        assert!(!black_excluded(&ex, &dir.path().join("pkg/api.py")));
        assert!(
            StyleResolver::new(Settings::default(), None, false)
                .black_excludes(dir.path())
                .is_empty(),
            "only for the Black formatter"
        );
    }

    #[test]
    fn project_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("p/.rustpy")).unwrap();
        std::fs::create_dir_all(dir.path().join("p/src/pkg")).unwrap();
        assert_eq!(
            project_root_of(&dir.path().join("p/src/pkg/m.py")),
            Some(dir.path().join("p"))
        );
        assert_eq!(
            project_root_of(&dir.path().join("p/src")),
            Some(dir.path().join("p"))
        );
    }
}

#[cfg(test)]
mod scheme_consumers {
    use super::*;
    #[test]
    fn project_scheme_and_editorconfig_precedence_reach_cli_options() {
        let root = tempfile::tempdir().unwrap();
        let mut scheme = rustpy_settings::code_style_schemes::Scheme::preset("Google").unwrap();
        scheme.indent.indent = 2;
        scheme.detect_indents = false;
        rustpy_settings::code_style_schemes::save_project(root.path(), &scheme).unwrap();
        let path = root.path().join("code.py");
        let mut resolver = StyleResolver::new(Settings::default(), None, false);
        let Decision::Format(options) = resolver.decide(&path, "x=1\n") else {
            panic!("project scheme was not selected")
        };
        assert_eq!(options.indent, 2);
        assert_eq!(options.quote, QuoteStyle::Single);
        std::fs::write(
            root.path().join(".editorconfig"),
            "root=true\n[*.py]\nindent_size=8\nij_python_quote_style=double\n",
        )
        .unwrap();
        let Decision::Format(options) = resolver.decide(&path, "x=1\n") else {
            panic!()
        };
        assert_eq!(options.indent, 8);
        assert_eq!(options.quote, QuoteStyle::Double)
    }
    #[test]
    fn cli_exclusions_precede_builtin_or_explicit_engine() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".rustpy")).unwrap();
        let mut settings = Settings::default();
        settings.code_style.do_not_format = vec!["*.py".into()];
        let mut resolver = StyleResolver::new(settings, None, false);
        assert!(matches!(
            resolver.decide(&root.path().join("code.py"), "x=1\n"),
            Decision::Skip(_)
        ))
    }
}

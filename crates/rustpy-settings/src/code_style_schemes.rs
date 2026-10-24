//! Code-style scheme snapshots. Active settings remain backwards compatible.
use crate::{CodeStyleSettings, EditorSettings, Settings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageStyle {
    pub indent: usize,
    pub tab_size: usize,
    pub tabs: bool,
    pub margin: usize,
}
impl Default for LanguageStyle {
    fn default() -> Self {
        Self {
            indent: 4,
            tab_size: 4,
            tabs: false,
            margin: 120,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scheme {
    pub style: CodeStyleSettings,
    pub indent: LanguageStyle,
    pub detect_indents: bool,
    pub wrap_on_typing: bool,
    pub visual_guides: Vec<usize>,
}
impl Default for Scheme {
    fn default() -> Self {
        Self {
            style: CodeStyleSettings::default(),
            indent: LanguageStyle::default(),
            detect_indents: true,
            wrap_on_typing: false,
            visual_guides: Vec::new(),
        }
    }
}
impl Scheme {
    pub fn capture(settings: &Settings) -> Self {
        let e = &settings.editor;
        Self {
            style: settings.code_style.clone(),
            indent: LanguageStyle {
                indent: e.indent_size,
                tab_size: e.tab_size,
                tabs: e.use_tab_character,
                margin: e.right_margin,
            },
            detect_indents: e.detect_indents,
            wrap_on_typing: e.wrap_on_typing,
            visual_guides: e.visual_guides.clone(),
        }
    }
    pub fn apply(&self, settings: &mut Settings) {
        settings.code_style = self.style.clone();
        if self.style.python.docstring_style != "auto" {
            settings.docstring_format = self.style.python.docstring_style.clone();
        }
        let e = &mut settings.editor;
        e.indent_size = self.indent.indent.clamp(1, 16);
        e.tab_size = self.indent.tab_size.clamp(1, 16);
        e.use_tab_character = self.indent.tabs;
        e.right_margin = self.indent.margin.clamp(1, 10000);
        e.detect_indents = self.detect_indents;
        e.wrap_on_typing = self.wrap_on_typing;
        e.visual_guides = self.visual_guides.clone();
    }
    pub fn preset(name: &str) -> Option<Self> {
        let mut s = Self::default();
        match name {
            "PEP 8" => s.indent.margin = 79,
            "Black-compatible" => {
                s.indent.margin = 88;
                s.style.quote_style = "double".into()
            }
            "Google" => {
                s.indent.margin = 80;
                s.style.quote_style = "single".into()
            }
            _ => return None,
        };
        Some(s)
    }
    pub fn export_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
    pub fn import_json(text: &str) -> Result<Self, String> {
        if text.len() > 1024 * 1024 {
            return Err("Scheme exceeds 1 MiB".into());
        }
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
    pub fn editorconfig(&self) -> String {
        let s = &self.style;
        let mut output=format!("root = true\n\n[*.{{py,pyi}}]\nindent_style = {}\nindent_size = {}\ntab_width = {}\nmax_line_length = {}\nij_python_blank_lines_around_top_level_classes_functions = {}\nij_python_blank_lines_around_method = {}\nij_python_quote_style = {}\nij_python_magic_trailing_comma = {}\nij_formatter_tags_enabled = {}\nij_formatter_off_tag = {}\nij_formatter_on_tag = {}\nij_python_keep_line_breaks = {}\nij_python_keep_simple_blocks_in_one_line = {}\nij_python_keep_first_column_comment = {}\n",if self.indent.tabs{"tab"}else{"space"},self.indent.indent,self.indent.tab_size,self.indent.margin,s.blank_lines_top_level,s.blank_lines_in_class,s.quote_style,s.magic_trailing_comma,s.formatter_markers,s.formatter_off.replace(['\r','\n'],""),s.formatter_on.replace(['\r','\n'],""),s.keep_line_breaks,s.keep_simple_blocks,s.keep_first_column_comments);
        output.push_str(&s.python.editorconfig());
        output
    }
    pub fn import_editorconfig(&mut self, text: &str) -> Result<(), String> {
        if text.len() > 1024 * 1024 {
            return Err("EditorConfig exceeds 1 MiB".into());
        }
        // Only the Python section/common properties; filesystem matching is handled
        // by the ordinary EditorConfig resolver when formatting individual files.
        let mut python = true;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with(['#', ';']) {
                continue;
            }
            if line.starts_with('[') {
                python = line.contains("py") || line == "[*]";
                continue;
            }
            if !python {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let number = value.parse::<usize>().ok().filter(|n| *n <= 10000);
            let boolean = match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            if self.style.python.import_property(key.trim(), value) {
                continue;
            }
            match key.trim() {
                "indent_style" => self.indent.tabs = value == "tab",
                "indent_size" => {
                    if let Some(n) = number.filter(|n| *n > 0) {
                        self.indent.indent = n.clamp(1, 16)
                    }
                }
                "tab_width" => {
                    if let Some(n) = number.filter(|n| *n > 0) {
                        self.indent.tab_size = n.clamp(1, 16)
                    }
                }
                "max_line_length" => {
                    if let Some(n) = number {
                        self.indent.margin = n
                    }
                }
                "ij_python_blank_lines_around_top_level_classes_functions" => {
                    if let Some(n) = number {
                        self.style.blank_lines_top_level = n.min(10)
                    }
                }
                "ij_python_blank_lines_around_method" => {
                    if let Some(n) = number {
                        self.style.blank_lines_in_class = n.min(10)
                    }
                }
                "ij_python_quote_style" => {
                    if matches!(value, "preserve" | "single" | "double") {
                        self.style.quote_style = value.into()
                    }
                }
                "ij_python_magic_trailing_comma" => {
                    if let Some(v) = boolean {
                        self.style.magic_trailing_comma = v
                    }
                }
                "ij_formatter_tags_enabled" => {
                    if let Some(v) = boolean {
                        self.style.formatter_markers = v
                    }
                }
                "ij_formatter_off_tag" => {
                    self.style.formatter_off = value.chars().take(256).collect()
                }
                "ij_formatter_on_tag" => {
                    self.style.formatter_on = value.chars().take(256).collect()
                }
                "ij_python_keep_line_breaks" => {
                    if let Some(v) = boolean {
                        self.style.keep_line_breaks = v
                    }
                }
                "ij_python_keep_simple_blocks_in_one_line" => {
                    if let Some(v) = boolean {
                        self.style.keep_simple_blocks = v
                    }
                }
                "ij_python_keep_first_column_comment" => {
                    if let Some(v) = boolean {
                        self.style.keep_first_column_comments = v
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schemes {
    pub selected: String,
    pub named: BTreeMap<String, Scheme>,
}
impl Default for Schemes {
    fn default() -> Self {
        Self {
            selected: "Project".into(),
            named: BTreeMap::new(),
        }
    }
}
impl Settings {
    pub fn select_code_style(&mut self, name: &str) -> Result<(), String> {
        let next = self
            .code_style_schemes
            .named
            .get(name)
            .cloned()
            .or_else(|| (name == "Default").then(Scheme::default))
            .or_else(|| (name == "Project").then(Scheme::default))
            .ok_or("Unknown code-style scheme")?;
        let current = Scheme::capture(self);
        self.code_style_schemes
            .named
            .insert(self.code_style_schemes.selected.clone(), current);
        next.apply(self);
        self.code_style_schemes.selected = name.into();
        Ok(())
    }
    pub fn copy_code_style(&mut self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if self.code_style_schemes.named.contains_key(name) {
            return Err("Scheme already exists".into());
        }
        let current = Scheme::capture(self);
        self.code_style_schemes.named.insert(name.into(), current);
        self.select_code_style(name)
    }
    pub fn rename_code_style(&mut self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        let old = self.code_style_schemes.selected.clone();
        if matches!(old.as_str(), "Project" | "Default") {
            return Err("Copy a built-in scheme before renaming it".into());
        }
        if self.code_style_schemes.named.contains_key(name) {
            return Err("Scheme already exists".into());
        }
        self.code_style_schemes.named.remove(&old);
        self.code_style_schemes.selected = name.into();
        self.code_style_schemes
            .named
            .insert(name.into(), Scheme::capture(self));
        Ok(())
    }
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.trim() != name
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || matches!(name, "Project" | "Default")
    {
        Err("Choose a unique named scheme (not Project or Default)".into())
    } else {
        Ok(())
    }
}
impl CodeStyleSettings {
    pub fn language_style(&self, path: &std::path::Path) -> Option<&LanguageStyle> {
        let ext = path.extension()?.to_str()?;
        let language = match ext {
            "py" | "pyi" => "Python",
            "json" | "jsonc" => "JSON",
            "yaml" | "yml" => "YAML",
            "html" | "htm" | "jinja" | "jinja2" => "HTML",
            "css" | "scss" => "CSS",
            "js" | "jsx" | "ts" | "tsx" => "JavaScript",
            _ => "Other",
        };
        self.languages.get(language)
    }
    pub fn apply_language(&self, path: &std::path::Path, editor: &mut EditorSettings) {
        if let Some(s) = self.language_style(path) {
            editor.indent_size = s.indent.clamp(1, 16);
            editor.tab_size = s.tab_size.clamp(1, 16);
            editor.use_tab_character = s.tabs;
            editor.right_margin = s.margin.clamp(1, 10000)
        }
    }
    pub fn excluded(&self, path: &std::path::Path, root: Option<&std::path::Path>) -> bool {
        let relative = root
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        self.do_not_format
            .iter()
            .take(256)
            .any(|mask| glob(mask, &relative) || glob(mask, &filename))
    }
}
fn glob(pattern: &str, value: &str) -> bool {
    if pattern.len() > 4096 || value.len() > 4096 {
        return false;
    }
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    let (mut a, mut b, mut star, mut retry) = (0, 0, None, 0);
    let mut steps = 0;
    while b < v.len() {
        steps += 1;
        if steps > 131072 {
            return false;
        }
        if a < p.len() && (p[a] == b'?' || p[a] == v[b]) {
            a += 1;
            b += 1
        } else if a < p.len() && p[a] == b'*' {
            star = Some(a);
            a += 1;
            retry = b
        } else if let Some(s) = star {
            retry += 1;
            b = retry;
            a = s + 1
        } else {
            return false;
        }
    }
    while a < p.len() && p[a] == b'*' {
        a += 1
    }
    a == p.len()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemes_preserve_separate_project_default_named_and_deep_copy() {
        let mut s = Settings::default();
        s.editor.indent_size = 2;
        s.copy_code_style("Team").unwrap();
        s.editor.indent_size = 8;
        s.select_code_style("Project").unwrap();
        assert_eq!(s.editor.indent_size, 2);
        s.select_code_style("Team").unwrap();
        assert_eq!(s.editor.indent_size, 8);
        s.rename_code_style("Shared").unwrap();
        assert!(s.select_code_style("Team").is_err());
        s.select_code_style("Default").unwrap();
        assert_eq!(s.editor.indent_size, 4)
    }
    #[test]
    fn presets_and_editorconfig_round_trip_supported_python_fields() {
        for name in ["PEP 8", "Black-compatible", "Google"] {
            let mut a = Scheme::preset(name).unwrap();
            a.style.formatter_markers = true;
            a.style.keep_line_breaks = true;
            a.style.formatter_off = "KEEP".into();
            let mut b = Scheme::default();
            b.import_editorconfig(&a.editorconfig()).unwrap();
            assert_eq!(a.indent, b.indent);
            assert_eq!(a.style, b.style);
            assert_eq!(Scheme::import_json(&a.export_json().unwrap()).unwrap(), a)
        }
    }
    #[test]
    fn languages_and_exclusions_are_real_settings() {
        let mut s = CodeStyleSettings::default();
        s.languages.insert(
            "YAML".into(),
            LanguageStyle {
                indent: 2,
                ..Default::default()
            },
        );
        let mut e = EditorSettings::default();
        s.apply_language(std::path::Path::new("a.yaml"), &mut e);
        assert_eq!(e.indent_size, 2);
        s.do_not_format = vec!["generated/*".into()];
        assert!(s.excluded(
            std::path::Path::new("/p/generated/x.py"),
            Some(std::path::Path::new("/p"))
        ));
        assert!(!s.excluded(
            std::path::Path::new("/p/src/x.py"),
            Some(std::path::Path::new("/p"))
        ))
    }
}

/// Shareable Project scheme, separate from machine/user named schemes.
pub fn save_project(root: &std::path::Path, scheme: &Scheme) -> Result<(), String> {
    let directory = root.join(crate::PROJECT_DIR_NAME).join("codeStyles");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let text = scheme.export_json()?;
    // Same-directory unique staging file; never truncate an existing scheme.
    use std::io::Write;
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stage = directory.join(format!(
        ".code-style-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&stage, directory.join("Project.json")).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(stage);
    result
}
impl Settings {
    pub fn load_project_code_style(&mut self, root: &std::path::Path) -> Result<(), String> {
        if !self.code_style_schemes.named.contains_key("Default") {
            let fallback = Scheme::capture(self);
            self.code_style_schemes
                .named
                .insert("Default".into(), fallback);
        }
        let canonical = root
            .join(crate::PROJECT_DIR_NAME)
            .join("codeStyles/Project.json");
        let path = if canonical.exists() {
            canonical
        } else {
            root.join(crate::PROJECT_DIR_NAME).join("code-style.json")
        };
        let scheme = match std::fs::metadata(&path) {
            Ok(meta) => {
                if meta.len() > 1024 * 1024 {
                    return Err("Project scheme exceeds 1 MiB".into());
                }
                Scheme::import_json(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self
                .code_style_schemes
                .named
                .get("Default")
                .cloned()
                .unwrap_or_default(),
            Err(e) => return Err(e.to_string()),
        };
        self.code_style_schemes
            .named
            .insert("Project".into(), scheme.clone());
        if self.code_style_schemes.selected == "Project" {
            scheme.apply(self)
        }
        Ok(())
    }
}

impl Settings {
    pub fn formatting_excluded(
        &self,
        path: &std::path::Path,
        context: &crate::scopes::ScopeContext,
    ) -> Result<bool, String> {
        if self.code_style.excluded(path, Some(&context.root)) {
            return Ok(true);
        }
        for name in self.code_style.do_not_format_scopes.iter().take(128) {
            if self.scopes.matches(
                &crate::scopes::ScopeSelection::Named(name.clone()),
                path,
                context,
            )? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod named_scope_tests {
    use super::*;
    #[test]
    fn named_do_not_format_scopes_use_shared_scope_language() {
        let mut settings = Settings::default();
        settings.scopes.named.push(crate::scopes::NamedScope {
            name: "Generated".into(),
            pattern: "file:generated//*".into(),
            color: None,
            shared: false,
        });
        settings
            .code_style
            .do_not_format_scopes
            .push("Generated".into());
        let context = crate::scopes::ScopeContext {
            root: "/p".into(),
            ..Default::default()
        };
        assert!(settings
            .formatting_excluded(std::path::Path::new("/p/generated/model.py"), &context)
            .unwrap());
        assert!(!settings
            .formatting_excluded(std::path::Path::new("/p/src/model.py"), &context)
            .unwrap());
        settings
            .code_style
            .do_not_format_scopes
            .push("Missing".into());
        assert!(settings
            .formatting_excluded(std::path::Path::new("/p/src/model.py"), &context)
            .is_err())
    }
}

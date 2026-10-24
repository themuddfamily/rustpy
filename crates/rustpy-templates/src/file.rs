//! File and Code Templates (roadmap §1.7, Settings › Editor › File and Code
//! Templates): the templates *New* creates files from, the includes they
//! share (*File Header*), a Velocity-like renderer, and the IDE-level
//! (`<config>/fileTemplates.json`) and per-project
//! (`<project>/.rustpy/fileTemplates.json`) storage, where the project wins.
//!
//! Template syntax (a subset of Apache Velocity):
//!
//! * `${NAME}` or `$NAME` inserts a variable; an unknown one stays as written,
//!   `$!{NAME}` inserts nothing instead. `${DS}`, `$$` and `\$` give a `$`,
//!   `\#` a `#`.
//! * `#if (${VAR})` … `#elseif (…)` … `#else` … `#end`; a condition is a
//!   variable (true when set and not empty or `false`), `!`, `==`, `!=`,
//!   `&&`, `||`, string literals and parentheses.
//! * `#set ($VAR = "text with ${OTHER}")`, `#parse ("File Header.py")`.
//! * `## comment` to the end of the line, `#* block comment *#`.
//!
//! A directive alone on its line removes the whole line, so `#if` blocks and
//! an empty `#parse` leave no blank lines. Other `#` text (Python comments,
//! `#!/usr/bin/env python`) is kept as it is.
//!
//! Project templates come from the repository, so rendering is bounded: an
//! include that parses itself, a `#set` that doubles a value or a deeply
//! nested condition stops at [`MAX_RENDER_BYTES`] of work or [`MAX_INCLUDES`]
//! includes ([`try_render`] then fails with [`TooLarge`]) instead of freezing
//! the IDE.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// A file template or an include.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileTemplate {
    /// Shown in the *New* menu ("Python File"); `#parse` names an include by
    /// `name` or `name.extension` ("File Header.py").
    pub name: String,
    /// Added to the typed file name when it has none ("py"; empty: nothing).
    pub extension: String,
    /// File name the *New* dialog proposes, without the extension ("script", "Dockerfile").
    pub file_name: String,
    pub body: String,
    /// Listed in the *New* menu.
    pub enabled: bool,
    /// Reformat the new file according to the code style (Python files).
    pub reformat: bool,
    /// Activate linked `$VAR$` fields and `$END$` in the created editor.
    pub live_templates: bool,
    pub description: String,
}

impl Default for FileTemplate {
    fn default() -> Self {
        Self {
            name: String::new(),
            extension: String::new(),
            file_name: String::new(),
            body: String::new(),
            enabled: true,
            reformat: false,
            live_templates: false,
            description: String::new(),
        }
    }
}

impl FileTemplate {
    pub fn new(
        name: &str,
        extension: &str,
        file_name: &str,
        body: &str,
        description: &str,
    ) -> Self {
        Self {
            name: name.into(),
            extension: extension.into(),
            file_name: file_name.into(),
            body: body.into(),
            description: description.into(),
            ..Default::default()
        }
    }

    /// `name.extension` ("File Header.py"), or the name without an extension.
    pub fn full_name(&self) -> String {
        if self.extension.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.name, self.extension)
        }
    }

    /// The file name the *New* dialog proposes ("script.py", "Dockerfile").
    pub fn default_file_name(&self) -> String {
        let base = if self.file_name.trim().is_empty() {
            "untitled"
        } else {
            self.file_name.trim()
        };
        self.file_name_for(base)
    }

    /// `typed` with the template's extension added when it has no extension yet.
    pub fn file_name_for(&self, typed: &str) -> String {
        let typed = typed.trim();
        let last = typed.rsplit(['/', '\\']).next().unwrap_or(typed);
        if self.extension.is_empty() || last.contains('.') {
            typed.to_string()
        } else {
            format!("{typed}.{}", self.extension)
        }
    }

    /// Whether a file called `file_name` is one this template makes: any name
    /// when the template has no extension, else a name with its extension
    /// (a `py` template also makes `.pyw` scripts). Python boilerplate must
    /// not end up in `notes.txt`.
    pub fn accepts(&self, file_name: &str) -> bool {
        let ext = self
            .extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if ext.is_empty() {
            return true;
        }
        let name = file_name.trim();
        let last = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        // Spellings of the same type: `ci.yml` is a YAML file, `index.htm` an HTML file.
        let alias: &[&str] = match ext.as_str() {
            "py" => &["pyw"],
            "yaml" => &["yml"],
            "yml" => &["yaml"],
            "html" => &["htm"],
            "htm" => &["html"],
            "jpeg" => &["jpg"],
            _ => &[],
        };
        last.ends_with(&format!(".{ext}")) || alias.iter().any(|a| last.ends_with(&format!(".{a}")))
    }
}

/// Where a template of a [`TemplateSet`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Shipped with the IDE, unchanged.
    Builtin,
    /// A builtin changed in the IDE-level templates.
    Modified,
    /// Added in the IDE-level templates.
    Custom,
    /// From the project's templates (they win over the IDE's).
    Project,
}

/// What `fileTemplates.json` holds: templates and includes that differ from
/// the builtins (same name: replaces the builtin), plus the user's own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateStore {
    pub templates: Vec<FileTemplate>,
    pub includes: Vec<FileTemplate>,
}

impl TemplateStore {
    /// The templates (`include == false`) or includes list.
    pub fn list_mut(&mut self, include: bool) -> &mut Vec<FileTemplate> {
        if include {
            &mut self.includes
        } else {
            &mut self.templates
        }
    }

    /// Adds or replaces (by name) a template or include.
    pub fn upsert(&mut self, include: bool, t: FileTemplate) {
        let list = self.list_mut(include);
        match list.iter_mut().find(|x| x.name == t.name) {
            Some(x) => *x = t,
            None => list.push(t),
        }
    }

    /// Removes a template or include by name; returns whether it was there.
    pub fn remove(&mut self, include: bool, name: &str) -> bool {
        let list = self.list_mut(include);
        let before = list.len();
        list.retain(|x| x.name != name);
        list.len() != before
    }
}

/// The templates in effect: builtins, IDE-level and project templates merged.
#[derive(Clone, Debug, Default)]
pub struct TemplateSet {
    pub templates: Vec<(FileTemplate, Origin)>,
    pub includes: Vec<(FileTemplate, Origin)>,
}

fn merge_into(
    list: &mut Vec<(FileTemplate, Origin)>,
    extra: &[FileTemplate],
    replaced: Origin,
    added: Origin,
) {
    for t in extra.iter().filter(|t| !t.name.trim().is_empty()) {
        match list.iter_mut().find(|(x, _)| x.name == t.name) {
            Some(slot) => *slot = (t.clone(), replaced),
            None => list.push((t.clone(), added)),
        }
    }
}

impl TemplateSet {
    /// Builtins, then the IDE-level store, then the project store (the last one wins).
    pub fn merged(ide: &TemplateStore, project: Option<&TemplateStore>) -> TemplateSet {
        let mut set = TemplateSet {
            templates: builtin_templates()
                .into_iter()
                .map(|t| (t, Origin::Builtin))
                .collect(),
            includes: builtin_includes()
                .into_iter()
                .map(|t| (t, Origin::Builtin))
                .collect(),
        };
        merge_into(
            &mut set.templates,
            &ide.templates,
            Origin::Modified,
            Origin::Custom,
        );
        merge_into(
            &mut set.includes,
            &ide.includes,
            Origin::Modified,
            Origin::Custom,
        );
        if let Some(p) = project {
            merge_into(
                &mut set.templates,
                &p.templates,
                Origin::Project,
                Origin::Project,
            );
            merge_into(
                &mut set.includes,
                &p.includes,
                Origin::Project,
                Origin::Project,
            );
        }
        set
    }

    pub fn template(&self, name: &str) -> Option<&FileTemplate> {
        self.templates
            .iter()
            .map(|(t, _)| t)
            .find(|t| t.name == name)
    }

    /// The include `#parse` names: by `name.extension` or by `name`.
    pub fn include(&self, name: &str) -> Option<&FileTemplate> {
        let name = name.trim();
        let includes = || self.includes.iter().map(|(t, _)| t);
        includes()
            .find(|t| t.full_name() == name)
            .or_else(|| includes().find(|t| t.name == name))
    }

    /// Template names for the *New* menu, in order.
    pub fn new_menu(&self) -> Vec<String> {
        self.templates
            .iter()
            .filter(|(t, _)| t.enabled && !t.name.trim().is_empty())
            .map(|(t, _)| t.name.clone())
            .collect()
    }

    /// Renders `template` with `vars`, resolving `#parse` against the includes
    /// (cut short at the limits, see [`try_render`]).
    pub fn render(&self, template: &FileTemplate, vars: &BTreeMap<String, String>) -> String {
        render(&template.body, vars, &|name| {
            self.include(name).map(|t| t.body.clone())
        })
    }

    /// [`TemplateSet::render`], or [`TooLarge`] when the template runs away.
    pub fn try_render(
        &self,
        template: &FileTemplate,
        vars: &BTreeMap<String, String>,
    ) -> Result<String, TooLarge> {
        try_render(&template.body, vars, &|name| {
            self.include(name).map(|t| t.body.clone())
        })
    }
}

/// The IDE-level store file.
pub fn ide_store_path(config_dir: &Path) -> PathBuf {
    config_dir.join("fileTemplates.json")
}

/// A project's store file.
pub fn project_store_path(project_root: &Path) -> PathBuf {
    project_root.join(".rustpy").join("fileTemplates.json")
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Increases with every [`save_store`], so cached template lists know to reload.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Loads a store (empty when the file is missing or invalid).
pub fn load_store(path: &Path) -> TemplateStore {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Saves a store as pretty JSON, creating the parent directory.
pub fn save_store(path: &Path, store: &TemplateStore) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(store).map_err(std::io::Error::other)?;
    std::fs::write(path, text)?;
    GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// The templates in effect for a project (or for no project).
pub fn load(config_dir: &Path, project_root: Option<&Path>) -> TemplateSet {
    let ide = load_store(&ide_store_path(config_dir));
    let project = project_root.map(|r| load_store(&project_store_path(r)));
    TemplateSet::merged(&ide, project.as_ref())
}

/// Name of the builtin include every Python template starts with.
pub const FILE_HEADER: &str = "File Header";

const HEADER_LINE: &str = "#parse(\"File Header.py\")\n";

/// The *.gitignore* template's lines for the project's `.rustpy/` directory:
/// personal state (`workspace.json`, histories, coverage data) stays out of
/// Git, while the files a team can share (run configurations, data sources,
/// file templates) can be committed.
pub const GITIGNORE_RUSTPY: &str = "# RustPy: personal state only; run configurations, data sources and file templates can be shared\n**/.rustpy/*\n!**/.rustpy/runConfigurations.json\n!**/.rustpy/dataSources.json\n!**/.rustpy/fileTemplates.json\n";

/// The builtin templates, in *New* menu order.
pub fn builtin_templates() -> Vec<FileTemplate> {
    let py = |name: &str, file: &str, body: &str, desc: &str| {
        FileTemplate::new(name, "py", file, &format!("{HEADER_LINE}{body}"), desc)
    };
    vec![
        py("Python File", "script", "\"\"\"${NAME}.\n\nCreated on ${DATE}.\n\"\"\"\n\n\ndef main() -> None:\n    pass\n\n\nif __name__ == \"__main__\":\n    main()\n", "A Python module with a main() function"),
        py("Python Unit Test", "test_something", "import unittest\n\n\nclass ${CLASS_NAME}(unittest.TestCase):\n    def test_something(self) -> None:\n        self.assertEqual(True, True)  # add assertion here\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n", "A unittest.TestCase"),
        py("Python pytest", "test_something", "import pytest\n\n\ndef test_something() -> None:\n    assert True\n", "A pytest test module"),
        FileTemplate::new("Python Stub", "pyi", "module", "from typing import Any\n\ndef function(arg: Any) -> Any: ...\n", "A type stub (.pyi)"),
        py("__init__.py", "__init__", "", "A package's __init__.py"),
        py("conftest.py", "conftest", "import pytest\n\n\n@pytest.fixture\ndef sample() -> dict:\n    return {}\n", "pytest fixtures shared by a directory"),
        py("setup.py", "setup", "from setuptools import find_packages, setup\n\nsetup(\n    name=\"${PROJECT_NAME}\",\n    version=\"0.1.0\",\n    packages=find_packages(),\n    python_requires=\">=3.8\",\n    install_requires=[],\n)\n", "setuptools setup script"),
        FileTemplate::new("pyproject.toml", "toml", "pyproject", "[build-system]\nrequires = [\"setuptools>=61\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[project]\nname = \"${PROJECT_NAME}\"\nversion = \"0.1.0\"\ndescription = \"\"\nreadme = \"README.md\"\nrequires-python = \">=3.8\"\ndependencies = []\n", "Project metadata (PEP 621)"),
        FileTemplate::new("requirements.txt", "txt", "requirements", "# Project dependencies (pip install -r requirements.txt)\n", "pip requirements"),
        FileTemplate::new("Markdown File", "md", "README", "# ${NAME}\n\n", "A Markdown document"),
        FileTemplate::new("JSON File", "json", "data", "{\n}\n", "A JSON document"),
        FileTemplate::new("YAML File", "yaml", "config", "#parse(\"File Header.py\")\n", "A YAML document"),
        FileTemplate::new("JavaScript File", "js", "script", "// ${NAME}\n\n", "A JavaScript source file"),
        FileTemplate::new("CSS File", "css", "style", "/* ${NAME} */\n\n", "A CSS stylesheet"),
        FileTemplate::new("reStructuredText File", "rst", "README", "${NAME}\n====================\n\n", "A reStructuredText document"),
        FileTemplate::new("HTML File", "html", "index", "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <title>${NAME}</title>\n</head>\n<body>\n\n</body>\n</html>\n", "An HTML5 page"),
        FileTemplate::new("Dockerfile", "", "Dockerfile", "#parse(\"File Header.py\")\nFROM python:3.12-slim\n\nWORKDIR /app\nCOPY requirements.txt .\nRUN pip install --no-cache-dir -r requirements.txt\nCOPY . .\n\nCMD [\"python\", \"main.py\"]\n", "A Docker image for the project"),
        FileTemplate::new(".dockerignore", "", ".dockerignore", ".git\n.gitignore\n.dockerignore\nDockerfile*\n**/__pycache__\n**/*.py[cod]\n.venv\nvenv\n.env\n.pytest_cache\n.mypy_cache\n.ruff_cache\n.coverage\nhtmlcov\nbuild\ndist\n*.egg-info\n.idea\n.rustpy\n*.md\n!README.md\n", "Files kept out of the Docker build context"),
        FileTemplate::new(".gitignore", "", ".gitignore", &format!("__pycache__/\n*.py[cod]\n.venv/\nvenv/\n.env\nbuild/\ndist/\n*.egg-info/\n.pytest_cache/\n.mypy_cache/\n.coverage\nhtmlcov/\n.idea/\n{GITIGNORE_RUSTPY}"), "Files Git should not track"),
        FileTemplate::new(".editorconfig", "", ".editorconfig", "root = true\n\n[*]\ncharset = utf-8\nend_of_line = lf\ninsert_final_newline = true\ntrim_trailing_whitespace = true\n\n[*.py]\nindent_style = space\nindent_size = 4\nmax_line_length = 88\n", "EditorConfig code style"),
    ]
}

/// The builtin includes: *File Header* (the copyright notice when Settings ›
/// Editor › Code Style › "Add to new Python files" is on).
pub fn builtin_includes() -> Vec<FileTemplate> {
    vec![FileTemplate::new(
        FILE_HEADER,
        "py",
        "",
        "#if (${COPYRIGHT})\n${COPYRIGHT}\n\n#end\n",
        "Included by #parse(\"File Header.py\") at the top of new files",
    )]
}

/// Variables offered to templates: `(name, description)`.
pub const VARIABLES: &[(&str, &str)] = &[
    ("NAME", "File name without the extension"),
    ("FILE_NAME", "File name with the extension"),
    (
        "CLASS_NAME",
        "NAME in CamelCase (test_user_api → TestUserApi)",
    ),
    (
        "PACKAGE_NAME",
        "Dotted package of the directory (pkg.sub; empty at the project root)",
    ),
    ("DIR_PATH", "Directory relative to the project root"),
    ("PROJECT_NAME", "Name of the project"),
    ("USER", "Login name of the current user"),
    ("DATE", "Current date (2026-10-10)"),
    ("TIME", "Current time (14:05)"),
    ("YEAR", "Current year"),
    ("MONTH", "Month number (01-12)"),
    ("MONTH_NAME_SHORT", "Month name (Oct)"),
    ("MONTH_NAME_FULL", "Month name (October)"),
    ("DAY", "Day of the month (01-31)"),
    ("DAY_NAME_SHORT", "Day name (Sat)"),
    ("DAY_NAME_FULL", "Day name (Saturday)"),
    ("HOUR", "Hour (00-23)"),
    ("MINUTE", "Minute (00-59)"),
    ("SECOND", "Second (00-59)"),
    (
        "COPYRIGHT",
        "Copyright notice as a comment (empty unless Code Style › Add to new files is on)",
    ),
    ("DS", "A dollar sign"),
];

/// What a new file's variables are computed from.
#[derive(Clone, Debug, Default)]
pub struct NewFile {
    /// File name with its extension ("test_api.py").
    pub file_name: String,
    /// The new file's directory relative to the project root ("" at the root).
    pub dir: PathBuf,
    pub project_name: String,
    pub user: String,
    /// The copyright notice, already a comment for this file type (see [`copyright_comment`]).
    pub copyright: Option<String>,
}

/// `test_user-api` → `TestUserApi`.
pub fn camel_case(stem: &str) -> String {
    stem.split(['_', '-', ' ', '.'])
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// The dotted package of a directory relative to the project root, when every
/// part is a Python identifier ("pkg/sub" → "pkg.sub").
pub fn package_name(dir: &Path) -> String {
    let parts: Vec<String> = dir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let ident = |s: &str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    if parts.iter().all(|p| ident(p)) {
        parts.join(".")
    } else {
        String::new()
    }
}

/// The template variables for a new file created at `now`.
pub fn variables(file: &NewFile, now: chrono::NaiveDateTime) -> BTreeMap<String, String> {
    let name = Path::new(&file.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut v = BTreeMap::new();
    let mut set = |k: &str, val: String| {
        v.insert(k.to_string(), val);
    };
    set("CLASS_NAME", camel_case(&name));
    set("NAME", name);
    set("FILE_NAME", file.file_name.clone());
    set("PACKAGE_NAME", package_name(&file.dir));
    set("DIR_PATH", file.dir.to_string_lossy().replace('\\', "/"));
    set("PROJECT_NAME", file.project_name.clone());
    set("USER", file.user.clone());
    for (k, f) in [
        ("DATE", "%Y-%m-%d"),
        ("TIME", "%H:%M"),
        ("YEAR", "%Y"),
        ("MONTH", "%m"),
        ("MONTH_NAME_SHORT", "%b"),
        ("MONTH_NAME_FULL", "%B"),
        ("DAY", "%d"),
        ("DAY_NAME_SHORT", "%a"),
        ("DAY_NAME_FULL", "%A"),
        ("HOUR", "%H"),
        ("MINUTE", "%M"),
        ("SECOND", "%S"),
    ] {
        set(k, now.format(f).to_string());
    }
    set("COPYRIGHT", file.copyright.clone().unwrap_or_default());
    set("DS", "$".to_string());
    v
}

/// Expands the copyright notice's own variables (`$today.year`, `$today.date`,
/// `$project`, `$user`, `$file`), as Code › Update Copyright does.
pub fn expand_copyright(
    notice: &str,
    now: chrono::NaiveDateTime,
    project: &str,
    user: &str,
    file: &str,
) -> String {
    notice
        .replace("$today.year", &now.format("%Y").to_string())
        .replace("$today.date", &now.format("%Y-%m-%d").to_string())
        .replace("$project", project)
        .replace("$user", user)
        .replace("$file", file)
}

/// The notice as a comment for a file named `file_name`; `None` when that
/// file type has no comments (JSON) or is unknown.
pub fn copyright_comment(notice: &str, file_name: &str) -> Option<String> {
    let notice = notice.trim_end();
    if notice.trim().is_empty() {
        return None;
    }
    let lower = file_name.to_ascii_lowercase();
    let ext = Path::new(&lower)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lines = || notice.lines();
    let hash = || {
        Some(
            lines()
                .map(|l| {
                    if l.is_empty() {
                        "#".to_string()
                    } else {
                        format!("# {l}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    match ext.as_str() {
        "py" | "pyi" | "pyw" | "pyx" | "toml" | "yaml" | "yml" | "cfg" | "ini" | "sh" | "bash"
        | "zsh" | "r" | "rb" | "pl" | "conf" | "env" | "properties" | "in" => hash(),
        "txt" if lower.starts_with("requirements") || lower.starts_with("constraints") => hash(),
        "html" | "htm" | "xml" | "svg" | "md" | "markdown" | "vue" => Some(format!(
            "<!--\n{}\n-->",
            lines()
                .map(|l| format!("  {l}").trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )),
        "css" | "scss" | "less" | "js" | "mjs" | "ts" | "jsx" | "tsx" | "java" | "c" | "h"
        | "cpp" | "hpp" | "rs" | "go" | "kt" | "swift" => Some(format!(
            "/*\n{}\n */",
            lines()
                .map(|l| format!(" * {l}").trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )),
        "sql" | "lua" => Some(
            lines()
                .map(|l| format!("-- {l}").trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "rst" => Some(
            lines()
                .enumerate()
                .map(|(i, l)| {
                    if i == 0 {
                        format!(".. {l}")
                    } else {
                        format!("   {l}")
                    }
                    .trim_end()
                    .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "" if matches!(
            lower.as_str(),
            "dockerfile"
                | "makefile"
                | ".gitignore"
                | ".dockerignore"
                | ".editorconfig"
                | ".env"
                | ".flake8"
                | ".pylintrc"
        ) || lower.starts_with("dockerfile") =>
        {
            hash()
        }
        _ => None,
    }
}

/// Escapes text so that it renders back unchanged (Tools › Save File as Template).
pub fn escape_literal(text: &str) -> String {
    // A backslash stays as it is: before an escaped `$` or `#` it reads as
    // itself. A `#` right after a backslash is therefore escaped too, even when
    // it starts nothing: `\#` alone would render as `#` (`r"\#"` in a regex).
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        match c {
            '$' => out.push_str("\\$"),
            '#' if text[..i].ends_with('\\')
                || directive_at(&text[i..], &mut 0).is_some()
                || text[i + 1..].starts_with('#')
                || text[i + 1..].starts_with('*') =>
            {
                out.push_str("\\#")
            }
            _ => out.push(c),
        }
    }
    out
}

// ----- renderer --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Directive {
    If(String),
    ElseIf(String),
    Else,
    End,
    Set(String),
    Parse(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Text(String),
    /// `$NAME` / `${NAME}`: `(name, quiet, source text)`.
    Ref(String, bool, String),
    Directive(Directive),
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Longest directive argument (`#if (…)`, `#set (…)`): the closing parenthesis
/// is looked for this far only, so text full of unclosed `#if(` is not scanned
/// to its end again and again.
const MAX_ARGUMENT_BYTES: usize = 1024;

/// The parenthesized argument at the start of `s` (after optional spaces):
/// `(inner, bytes consumed)`.
fn paren_arg(s: &str) -> Option<(String, usize)> {
    paren_arg_counted(s, &mut 0)
}

/// [`paren_arg`], adding the bytes it scanned to `work`: text full of
/// unclosed `#if(` costs far more to tokenize than its length.
fn paren_arg_counted(s: &str, work: &mut usize) -> Option<(String, usize)> {
    let skip = s.len() - s.trim_start_matches([' ', '\t']).len();
    let rest = &s[skip..];
    if !rest.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut found = None;
    let mut scanned = 0;
    for (i, c) in rest.char_indices() {
        scanned = i + c.len_utf8();
        if i > MAX_ARGUMENT_BYTES {
            break;
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        found = Some((rest[1..i].to_string(), skip + i + 1));
                        break;
                    }
                }
                _ => {}
            },
        }
    }
    *work += skip + scanned;
    found
}

/// A directive at the start of `s` (which starts with `#`): `(directive, bytes
/// consumed)`; the bytes scanned for its argument are added to `work`.
fn directive_at(s: &str, work: &mut usize) -> Option<(Directive, usize)> {
    directive_in_line(s, work, true)
}

/// [`directive_at`]; `close_paren_ahead` false means the line has no `)` after
/// this `#`, so a directive with an argument cannot be complete (it is text,
/// found without scanning: text full of `#if(` stays linear to tokenize).
fn directive_in_line(
    s: &str,
    work: &mut usize,
    close_paren_ahead: bool,
) -> Option<(Directive, usize)> {
    let body = s.strip_prefix('#')?;
    let (word, used) = if let Some(rest) = body.strip_prefix('{') {
        // `#{else}`: only letters up to the brace (no scan for a far-away `}`).
        let end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        if !rest[end..].starts_with('}') {
            return None;
        }
        (&rest[..end], end + 2)
    } else {
        let end = body
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(body.len());
        (&body[..end], end)
    };
    let after = &body[used..];
    let bare = |d: Directive| {
        // `#else` / `#end` must not run into a word (`#endif` is text).
        if after.chars().next().is_some_and(is_ident) && !body.starts_with('{') {
            None
        } else {
            Some((d, 1 + used))
        }
    };
    match word {
        "else" => bare(Directive::Else),
        "end" => bare(Directive::End),
        "if" | "elseif" | "set" | "parse" => {
            if !close_paren_ahead {
                return None;
            }
            let (arg, n) = paren_arg_counted(after, work)?;
            let d = match word {
                "if" => Directive::If(arg),
                "elseif" => Directive::ElseIf(arg),
                "set" => Directive::Set(arg),
                _ => Directive::Parse(arg),
            };
            Some((d, 1 + used + n))
        }
        _ => None,
    }
}

/// A reference at the start of `s` (which starts with `$`): `(name, quiet, bytes consumed)`.
fn reference_at(s: &str) -> Option<(String, bool, usize)> {
    let mut rest = &s[1..];
    let mut used = 1;
    let quiet = rest.starts_with('!');
    if quiet {
        rest = &rest[1..];
        used += 1;
    }
    if let Some(inner) = rest.strip_prefix('{') {
        // `${NAME}`: only name characters up to the brace (no scan for a far-away `}`).
        let end = inner.find(|c: char| !is_ident(c)).unwrap_or(inner.len());
        let name = &inner[..end];
        if inner[end..].starts_with('}') && name.chars().next().is_some_and(is_ident_start) {
            return Some((name.to_string(), quiet, used + end + 2));
        }
        return None;
    }
    let len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
    let name = &rest[..len];
    if name.chars().next().is_some_and(is_ident_start) {
        Some((name.to_string(), quiet, used + len))
    } else {
        None
    }
}

/// Splits a template into tokens (a line holding only a directive or a `##`
/// comment disappears with its line break), with the work it took: the bytes
/// read plus the bytes scanned for directive arguments (what rendering charges).
fn tokenize_counted(src: &str) -> (Vec<Token>, usize) {
    let mut work = src.len();
    let mut out: Vec<Token> = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Token>| {
        if !text.is_empty() {
            out.push(Token::Text(std::mem::take(text)));
        }
    };
    let mut pos = 0;
    while pos < src.len() {
        let line_end = src[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(src.len());
        let line = &src[pos..line_end];
        // Where this line's last `)` is: openers after it cannot close.
        let last_close = line.rfind(')').map(|k| pos + k);
        let trimmed = line.trim_start_matches([' ', '\t']);
        // A line that is only a directive (and maybe a `##` comment) or only a comment.
        if trimmed.starts_with("##") {
            pos = line_end;
            continue;
        }
        if trimmed.starts_with('#') {
            let at = line_end - trimmed.len();
            if let Some((d, n)) =
                directive_in_line(trimmed, &mut work, last_close.is_some_and(|c| c > at))
            {
                let rest = trimmed[n..].trim_end_matches(['\n', '\r']);
                let rest = rest.trim_start_matches([' ', '\t']);
                if rest.is_empty() || rest.starts_with("##") {
                    flush(&mut text, &mut out);
                    out.push(Token::Directive(d));
                    pos = line_end;
                    continue;
                }
            }
        }
        // Inline scanning of this line.
        let mut i = pos;
        while i < line_end {
            let rest = &src[i..];
            let c = rest.chars().next().unwrap();
            if c == '\\' && (rest[1..].starts_with('$') || rest[1..].starts_with('#')) {
                text.push(rest.as_bytes()[1] as char);
                i += 2;
            } else if c == '$' && rest[1..].starts_with('$') {
                text.push('$');
                i += 2;
            } else if c == '$' {
                match reference_at(rest) {
                    Some((name, quiet, n)) => {
                        flush(&mut text, &mut out);
                        out.push(Token::Ref(name, quiet, rest[..n].to_string()));
                        i += n;
                    }
                    None => {
                        text.push('$');
                        i += 1;
                    }
                }
            } else if c == '#' && rest[1..].starts_with('#') {
                // `## comment`: up to the end of the line (the line break stays).
                let end = rest.find('\n').map(|e| i + e).unwrap_or(src.len());
                i = end;
            } else if c == '#' && rest[1..].starts_with('*') {
                let end = rest[2..]
                    .find("*#")
                    .map(|e| i + 2 + e + 2)
                    .unwrap_or(src.len());
                i = end;
            } else if c == '#' {
                match directive_in_line(rest, &mut work, last_close.is_some_and(|c| c > i)) {
                    Some((d, n)) => {
                        flush(&mut text, &mut out);
                        out.push(Token::Directive(d));
                        i += n;
                    }
                    None => {
                        text.push('#');
                        i += 1;
                    }
                }
            } else {
                text.push(c);
                i += c.len_utf8();
            }
        }
        // A block comment may have run past this line.
        pos = i.max(line_end);
    }
    flush(&mut text, &mut out);
    (out, work)
}

#[derive(Debug)]
enum Node {
    Text(String),
    Ref(String, bool, String),
    If(Branches),
    Set(String),
    Parse(String),
}

/// The branches of an `#if`: `(condition or None for #else, body)`.
type Branches = Vec<(Option<String>, Vec<Node>)>;

/// An `#if` being built: (finished branches, current condition, current body).
type OpenIf = (Branches, Option<String>, Vec<Node>);

/// Deepest `#if` nesting a template may have: evaluating (and dropping) the
/// tree recurses once per level.
const MAX_IF_DEPTH: usize = 64;

/// Builds the tree; a stray `#else`/`#end` is kept as text, an unclosed `#if` is closed at the end.
fn build(tokens: Vec<Token>) -> Vec<Node> {
    build_checked(tokens).0
}

/// [`build`], and whether the `#if` nesting went past [`MAX_IF_DEPTH`] (the
/// rest of the template is then left out).
fn build_checked(tokens: Vec<Token>) -> (Vec<Node>, bool) {
    let mut too_deep = false;
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<OpenIf> = Vec::new();
    fn current<'a>(root: &'a mut Vec<Node>, stack: &'a mut [OpenIf]) -> &'a mut Vec<Node> {
        match stack.last_mut() {
            Some((_, _, body)) => body,
            None => root,
        }
    }
    for t in tokens {
        match t {
            Token::Text(s) => current(&mut root, &mut stack).push(Node::Text(s)),
            Token::Ref(n, q, raw) => current(&mut root, &mut stack).push(Node::Ref(n, q, raw)),
            Token::Directive(Directive::If(c)) => {
                if stack.len() >= MAX_IF_DEPTH {
                    too_deep = true;
                    break;
                }
                stack.push((Vec::new(), Some(c), Vec::new()));
            }
            Token::Directive(Directive::ElseIf(c)) => match stack.last_mut() {
                Some((branches, cond, body)) => {
                    branches.push((cond.take(), std::mem::take(body)));
                    *cond = Some(c);
                }
                None => current(&mut root, &mut stack).push(Node::Text(format!("#elseif({c})"))),
            },
            Token::Directive(Directive::Else) => match stack.last_mut() {
                Some((branches, cond, body)) => {
                    branches.push((cond.take(), std::mem::take(body)));
                }
                None => current(&mut root, &mut stack).push(Node::Text("#else".into())),
            },
            Token::Directive(Directive::End) => match stack.pop() {
                Some((mut branches, cond, body)) => {
                    branches.push((cond, body));
                    current(&mut root, &mut stack).push(Node::If(branches));
                }
                None => current(&mut root, &mut stack).push(Node::Text("#end".into())),
            },
            Token::Directive(Directive::Set(e)) => {
                current(&mut root, &mut stack).push(Node::Set(e))
            }
            Token::Directive(Directive::Parse(e)) => {
                current(&mut root, &mut stack).push(Node::Parse(e))
            }
        }
    }
    while let Some((mut branches, cond, body)) = stack.pop() {
        branches.push((cond, body));
        current(&mut root, &mut stack).push(Node::If(branches));
    }
    (root, too_deep)
}

/// Splits `expr` at top-level occurrences of `op` (outside quotes and parentheses).
fn split_top(expr: &str, op: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in expr.char_indices() {
        if i < start {
            continue; // inside the operator just split at
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0 && expr[i..].starts_with(op) => {
                    parts.push(expr[start..i].to_string());
                    start = i + op.len();
                }
                _ => {}
            },
        }
    }
    parts.push(expr[start..].to_string());
    parts
}

/// Work one render may do, in bytes: the output, the values `#set` and
/// conditions compute, the includes it reads and the conditions it scans
/// (real templates need a few kilobytes).
pub const MAX_RENDER_BYTES: usize = 8 << 20;
/// `#parse` includes one render may expand, nested ones included.
pub const MAX_INCLUDES: usize = 64;
/// How deep includes may include others; deeper `#parse`s are skipped (an
/// include that parses itself once stops here quietly).
const MAX_PARSE_DEPTH: usize = 8;
/// How deep `!`, parentheses, `&&` and `||` may nest in a condition.
const MAX_CONDITION_DEPTH: usize = 64;

/// A template stopped at a limit of [`try_render`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the template does too much work (over {} MB, {MAX_INCLUDES} #parse includes, {MAX_IF_DEPTH} nested #if blocks or {MAX_CONDITION_DEPTH} nested conditions); does an include parse itself?", MAX_RENDER_BYTES >> 20)
    }
}

impl std::error::Error for TooLarge {}

struct Ctx<'a> {
    vars: BTreeMap<String, String>,
    includes: &'a dyn Fn(&str) -> Option<String>,
    depth: usize,
    /// Bytes of work left (see [`MAX_RENDER_BYTES`]).
    budget: usize,
    /// `#parse` expansions left (see [`MAX_INCLUDES`]).
    includes_left: usize,
    /// A limit was reached: evaluation stops, the result is [`TooLarge`].
    stopped: bool,
}

impl Ctx<'_> {
    /// Spends `n` bytes of work; false, and rendering stops, when the budget is used up.
    fn charge(&mut self, n: usize) -> bool {
        if self.stopped || n > self.budget {
            self.stopped = true;
            return false;
        }
        self.budget -= n;
        true
    }

    fn emit(&mut self, out: &mut String, s: &str) {
        if self.charge(s.len()) {
            out.push_str(s);
        }
    }

    /// Appends a reference's value (its source text when it is unknown and not quiet).
    fn emit_ref(&mut self, out: &mut String, name: &str, quiet: bool, raw: &str) {
        let text = match self.vars.get(name) {
            Some(v) => v.as_str(),
            None if quiet => "",
            None => raw,
        };
        if self.stopped || text.len() > self.budget {
            self.stopped = true;
        } else {
            self.budget -= text.len();
            out.push_str(text);
        }
    }

    /// The value of an operand: a reference, a string literal, `true`/`false` or a number.
    fn operand(&mut self, s: &str) -> Option<String> {
        let s = s.trim();
        if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            return Some(self.interpolate(inner));
        }
        if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
            return Some(inner.to_string());
        }
        if s.starts_with('$') {
            let (name, _, _) = reference_at(s).filter(|(_, _, n)| *n == s.len())?;
            // Copying a value is work too (`#set ($A = $B)` on a long value, many times).
            let len = self.vars.get(&name)?.len();
            if !self.charge(len) {
                return None;
            }
            return self.vars.get(&name).cloned();
        }
        if s == "true" || s == "false" || s.parse::<f64>().is_ok() {
            return Some(s.to_string());
        }
        None
    }

    fn condition(&mut self, expr: &str, depth: usize) -> bool {
        // Every level scans the expression again: that is work, and the nesting is bounded.
        if depth > MAX_CONDITION_DEPTH {
            self.stopped = true;
        }
        if !self.charge(expr.len()) {
            return false;
        }
        let ors = split_top(expr, "||");
        if ors.len() > 1 {
            for e in &ors {
                if self.condition(e, depth + 1) {
                    return true;
                }
            }
            return false;
        }
        let ands = split_top(expr, "&&");
        if ands.len() > 1 {
            for e in &ands {
                if !self.condition(e, depth + 1) {
                    return false;
                }
            }
            return true;
        }
        let e = expr.trim();
        if let Some(inner) = e.strip_prefix('!') {
            if !inner.starts_with('=') {
                return !self.condition(inner, depth + 1);
            }
        }
        if e.starts_with('(') && e.ends_with(')') && paren_arg(e).is_some_and(|(_, n)| n == e.len())
        {
            return self.condition(&e[1..e.len() - 1], depth + 1);
        }
        for (op, eq) in [("==", true), ("!=", false)] {
            let parts = split_top(e, op);
            if parts.len() == 2 {
                let a = self.operand(&parts[0]).unwrap_or_default();
                let b = self.operand(&parts[1]).unwrap_or_default();
                return (a == b) == eq;
            }
        }
        self.operand(e)
            .is_some_and(|v| !v.is_empty() && v != "false")
    }

    /// `"text with ${VAR}"` inside a `#set` or a condition.
    fn interpolate(&mut self, s: &str) -> String {
        let (tokens, work) = tokenize_counted(s);
        if !self.charge(work) {
            return String::new();
        }
        let nodes = build(tokens);
        let mut out = String::new();
        for n in &nodes {
            match n {
                Node::Text(t) => self.emit(&mut out, t),
                Node::Ref(name, quiet, raw) => self.emit_ref(&mut out, name, *quiet, raw),
                _ => {}
            }
        }
        out
    }

    fn eval(&mut self, nodes: &[Node], out: &mut String) {
        for n in nodes {
            if self.stopped {
                return;
            }
            match n {
                Node::Text(t) => self.emit(out, t),
                Node::Ref(name, quiet, raw) => self.emit_ref(out, name, *quiet, raw),
                Node::If(branches) => {
                    for (cond, body) in branches {
                        if cond.as_ref().is_none_or(|c| self.condition(c, 0)) {
                            self.eval(body, out);
                            break;
                        }
                    }
                }
                Node::Set(expr) => {
                    if let Some((lhs, rhs)) = expr.split_once('=') {
                        let lhs = lhs.trim();
                        if let Some((name, _, _)) =
                            reference_at(lhs).filter(|(_, _, n)| *n == lhs.len())
                        {
                            let value = self.operand(rhs).unwrap_or_default();
                            self.vars.insert(name, value);
                        }
                    }
                }
                Node::Parse(expr) => {
                    // Includes may include others, but not forever.
                    if self.depth >= MAX_PARSE_DEPTH {
                        continue;
                    }
                    let Some(name) = self.operand(expr) else {
                        continue;
                    };
                    if let Some(body) = (self.includes)(&name) {
                        // Reading an include is work too (scanning included), and their
                        // number and nesting are bounded.
                        if self.includes_left == 0 {
                            self.stopped = true;
                            return;
                        }
                        let (tokens, work) = tokenize_counted(&body);
                        if !self.charge(work) {
                            return;
                        }
                        self.includes_left -= 1;
                        let (nodes, too_deep) = build_checked(tokens);
                        if too_deep {
                            self.stopped = true;
                            return;
                        }
                        self.depth += 1;
                        self.eval(&nodes, out);
                        self.depth -= 1;
                    }
                }
            }
        }
    }
}

/// Renders `body` with `vars`: `(text, whether a limit stopped it early)`.
fn render_bounded(
    body: &str,
    vars: &BTreeMap<String, String>,
    includes: &dyn Fn(&str) -> Option<String>,
) -> (String, bool) {
    let (tokens, work) = tokenize_counted(body);
    let (nodes, too_deep) = build_checked(tokens);
    let mut ctx = Ctx {
        vars: vars.clone(),
        includes,
        depth: 0,
        budget: MAX_RENDER_BYTES,
        includes_left: MAX_INCLUDES,
        stopped: too_deep,
    };
    let mut out = String::new();
    if ctx.charge(work) {
        ctx.eval(&nodes, &mut out);
    }
    (out, ctx.stopped)
}

/// Renders a template body. `includes` returns an include's body by the name
/// `#parse` gives. A template that reaches a limit of [`try_render`] is cut
/// short there.
pub fn render(
    body: &str,
    vars: &BTreeMap<String, String>,
    includes: &dyn Fn(&str) -> Option<String>,
) -> String {
    render_bounded(body, vars, includes).0
}

/// [`render`], or [`TooLarge`] when the template does too much work: more than
/// [`MAX_RENDER_BYTES`] of output, `#set` values, includes and conditions,
/// more than [`MAX_INCLUDES`] includes, or conditions nested too deeply.
pub fn try_render(
    body: &str,
    vars: &BTreeMap<String, String>,
    includes: &dyn Fn(&str) -> Option<String>,
) -> Result<String, TooLarge> {
    match render_bounded(body, vars, includes) {
        (out, false) => Ok(out),
        (_, true) => Err(TooLarge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn r(body: &str, v: &[(&str, &str)]) -> String {
        render(body, &vars(v), &|_| None)
    }

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 10)
            .unwrap()
            .and_hms_opt(14, 5, 9)
            .unwrap()
    }

    #[test]
    fn references() {
        let v = [("NAME", "api"), ("USER", "jane"), ("DS", "$")];
        assert_eq!(r("${NAME}.py by $USER", &v), "api.py by jane");
        assert_eq!(
            r("[${UNKNOWN}] [$UNKNOWN] [$!{UNKNOWN}] [$!UNKNOWN]", &v),
            "[${UNKNOWN}] [$UNKNOWN] [] []"
        );
        assert_eq!(
            r("cost: ${DS}5, $$5, \\$5, 5$, $ 5, ${ NAME }", &v),
            "cost: $5, $5, $5, 5$, $ 5, ${ NAME }"
        );
        assert_eq!(r("$NAME-test $NAME.upper()", &v), "api-test api.upper()");
    }

    #[test]
    fn python_comments_and_shebangs_are_text() {
        let src = "#!/usr/bin/env python\n# -*- coding: utf-8 -*-\n# if this were a directive...\nx = 1  #: doc\n#endif\nprint(\"#set\")\n";
        assert_eq!(r(src, &[]), src);
    }

    #[test]
    fn if_else_with_whole_line_directives() {
        let src = "a\n#if (${AUTHOR})\nby ${AUTHOR}\n#elseif ($TEAM == \"core\")\ncore team\n#else\n  anonymous\n#end\nz\n";
        assert_eq!(r(src, &[("AUTHOR", "jane")]), "a\nby jane\nz\n");
        assert_eq!(r(src, &[("TEAM", "core")]), "a\ncore team\nz\n");
        assert_eq!(r(src, &[("TEAM", "web")]), "a\n  anonymous\nz\n");
        // Empty and "false" values are false; operators.
        let c = |cond: &str, v: &[(&str, &str)]| r(&format!("#if ({cond})yes#{{else}}no#end"), v);
        assert_eq!(c("$X", &[("X", "")]), "no");
        assert_eq!(c("$X", &[("X", "false")]), "no");
        assert_eq!(c("!$X", &[]), "yes");
        assert_eq!(c("$A && $B", &[("A", "1")]), "no");
        assert_eq!(c("$A || $B", &[("A", "1")]), "yes");
        assert_eq!(
            c("$A != 'x' && ($B || $C)", &[("A", "y"), ("C", "1")]),
            "yes"
        );
        assert_eq!(c("${NAME} == \"api\"", &[("NAME", "api")]), "yes");
        // Nesting, inline blocks, an unclosed #if and a stray #end.
        assert_eq!(r("#if($A)a#if($B)b#end!#end.", &[("A", "1")]), "a!.");
        assert_eq!(r("#if($A)\nopen\n", &[("A", "1")]), "open\n");
        assert_eq!(r("x #end y", &[]), "x #end y");
    }

    #[test]
    fn set_parse_and_comments() {
        let includes = |name: &str| match name {
            "File Header.py" => Some("# (c) ${AUTHOR}\n".to_string()),
            "Loop" => Some("#parse(\"Loop\")x".to_string()),
            _ => None,
        };
        let src = "#set ($AUTHOR = \"${USER} <${USER}@example.com>\")\n#parse(\"File Header.py\")\n## a template comment\nbody #* hidden *#text ## trailing\n#parse('Missing')\n#parse(\"Loop\")\n";
        let out = render(src, &vars(&[("USER", "jane")]), &includes);
        assert_eq!(out, "# (c) jane <jane@example.com>\nbody text \nxxxxxxxx");
    }

    #[test]
    fn builtin_python_file_renders_as_before() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let file = NewFile {
            file_name: "script.py".into(),
            dir: PathBuf::new(),
            project_name: "proj".into(),
            user: "jane".into(),
            copyright: None,
        };
        let v = variables(&file, now());
        let t = set.template("Python File").unwrap();
        assert_eq!(set.render(t, &v), "\"\"\"script.\n\nCreated on 2026-10-10.\n\"\"\"\n\n\ndef main() -> None:\n    pass\n\n\nif __name__ == \"__main__\":\n    main()\n");
        let test = NewFile {
            file_name: "test_user_api.py".into(),
            ..file.clone()
        };
        let ut = set.render(
            set.template("Python Unit Test").unwrap(),
            &variables(&test, now()),
        );
        assert!(
            ut.starts_with("import unittest\n\n\nclass TestUserApi(unittest.TestCase):\n"),
            "{ut}"
        );
        assert_eq!(
            set.render(
                set.template("Markdown File").unwrap(),
                &variables(
                    &NewFile {
                        file_name: "README.md".into(),
                        ..file.clone()
                    },
                    now()
                )
            ),
            "# README\n\n"
        );
        assert!(set
            .render(set.template("setup.py").unwrap(), &v)
            .contains("name=\"proj\","));
        // Every builtin renders without leftovers.
        for (t, _) in &set.templates {
            let out = set.render(t, &v);
            assert!(
                !out.contains("#parse") && !out.contains("${"),
                "{}: {out}",
                t.name
            );
        }
    }

    #[test]
    fn file_header_adds_the_copyright_notice() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let notice = expand_copyright(
            "Copyright (c) $today.year $user.\nAll rights reserved.",
            now(),
            "proj",
            "jane",
            "a.py",
        );
        let file = NewFile {
            file_name: "a.py".into(),
            copyright: copyright_comment(&notice, "a.py"),
            ..Default::default()
        };
        let out = set.render(
            set.template("Python pytest").unwrap(),
            &variables(&file, now()),
        );
        assert!(
            out.starts_with(
                "# Copyright (c) 2026 jane.\n# All rights reserved.\n\nimport pytest\n"
            ),
            "{out}"
        );
        let docker = NewFile {
            file_name: "Dockerfile".into(),
            copyright: copyright_comment(&notice, "Dockerfile"),
            ..Default::default()
        };
        assert!(set
            .render(
                set.template("Dockerfile").unwrap(),
                &variables(&docker, now())
            )
            .starts_with("# Copyright (c) 2026 jane.\n"));
        assert_eq!(copyright_comment(&notice, "data.json"), None);
        assert_eq!(
            copyright_comment("(c) me", "index.html").as_deref(),
            Some("<!--\n  (c) me\n-->")
        );
        assert_eq!(
            copyright_comment("(c) me", "app.js").as_deref(),
            Some("/*\n * (c) me\n */")
        );
        assert_eq!(
            copyright_comment("(c) me\n\nmore", "requirements-dev.txt").as_deref(),
            Some("# (c) me\n#\n# more")
        );
        assert_eq!(copyright_comment("  ", "a.py"), None);
    }

    #[test]
    fn project_templates_win_over_ide_templates_over_builtins() {
        let mut ide = TemplateStore::default();
        ide.upsert(
            false,
            FileTemplate::new("Python File", "py", "module", "# ide\n", ""),
        );
        ide.upsert(
            false,
            FileTemplate::new(
                "Django Model",
                "py",
                "models",
                "from django.db import models\n",
                "",
            ),
        );
        ide.upsert(
            true,
            FileTemplate::new(FILE_HEADER, "py", "", "# header by ${USER}\n", ""),
        );
        let mut project = TemplateStore::default();
        project.upsert(
            false,
            FileTemplate::new("Django Model", "py", "models", "# project model\n", ""),
        );
        project.upsert(
            false,
            FileTemplate::new("Fixture", "json", "fixture", "[]\n", ""),
        );
        let set = TemplateSet::merged(&ide, Some(&project));
        let origin = |n: &str| {
            set.templates
                .iter()
                .find(|(t, _)| t.name == n)
                .map(|(_, o)| *o)
        };
        assert_eq!(origin("Python File"), Some(Origin::Modified));
        assert_eq!(origin("Django Model"), Some(Origin::Project));
        assert_eq!(origin("Fixture"), Some(Origin::Project));
        assert_eq!(origin("Python pytest"), Some(Origin::Builtin));
        assert_eq!(
            set.template("Django Model").unwrap().body,
            "# project model\n"
        );
        assert_eq!(
            set.include("File Header.py").unwrap().body,
            "# header by ${USER}\n"
        );
        assert_eq!(
            set.include("File Header").unwrap().body,
            "# header by ${USER}\n"
        );
        let menu = set.new_menu();
        assert_eq!(menu.first().map(String::as_str), Some("Python File"));
        assert!(menu.contains(&"Fixture".to_string()));
        // Disabled templates leave the New menu; an IDE store without a project.
        let mut hidden = ide.clone();
        let mut t = set.template(".gitignore").unwrap().clone();
        t.enabled = false;
        hidden.upsert(false, t);
        assert!(!TemplateSet::merged(&hidden, None)
            .new_menu()
            .contains(&".gitignore".to_string()));
        assert!(hidden.remove(false, "Django Model") && !hidden.remove(false, "Django Model"));
    }

    #[test]
    fn stores_round_trip_and_bump_the_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = project_store_path(dir.path());
        assert_eq!(load_store(&path), TemplateStore::default());
        let mut store = TemplateStore::default();
        store.upsert(
            false,
            FileTemplate::new("Fixture", "json", "fixture", "[]\n", "test data"),
        );
        let before = generation();
        save_store(&path, &store).unwrap();
        assert!(generation() > before);
        assert_eq!(load_store(&path), store);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_store(&path), TemplateStore::default());
        let set = load(dir.path(), Some(dir.path()));
        assert!(set.template("Python File").is_some());
    }

    #[test]
    fn file_names_and_variables() {
        let py = FileTemplate::new("Python File", "py", "script", "", "");
        assert_eq!(py.default_file_name(), "script.py");
        assert_eq!(py.file_name_for("tool"), "tool.py");
        assert_eq!(py.file_name_for("tool.pyw"), "tool.pyw");
        assert_eq!(py.file_name_for("sub.dir/tool"), "sub.dir/tool.py");
        let docker = FileTemplate::new("Dockerfile", "", "Dockerfile", "", "");
        assert_eq!(docker.default_file_name(), "Dockerfile");
        assert_eq!(
            FileTemplate::new("x", "md", "", "", "").default_file_name(),
            "untitled.md"
        );
        let v = variables(
            &NewFile {
                file_name: "test_api.py".into(),
                dir: PathBuf::from("pkg").join("sub"),
                project_name: "proj".into(),
                user: "jane".into(),
                copyright: None,
            },
            now(),
        );
        assert_eq!(v["NAME"], "test_api");
        assert_eq!(v["CLASS_NAME"], "TestApi");
        assert_eq!(v["PACKAGE_NAME"], "pkg.sub");
        assert_eq!(v["DIR_PATH"], "pkg/sub");
        assert_eq!(
            (
                v["DATE"].as_str(),
                v["TIME"].as_str(),
                v["YEAR"].as_str(),
                v["MONTH_NAME_SHORT"].as_str(),
                v["DAY_NAME_FULL"].as_str()
            ),
            ("2026-10-10", "14:05", "2026", "Oct", "Saturday")
        );
        assert_eq!(v["COPYRIGHT"], "");
        assert_eq!(package_name(Path::new("my dir/pkg")), "");
        for (name, _) in VARIABLES {
            assert!(v.contains_key(*name), "{name}");
        }
    }

    #[test]
    fn escaped_files_render_back_unchanged() {
        let text = "#!/usr/bin/env python\n## Section\nprice = \"$5 ${x} $$\"  # cost\n#if(x) #end \\$ #* not a comment *#\nend\n";
        let escaped = escape_literal(text);
        assert_eq!(render(&escaped, &vars(&[("x", "X")]), &|_| None), text);
    }

    #[test]
    fn a_backslash_before_a_plain_hash_survives_save_as_template() {
        // `\#` reads as an escaped `#`, so the backslash used to be lost.
        for text in [
            "pattern = re.compile(r\"\\#\")\n",
            "echo \\# x\n",
            "x = '\\#'\n",
            "a\\\\#b \\#if(x) \\\\$y \\$z\n",
            "\\#\n#\\#\\\n",
        ] {
            let escaped = escape_literal(text);
            assert_eq!(
                try_render(
                    &escaped,
                    &vars(&[("x", "X"), ("y", "Y"), ("z", "Z")]),
                    &|_| Some("INCLUDED".into())
                )
                .as_deref(),
                Ok(text),
                "escaped as {escaped:?}"
            );
        }
    }

    #[test]
    fn any_text_survives_save_as_template() {
        // Random text made of the renderer's special characters and words.
        const PIECES: &[&str] = &[
            "#", "$", "\\", "!", "{", "}", "*", "(", ")", "\"", "'", "=", "==", "&&", "||", "if",
            "elseif", "else", "end", "set", "parse", "X", "NAME", "_", "1", " ", "\t", "\n", "##",
            "#*", "*#", "$!", "${", "#{", "\\#", "\\$", "$$", "é",
        ];
        let vars = vars(&[("X", "1"), ("NAME", "api"), ("if", "IF"), ("set", "SET")]);
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..20_000 {
            let len = 1 + next(30);
            let text: String = (0..len).map(|_| PIECES[next(PIECES.len())]).collect();
            let escaped = escape_literal(&text);
            assert_eq!(
                try_render(&escaped, &vars, &|_| Some("INCLUDED".into())).as_deref(),
                Ok(text.as_str()),
                "escaped as {escaped:?}"
            );
        }
    }

    #[test]
    fn runaway_templates_stop_at_a_limit() {
        let start = std::time::Instant::now();
        let none = BTreeMap::new();
        // An include that parses itself ten times would write 10^8 copies of itself.
        let bomb = format!("{}x", "#parse(\"A\")".repeat(10));
        let includes = |name: &str| (name == "A").then(|| bomb.clone());
        assert_eq!(try_render("#parse(\"A\")", &none, &includes), Err(TooLarge));
        assert!(render("#parse(\"A\")", &none, &includes).len() <= MAX_RENDER_BYTES);
        let set = TemplateSet {
            templates: vec![(
                FileTemplate::new("Bomb", "py", "bomb", "#parse(\"A\")", ""),
                Origin::Project,
            )],
            includes: vec![(FileTemplate::new("A", "", "", &bomb, ""), Origin::Project)],
        };
        assert_eq!(
            set.try_render(set.template("Bomb").unwrap(), &none),
            Err(TooLarge)
        );
        // A #set that doubles its value, and copies of a long value.
        let doubling = format!(
            "#set ($A = \"ab\")\n{}${{A}}\n",
            "#set ($A = \"$A$A\")\n".repeat(64)
        );
        assert_eq!(try_render(&doubling, &none, &|_| None), Err(TooLarge));
        let long = vars(&[("B", &"b".repeat(1 << 20))]);
        assert_eq!(
            try_render(&"#set ($A = $B)\n".repeat(100), &long, &|_| None),
            Err(TooLarge)
        );
        assert_eq!(
            try_render(&"#if ($B == $B)y#end".repeat(100), &long, &|_| None),
            Err(TooLarge)
        );
        // Conditions nested too deeply (they used to recurse once per level).
        let x = vars(&[("X", "1")]);
        assert_eq!(
            try_render(
                &format!("#if ({}$X{})y#end", "(".repeat(200), ")".repeat(200)),
                &x,
                &|_| None
            ),
            Err(TooLarge)
        );
        assert_eq!(
            try_render(&format!("#if ({}$X)y#end", "!".repeat(500)), &x, &|_| None),
            Err(TooLarge)
        );
        // Openers that never close are not scanned to the end of the text once each.
        for src in [
            "#if(".repeat(50_000),
            "#set (\"".repeat(50_000),
            "${".repeat(300_000),
            "$!{".repeat(200_000),
            "#{".repeat(300_000),
        ] {
            assert_eq!(
                try_render(&src, &none, &|_| None).as_deref(),
                Ok(src.as_str())
            );
        }
        // Normal templates are not affected, nor is an include parsed once per level.
        assert_eq!(
            try_render("#if (((($X))) && !(!$X))yes#end", &x, &|_| None).as_deref(),
            Ok("yes")
        );
        let loop_once = |name: &str| (name == "Loop").then(|| "#parse(\"Loop\")x".to_string());
        assert_eq!(
            try_render("#parse(\"Loop\")", &none, &loop_once).as_deref(),
            Ok("xxxxxxxx")
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn the_dockerignore_template_follows_the_dockerfile_in_the_new_menu() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let menu = set.new_menu();
        let at = |name: &str| menu.iter().position(|item| item == name).unwrap();
        assert_eq!(at(".dockerignore"), at("Dockerfile") + 1);
        let template = set.template(".dockerignore").unwrap();
        assert_eq!(template.default_file_name(), ".dockerignore");
        let text = set.render(template, &BTreeMap::new());
        let lines: Vec<&str> = text.lines().collect();
        // VCS metadata, virtualenvs and bytecode stay out of the context; the
        // README survives the `*.md` exclusion through a later exception.
        for pattern in [".git", ".venv", "**/__pycache__", "*.md", "!README.md"] {
            assert!(lines.contains(&pattern), "{pattern}: {text}");
        }
        assert!(at_line(&lines, "*.md") < at_line(&lines, "!README.md"));
        // Docker reads column-1 `#` lines as comments: the copyright notice too.
        assert_eq!(
            copyright_comment("(c) me", ".dockerignore").as_deref(),
            Some("# (c) me")
        );
    }

    fn at_line(lines: &[&str], line: &str) -> usize {
        lines.iter().position(|l| *l == line).unwrap()
    }

    #[test]
    fn the_gitignore_template_keeps_the_shareable_project_files() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let text = set.render(set.template(".gitignore").unwrap(), &BTreeMap::new());
        let lines: Vec<&str> = text.lines().collect();
        // Personal state is ignored, but not the whole directory: a negated pattern
        // only works when the directory itself is not excluded.
        // `**/` matches at any depth: a nested project's personal state stays out too.
        assert!(
            lines.contains(&"**/.rustpy/*")
                && !lines.contains(&".rustpy/")
                && !lines.contains(&".rustpy"),
            "{text}"
        );
        for shared in [
            "runConfigurations.json",
            "dataSources.json",
            "fileTemplates.json",
        ] {
            assert!(
                lines.contains(&format!("!**/.rustpy/{shared}").as_str()),
                "{shared}: {text}"
            );
        }
        assert!(
            lines.contains(&"__pycache__/") && lines.contains(&".venv/"),
            "{text}"
        );
    }

    #[test]
    fn templates_accept_only_names_with_their_extension() {
        let py = FileTemplate::new("Python File", "py", "script", "", "");
        assert!(py.accepts("tool.py") && py.accepts("pkg/Tool.PY") && py.accepts("gui.pyw"));
        assert!(
            !py.accepts("notes.txt") && !py.accepts("Makefile") && !py.accepts("archive.py.bak")
        );
        assert!(
            py.accepts(&py.file_name_for("Makefile")),
            "the extension is added to a bare name"
        );
        let docker = FileTemplate::new("Dockerfile", "", "Dockerfile", "", "");
        assert!(docker.accepts("Dockerfile") && docker.accepts("Dockerfile.dev"));
        assert!(
            FileTemplate::new("Stub", "pyi", "m", "", "").accepts("m.pyi")
                && !FileTemplate::new("Stub", "pyi", "m", "", "").accepts("m.py")
        );
    }

    #[test]
    fn deeply_nested_ifs_stop_instead_of_overflowing_the_stack() {
        // Review: a project template with thousands of nested `#if` crashed the IDE.
        let deep = "#if(true)\n".repeat(100_000) + "x\n" + &"#end\n".repeat(100_000);
        let none = BTreeMap::new();
        assert!(try_render(&deep, &none, &|_| None).is_err());
        // The same through an include.
        let includes = |name: &str| (name == "Deep").then(|| deep.clone());
        assert!(try_render("#parse(\"Deep\")", &none, &includes).is_err());
        // Ordinary nesting renders.
        let ok = "#if(true)\n".repeat(10) + "x\n" + &"#end\n".repeat(10);
        assert_eq!(try_render(&ok, &none, &|_| None).unwrap(), "x\n");
    }

    #[test]
    fn scanning_unclosed_directives_is_charged_as_work() {
        // Review: `#if(` openers that never close made tokenizing cost ~256 times
        // what the byte budget charged; 64 includes of such text froze the UI.
        let openers = "#if(".repeat(250) + "\n";
        let (_, work) = tokenize_counted(&openers);
        assert!(
            work < openers.len() * 2,
            "no `)` ahead: nothing to scan ({work} for {} bytes)",
            openers.len()
        );
        // With a `)` at the end of the line, every opener scans (up to its limit): charged.
        let closing = "#if(".repeat(250) + ")\n";
        let (_, work) = tokenize_counted(&closing);
        assert!(
            work > closing.len() * 100,
            "work {work} for {} bytes",
            closing.len()
        );
        let body = closing.repeat(64);
        let includes = |name: &str| (name == "A").then(|| body.clone());
        let start = std::time::Instant::now();
        let r = try_render(&"#parse(\"A\")\n".repeat(64), &BTreeMap::new(), &includes);
        assert!(r.is_err(), "the work budget stops it");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn file_types_accept_their_other_spellings() {
        let yaml = FileTemplate::new("YAML File", "yaml", "config", "", "");
        assert!(
            yaml.accepts("ci.yml")
                && yaml.accepts("docker-compose.yaml")
                && !yaml.accepts("notes.txt")
        );
        let html = FileTemplate::new("HTML File", "html", "index", "", "");
        assert!(html.accepts("index.htm") && html.accepts("page.HTML"));
        let py = FileTemplate::new("Python File", "py", "main", "", "");
        assert!(py.accepts("tool.pyw") && !py.accepts("README.md"));
    }
}

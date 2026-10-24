//! Source and schema completion for data documents; no tool execution.
use super::*;
fn item(label: &str, insert: String, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        insert,
        detail: detail.into(),
        kind: CompletionKind::Variable,
        caret_offset: None,
        priority: 0,
    }
}
fn entries(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("export ")
                .unwrap_or(line.trim())
                .split_once('=')
                .map(|(key, _)| key.trim().to_owned())
        })
        .filter(|key| !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}
/// Only a tag or attribute NAME can own automatic native HTML suggestions.
/// Values, prose, comments, declarations and raw-text elements never do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HtmlNameContext {
    Tag(usize),
    Attribute(usize),
}
impl HtmlNameContext {
    pub(super) fn start(self) -> usize {
        match self {
            Self::Tag(start) | Self::Attribute(start) => start,
        }
    }
}
fn html_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':')
}
pub(super) fn html_name_context(source: &str, caret: usize) -> Option<HtmlNameContext> {
    let before = source.get(..caret)?;
    let bytes = before.as_bytes();
    let mut index = 0;
    let mut raw: Option<&[u8]> = None;
    while index < bytes.len() {
        if let Some(name) = raw {
            // Script/style and RCDATA contents are not HTML tag-name contexts.
            let closing = bytes[index..]
                .windows(name.len() + 2)
                .position(|part| part.starts_with(b"</") && part[2..].eq_ignore_ascii_case(name))
                .map(|at| index + at)?;
            let after = closing + name.len() + 2;
            if bytes
                .get(after)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
            {
                index = after;
                continue;
            }
            index = closing;
            raw = None;
        }
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"<!--") {
            index += 4;
            index += bytes[index..].windows(3).position(|part| part == b"-->")? + 3;
            continue;
        }
        if matches!(bytes.get(index + 1), Some(b'!' | b'?')) {
            index += 2;
            let mut quote = None;
            loop {
                let byte = *bytes.get(index)?;
                index += 1;
                if quote == Some(byte) {
                    quote = None;
                } else if quote.is_none() {
                    if matches!(byte, b'\'' | b'"') {
                        quote = Some(byte);
                    } else if byte == b'>' {
                        break;
                    }
                }
            }
            continue;
        }
        index += 1;
        let closing = bytes.get(index) == Some(&b'/');
        if closing {
            index += 1;
        }
        let tag_start = index;
        while bytes.get(index).is_some_and(|byte| html_name_byte(*byte)) {
            index += 1;
        }
        if index == bytes.len() {
            return Some(HtmlNameContext::Tag(tag_start));
        }
        if index == tag_start {
            continue;
        }
        let name = &bytes[tag_start..index];
        loop {
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            if index == bytes.len() {
                return (!closing).then_some(HtmlNameContext::Attribute(index));
            }
            if bytes[index] == b'>' || bytes[index..].starts_with(b"/>") {
                let self_closed = bytes[index] == b'/';
                index += if self_closed { 2 } else { 1 };
                if !closing
                    && !self_closed
                    && [
                        b"script".as_slice(),
                        b"style",
                        b"textarea",
                        b"title",
                        b"xmp",
                        b"iframe",
                        b"noembed",
                        b"noframes",
                    ]
                    .iter()
                    .any(|tag| name.eq_ignore_ascii_case(tag))
                {
                    raw = Some(name);
                }
                break;
            }
            if closing {
                return None;
            }
            let start = index;
            while bytes.get(index).is_some_and(|byte| html_name_byte(*byte)) {
                index += 1;
            }
            if index == bytes.len() {
                return Some(HtmlNameContext::Attribute(start));
            }
            if index == start {
                return None;
            }
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            if bytes.get(index) != Some(&b'=') {
                continue;
            }
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            let first = *bytes.get(index)?;
            if matches!(first, b'\'' | b'"') {
                index += 1;
                index += bytes[index..].iter().position(|byte| *byte == first)? + 1;
            } else {
                while bytes
                    .get(index)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                {
                    index += 1;
                }
            }
            if index == bytes.len() {
                return None;
            }
        }
    }
    None
}

impl IdeApp {
    pub(super) fn open_data_completion(&mut self, explicit: bool) -> bool {
        let project_safe_mode = self.project_safe_mode();
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.is_large_file() || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024 {
            return false;
        }
        let source = tab.state.doc.text();
        let end = usize::from(tab.state.primary().head());
        let before = &source[..end];
        let language = tab.state.language;
        let path = tab.state.path.clone().unwrap_or_default();
        let line = before.rsplit('\n').next().unwrap_or_default();
        let mut start = before
            .rfind(|c: char| !(rustpy_core::is_ident_continue(c) || matches!(c, '-' | '@' | '.')))
            .map_or(0, |at| at + before[at..].chars().next().unwrap().len_utf8());
        if matches!(language, Language::Toml | Language::Ini) && line.trim_start().starts_with('[')
        {
            start = end - line.len() + line.len() - line.trim_start().len();
        }
        let html_context = (language == Language::Html)
            .then(|| html_name_context(&source, end))
            .flatten();
        if language == Language::Html {
            if let Some(context) = html_context {
                start = context.start();
            }
            // Manual invocation may browse HTML vocabulary anywhere, but an
            // automatic popup must have an actual nonempty name token.
            if !explicit && (html_context.is_none() || start == end) {
                return false;
            }
        }
        let prefix = &source[start..end];
        let mut found = Vec::new();
        if language == Language::Python {
            let context = before[..start].trim_end();
            if ![
                "os.environ[\"",
                "os.environ['",
                "os.getenv(\"",
                "os.getenv('",
                "os.environ.get(\"",
                "os.environ.get('",
            ]
            .iter()
            .any(|marker| context.ends_with(marker))
            {
                return false;
            }
            let mut names = std::collections::BTreeSet::new();
            if let Some(project) = &self.project {
                for file in [project.root.join(".env"), project.root.join(".env.local")] {
                    if let Some(tab) = self
                        .tabs
                        .iter()
                        .find(|tab| tab.state.path.as_ref() == Some(&file))
                    {
                        if usize::from(tab.state.doc.len()) <= 65536 {
                            names.extend(entries(&tab.state.doc.text()));
                        }
                    } else {
                        use std::io::Read;
                        let mut source = String::new();
                        if let Ok(file) = std::fs::File::open(file) {
                            let _ = file.take(65537).read_to_string(&mut source);
                            if source.len() <= 65536 {
                                names.extend(entries(&source));
                            }
                        }
                    }
                }
            }
            names.extend(self.settings.python_console.environment.keys().cloned());
            found.extend(names.into_iter().map(|name| {
                item(
                    &name,
                    name.clone(),
                    "Environment key from .env; values are never exposed",
                )
            }));
        } else if path
            .extension()
            .is_some_and(|extension| extension == "xml" || extension == "xsd")
        {
            let mappings = self
                .settings
                .data_files
                .schemas
                .iter()
                .filter(|mapping| mapping.enabled)
                .map(|mapping| (mapping.pattern.clone(), mapping.schema.clone()))
                .collect::<Vec<_>>();
            if let Some(resource) = rustpy_project::data_schemas::resource(
                &source,
                &path,
                &mappings,
                self.project.as_ref().map(|project| project.root.as_path()),
            ) {
                rustpy_project::schema_store::request_resource(&resource, !project_safe_mode);
                let (_, schemas) = rustpy_project::schema_store::snapshot();
                if let Some((_, schema)) = schemas.iter().find(|(key, _)| key == &resource) {
                    if let Ok(document) = roxmltree::Document::parse(schema) {
                        for node in document
                            .descendants()
                            .filter(|node| node.is_element())
                            .take(20000)
                        {
                            if matches!(node.tag_name().name(), "element" | "attribute") {
                                if let Some(name) = node.attribute("name") {
                                    found.push(item(
                                        name,
                                        name.into(),
                                        if node.tag_name().name() == "attribute" {
                                            "XSD attribute"
                                        } else {
                                            "XSD element"
                                        },
                                    ));
                                }
                            }
                            if node.tag_name().name() == "enumeration" {
                                if let Some(value) = node.attribute("value") {
                                    found.push(item(value, value.into(), "XSD enumeration"));
                                }
                            }
                        }
                    }
                }
            }
        } else if matches!(language, Language::Json | Language::Yaml) {
            let mappings = self
                .settings
                .data_files
                .schemas
                .iter()
                .filter(|mapping| mapping.enabled)
                .map(|mapping| (mapping.pattern.clone(), mapping.schema.clone()))
                .collect::<Vec<_>>();
            let Some(resource) = rustpy_project::data_schemas::resource(
                &source,
                &path,
                &mappings,
                self.project.as_ref().map(|project| project.root.as_path()),
            ) else {
                return false;
            };
            rustpy_project::schema_store::request_resource(&resource, !project_safe_mode);
            let (_, values) = rustpy_project::schema_store::snapshot();
            let Some(schema) = rustpy_project::data_schemas::schema(&resource, &values) else {
                return false;
            };
            let pointer = if language == Language::Json {
                rustpy_project::data_formats::value(&source, &path)
                    .ok()
                    .and_then(|parsed| {
                        parsed
                            .nodes
                            .into_iter()
                            .filter(|node| node.range.start <= end && end <= node.range.end)
                            .max_by_key(|node| node.depth)
                            .map(|node| node.path)
                    })
                    .unwrap_or_default()
            } else {
                let mut stack: Vec<(usize, String)> = Vec::new();
                let indent = line.len() - line.trim_start().len();
                for previous in before
                    .lines()
                    .take(before.lines().count().saturating_sub(1))
                {
                    let depth = previous.len() - previous.trim_start().len();
                    while stack.last().is_some_and(|(old, _)| *old >= depth) {
                        stack.pop();
                    }
                    if let Some((key, value)) = previous.trim().split_once(':') {
                        if value.trim().is_empty() {
                            stack.push((depth, key.trim().to_owned()));
                        }
                    }
                }
                while stack.last().is_some_and(|(depth, _)| *depth >= indent) {
                    stack.pop();
                }
                stack
                    .into_iter()
                    .map(|(_, key)| format!("/{}", key.replace('~', "~0").replace('/', "~1")))
                    .collect()
            };
            let branch = rustpy_project::data_schemas::completion_schema(&schema, &pointer);
            if let Some(properties) = branch["properties"].as_object() {
                for (name, schema) in properties {
                    let insert = if language == Language::Json {
                        if before[..start].ends_with('"') {
                            name.clone()
                        } else {
                            format!("{}: ", serde_json::to_string(name).unwrap())
                        }
                    } else {
                        format!("{name}: ")
                    };
                    found.push(item(
                        name,
                        insert,
                        schema["description"].as_str().unwrap_or("Schema property"),
                    ));
                }
            }
            if let Some(values) = branch["enum"].as_array() {
                for value in values {
                    let label = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    found.push(item(&label, label.clone(), "Schema enum value"));
                }
            }
        } else {
            let section = before
                .lines()
                .rfind(|line| line.trim().starts_with('[') && line.trim().ends_with(']'))
                .map(str::trim)
                .unwrap_or("");
            let words: &[&str] = match language {
                Language::Ini => match section {
                    "[tool:pytest]" | "[pytest]" => &[
                        "testpaths",
                        "python_files",
                        "python_classes",
                        "python_functions",
                        "addopts",
                        "markers",
                        "filterwarnings",
                        "minversion",
                        "required_plugins",
                    ],
                    "[flake8]" => &[
                        "max-line-length",
                        "ignore",
                        "extend-ignore",
                        "select",
                        "exclude",
                        "per-file-ignores",
                        "max-complexity",
                    ],
                    "[tox]" => &[
                        "envlist",
                        "minversion",
                        "isolated_build",
                        "requires",
                        "skipsdist",
                    ],
                    section if section.starts_with("[testenv") => &[
                        "deps",
                        "commands",
                        "commands_pre",
                        "commands_post",
                        "passenv",
                        "setenv",
                        "allowlist_externals",
                        "basepython",
                        "skip_install",
                        "changedir",
                    ],
                    "[metadata]" => &[
                        "name",
                        "version",
                        "description",
                        "long_description",
                        "author",
                        "license",
                        "classifiers",
                    ],
                    _ => &[
                        "[metadata]",
                        "[options]",
                        "[tool:pytest]",
                        "[flake8]",
                        "[tox]",
                        "[testenv]",
                        "[pytest]",
                    ],
                },
                Language::Toml => {
                    // Poetry/uv/setuptools tables only exist in pyproject.toml;
                    // Cargo.toml and other TOML files have no such vocabulary.
                    let pyproject = path
                        .file_name()
                        .is_some_and(|name| name == "pyproject.toml");
                    if !pyproject {
                        return false;
                    }
                    if !section.starts_with("[tool.poetry")
                        && !section.starts_with("[tool.uv")
                        && !section.starts_with("[tool.setuptools")
                        && !line.trim_start().starts_with("[tool.p")
                        && !line.trim_start().starts_with("[tool.u")
                        && !line.trim_start().starts_with("[tool.s")
                    {
                        return false;
                    }
                    if line.trim_start().starts_with('[') {
                        &[
                            "[tool.poetry]",
                            "[tool.poetry.dependencies]",
                            "[tool.poetry.group.dev.dependencies]",
                            "[tool.uv]",
                            "[tool.uv.sources]",
                            "[tool.setuptools]",
                            "[tool.setuptools.packages.find]",
                        ]
                    } else if section.starts_with("[tool.poetry") {
                        &[
                            "name",
                            "version",
                            "description",
                            "authors",
                            "packages",
                            "include",
                            "exclude",
                            "python",
                            "source",
                            "optional",
                            "group",
                            "dependencies",
                        ]
                    } else if section.starts_with("[tool.uv") {
                        &[
                            "index",
                            "sources",
                            "dev-dependencies",
                            "dependency-groups",
                            "managed",
                            "package",
                            "required-version",
                            "constraint-dependencies",
                            "override-dependencies",
                            "environments",
                            "python-preference",
                        ]
                    } else {
                        &[
                            "packages",
                            "py-modules",
                            "package-dir",
                            "include-package-data",
                            "where",
                            "include",
                            "exclude",
                            "namespaces",
                        ]
                    }
                }
                Language::Css => &[
                    "color",
                    "background",
                    "background-color",
                    "display",
                    "position",
                    "margin",
                    "padding",
                    "border",
                    "font-size",
                    "font-family",
                    "width",
                    "height",
                    "flex",
                    "grid-template-columns",
                    "align-items",
                    "justify-content",
                    "@media",
                    "@import",
                ],
                Language::JavaScript => &[
                    "const",
                    "let",
                    "function",
                    "class",
                    "import",
                    "export",
                    "async",
                    "await",
                    "return",
                    "interface",
                    "type",
                    "implements",
                    "extends",
                    "readonly",
                    "namespace",
                    "console",
                    "document",
                    "window",
                    "Promise",
                ],
                Language::Sql => &[
                    "SELECT",
                    "FROM",
                    "WHERE",
                    "JOIN",
                    "LEFT JOIN",
                    "GROUP BY",
                    "ORDER BY",
                    "HAVING",
                    "INSERT INTO",
                    "UPDATE",
                    "DELETE FROM",
                    "CREATE TABLE",
                    "ALTER TABLE",
                    "WITH",
                    "LIMIT",
                    "RETURNING",
                ],
                Language::Shell => &[
                    "if", "then", "elif", "else", "fi", "for", "while", "do", "done", "case",
                    "esac", "function", "export", "set", "printf", "echo", "cd", "pwd", "read",
                    "test", "find", "grep", "sed", "awk", "python", "git",
                ],
                Language::Dockerfile => {
                    if line.trim_start().starts_with("FROM ") {
                        &[
                            "python:3.13-slim",
                            "python:3.12-slim",
                            "ubuntu:24.04",
                            "alpine:3",
                            "node:22-alpine",
                            "scratch",
                        ]
                    } else {
                        &[
                            "FROM",
                            "ARG",
                            "ENV",
                            "RUN",
                            "COPY",
                            "ADD",
                            "WORKDIR",
                            "USER",
                            "EXPOSE",
                            "VOLUME",
                            "ENTRYPOINT",
                            "CMD",
                            "HEALTHCHECK",
                            "LABEL",
                            "SHELL",
                            "STOPSIGNAL",
                            "ONBUILD",
                        ]
                    }
                }
                Language::Html => match html_context {
                    Some(HtmlNameContext::Tag(_)) => &[
                        "html", "head", "body", "title", "meta", "link", "script", "style", "div",
                        "span", "section", "main", "a", "img", "form", "input", "button",
                    ],
                    Some(HtmlNameContext::Attribute(_)) => &[
                        "class",
                        "id",
                        "href",
                        "src",
                        "alt",
                        "type",
                        "rel",
                        "charset",
                        "name",
                        "value",
                        "data-phase",
                        "disabled",
                        "checked",
                        "title",
                    ],
                    None => &[
                        "html", "head", "body", "title", "meta", "link", "script", "style", "div",
                        "span", "section", "main", "a", "img", "form", "input", "button", "class",
                        "id", "href", "src", "alt", "type",
                    ],
                },
                Language::GitIgnore => &[
                    "__pycache__/",
                    "*.py[cod]",
                    ".venv/",
                    "venv/",
                    ".pytest_cache/",
                    ".mypy_cache/",
                    ".ruff_cache/",
                    "build/",
                    "dist/",
                    "*.egg-info/",
                    "node_modules/",
                    ".env",
                ],
                Language::DockerIgnore => &[
                    ".git",
                    ".gitignore",
                    ".dockerignore",
                    "Dockerfile",
                    "**/__pycache__",
                    "**/*.py[cod]",
                    ".venv",
                    "venv",
                    ".pytest_cache",
                    ".mypy_cache",
                    ".ruff_cache",
                    ".idea",
                    "*.md",
                    "!README.md",
                    "build",
                    "dist",
                    "*.egg-info",
                    "node_modules",
                    ".env",
                ],
                _ => return false,
            };
            for word in words {
                found.push(item(
                    word,
                    if language == Language::Ini && !word.starts_with('[') {
                        format!("{word} = ")
                    } else {
                        (*word).into()
                    },
                    "Data-document keyword",
                ));
            }
            if matches!(
                language,
                Language::GitIgnore | Language::DockerIgnore | Language::Shell
            ) {
                let fragment = line.trim_start().trim_start_matches('!');
                let relative = Path::new(fragment);
                let directory = relative.parent().unwrap_or(Path::new(""));
                let root = self
                    .project
                    .as_ref()
                    .map(|project| project.root.as_path())
                    .or_else(|| path.parent())
                    .unwrap_or(Path::new("."));
                if !directory.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                }) {
                    if let Ok(children) = std::fs::read_dir(root.join(directory)) {
                        for entry in children.take(512).flatten() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            let insert = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                                format!("{name}/")
                            } else {
                                name.clone()
                            };
                            found.push(item(&name, insert, "Project path"));
                        }
                    }
                }
            }
        }
        found.retain(|item| {
            item.label
                .to_lowercase()
                .starts_with(&prefix.to_lowercase())
        });
        found.sort_by(|a, b| a.label.cmp(&b.label));
        found.dedup_by(|a, b| a.label == b.label);
        if found.is_empty() {
            return false;
        }
        self.completion = CompletionPopup {
            visible: true,
            imports: vec![None; found.len()],
            items: found,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: TextSize::new(start as u32),
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            rect: None,
            explicit,
        };
        self.completion_ui.paths = true;
        self.import_completion = None;
        self.completion_ui.due = None;
        true
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn environment_completion_never_retains_values() {
        assert_eq!(
            entries("export TOKEN=secret\nURL='private'\n#ignored=x\n"),
            vec!["TOKEN", "URL"]
        );
    }
}

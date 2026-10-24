//! RustPy IDE — syntax highlighting and color schemes (roadmap §1.1, §2.3).
//!
//! Python keeps lexical coloring on broken code and refines declarations,
//! annotations, decorators and docstrings with the lossless syntax tree.
//! Besides Python, the
//! bundled languages (JSON, YAML, TOML, INI, Markdown, HTML/XML, CSS,
//! JavaScript/TypeScript, shell, reStructuredText, requirements.txt,
//! Dockerfile, SQL) get token-based highlighters; `highlight_for` dispatches
//! by language name.

mod bundled_extra;
mod python_defaults;
mod python_keys;
pub use python_keys::PYTHON_ATTRIBUTE_KEYS;
mod common;
pub use bundled_extra::{
    highlight_csv, highlight_diff, highlight_dockerignore, highlight_ignore, highlight_makefile,
    highlight_properties, highlight_regex,
};
pub mod css;
pub mod dockerfile;
pub mod html;
pub mod ini;
pub mod javascript;
pub mod jinja;
pub mod json;
pub mod jsx;
pub mod markdown;
pub mod python;
pub mod requirements;
pub mod rst;
pub mod scheme;
pub mod shell;
pub mod sql;
pub mod toml;
pub mod yaml;

pub use css::highlight_css;
pub use dockerfile::highlight_dockerfile;
pub use html::highlight_html;
pub use ini::highlight_ini;
pub use javascript::highlight_javascript;
pub use jinja::highlight_jinja;
pub use json::highlight_json;
pub use jsx::highlight_jsx;
pub use markdown::highlight_markdown;
pub use python::{highlight_python, highlight_python_with_parse};
pub use requirements::highlight_requirements;
pub use rst::highlight_rst;
pub use rustpy_core::TextRange;
pub use scheme::{ColorScheme, Rgb, TextStyle};
pub use shell::highlight_shell;
pub use sql::highlight_sql;
pub use toml::highlight_toml;
pub use yaml::highlight_yaml;

/// Semantic category of a highlighted span, mirroring the `.icls` colour scheme
/// attribute keys for Python (plus a few shared by the bundled languages).
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum HighlightTag {
    Default,
    NestedFunctionDefinition,
    MethodCall,
    PredefinedDefinition,
    PredefinedUsage,
    DocCommentTag,
    InvalidEscapeSequence,
    TypeParameter,
    FStringColon,
    FStringConversion,
    FStringFormatCharacter,
    FStringFormatNumber,
    Keyword,
    SoftKeyword,
    Builtin,
    SelfParameter,
    Decorator,
    FunctionDefinition,
    ClassDefinition,
    FunctionCall,
    Number,
    String,
    Bytes,
    FString,
    /// `{` / `}` delimiting an f-string replacement field.
    FStringBrace,
    EscapeSequence,
    Docstring,
    Comment,
    TodoComment,
    Operator,
    Punctuation,
    Brace,
    Bracket,
    Parenthesis,
    Comma,
    Dot,
    Dunder,
    KeywordArgument,
    Parameter,
    TypeAnnotation,
    LocalVariable,
    Error,
    /// Keys in JSON/YAML/TOML/INI, CSS properties, requirement names, field lists.
    Key,
    /// HTML/XML tag names (with their angle brackets), CSS element selectors.
    Tag,
    /// Markdown / reStructuredText headings, TOML tables, INI sections.
    Heading,
    /// Hyperlinks and link references.
    Link,
    /// Emphasized (italic) markup.
    Emphasis,
    /// Strong (bold) markup.
    Strong,
    /// Shell / Dockerfile / YAML / SQL variables, anchors and parameters.
    Variable,
    /// Semantic highlighting of local variables / parameters (rainbow).
    Rainbow1,
    Rainbow2,
    Rainbow3,
    Rainbow4,
    Rainbow5,
    Rainbow6,
    /// Unused imports and local variables (grayed out instead of underlined).
    Unused,
}

/// A highlighted byte range.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: TextRange,
    pub tag: HighlightTag,
}

/// Dispatches to the highlighter for `language` (case-insensitive name such as
/// `"python"`, `"json"`, `"yaml"`, `"toml"`, `"ini"`, `"markdown"`, `"html"`,
/// `"xml"`, `"css"`, `"javascript"`, `"typescript"`, `"shell"`, `"rst"`,
/// `"requirements"`, `"dockerfile"`, `"sql"`). Unknown names yield no spans.
pub fn highlight_for(language: &str, src: &str) -> Vec<Span> {
    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "py" | "pyi" => highlight_python(src),
        "json" | "jsonc" | "json5" | "ipynb" => highlight_json(src),
        "yaml" | "yml" => highlight_yaml(src),
        "toml" => highlight_toml(src),
        "ini" | "cfg" | "editorconfig" => highlight_ini(src),
        "properties" => highlight_properties(src),
        "dotenv" | "env" | ".env" => highlight_shell(src),
        "gitignore" | ".gitignore" | "ignore" => highlight_ignore(src),
        "dockerignore" | ".dockerignore" => highlight_dockerignore(src),
        "makefile" | "make" | "gnumakefile" => highlight_makefile(src),
        "regex" | "regexp" => highlight_regex(src),
        "diff" | "patch" => highlight_diff(src),
        "csv" | "tsv" => highlight_csv(src),
        "text" | "plain text" | "plaintext" => Vec::new(),
        "markdown" | "md" => highlight_markdown(src),
        "html" | "htm" | "xml" | "svg" | "xhtml" | "vue" | "svelte" => highlight_html(src),
        "jinja" | "jinja2" | "j2" | "django" | "django-html" | "html+jinja" | "template" => {
            highlight_jinja(src)
        }
        "css" | "scss" | "less" => highlight_css(src),
        "jsx" | "tsx" | "javascriptreact" | "typescriptreact" => highlight_jsx(src),
        "javascript" | "js" | "mjs" | "cjs" | "mts" | "cts" | "typescript" | "ts" => {
            highlight_javascript(src)
        }
        "shell" | "sh" | "bash" | "zsh" | "shell script" => highlight_shell(src),
        "rst" | "restructuredtext" => highlight_rst(src),
        "requirements" | "requirements.txt" | "pip" => highlight_requirements(src),
        "dockerfile" | "docker" => highlight_dockerfile(src),
        "sql" | "sqlite" | "postgresql" | "mysql" => highlight_sql(src),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_by_name() {
        assert!(!highlight_for("Python", "import os\n").is_empty());
        assert!(!highlight_for("JSON", "{\"a\": 1}").is_empty());
        assert!(!highlight_for("yaml", "a: 1\n").is_empty());
        assert!(!highlight_for("TypeScript", "const x: number = 1;").is_empty());
        assert!(!highlight_for("Dockerfile", "FROM scratch\n").is_empty());
        assert!(!highlight_for("sql", "SELECT 1;").is_empty());
        assert!(highlight_for("brainfuck", "+++").is_empty());
        assert!(highlight_for("", "x").is_empty());
    }

    #[test]
    fn every_language_handles_empty_and_garbage() {
        let names = [
            "python",
            "json",
            "yaml",
            "toml",
            "ini",
            "markdown",
            "html",
            "css",
            "javascript",
            "shell",
            "rst",
            "requirements",
            "dockerfile",
            "dockerignore",
            "sql",
        ];
        let garbage = "\u{0}\u{1}\"'`\\<>{}[]()$#;:=*_~|&@!-\n\t\u{fffd}ü\n";
        for n in names {
            assert!(highlight_for(n, "").is_empty(), "{n} on empty input");
            let spans = highlight_for(n, garbage);
            let mut last = 0u32;
            for s in spans {
                assert!(u32::from(s.range.start()) >= last, "{n}: overlapping spans");
                assert!(usize::from(s.range.end()) <= garbage.len());
                last = u32::from(s.range.end());
            }
        }
    }
}

pub mod template_engines;

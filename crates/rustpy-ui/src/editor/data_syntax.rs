//! Version-bound non-Python syntax diagnostics; no document/history mutations.
use super::Language;
use rustpy_core::{Diagnostic, TextRange, TextSize};
use rustpy_text::{Document, DocumentId};
use std::ops::Range;
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    document: DocumentId,
    version: u64,
    language: Language,
    path: Option<PathBuf>,
    framework: u64,
}
#[derive(Debug, Default)]
pub(super) struct Cache {
    value: Option<(Key, Vec<Diagnostic>)>,
    pub(super) fixes: Vec<(TextRange, Vec<rustpy_analysis::inspections::QuickFix>)>,
}
pub(super) fn supports(language: Language) -> bool {
    matches!(
        language,
        Language::Json
            | Language::Toml
            | Language::Ini
            | Language::Yaml
            | Language::Template
            | Language::Rst
            | Language::Requirements
            | Language::Dockerfile
            | Language::Markdown
            | Language::GitIgnore
            | Language::DockerIgnore
            | Language::DotEnv
            | Language::Shell
            | Language::Html
            | Language::Css
            | Language::JavaScript
            | Language::Sql
            | Language::Http
    )
}
impl Cache {
    pub(super) fn current(
        &self,
        doc: &Document,
        language: Language,
        path: Option<&Path>,
        inputs: &rustpy_analysis::inspections::framework_tools::Inputs,
    ) -> bool {
        self.value.as_ref().is_some_and(|(key, diagnostics)| {
            key.framework == inputs_key(inputs)
                && key.document == doc.identity()
                && key.version == doc.version()
                && key.language == language
                && key.path.as_deref() == path
                && !diagnostics.iter().any(|diagnostic| {
                    diagnostic.code.as_str() == "data-syntax-limit"
                        || diagnostic.code.as_str() == "data-syntax-pending"
                })
        })
    }
    pub(super) fn diagnostics(
        &mut self,
        doc: &Document,
        language: Language,
        path: Option<&Path>,
        inputs: &rustpy_analysis::inspections::framework_tools::Inputs,
    ) -> Vec<Diagnostic> {
        let key = Key {
            document: doc.identity(),
            version: doc.version(),
            language,
            path: path.map(Path::to_path_buf),
            framework: inputs_key(inputs),
        };
        if let Some((old, diagnostics)) = &self.value {
            if old == &key {
                return diagnostics.clone();
            }
        }
        // Every new authority replaces the old fix payload, including limit
        // and pending results that cannot publish any actionable ranges.
        self.fixes.clear();
        // Refuse before rope flattening; large-file and binary/viewer presentation
        // must not cause synchronous parsing of arbitrary source sizes.
        let diagnostics = if usize::from(doc.len()) > MAX_BYTES {
            vec![Diagnostic::warning(
                TextRange::empty(TextSize::new(0)),
                "data-syntax-limit",
                "Syntax checking skipped: document exceeds 2 MiB",
            )]
        } else {
            let source = doc.text();
            if source.len() > MAX_BYTES {
                return vec![Diagnostic::warning(
                    TextRange::empty(TextSize::new(0)),
                    "data-syntax-limit",
                    "Syntax checking skipped: document exceeds 2 MiB",
                )];
            }
            if matches!(language, Language::Json | Language::Yaml | Language::Toml) {
                if let Some(resource) = path.and_then(|path| {
                    rustpy_project::data_schemas::resource(
                        &source,
                        path,
                        &inputs.schema_mappings,
                        inputs.root.as_deref(),
                    )
                }) {
                    rustpy_project::schema_store::request_resource(&resource, inputs.trusted);
                }
                if let Some(url) =
                    path.and_then(|path| rustpy_project::schema_store::schema_url(&source, path))
                {
                    rustpy_project::schema_store::request(&url, inputs.trusted);
                }
            }
            let mut diagnostics = parse(&source, language, path, inputs.django_template_engine);
            if let Some(path) = path {
                for issue in rustpy_project::document_tools::inspect(
                    &source,
                    path,
                    inputs.interpreter.as_deref(),
                    inputs.sql_dialect.as_deref(),
                    inputs.check_unused_ignore_patterns,
                    inputs.trusted,
                    inputs.project_revision,
                ) {
                    diagnostics.push(Diagnostic::warning(
                        at(
                            &source,
                            line_offset(&source, issue.line, issue.column, true),
                        ),
                        &issue.code,
                        issue.message,
                    ));
                }
            }

            if let Some(path) = path {
                for problem in
                    rustpy_analysis::inspections::framework_tools::documents::inspect_with_inputs(
                        &source, path, inputs,
                    )
                {
                    if !problem.fixes.is_empty() {
                        self.fixes.push((problem.diagnostic.range, problem.fixes));
                    }
                    diagnostics.push(problem.diagnostic);
                }
            }
            diagnostics
        };
        if doc.identity() != key.document || doc.version() != key.version {
            // A shared editor changed during snapshot/parse: never publish an old
            // result as diagnostics for its new version, or cache it.
            self.fixes.clear();
            return vec![Diagnostic::warning(
                TextRange::empty(TextSize::new(0)),
                "data-syntax-pending",
                "Document changed during syntax checking; retry on the next frame",
            )];
        }
        self.value = Some((key, diagnostics.clone()));
        diagnostics
    }
}
fn range(source: &str, requested: Range<usize>) -> TextRange {
    let start = rustpy_core::floor_char_boundary(source, requested.start);
    let mut end = requested.end.min(source.len()).max(start);
    while !source.is_char_boundary(end) {
        end += 1;
    }
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}
fn at(source: &str, offset: usize) -> TextRange {
    let start = rustpy_core::floor_char_boundary(source, offset);
    let end = source[start..]
        .chars()
        .next()
        .map_or(start, |ch| start + ch.len_utf8());
    range(source, start..end)
}
// JSON reports one-based byte columns; YAML reports one-based character columns.
fn line_offset(source: &str, line: usize, column: usize, character_column: bool) -> usize {
    let mut start = 0;
    for _ in 1..line.max(1) {
        let Some(next) = source[start..].find('\n') else {
            return source.len();
        };
        start += next + 1;
    }
    let end = source[start..]
        .find('\n')
        .map_or(source.len(), |n| start + n);
    let text = &source[start..end];
    let column = column.saturating_sub(1);
    start
        + if character_column {
            text.char_indices()
                .nth(column)
                .map_or(text.len(), |(index, _)| index)
        } else {
            column.min(text.len())
        }
}
fn parse(
    source: &str,
    language: Language,
    path: Option<&Path>,
    django_project: bool,
) -> Vec<Diagnostic> {
    let mut result = match language {
        Language::Json => match rustpy_project::data_formats::parse_json(
            source,
            path.map(rustpy_project::data_formats::mode)
                .unwrap_or(rustpy_project::data_formats::JsonMode::Strict),
        ) {
            Ok(_) => vec![],
            Err(error) => vec![Diagnostic::error(
                at(source, error.offset),
                "json-syntax",
                error.message,
            )],
        },
        Language::Toml => match toml::from_str::<toml::Value>(source) {
            Ok(_) => vec![],
            Err(error) => vec![Diagnostic::error(
                range(source, error.span().unwrap_or(0..0)),
                "toml-syntax",
                error.message(),
            )],
        },
        Language::Ini => {
            let editorconfig = path
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".editorconfig");
            if editorconfig {
                return rustpy_settings::editorconfig::document::parse(source)
                    .issues
                    .into_iter()
                    .filter(|issue| issue.error)
                    .take(MAX_DIAGNOSTICS + 1)
                    .map(|issue| {
                        Diagnostic::error(range(source, issue.range), "ini-syntax", issue.message)
                    })
                    .collect();
            }
            let dialect = rustpy_syntax::ini::Dialect::ConfigParser;
            rustpy_syntax::ini::parse(source, dialect)
                .diagnostics
                .into_iter()
                .take(MAX_DIAGNOSTICS + 1)
                .map(|error| {
                    Diagnostic::error(
                        range(source, error.range),
                        "ini-syntax",
                        format!("{:?}", error.kind),
                    )
                })
                .collect()
        }
        Language::Yaml => match rustpy_syntax::yaml::parse(source) {
            Ok(_) => vec![],
            Err(error) => vec![Diagnostic::error(
                at(source, line_offset(source, error.line, error.column, true)),
                "yaml-syntax",
                error.message,
            )],
        },
        Language::Template => {
            // .djhtml is explicitly Django; remaining existing Template file types
            // use Jinja's default-delimiter structure. No engine execution occurs.
            let django = path
                .and_then(Path::extension)
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("djhtml")
                        || (django_project && !matches!(ext, "j2" | "jinja" | "jinja2"))
                });
            let dialect = if django {
                rustpy_syntax::template::Dialect::Django
            } else {
                rustpy_syntax::template::Dialect::Jinja
            };
            rustpy_syntax::template::parse(source, dialect)
                .diagnostics
                .into_iter()
                .take(MAX_DIAGNOSTICS + 1)
                .map(|error| {
                    Diagnostic::error(
                        range(source, error.range),
                        "template-syntax",
                        format!("{:?}", error.error),
                    )
                })
                .collect()
        }
        Language::Rst => rustpy_syntax::rst::parse(source)
            .diagnostics
            .into_iter()
            .take(MAX_DIAGNOSTICS + 1)
            .map(|error| {
                let diagnostic_range = range(source, error.range);
                let message = format!("{:?}", error.kind);
                if error.kind == rustpy_syntax::rst::ErrorKind::ShortAdornment {
                    // The lightweight block inventory counts scalars rather than
                    // display cells; combining marks can make this a valid title.
                    Diagnostic::warning(diagnostic_range, "rst-syntax", message)
                } else {
                    Diagnostic::error(diagnostic_range, "rst-syntax", message)
                }
            })
            .collect(),
        _ => vec![],
    };
    if result.len() > MAX_DIAGNOSTICS {
        result.truncate(MAX_DIAGNOSTICS - 1);
        result.push(Diagnostic::warning(
            TextRange::empty(TextSize::new(0)),
            "data-syntax-limit",
            "Additional syntax diagnostics omitted (limit 128)",
        ));
    }
    result
}

fn inputs_key(inputs: &rustpy_analysis::inspections::framework_tools::Inputs) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    inputs.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::super::{Caret, EditorState};
    use super::*;
    use rustpy_text::{Edit, EditGroup};
    #[test]
    fn template_statement_errors_follow_real_file_edit_and_undo() {
        let source = "{% for π in rows %}🙂{% endfor %}";
        let mut state = EditorState::new(
            Document::new(source),
            Some(PathBuf::from("templates/list.j2")),
        );
        assert_eq!(state.language, Language::Template);
        assert!(state.diagnostics().is_empty());
        let offset = TextSize::new(source.find("in rows").unwrap() as u32);
        assert!(state.apply(
            vec![Edit::delete(TextRange::new(
                offset,
                offset + TextSize::new(3)
            ))],
            vec![Caret::at(offset)],
            EditGroup::Separate,
            Some("remove loop clause")
        ));
        let version = state.doc.version();
        let errors = state.diagnostics();
        assert!(errors
            .iter()
            .any(|d| { d.code.as_str() == "template-syntax" && d.message == "InvalidStatement" }));
        assert_eq!(state.doc.version(), version);
        assert!(state.data_syntax_current());
        for error in errors {
            assert!(state
                .doc
                .text()
                .get(usize::from(error.range.start())..usize::from(error.range.end()))
                .is_some());
        }
        assert!(state.undo());
        assert_eq!(state.doc.text(), source);
        assert!(state.diagnostics().is_empty());
        assert!(!state.doc.can_undo());
    }
    #[test]
    fn combining_mark_rst_title_width_hint_is_not_a_strong_syntax_error() {
        let source = "e\u{301}\n=\n";
        let mut state = EditorState::from_text(source, Language::Rst);
        let diagnostics = state.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "ShortAdornment");
        assert_eq!(diagnostics[0].severity, rustpy_core::Severity::Warning);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }
    #[test]
    fn actual_editor_json_edit_undo_and_language_change_recompute_diagnostics_without_extra_history(
    ) {
        let mut state = EditorState::from_text("{\"π\": 1}", Language::Json);
        let identity = state.doc.identity();
        let version = state.doc.version();
        let carets = state.carets.clone();
        assert!(state.diagnostics().is_empty());
        assert_eq!(state.doc.identity(), identity);
        assert_eq!(state.doc.version(), version);
        assert_eq!(state.carets, carets);
        assert!(!state.doc.can_undo());
        let end = state.doc.len();
        assert!(state.apply(
            vec![Edit::delete(TextRange::new(end - TextSize::new(1), end))],
            vec![Caret::at(end - TextSize::new(1))],
            EditGroup::Separate,
            Some("invalid JSON")
        ));
        let invalid_version = state.doc.version();
        assert!(!state.data_syntax_current());
        let errors = state.diagnostics();
        assert!(state.data_syntax_current());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_str(), "json-syntax");
        assert!(state
            .doc
            .text()
            .get(usize::from(errors[0].range.start())..usize::from(errors[0].range.end()))
            .is_some());
        assert_eq!(state.doc.version(), invalid_version);
        assert!(state.undo());
        assert!(!state.data_syntax_current());
        assert!(state.diagnostics().is_empty());
        assert!(state.data_syntax_current());
        assert_eq!(state.doc.text(), "{\"π\": 1}");
        assert!(!state.doc.can_undo());
        state.set_language(Language::Toml);
        assert!(!state.diagnostics().is_empty());
        state.set_language(Language::Json);
        assert!(state.diagnostics().is_empty());
    }
    #[test]
    fn exact_editorconfig_path_changes_dialect_even_at_same_version() {
        assert_eq!(
            Language::from_path(Path::new(".editorconfig")),
            Language::Ini
        );
        let mut state = EditorState::new(
            Document::new("root=true\n[*.py]\nindent_size=2\n[*.py]\nindent_size=4\n"),
            Some(PathBuf::from(".editorconfig")),
        );
        assert!(state.diagnostics().is_empty());
        let version = state.doc.version();
        state.path = Some(PathBuf::from("setup.cfg"));
        assert!(state
            .diagnostics()
            .iter()
            .any(|d| d.message == "MissingSection"));
        assert_eq!(state.doc.version(), version);
        state.path = Some(PathBuf::from(".editorconfig"));
        assert!(state.diagnostics().is_empty());
        assert!(!state.doc.can_undo());
    }
    #[test]
    fn all_supported_languages_route_to_real_structural_parser_errors() {
        for (language, valid, invalid, code) in [
            (Language::Toml, "\"π\"=1\n", "x=[\n", "toml-syntax"),
            (Language::Yaml, "name: π\n", "name: [\n", "yaml-syntax"),
            (Language::Ini, "[x]\na=π\n", "[broken\n", "ini-syntax"),
            (
                Language::Template,
                "{% if ready %}π{% endif %}",
                "{% if ready %}π",
                "template-syntax",
            ),
            (Language::Rst, "Title\n=====\n", "Title\n==\n", "rst-syntax"),
        ] {
            let mut state = EditorState::from_text(valid, language);
            assert!(state.diagnostics().is_empty(), "{language:?}");
            state.doc = Document::new(invalid);
            assert!(
                state.diagnostics().iter().any(|d| d.code.as_str() == code),
                "{language:?}"
            );
            assert_eq!(state.doc.text(), invalid);
            assert!(!state.doc.can_undo());
        }
    }
    #[test]
    fn markdown_structural_frames_preserve_current_prose_fixes_without_accumulating() {
        use rustpy_analysis::inspections::QuickFix;
        let source = "the the π\n# First\n### Third\n";
        let mut state = EditorState::from_text(source, Language::Markdown);
        state.path = Some(PathBuf::from("notes.md"));
        let range = TextRange::new(TextSize::new(0), TextSize::new(7));
        state.set_prose_diagnostics(
            vec![Diagnostic::warning(range, "Grammar", "Repeated word")],
            vec![(
                range,
                vec![QuickFix {
                    title: "Replace with 'the'".into(),
                    edits: vec![(range, "the".into())],
                }],
            )],
        );
        for _ in 0..3 {
            let diagnostics = state.diagnostics();
            assert!(
                diagnostics.iter().any(|d| d.code == "Markdown"),
                "{diagnostics:?}"
            );
            assert_eq!(
                diagnostics.iter().filter(|d| d.code == "Grammar").count(),
                1
            );
            assert_eq!(state.quick_fixes.len(), 1);
        }
        // Read-only documents retain discoverable fixes, but refuse their edits.
        state.doc.set_read_only(true);
        state.diagnostics();
        let edits = state.quick_fixes[0].1[0].edits.clone();
        state.apply_text_edits(edits, "Grammar correction");
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
        // Replacing a document at the same numeric version is a new authority.
        state.doc = Document::new(source);
        let diagnostics = state.diagnostics();
        assert!(!diagnostics.iter().any(|d| d.code == "Grammar"));
        assert!(state.quick_fixes.is_empty());
        // A fresh publication can be hidden by Syntax mode and restored without
        // destroying its separately owned fixes or reviving a replaced snapshot.
        state.set_prose_diagnostics(
            vec![Diagnostic::warning(range, "Grammar", "Repeated word")],
            vec![(
                range,
                vec![QuickFix {
                    title: "Fix repeat".into(),
                    edits: vec![],
                }],
            )],
        );
        state.inspection_level = rustpy_settings::inspection_profiles::HighlightingLevel::Syntax;
        assert!(!state.diagnostics().iter().any(|d| d.code == "Grammar"));
        assert!(state.quick_fixes.is_empty());
        state.inspection_level = rustpy_settings::inspection_profiles::HighlightingLevel::All;
        assert!(state.diagnostics().iter().any(|d| d.code == "Grammar"));
        assert_eq!(state.quick_fixes.len(), 1);
        state.semantic_version = None;
        assert!(!state.diagnostics().iter().any(|d| d.code == "Grammar"));
        assert!(state.quick_fixes.is_empty());
        state.set_language(Language::PlainText);
        assert!(state.diagnostics().is_empty());
    }

    #[test]
    fn semantic_result_for_same_version_cannot_hide_nonpython_syntax_errors() {
        let mut state = EditorState::from_text("[", Language::Json);
        state.semantic_version = Some(state.doc.version());
        state.extra_diagnostics.clear();
        assert_eq!(state.diagnostics()[0].code.as_str(), "json-syntax");
        state.set_language(Language::PlainText);
        assert!(state.diagnostics().is_empty());
    }
    #[test]
    fn source_limit_precedes_flattening_and_reports_instead_of_false_clean_result() {
        let mut state = EditorState::from_text(&" ".repeat(MAX_BYTES + 1), Language::Json);
        let version = state.doc.version();
        assert_eq!(state.diagnostics()[0].code.as_str(), "data-syntax-limit");
        assert_eq!(state.doc.version(), version);
        assert!(!state.doc.can_undo());
    }
    #[test]
    fn unicode_line_columns_and_malformed_byte_ranges_never_split_scalars() {
        let source = "first\nπ🙂x\n";
        assert_eq!(line_offset(source, 2, 2, true), 8);
        assert_eq!(
            at(source, 7),
            TextRange::new(TextSize::new(6), TextSize::new(8))
        );
        assert_eq!(
            range(source, 7..10),
            TextRange::new(TextSize::new(6), TextSize::new(12))
        );
    }
}

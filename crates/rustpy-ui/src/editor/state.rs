//! Editor state: document + multiple carets + per-editor view flags.

#[path = "data_syntax.rs"]
mod data_syntax;

#[path = "todo_highlights.rs"]
pub(super) mod todo_highlights;
#[path = "virtual_space.rs"]
pub mod virtual_space;

use std::path::{Path, PathBuf};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use rustpy_core::{Diagnostic, Position, TextRange, TextSize};
use rustpy_highlight::{HighlightTag, Span};
use rustpy_syntax::Parse;
use rustpy_text::{
    Affinity, Document, DocumentId, Edit, EditGroup, MarkerId, MarkerPosition, PointDeletion,
    Selection,
};

use super::syntax_support::{self, FoldKind, FoldRegion, StructureItem};
use super::textmate::{TextMateHighlight, FRAME_BUDGET, LOOKAHEAD_LINES};

/// Lines Copy as Rich Text may tokenize beyond what the view already has.
const RICH_COPY_EXTRA_LINES: usize = 3_000;

/// Independently owned prose results; displayed data fixes must never become
/// the source of a later merge, even after repeated renderer frames.
#[derive(Debug)]
struct ProseFindings {
    document: DocumentId,
    version: u64,
    diagnostics: Vec<Diagnostic>,
    fixes: Vec<(TextRange, Vec<rustpy_analysis::inspections::QuickFix>)>,
}

/// Language of the edited file (drives highlighting and smart keys).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    PlainText,
    Markdown,
    Json,
    Toml,
    Yaml,
    Ini,
    Shell,
    Html,
    Css,
    JavaScript,
    Http,
    Sql,
    Rst,
    Dockerfile,
    Requirements,
    Cython,
    /// Jinja2 / Django templates.
    Template,
    /// Gherkin `.feature` files (behave / pytest-bdd).
    Gherkin,
    GitIgnore,
    /// `.dockerignore` (and BuildKit's per-Dockerfile `<name>.dockerignore`):
    /// `.gitignore`-style patterns matched against the build context.
    DockerIgnore,
    DotEnv,
    Makefile,
    Regex,
    Diff,
    Csv,
    Properties,
    Other,
}

impl Language {
    pub fn from_path(path: &Path) -> Language {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, ".gitignore" | ".ignore") {
            return Language::GitIgnore;
        }
        if name.ends_with(".dockerignore") {
            return Language::DockerIgnore;
        }
        if name == ".env" || name.starts_with(".env.") {
            return Language::DotEnv;
        }
        if matches!(name, "Makefile" | "makefile" | "GNUmakefile") {
            return Language::Makefile;
        }
        if matches!(name, "Pipfile" | "poetry.lock" | "uv.lock") {
            return Language::Toml;
        }
        if name == "Pipfile.lock" {
            return Language::Json;
        }
        if name == ".flake8" {
            return Language::Ini;
        }
        if name == ".editorconfig" {
            return Language::Ini;
        }
        if name.starts_with("Dockerfile") || name.ends_with(".dockerfile") {
            return Language::Dockerfile;
        }
        if name.starts_with("requirements") && name.ends_with(".txt") || name == "constraints.txt" {
            return Language::Requirements;
        }
        if name.ends_with(".rst") {
            return Language::Rst;
        }
        if name.ends_with(".pyx") || name.ends_with(".pxd") || name.ends_with(".pxi") {
            return Language::Cython;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(
            ext.as_str(),
            "jinja" | "jinja2" | "j2" | "djhtml" | "mako" | "pt" | "cpt" | "zpt"
        ) {
            return Language::Template;
        }
        if ext == "feature" {
            return Language::Gherkin;
        }
        // HTML/text files inside a `templates` folder are template files (Flask/Django convention).
        if matches!(ext.as_str(), "html" | "htm" | "xml" | "txt")
            && path
                .ancestors()
                .skip(1)
                .any(|a| a.file_name().is_some_and(|n| n == "templates"))
        {
            return Language::Template;
        }
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("py" | "pyi" | "pyw") => Language::Python,
            Some("md" | "markdown") => Language::Markdown,
            Some("json" | "json5" | "jsonc" | "jsonl" | "ndjson" | "ipynb") => Language::Json,
            Some("toml") => Language::Toml,
            Some("yml" | "yaml") => Language::Yaml,
            Some("ini" | "cfg" | "flake8" | "editorconfig") => Language::Ini,
            Some("sh" | "bash" | "zsh") => Language::Shell,
            Some("html" | "htm" | "xml" | "vue" | "svelte") => Language::Html,
            Some("css" | "scss" | "less") => Language::Css,
            Some("js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs" | "mts" | "cts") => {
                Language::JavaScript
            }
            Some("http" | "rest") => Language::Http,
            Some("sql") => Language::Sql,
            Some("gitignore") => Language::GitIgnore,
            Some("env") => Language::DotEnv,
            Some("mk" | "mak") => Language::Makefile,
            Some("regex" | "regexp") => Language::Regex,
            Some("diff" | "patch") => Language::Diff,
            Some("csv" | "tsv") => Language::Csv,
            Some("properties") => Language::Properties,
            Some("txt" | "rst" | "log") | None if !name.starts_with("Dockerfile") => {
                Language::PlainText
            }
            _ => Language::Other,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Language::Python => "Python",
            Language::PlainText => "Plain text",
            Language::Markdown => "Markdown",
            Language::Json => "JSON",
            Language::Toml => "TOML",
            Language::Yaml => "YAML",
            Language::Ini => "INI",
            Language::Shell => "Shell Script",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::JavaScript => "JavaScript",
            Language::Http => "HTTP Request",
            Language::Sql => "SQL",
            Language::Rst => "reStructuredText",
            Language::Dockerfile => "Dockerfile",
            Language::Requirements => "Requirements",
            Language::Cython => "Cython",
            Language::Template => "Jinja2 / Django Template",
            Language::Gherkin => "Gherkin",
            Language::GitIgnore => "Gitignore",
            Language::DockerIgnore => "Dockerignore",
            Language::DotEnv => "Dotenv",
            Language::Makefile => "Makefile",
            Language::Regex => "Regex",
            Language::Diff => "Diff",
            Language::Csv => "CSV",
            Language::Properties => "Properties",
            Language::Other => "Text",
        }
    }

    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Language::Python
            | Language::Cython
            | Language::Gherkin
            | Language::Toml
            | Language::Yaml
            | Language::Shell
            | Language::Ini
            | Language::Dockerfile
            | Language::Requirements
            | Language::Http
            | Language::GitIgnore
            | Language::DockerIgnore
            | Language::DotEnv
            | Language::Makefile
            | Language::Properties => Some("#"),
            Language::JavaScript => Some("//"),
            Language::Sql => Some("--"),
            _ => None,
        }
    }

    /// Block comment delimiters (`None` for languages without block comments,
    /// e.g. Python, where *Comment with Block Comment* falls back to line comments).
    pub fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Html | Language::Markdown => Some(("<!--", "-->")),
            Language::Css | Language::JavaScript | Language::Sql => Some(("/*", "*/")),
            Language::Template => Some(("{#", "#}")),
            _ => None,
        }
    }

    /// Python-like languages get Python smart keys (indentation, docstrings, `self`).
    pub fn is_pythonic(self) -> bool {
        matches!(self, Language::Python | Language::Cython)
    }
}

/// A caret with an optional "goal" column remembered across vertical moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Caret {
    pub sel: Selection,
    /// Character column to aim for when moving up/down (with soft wraps on:
    /// the display column within a visual row).
    pub goal_column: Option<u32>,
}

impl Caret {
    pub fn at(offset: TextSize) -> Self {
        Self {
            sel: Selection::caret(offset),
            goal_column: None,
        }
    }
    pub fn new(anchor: TextSize, head: TextSize) -> Self {
        Self {
            sel: Selection::new(anchor, head),
            goal_column: None,
        }
    }
    pub fn head(&self) -> TextSize {
        self.sel.head
    }
    pub fn range(&self) -> TextRange {
        self.sel.range()
    }
    pub fn has_selection(&self) -> bool {
        !self.sel.is_empty()
    }
}

/// Code-style and smart-key options consulted by editing commands.
#[derive(Clone, Debug, PartialEq)]
pub struct EditOptions {
    pub confirm_refactoring_undo: bool,
    pub tab_size: usize,
    pub indent_size: usize,
    pub continuation_indent: Option<rustpy_format::ContinuationIndent>,
    pub use_tabs: bool,
    pub smart_tabs: Option<bool>,
    pub auto_indent: bool,
    pub auto_close_brackets: bool,
    pub auto_close_quotes: bool,
    pub surround_selection: bool,
    pub smart_home: bool,
    pub allow_caret_after_end_of_line: bool,
    pub allow_caret_after_end_of_line_in_block_selection: bool,
    pub smart_backspace: bool,
    pub camel_humps: bool,
    /// Lines visible in the viewport (for page movement).
    pub page_lines: usize,
    /// Reformat on paste: shift pasted Python blocks to the caret's indentation.
    pub paste_indent_block: bool,
    pub paste_reformat: bool,
    pub paste_format_options: rustpy_format::FormatOptions,
    /// Typing `(` after `def name` inside a class inserts `self` (`cls` for class methods).
    pub insert_self: bool,
    /// Tab moves the caret past an auto-inserted closing bracket or quote.
    pub jump_outside_brackets_with_tab: bool,
    /// Typing the third quote of `"""` inserts the closing `"""`.
    pub close_triple_quotes: bool,
    /// Right margin / hard wrap column (EditorConfig `max_line_length` overrides the setting).
    pub right_margin: usize,
    /// File-specific EditorConfig guides; None preserves the global setting.
    pub visual_guides: Option<Vec<usize>>,
    /// Insert safe physical line breaks while typing at the file margin.
    pub wrap_on_typing: bool,
}

impl Default for EditOptions {
    fn default() -> Self {
        Self {
            confirm_refactoring_undo: false,
            tab_size: 4,
            indent_size: 4,
            continuation_indent: None,
            use_tabs: false,
            smart_tabs: None,
            auto_indent: true,
            auto_close_brackets: true,
            auto_close_quotes: true,
            surround_selection: true,
            smart_home: true,
            allow_caret_after_end_of_line: false,
            allow_caret_after_end_of_line_in_block_selection: true,
            smart_backspace: true,
            camel_humps: false,
            page_lines: 30,
            paste_indent_block: true,
            paste_reformat: false,
            paste_format_options: rustpy_format::FormatOptions::pep8(),
            insert_self: true,
            jump_outside_brackets_with_tab: true,
            close_triple_quotes: true,
            right_margin: 120,
            visual_guides: None,
            wrap_on_typing: false,
        }
    }
}

impl EditOptions {
    /// Options for the editor settings (code style overrides such as
    /// `.editorconfig` are applied by the caller).
    pub fn from_settings(e: &rustpy_settings::EditorSettings) -> Self {
        Self {
            confirm_refactoring_undo: false,
            tab_size: e.tab_size,
            indent_size: e.indent_size,
            continuation_indent: None,
            use_tabs: e.use_tab_character,
            smart_tabs: None,
            auto_indent: e.auto_indent,
            auto_close_brackets: e.auto_close_brackets,
            auto_close_quotes: e.auto_close_quotes,
            surround_selection: e.surround_selection_on_typing,
            smart_home: e.smart_home_end,
            allow_caret_after_end_of_line: e.allow_caret_after_end_of_line,
            allow_caret_after_end_of_line_in_block_selection: e
                .allow_caret_after_end_of_line_in_block_selection,
            smart_backspace: e.smart_backspace,
            camel_humps: e.camel_humps_words,
            page_lines: 30,
            paste_indent_block: e.reformat_on_paste != rustpy_settings::ReformatOnPaste::None,
            paste_reformat: e.reformat_on_paste == rustpy_settings::ReformatOnPaste::Reformat,
            paste_format_options: rustpy_format::FormatOptions::pep8(),
            insert_self: e.insert_self_in_methods,
            jump_outside_brackets_with_tab: e.jump_outside_brackets_with_tab,
            close_triple_quotes: e.auto_close_triple_quotes,
            right_margin: e.right_margin,
            visual_guides: None,
            wrap_on_typing: e.wrap_on_typing,
        }
    }
}

/// What the last command did, for undo-group merging.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LastOp {
    None,
    Typing,
    Deleting,
    Other,
}

/// A single open editor (one document, many carets).
#[derive(Debug)]
pub struct EditorState {
    pub(crate) preferences: rustpy_settings::editor_pages::Preferences,
    data_syntax: data_syntax::Cache,
    pub framework_inputs: std::sync::Arc<rustpy_analysis::inspections::framework_tools::Inputs>,
    pub doc: Document,
    pub(super) shared_anchors: Option<super::shared_views::Anchors>,
    pub path: Option<PathBuf>,
    pub language: Language,
    pub carets: Vec<Caret>,
    pub overwrite_mode: bool,
    pub column_mode: bool,
    pub virtual_space: virtual_space::VirtualSpace,
    pub sticky_selection: bool,
    /// Rainbow identifiers (set from settings each frame; changes invalidate the highlight cache).
    pub semantic_highlighting: bool,
    pub rainbow_brackets: bool,
    /// Set by commands when the view should scroll to the primary caret.
    pub scroll_to_caret: bool,
    /// An edit was refused because the document is read-only (the app says so
    /// once; see [`EditorState::take_read_only_refusal`]).
    pub read_only_refused: bool,
    pub last_op: LastOp,
    /// Captured by Undo, consumed by the App confirmation dialog.
    pub pending_refactoring_undo: Option<(rustpy_text::HistoryId, u64)>,
    /// Highlight cache: (document version, spans).
    highlight: Option<(u64, Vec<Span>)>,
    template_policy: Option<(String, String)>,
    todo_highlights: todo_highlights::Cache,
    temporary_injection: Option<super::language_injections::Temporary>,
    injection_cache: Option<(DocumentId, u64, Option<PathBuf>, Vec<Span>)>,
    /// Syntax tree cache: (document version, parse, fold regions).
    syntax: Option<(u64, Parse, Vec<FoldRegion>)>,
    /// The text `syntax` was parsed from: the next reparse diffs against it
    /// (rebuilding it from the tree walked the whole file per keystroke).
    syntax_text: String,
    /// Built-in Python highlighting before overlays: (document, version, spans).
    python_base: Option<(DocumentId, u64, Vec<Span>)>,
    /// Rainbow (semantic) spans of a large file, computed off the UI thread.
    rainbow: BackgroundSpans,
    /// Language injection and doctest spans of a large file, likewise.
    injections: BackgroundSpans,
    /// Built-in highlighting of a large Python file, likewise.
    python_spans: BackgroundSpans,
    /// A large Python file's reparse running off the UI thread:
    /// (document, version, run giving the parse, its fold regions and text).
    syntax_job: Option<(DocumentId, u64, rustpy_core::TaskHandle<ParsedText>)>,
    /// A large Python file's fold regions and folded starts projected from
    /// an older parse onto the current version: (document, version, regions, folded).
    fold_projection: Option<(DocumentId, u64, Vec<FoldRegion>, HashSet<TextSize>)>,
    /// Test hook: reparses of a large Python file made on the UI thread.
    #[cfg(test)]
    pub(crate) blocking_reparses: usize,
    /// Current syntax projection; persistent fold intent lives in document markers.
    folded: HashSet<TextSize>,
    fold_markers: HashMap<MarkerId, FoldKind>,
    fold_document: DocumentId,
    /// Whether the "collapse imports/docstrings by default" settings were applied.
    pub default_folds_applied: bool,
    /// Offsets of closing brackets/quotes inserted by auto-pairing (Tab jumps past them).
    pub auto_closers: Vec<TextSize>,
    /// Lines to scroll the view by (Ctrl+Up/Down); consumed by the view.
    pub pending_scroll_lines: i32,
    /// How the view soft-wraps lines (set by the view each frame; `None` while
    /// soft wraps are off): Up/Down then move by visual rows.
    pub soft_wrap: Option<super::soft_wrap::SoftWrap>,
    /// Semantic diagnostics (unresolved references, …) computed by the app.
    pub extra_diagnostics: Vec<Diagnostic>,
    pub inspection_level: rustpy_settings::inspection_profiles::HighlightingLevel,
    /// Ruff results belong to this exact immutable document version.
    pub external_diagnostics: Option<(u64, Vec<Diagnostic>)>,
    pub plugin_diagnostics: Option<(u64, Vec<Diagnostic>)>,
    /// Inlay hints for `semantic_version` (sorted by offset).
    pub inlay_hints: Vec<rustpy_semantic::features::InlayHint>,
    /// Quick fixes per diagnostic range (parallel to `extra_diagnostics` entries that have fixes).
    pub quick_fixes: Vec<(TextRange, Vec<rustpy_analysis::inspections::QuickFix>)>,
    prose_findings: Option<ProseFindings>,
    /// Usages of the symbol under the caret: `(range, is_write)`.
    pub usage_highlights: Vec<(TextRange, bool)>,
    /// Caret offset the usage highlights were computed for.
    pub usage_highlight_caret: Option<(u64, TextSize)>,
    /// Document version the semantic diagnostics were computed for.
    pub semantic_version: Option<u64>,
    /// Document version last pushed to the analysis database.
    pub synced_version: Option<u64>,
    /// Stack of previous selection sets for *Shrink Selection*.
    pub selection_history: Vec<Vec<Caret>>,
    /// Carets right after the last Extend/Shrink Selection step.
    pub selection_extended: Option<Vec<Caret>>,
    /// Alt+J session: occurrences in the order they were added, and whether
    /// it started from the word at the caret (then whole words only).
    pub occurrences: Option<(Vec<TextRange>, bool)>,
    /// Column selection mode: block anchor and head (line, column) last set
    /// with Shift+arrows.
    pub block: Option<(rustpy_core::Position, rustpy_core::Position)>,
    /// Navigation: caret offsets before jumps (for Back/Forward).
    pub last_edit_offset: Option<TextSize>,
    /// TextMate grammar highlighting a file type without a built-in highlighter.
    textmate: Option<Box<TextMateHighlight>>,
    /// Indentation found in the file when it was opened or reloaded.
    pub detected_indent: Option<rustpy_text::util::IndentStyle>,
    /// Indentation chosen for this file in the status bar (wins over settings,
    /// detection and EditorConfig).
    pub indent_override: Option<rustpy_text::util::IndentStyle>,
    /// Rendered documentation comments and reader mode (see `doc_render`).
    pub doc_render: super::doc_render::DocRender,
}

impl EditorState {
    /// Whether typing was refused because the file is read-only since the last call.
    pub fn take_read_only_refusal(&mut self) -> bool {
        std::mem::take(&mut self.read_only_refused)
    }

    pub fn new(doc: Document, path: Option<PathBuf>) -> Self {
        let language = path
            .as_deref()
            .map(Language::from_path)
            .unwrap_or(Language::PlainText);
        let fold_document = doc.identity();
        let mut s = Self {
            preferences: Default::default(),
            data_syntax: Default::default(),
            framework_inputs: Default::default(),
            doc,
            shared_anchors: None,
            path,
            language,
            carets: vec![Caret::at(TextSize::new(0))],
            overwrite_mode: false,
            column_mode: false,
            virtual_space: Default::default(),
            sticky_selection: false,
            semantic_highlighting: false,
            rainbow_brackets: false,
            scroll_to_caret: true,
            read_only_refused: false,
            last_op: LastOp::None,
            pending_refactoring_undo: None,
            highlight: None,
            template_policy: None,
            todo_highlights: Default::default(),
            temporary_injection: None,
            injection_cache: None,
            syntax: None,
            syntax_text: String::new(),
            python_base: None,
            rainbow: BackgroundSpans::default(),
            injections: BackgroundSpans::default(),
            python_spans: BackgroundSpans::default(),
            syntax_job: None,
            fold_projection: None,
            #[cfg(test)]
            blocking_reparses: 0,
            folded: HashSet::new(),
            fold_markers: HashMap::new(),
            fold_document,
            default_folds_applied: false,
            auto_closers: Vec::new(),
            pending_scroll_lines: 0,
            soft_wrap: None,
            extra_diagnostics: Vec::new(),
            inspection_level: Default::default(),
            external_diagnostics: None,
            plugin_diagnostics: None,
            inlay_hints: Vec::new(),
            quick_fixes: Vec::new(),
            prose_findings: None,
            usage_highlights: Vec::new(),
            usage_highlight_caret: None,
            semantic_version: None,
            synced_version: None,
            selection_history: Vec::new(),
            selection_extended: None,
            occurrences: None,
            block: None,
            last_edit_offset: None,
            textmate: None,
            detected_indent: None,
            indent_override: None,
            doc_render: Default::default(),
        };
        s.redetect_indent();
        s
    }

    pub fn from_text(text: &str, language: Language) -> Self {
        let mut s = Self::new(Document::new(text), None);
        s.language = language;
        s.redetect_indent();
        s
    }

    /// Detects the file's indentation from (the start of) its text.
    pub fn redetect_indent(&mut self) {
        const SAMPLE_LINES: usize = 4000;
        let rope = self.doc.rope();
        let end = rope.line_to_byte(rope.len_lines().min(SAMPLE_LINES));
        let sample = rope.byte_slice(..end).to_string();
        self.detected_indent =
            rustpy_text::util::detect_indent(&sample, self.language.is_pythonic());
    }

    /// Reload from disk: replaces the text (keeping the file's line separators
    /// and encoding) and detects the indentation again.
    pub fn reload_from_bytes(&mut self, bytes: &[u8]) -> Result<(), rustpy_text::EncodingError> {
        self.reload_from_bytes_with_encoding(bytes, None)
    }

    pub fn reload_from_bytes_with_encoding(
        &mut self,
        bytes: &[u8],
        selected: Option<rustpy_text::Encoding>,
    ) -> Result<(), rustpy_text::EncodingError> {
        let policy = self
            .path
            .as_deref()
            .map(rustpy_text::SourcePolicy::for_path)
            .unwrap_or(rustpy_text::SourcePolicy::Generic);
        let loaded = Document::from_bytes_with_policy(bytes, policy, selected)?;
        self.reload_from_document(loaded);
        Ok(())
    }

    /// [`EditorState::reload_from_bytes`] with a document decoded elsewhere
    /// (a background reload of a large file): the carets stay where they were.
    pub fn reload_from_document(&mut self, loaded: Document) {
        self.clear_virtual_history();
        self.sticky_selection = false;
        let carets: Vec<_> = self
            .carets
            .iter()
            .map(|caret| {
                (
                    self.doc.offset_to_position(caret.sel.anchor),
                    self.doc.offset_to_position(caret.sel.head),
                )
            })
            .collect();
        self.doc.reload_from(loaded);
        self.set_carets(
            carets
                .into_iter()
                .map(|(anchor, head)| {
                    Caret::new(
                        self.doc.position_to_offset(anchor),
                        self.doc.position_to_offset(head),
                    )
                })
                .collect(),
        );
        self.redetect_indent();
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "Untitled".into())
    }

    // ----- carets -------------------------------------------------------------

    pub fn primary(&self) -> Caret {
        self.carets[0]
    }

    pub fn primary_position(&self) -> Position {
        self.doc.offset_to_position(self.primary().head())
    }

    pub fn selections(&self) -> Vec<Selection> {
        self.carets.iter().map(|c| c.sel).collect()
    }

    pub fn has_selection(&self) -> bool {
        self.carets.iter().any(Caret::has_selection)
            || (self.virtual_positions_enabled()
                && (0..self.carets.len()).any(|i| {
                    let position = self.virtual_caret(i);
                    position.anchor != position.head
                }))
    }

    /// Replaces all carets, clamping to the document and merging overlaps.
    pub fn set_carets(&mut self, carets: Vec<Caret>) {
        // Offsets may be stale (navigation history, analysis results): keep
        // them inside the text and on character boundaries.
        let mut carets: Vec<Caret> = carets
            .into_iter()
            .map(|mut c| {
                c.sel.anchor = self.doc.clamp(c.sel.anchor);
                c.sel.head = self.doc.clamp(c.sel.head);
                c
            })
            .collect();
        if carets.is_empty() {
            carets.push(Caret::at(TextSize::new(0)));
        }
        // Keep the primary caret first; sort the rest by position.
        let primary = carets[0];
        carets.sort_by_key(|c| (c.range().start(), c.range().end()));
        let mut merged: Vec<Caret> = Vec::with_capacity(carets.len());
        for c in carets {
            if let Some(last) = merged.last_mut() {
                let overlaps = if last.has_selection() || c.has_selection() {
                    c.range().start() < last.range().end()
                        || (c.range().start() == last.range().end()
                            && (last.has_selection() && c.has_selection()))
                } else {
                    c.sel.head == last.sel.head
                        && (!self.virtual_space.enabled || c.goal_column == last.goal_column)
                };
                if overlaps {
                    let start = last.range().start().min(c.range().start());
                    let end = last.range().end().max(c.range().end());
                    let forward = c.sel.head >= c.sel.anchor;
                    last.sel = if forward {
                        Selection::new(start, end)
                    } else {
                        Selection::new(end, start)
                    };
                    last.goal_column = None;
                    continue;
                }
            }
            merged.push(c);
        }
        if let Some(idx) = merged.iter().position(|c| {
            c.sel == primary.sel
                && (!self.virtual_space.enabled || c.goal_column == primary.goal_column)
        }) {
            merged.swap(0, idx);
        }
        self.carets = merged;
        self.scroll_to_caret = true;
        self.forget_passed_closers();
    }

    /// Drops auto-inserted closers that no caret can still jump over: the
    /// caret moved past them or left their line.
    fn forget_passed_closers(&mut self) {
        if self.auto_closers.is_empty() {
            return;
        }
        let doc = &self.doc;
        let carets = &self.carets;
        self.auto_closers.retain(|&c| {
            c < doc.len()
                && carets.iter().any(|k| {
                    !k.has_selection() && k.head() <= c && doc.line_of(k.head()) == doc.line_of(c)
                })
        });
    }

    pub fn set_selections(&mut self, sels: Vec<Selection>) {
        self.set_carets(
            sels.into_iter()
                .map(|sel| Caret {
                    sel,
                    goal_column: None,
                })
                .collect(),
        );
    }

    /// Collapses to the primary caret (Esc).
    pub fn collapse_to_primary(&mut self) {
        let mut p = self.primary();
        p.sel = Selection::caret(p.sel.head);
        self.carets = vec![p];
        self.scroll_to_caret = true;
    }

    pub fn move_primary_to(&mut self, pos: Position) {
        let off = self.doc.position_to_offset(pos);
        self.set_carets(vec![Caret::at(off)]);
        self.last_op = LastOp::Other;
    }

    // ----- editing primitives ------------------------------------------------

    /// Applies `edits` and sets the given new carets (in post-edit coordinates).
    pub fn apply(
        &mut self,
        edits: Vec<Edit>,
        new_carets: Vec<Caret>,
        group: EditGroup,
        label: Option<&str>,
    ) -> bool {
        let virtual_before = self.virtual_history_snapshot();
        let before = self.selections();
        let after: Vec<Selection> = new_carets.iter().map(|c| c.sel).collect();
        let mut sorted_edits = if self.auto_closers.is_empty() {
            Vec::new()
        } else {
            edits.clone()
        };
        match self.doc.edit(edits, &before, &after, group, label) {
            Ok(()) => {
                self.remember_virtual_history(virtual_before);
                if !self.auto_closers.is_empty() {
                    // Closers move with the text; a closer whose character was replaced is gone.
                    sorted_edits.sort_by_key(|e| (e.range.start(), e.range.end()));
                    let closers = std::mem::take(&mut self.auto_closers);
                    self.auto_closers = closers
                        .into_iter()
                        .filter(|&c| {
                            !sorted_edits
                                .iter()
                                .any(|e| e.range.start() <= c && c < e.range.end())
                        })
                        .map(|c| map_offset(&sorted_edits, c, true))
                        .collect();
                }
                self.set_carets(new_carets);
                self.last_edit_offset = Some(self.primary().head());
                true
            }
            Err(e) => {
                if matches!(e, rustpy_text::EditError::ReadOnly) {
                    self.read_only_refused = true;
                } else {
                    log::warn!("edit rejected: {e}");
                }
                false
            }
        }
    }

    /// Undo-group policy for a command of the given kind.
    pub fn group_for(&mut self, op: LastOp) -> EditGroup {
        let g = if op == self.last_op && matches!(op, LastOp::Typing | LastOp::Deleting) {
            EditGroup::Merge
        } else {
            EditGroup::Open
        };
        self.last_op = op;
        g
    }

    pub fn undo(&mut self) -> bool {
        self.last_op = LastOp::Other;
        self.auto_closers.clear();
        match self.doc.undo() {
            Some(sels) => {
                self.set_selections(sels);
                self.restore_virtual_history();
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        self.last_op = LastOp::Other;
        self.auto_closers.clear();
        match self.doc.redo() {
            Some(sels) => {
                self.set_selections(sels);
                self.restore_virtual_history();
                true
            }
            None => false,
        }
    }

    /// Return to a retained undo branch and restore its editor selection.
    pub fn select_history(
        &mut self,
        id: rustpy_text::HistoryId,
    ) -> Result<(), rustpy_text::HistoryError> {
        if self.doc.is_read_only() {
            return Err(rustpy_text::HistoryError::ReadOnly);
        }
        if self.doc.transaction_depth() > 0 {
            return Err(rustpy_text::HistoryError::TransactionActive);
        }
        if id == self.doc.current_history() {
            return Ok(());
        }
        let selections = self.doc.select_history(id)?;
        self.last_op = LastOp::Other;
        self.auto_closers.clear();
        self.set_selections(selections);
        self.restore_virtual_history();
        Ok(())
    }

    pub fn begin_transaction(
        &mut self,
        label: &str,
    ) -> Result<rustpy_text::TransactionToken, rustpy_text::HistoryError> {
        self.doc.begin_transaction(&self.selections(), Some(label))
    }

    pub fn commit_transaction(
        &mut self,
        token: rustpy_text::TransactionToken,
    ) -> Result<(), rustpy_text::HistoryError> {
        self.doc.commit_transaction(token, &self.selections())
    }

    pub fn rollback_transaction(
        &mut self,
        token: rustpy_text::TransactionToken,
    ) -> Result<(), rustpy_text::HistoryError> {
        let selections = self.doc.rollback_transaction(token)?;
        self.last_op = LastOp::Other;
        self.auto_closers.clear();
        self.set_selections(selections);
        Ok(())
    }

    pub(crate) fn inject_language(
        &mut self,
        language: super::language_injections::Language,
    ) -> Result<(), String> {
        if self.language != Language::Python {
            return Err("Language injection requires a Python string".into());
        }
        let parse = self.syntax().clone();
        let temporary = super::language_injections::Temporary::new(
            self,
            &parse,
            self.primary().head(),
            language,
        )?;
        self.temporary_injection = Some(temporary);
        self.injection_cache = None;
        self.highlight = None;
        Ok(())
    }
    pub(crate) fn remove_temporary_injection(&mut self) {
        self.temporary_injection = None;
        self.injection_cache = None;
        self.highlight = None;
    }
    pub(crate) fn current_injection(&mut self) -> Option<super::language_injections::Injection> {
        if self.language != Language::Python {
            return None;
        }
        // Injections live in string literals: elsewhere (most keystrokes of a
        // large file) the per-frame tree tells without a reparse.
        if self.reparses_in_background() && !self.refresh_frame_syntax() {
            let caret = self.primary().head();
            let root = self.syntax.as_ref().unwrap().1.syntax_node();
            let at = caret.min(root.text_range().end());
            let in_string = [
                root.token_at_offset(at).left_biased(),
                root.token_at_offset(at).right_biased(),
            ]
            .into_iter()
            .flatten()
            .any(|token| {
                matches!(
                    token.kind(),
                    rustpy_syntax::SyntaxKind::STRING
                        | rustpy_syntax::SyntaxKind::FSTRING_MIDDLE
                        | rustpy_syntax::SyntaxKind::FSTRING_START
                        | rustpy_syntax::SyntaxKind::FSTRING_END
                )
            });
            if !in_string {
                return None;
            }
        }
        let source = self.doc.text();
        let parse = self.syntax().clone();
        let caret = self.primary().head();
        super::language_injections::discover(
            &source,
            &parse,
            self.temporary_injection.as_ref(),
            self,
        )
        .into_iter()
        .find(|i| i.content.contains_inclusive(caret))
    }
    fn injected_highlights(&mut self) -> Vec<Span> {
        if self.language != Language::Python || self.is_large_file() {
            return vec![];
        }
        if self
            .temporary_injection
            .as_ref()
            .is_some_and(|temp| !temp.current(self))
        {
            self.temporary_injection = None;
            self.injection_cache = None;
        }
        if let Some((id, version, path, spans)) = &self.injection_cache {
            if *id == self.doc.identity() && *version == self.doc.version() && *path == self.path {
                return spans.clone();
            }
        }
        // A large file's are found off the UI thread (they walk the whole
        // tree); meanwhile the newest result, moved through the edits since,
        // stands in. A temporary injection is the user's explicit choice and
        // shows at once.
        if usize::from(self.doc.len()) >= BACKGROUND_SEMANTICS_BYTES
            && self.temporary_injection.is_none()
        {
            let base = self.reparse_base();
            let (doc, preferences, path) = (&self.doc, &self.preferences, &self.path);
            return self
                .injections
                .spans_for(doc, "Language injections", || {
                    let (text, preferences, path) = (doc.text(), preferences.clone(), path.clone());
                    move || {
                        let (parse, source) = reparsed(base, text);
                        injected_spans(&source, &parse, None, &preferences, path.as_deref())
                    }
                })
                .0;
        }
        let parse = self.syntax().clone();
        let source = self.doc.text();
        let spans = injected_spans(
            &source,
            &parse,
            self.temporary_injection.as_ref(),
            &self.preferences,
            self.path.as_deref(),
        );
        self.injection_cache = Some((
            self.doc.identity(),
            self.doc.version(),
            self.path.clone(),
            spans.clone(),
        ));
        spans
    }

    /// App supplies persisted TODO policy; unchanged settings never recompile.
    pub fn set_todo_settings(&mut self, settings: &rustpy_settings::todo::TodoSettings) {
        self.todo_highlights.configure(settings);
    }

    pub(super) fn todo_paint(&mut self, spans: &[Span]) -> Vec<todo_highlights::Paint> {
        self.todo_highlights.paint(&self.doc, spans).to_vec()
    }

    #[cfg(test)]
    pub(super) fn todo_cache_builds(&self) -> usize {
        self.todo_highlights.builds
    }

    // ----- highlighting -----------------------------------------------------

    /// Highlight spans for the current document version (cached). With a
    /// TextMate grammar: the tokenized lines, at least through the furthest
    /// caret (the view tokenizes the rest lazily, see
    /// [`Self::view_highlight_spans`]).
    pub fn highlight_spans(&mut self) -> &[Span] {
        let policy = (
            self.framework_inputs.template_engine.clone(),
            self.framework_inputs.template_data_language.clone(),
        );
        if self.language == Language::Template && self.template_policy.as_ref() != Some(&policy) {
            self.highlight = None;
            self.template_policy = Some(policy);
        }
        let version = self.doc.version();
        let stale = self.textmate.is_some()
            || self
                .injection_cache
                .as_ref()
                .is_some_and(|(id, _, path, _)| *id != self.doc.identity() || *path != self.path)
            || self
                .highlight
                .as_ref()
                .map(|(v, _)| *v != version)
                .unwrap_or(true);
        if stale {
            let spans = match self.language {
                _ if self.is_large_file() => Vec::new(),
                _ if self.textmate.is_some() => {
                    // What Copy as Rich Text needs; usually tokenized already. At most
                    // a few thousand more lines (about a second of work) are tokenized
                    // here on the UI thread; the rest is copied without colors.
                    let last = self
                        .carets
                        .iter()
                        .map(|c| self.doc.line_of(c.range().end()))
                        .max()
                        .unwrap_or(0);
                    self.textmate
                        .as_mut()
                        .map(|tm| {
                            let last = last.min(tm.up_to_date_lines() + RICH_COPY_EXTRA_LINES);
                            tm.spans_through(&self.doc, last)
                        })
                        .unwrap_or_default()
                }
                Language::Python if self.reparses_in_background() => {
                    let base = self.reparse_base();
                    let mut spans = self.background_python_spans(&base);
                    if self.semantic_highlighting {
                        let doc = &self.doc;
                        let rainbow = self.rainbow.spans_for(doc, "Semantic highlighting", || {
                            let text = doc.text();
                            move || Self::rainbow_spans(&reparsed(base, text).0)
                        });
                        spans = apply_overlay(spans, rainbow.0);
                    }
                    spans
                }
                Language::Python => {
                    let parse = self.syntax().clone();
                    let document = self.doc.identity();
                    let mut spans = match &self.python_base {
                        Some((d, v, spans)) if *d == document && *v == version => spans.clone(),
                        _ => {
                            let spans = python_syntax_spans(&self.doc.text(), &parse);
                            self.python_base = Some((document, version, spans.clone()));
                            spans
                        }
                    };
                    if self.semantic_highlighting {
                        spans = apply_overlay(spans, Self::rainbow_spans(&parse));
                    }
                    spans
                }
                Language::GitIgnore => {
                    rustpy_highlight::highlight_for("gitignore", &self.doc.text())
                }
                Language::DockerIgnore => {
                    rustpy_highlight::highlight_for("dockerignore", &self.doc.text())
                }
                Language::DotEnv => rustpy_highlight::highlight_for("dotenv", &self.doc.text()),
                Language::Makefile => rustpy_highlight::highlight_for("makefile", &self.doc.text()),
                Language::Regex => rustpy_highlight::highlight_for("regex", &self.doc.text()),
                Language::Diff => rustpy_highlight::highlight_for("diff", &self.doc.text()),
                Language::Csv => rustpy_highlight::highlight_for("csv", &self.doc.text()),
                Language::Properties => {
                    rustpy_highlight::highlight_for("properties", &self.doc.text())
                }
                Language::Markdown => rustpy_highlight::highlight_for("markdown", &self.doc.text()),
                Language::Json => rustpy_highlight::highlight_for("json", &self.doc.text()),
                Language::Toml => rustpy_highlight::highlight_for("toml", &self.doc.text()),
                Language::Yaml => rustpy_highlight::highlight_for("yaml", &self.doc.text()),
                Language::Ini => rustpy_highlight::highlight_for("ini", &self.doc.text()),
                Language::Shell => rustpy_highlight::highlight_for("shell", &self.doc.text()),
                Language::Html => rustpy_highlight::highlight_for("html", &self.doc.text()),
                Language::Css => rustpy_highlight::highlight_for("css", &self.doc.text()),
                Language::JavaScript => rustpy_highlight::highlight_for(
                    if self
                        .path
                        .as_ref()
                        .and_then(|path| path.extension())
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| matches!(extension, "jsx" | "tsx"))
                    {
                        "jsx"
                    } else {
                        "javascript"
                    },
                    &self.doc.text(),
                ),
                Language::Sql => rustpy_highlight::highlight_for("sql", &self.doc.text()),
                Language::Rst => rustpy_highlight::highlight_for("rst", &self.doc.text()),
                Language::Dockerfile => {
                    rustpy_highlight::highlight_for("dockerfile", &self.doc.text())
                }
                Language::Requirements => {
                    rustpy_highlight::highlight_for("requirements", &self.doc.text())
                }
                Language::Template => rustpy_highlight::template_engines::highlight(
                    &self.doc.text(),
                    self.path
                        .as_ref()
                        .and_then(|path| path.extension())
                        .and_then(|extension| extension.to_str())
                        .unwrap_or_default(),
                    &self.framework_inputs.template_engine,
                    &self.framework_inputs.template_data_language,
                ),
                Language::Gherkin => {
                    let text = self.doc.text();
                    rustpy_gherkin::highlight(&text)
                        .into_iter()
                        .filter_map(|(r, tag)| {
                            let tag = match tag {
                                "Keyword" => HighlightTag::Keyword,
                                "String" => HighlightTag::String,
                                "Comment" => HighlightTag::Comment,
                                "Tag" => HighlightTag::Decorator,
                                "Variable" => HighlightTag::Variable,
                                "Key" => HighlightTag::Key,
                                "Number" => HighlightTag::Number,
                                _ => return None,
                            };
                            Some(Span {
                                range: TextRange::new(
                                    TextSize::new(r.start as u32),
                                    TextSize::new(r.end as u32),
                                ),
                                tag,
                            })
                        })
                        .collect()
                }
                Language::Cython => {
                    let text = self.doc.text();
                    let spans = rustpy_highlight::highlight_python(&text);
                    apply_overlay(spans, cython_keyword_spans(&text))
                }
                _ => Vec::new(),
            };
            let spans = if self.rainbow_brackets && !self.is_large_file() {
                with_rainbow_brackets(spans, &self.doc.text())
            } else {
                spans
            };
            let spans = apply_overlay(spans, self.injected_highlights());
            let spans = apply_overlay(spans, super::preferences::keywords(self));
            self.highlight = Some((version, spans));
        }
        &self.highlight.as_ref().unwrap().1
    }

    /// Highlight spans for a view showing lines `first..=last`, and whether
    /// highlighting is still in progress (the view should paint another
    /// frame). Built-in highlighters return the spans of the whole document;
    /// a TextMate grammar tokenizes lazily, within a time budget per frame,
    /// and returns the spans of these lines.
    pub fn view_highlight_spans(&mut self, first: usize, last: usize) -> (Vec<Span>, bool) {
        if !self.is_large_file() {
            if let Some(tm) = self.textmate.as_mut() {
                if tm.warming_up() {
                    return (Vec::new(), true);
                }
                let done = tm.update(
                    &self.doc,
                    last.saturating_add(LOOKAHEAD_LINES),
                    Some(Instant::now() + FRAME_BUDGET),
                );
                let mut spans = tm.spans(&self.doc, first, last);
                if self.rainbow_brackets {
                    spans = apply_overlay(spans, tm.rainbow_overlay(&self.doc, first, last));
                }
                return (spans, !done);
            }
        }
        // A background run for a large file finished: overlay its spans (the
        // task's completion repainted the view).
        let rainbow = self.rainbow.poll() && self.semantic_highlighting;
        let injections = self.injections.poll();
        if self.python_spans.poll() || injections || rainbow {
            self.highlight = None;
        }
        (self.highlight_spans().to_vec(), false)
    }

    /// Built-in highlighting of a large Python file, computed off the UI
    /// thread (walking the whole tree takes longer than a frame in an
    /// unoptimised build). Until the run for this version is in, the newest
    /// result moved through the edits stands in, with the lines of the carets
    /// (where the typing is) lexed on their own to fill the gaps the edits left.
    fn background_python_spans(&mut self, base: &(Parse, String)) -> Vec<Span> {
        let doc = &self.doc;
        let (spans, current) = self.python_spans.spans_for(doc, "Highlighting", || {
            let (text, base) = (doc.text(), base.clone());
            move || {
                let (parse, text) = reparsed(base, text);
                python_syntax_spans(&text, &parse)
            }
        });
        if current {
            return spans;
        }
        let mut lines: Vec<usize> = self
            .carets
            .iter()
            .map(|caret| self.doc.line_of(caret.head()))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let mut extra = Vec::new();
        for line in lines.into_iter().take(64) {
            let range = self.doc.line_range(line);
            // Inside a multi-line string the line is not code.
            let i = spans.partition_point(|span| span.range.end() <= range.start());
            if spans.get(i).is_some_and(|span| {
                span.range.start() < range.start() && span.range.end() >= range.end()
            }) {
                continue;
            }
            let text = self.doc.line_text(line);
            extra.extend(
                rustpy_highlight::highlight_python(&text)
                    .into_iter()
                    .map(|span| Span {
                        range: span.range + range.start(),
                        tag: span.tag,
                    }),
            );
        }
        fill_gaps(spans, extra)
    }

    /// Highlights the file with a TextMate grammar whose language is called
    /// `name`, or stops doing so (`None`). A file highlighted by a grammar is
    /// source code in a language without built-in support
    /// ([`Language::Other`], so plain-text checks no longer apply); its
    /// previous language comes back when the grammar is removed.
    pub fn set_textmate_grammar(
        &mut self,
        grammar: Option<(Arc<rustpy_textmate::Grammar>, String)>,
    ) {
        match grammar {
            Some((grammar, name)) => {
                if let Some(tm) = self
                    .textmate
                    .as_mut()
                    .filter(|tm| Arc::ptr_eq(tm.grammar(), &grammar))
                {
                    tm.set_name(name);
                    return;
                }
                let base = self.base_language();
                self.textmate = Some(Box::new(TextMateHighlight::with_warm_up(
                    grammar, name, base, &self.doc,
                )));
                self.language = Language::Other;
            }
            None => {
                let Some(tm) = self.textmate.take() else {
                    return;
                };
                self.language = tm.base_language();
            }
        }
        self.highlight = None;
        // Findings of the previous language (plain-text spelling and grammar).
        self.extra_diagnostics.clear();
        self.quick_fixes.clear();
        self.prose_findings = None;
        self.semantic_version = None;
    }

    /// The TextMate grammar highlighting the file, if any.
    pub fn textmate_grammar(&self) -> Option<&Arc<rustpy_textmate::Grammar>> {
        self.textmate.as_ref().map(|tm| tm.grammar())
    }

    /// The language the file had before a TextMate grammar was attached
    /// (its language when none is).
    pub fn base_language(&self) -> Language {
        self.textmate
            .as_ref()
            .map_or(self.language, |tm| tm.base_language())
    }

    /// The language name for the status bar: the TextMate grammar's language
    /// when one highlights the file.
    pub fn language_label(&self) -> String {
        match &self.textmate {
            Some(tm) => tm.name().to_string(),
            None => self.language.display_name().to_string(),
        }
    }

    // ----- syntax -------------------------------------------------------------

    /// The parse tree for the current document version (cached). Only Python
    /// files are parsed; other languages return an empty module.
    pub fn syntax(&mut self) -> &Parse {
        self.ensure_syntax();
        &self.syntax.as_ref().unwrap().1
    }

    /// Files above this size get plain-text treatment (no parsing / highlighting).
    pub const LARGE_FILE_BYTES: usize = 4 * 1024 * 1024;

    pub fn is_large_file(&self) -> bool {
        usize::from(self.doc.len()) > Self::LARGE_FILE_BYTES
            || self.doc.line_count() > self.preferences.max_editor_lines.clamp(1000, 1_000_000)
    }

    pub fn invalidate_highlight(&mut self) {
        self.highlight = None;
    }

    /// Changes the language (a scratch file's "Change Language", a renamed
    /// extension): highlighting, syntax and analysis state of the old one are dropped.
    pub fn set_language(&mut self, language: Language) {
        if self.language == language {
            return;
        }
        self.language = language;
        self.highlight = None;
        self.syntax = None;
        self.expand_all_folds();
        // Language changes are editor metadata, not part of a text transaction.
        self.fold_markers.clear();
        self.semantic_version = None;
        self.synced_version = None;
        self.extra_diagnostics.clear();
        self.quick_fixes.clear();
        self.prose_findings = None;
        self.inlay_hints.clear();
        self.redetect_indent();
    }

    /// Rainbow spans for local variables and parameters (semantic highlighting).
    fn rainbow_spans(parse: &rustpy_syntax::Parse) -> Vec<Span> {
        use rustpy_semantic::scope::{BindingKind, ScopeKind};
        let model = rustpy_semantic::scope::build(parse);
        let mut out = Vec::new();
        let tags = [
            HighlightTag::Rainbow1,
            HighlightTag::Rainbow2,
            HighlightTag::Rainbow3,
            HighlightTag::Rainbow4,
            HighlightTag::Rainbow5,
            HighlightTag::Rainbow6,
        ];
        for (bi, b) in model.bindings.iter().enumerate() {
            let scope = model.scope(b.scope);
            if !matches!(
                scope.kind,
                ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
            ) {
                continue;
            }
            if !matches!(
                b.kind,
                BindingKind::Assignment
                    | BindingKind::AugAssignment
                    | BindingKind::Annotation
                    | BindingKind::Param
                    | BindingKind::StarParam
                    | BindingKind::StarStarParam
                    | BindingKind::For
                    | BindingKind::With
                    | BindingKind::Walrus
                    | BindingKind::Except
            ) {
                continue;
            }
            if b.name.as_str() == "self" || b.name.as_str() == "cls" || b.name.starts_with('_') {
                continue;
            }
            // Stable color per name within the scope.
            let mut h: u32 = 2166136261;
            for c in b.name.bytes() {
                h = (h ^ c as u32).wrapping_mul(16777619);
            }
            let tag = tags[(h as usize) % tags.len()];
            out.push(Span {
                range: b.range,
                tag,
            });
            for r in model.references_to(rustpy_semantic::scope::BindingId(bi as u32)) {
                out.push(Span {
                    range: r.range,
                    tag,
                });
            }
        }
        out.sort_by_key(|s| s.range.start());
        out.dedup_by_key(|s| s.range.start());
        out
    }

    fn ensure_syntax(&mut self) {
        if self.fold_document != self.doc.identity() {
            self.fold_document = self.doc.identity();
            self.fold_markers.clear();
            self.folded.clear();
            self.syntax = None;
            self.default_folds_applied = false;
        }
        let version = self.doc.version();
        // A finished background reparse may already be of this version.
        self.poll_syntax_job();
        let stale = self
            .syntax
            .as_ref()
            .map(|(v, _, _)| *v != version)
            .unwrap_or(true);
        if stale {
            #[cfg(test)]
            if self.syntax.is_some() && self.reparses_in_background() {
                self.blocking_reparses += 1;
                // Finds the caller when a latency test catches one.
                if std::env::var_os("RUSTPY_TRACE_REPARSE").is_some() {
                    eprintln!(
                        "BLOCKING REPARSE\n{}",
                        std::backtrace::Backtrace::force_capture()
                    );
                }
            }
            let text = if self.language == Language::Python && !self.is_large_file() {
                self.doc.text()
            } else {
                String::new()
            };
            // Incremental: reuse the subtrees of the previous parse the edits cannot affect.
            let old_text = std::mem::take(&mut self.syntax_text);
            let parse = match self.syntax.take() {
                Some((_, old, _)) => {
                    if old_text == text {
                        old
                    } else {
                        rustpy_syntax::reparse::reparse_text(&old, &old_text, &text).0
                    }
                }
                None => rustpy_syntax::parse(&text),
            };
            let doc = &self.doc;
            let folds = if self.language == Language::Python {
                syntax_support::fold_regions(&parse, &|off| doc.line_of(off))
            } else if self.is_large_file() || !super::text_folding::supports(self.language) {
                Vec::new()
            } else {
                super::text_folding::fold_regions(self.language, &doc.text(), &|off| {
                    doc.line_of(off)
                })
            };
            self.syntax = Some((version, parse, folds));
            self.syntax_text = text;
        }
        self.sync_fold_markers();
    }

    /// Whether the file is a large Python one, reparsed off the UI thread for
    /// the per-frame consumers (see [`Self::frame_syntax`]).
    fn reparses_in_background(&self) -> bool {
        self.language == Language::Python
            && !self.is_large_file()
            && usize::from(self.doc.len()) >= BACKGROUND_SEMANTICS_BYTES
    }

    /// Installs a finished background reparse when it is newer than the
    /// cached parse (fold markers are projected onto it only when it is of
    /// the current version: an older tree's regions do not match them).
    fn poll_syntax_job(&mut self) {
        let Some(result) = self
            .syntax_job
            .as_mut()
            .and_then(|(_, _, handle)| handle.try_result())
        else {
            return;
        };
        let Some((document, version, _)) = self.syntax_job.take() else {
            return;
        };
        let Ok((parse, folds, text)) = result else {
            return;
        };
        if document != self.doc.identity() || document != self.fold_document {
            return;
        }
        if self
            .syntax
            .as_ref()
            .is_some_and(|(installed, _, _)| *installed >= version)
        {
            return;
        }
        self.syntax = Some((version, parse, folds));
        self.syntax_text = text;
        self.fold_projection = None;
        if version == self.doc.version() {
            self.sync_fold_markers();
        }
    }

    /// For the consumers that run every frame: true when the cached parse is
    /// of the current version. A large Python file is not reparsed here: the
    /// reparse runs off the UI thread and the previous tree stands in until it
    /// is in (positions in it may be a few characters off; callers clamp).
    /// Other files are reparsed at once, as [`Self::syntax`] does.
    pub fn refresh_frame_syntax(&mut self) -> bool {
        if !self.reparses_in_background()
            || self.fold_document != self.doc.identity()
            || self.syntax.is_none()
        {
            self.ensure_syntax();
            return true;
        }
        self.poll_syntax_job();
        let version = self.doc.version();
        if self.syntax.as_ref().is_some_and(|(v, _, _)| *v == version) {
            self.sync_fold_markers();
            return true;
        }
        if self.syntax_job.is_none() {
            let (_, old, _) = self.syntax.as_ref().unwrap();
            let (old, old_text, text) = (old.clone(), self.syntax_text.clone(), self.doc.text());
            let handle = rustpy_core::TaskManager::global()
                .task("Parsing")
                .hidden()
                .spawn(move |_| Ok(parse_with_folds(&old, &old_text, text)));
            self.syntax_job = Some((self.doc.identity(), version, handle));
        }
        false
    }

    /// Where lexing may start to classify the text up to `offset` (a comment,
    /// a string, code) as lexing the whole text before it would: the start of
    /// its line, unless a string of the per-frame tree runs into the line or
    /// an edit since that tree moved it (then 0, as for small files, whose
    /// prefix is cheap to lex). Keeps the per-keystroke completion checks of
    /// a large file to one line.
    pub fn lex_restart(&mut self, offset: TextSize) -> usize {
        use rustpy_syntax::SyntaxKind as K;
        if !self.reparses_in_background() {
            return 0;
        }
        let offset = offset.min(self.doc.len());
        let line_start = self.doc.line_range(self.doc.line_of(offset)).start();
        if line_start == TextSize::new(0) {
            return 0;
        }
        self.refresh_frame_syntax();
        let (version, parse, _) = self.syntax.as_ref().unwrap();
        let Some(steps) = self.doc.changes_since(*version) else {
            return 0;
        };
        if steps
            .iter()
            .flatten()
            .any(|edit| edit.range.start() < line_start)
        {
            return 0;
        }
        let newline = line_start - TextSize::new(1);
        let root = parse.syntax_node();
        if newline >= root.text_range().end() {
            return 0;
        }
        let inside_string = [
            root.token_at_offset(newline).left_biased(),
            root.token_at_offset(newline).right_biased(),
        ]
        .into_iter()
        .flatten()
        .any(|token| {
            token.text_range().start() <= newline
                && token.text_range().end() > newline
                && matches!(
                    token.kind(),
                    K::STRING
                        | K::FSTRING_START
                        | K::FSTRING_MIDDLE
                        | K::FSTRING_END
                        | K::TSTRING_START
                        | K::TSTRING_MIDDLE
                        | K::TSTRING_END
                )
        });
        if inside_string {
            0
        } else {
            usize::from(line_start)
        }
    }

    /// The completion context at `offset` (see
    /// [`rustpy_semantic::completion_ctx::completion_context`]), lexing a
    /// large file from [`Self::lex_restart`] only.
    pub fn completion_context_at(
        &mut self,
        offset: TextSize,
    ) -> rustpy_semantic::completion_ctx::CompletionContext {
        let restart = self.lex_restart(offset);
        let end = usize::from(offset.min(self.doc.len()));
        let text = self.doc.slice(TextRange::new(
            TextSize::new(restart as u32),
            TextSize::new(end as u32),
        ));
        rustpy_semantic::features::completion_context(&text, TextSize::new((end - restart) as u32))
    }

    /// The parse tree for per-frame consumers: see [`Self::refresh_frame_syntax`].
    pub fn frame_syntax(&mut self) -> &Parse {
        self.refresh_frame_syntax();
        &self.syntax.as_ref().unwrap().1
    }

    /// The tree [`Self::frame_syntax`] gives and the text it was parsed from,
    /// for a background run that reparses the current text from them.
    pub(crate) fn reparse_base(&mut self) -> (Parse, String) {
        self.refresh_frame_syntax();
        let (_, parse, _) = self.syntax.as_ref().unwrap();
        (parse.clone(), self.syntax_text.clone())
    }

    /// Fold regions and folded starts for per-frame consumers: the current
    /// ones, or for a large file whose reparse is in flight, the previous
    /// ones moved through the edits made since (fold markers are left alone).
    fn frame_folds(&mut self) -> (Vec<FoldRegion>, HashSet<TextSize>) {
        if self.refresh_frame_syntax() {
            return (self.syntax.as_ref().unwrap().2.clone(), self.folded.clone());
        }
        let (document, version) = (self.doc.identity(), self.doc.version());
        if let Some((d, v, regions, folded)) = &self.fold_projection {
            if *d == document && *v == version {
                return (regions.clone(), folded.clone());
            }
        }
        let (parsed, _, regions) = self.syntax.as_ref().unwrap();
        let Some(steps) = self.doc.changes_since(*parsed) else {
            return (Vec::new(), HashSet::new());
        };
        let regions: Vec<FoldRegion> = regions
            .iter()
            .filter_map(|region| {
                let range = map_range_through_edits(region.range, &steps)?;
                let range = TextRange::new(
                    range.start().min(self.doc.len()),
                    range.end().min(self.doc.len()),
                );
                let (first_line, last_line) = (
                    self.doc.line_of(range.start()),
                    self.doc.line_of(range.end()),
                );
                (last_line > first_line).then(|| FoldRegion {
                    range,
                    first_line,
                    last_line,
                    placeholder: region.placeholder.clone(),
                    kind: region.kind,
                })
            })
            .collect();
        let folded: HashSet<TextSize> = regions
            .iter()
            .filter(|region| self.is_folded(region))
            .map(|region| region.range.start())
            .collect();
        self.fold_projection = Some((document, version, regions.clone(), folded.clone()));
        (regions, folded)
    }

    /// Fold regions for the view, which draws every frame (see [`Self::frame_folds`]).
    pub fn frame_fold_regions(&mut self) -> Vec<FoldRegion> {
        self.frame_folds().0
    }

    /// Refresh even without a text change: rollback can restore marker membership.
    fn sync_fold_markers(&mut self) {
        let regions = &self.syntax.as_ref().unwrap().2;
        let pending = self.doc.transaction_depth() != 0;
        self.folded.clear();
        self.fold_markers.retain(|id, kind| {
            let projected = match self.doc.marker(*id) {
                Ok(Some(MarkerPosition::Point(anchor))) => regions
                    .iter()
                    .find(|region| Self::fold_anchor_matches(*kind, anchor, region)),
                _ => None,
            };
            if let Some(region) = projected {
                self.folded.insert(region.range.start());
                true
            } else if pending {
                true
            } else {
                let _ = self.doc.remove_marker(*id);
                false
            }
        });
    }

    fn fold_anchor_matches(kind: FoldKind, anchor: TextSize, region: &FoldRegion) -> bool {
        kind == region.kind
            && (anchor == region.range.start()
                || (matches!(kind, FoldKind::Imports | FoldKind::Docstring)
                    && region.range.contains(anchor)))
    }

    /// Fresh folded starts for rendering clients, including metadata-only rollback.
    pub fn folded_starts(&mut self) -> HashSet<TextSize> {
        self.frame_folds().1
    }

    fn add_fold_start(&mut self, start: TextSize) {
        let Some(region) = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .find(|region| region.range.start() == start)
        else {
            return;
        };
        if self.is_folded(region) {
            return;
        }
        let kind = region.kind;
        // Token starts follow a prefix; colon/opening-token ends remain before
        // trailing header text. Import anchors can project into an enlarged group.
        let affinity = if matches!(
            kind,
            FoldKind::Imports | FoldKind::Docstring | FoldKind::Region
        ) {
            Affinity::After
        } else {
            Affinity::Before
        };
        if let Ok(id) = self
            .doc
            .point_marker(start, affinity, PointDeletion::InvalidateInterior)
        {
            self.fold_markers.insert(id, kind);
            self.folded.insert(start);
        }
    }

    /// Expand a header through its document-owned anchor. Removed IDs remain known
    /// during a transaction so rollback can restore the pre-scope fold membership.
    pub fn unfold_start(&mut self, start: TextSize) {
        self.ensure_syntax();
        let regions = &self.syntax.as_ref().unwrap().2;
        let ids: Vec<_> = self
            .fold_markers
            .iter()
            .filter_map(|(id, kind)| {
                let Ok(Some(MarkerPosition::Point(anchor))) = self.doc.marker(*id) else {
                    return None;
                };
                regions
                    .iter()
                    .any(|region| {
                        region.range.start() == start
                            && Self::fold_anchor_matches(*kind, anchor, region)
                    })
                    .then_some(*id)
            })
            .collect();
        for id in ids {
            let _ = self.doc.remove_marker(id);
            if self.doc.transaction_depth() == 0 {
                self.fold_markers.remove(&id);
            }
        }
        self.folded.remove(&start);
    }

    pub fn fold_regions(&mut self) -> &[FoldRegion] {
        self.ensure_syntax();
        &self.syntax.as_ref().unwrap().2
    }

    /// Diagnostics for the current version: semantic ones when up to date
    /// (they include syntax errors), otherwise syntax errors only.
    /// Structured data syntax has a maintained parser independent of Python analysis.
    pub fn has_data_syntax(&self) -> bool {
        data_syntax::supports(self.language)
    }

    /// True only after diagnostics captured this exact authority without a limit.
    pub fn data_syntax_current(&self) -> bool {
        self.has_data_syntax()
            && self.data_syntax.current(
                &self.doc,
                self.language,
                self.path.as_deref(),
                &self.framework_inputs,
            )
    }

    pub fn diagnostics(&mut self) -> Vec<Diagnostic> {
        let mut diagnostics = self.built_in_diagnostics();
        use rustpy_settings::inspection_profiles::HighlightingLevel;
        if self.inspection_level != HighlightingLevel::None
            && self.inspection_level != HighlightingLevel::Syntax
        {
            if let Some((version, plugin)) = &self.plugin_diagnostics {
                if *version == self.doc.version() {
                    diagnostics.extend(plugin.iter().cloned());
                }
            }
        }
        diagnostics
    }

    /// Publish prose analysis independently of structural language results.
    pub fn set_prose_diagnostics(
        &mut self,
        diagnostics: Vec<Diagnostic>,
        fixes: Vec<(TextRange, Vec<rustpy_analysis::inspections::QuickFix>)>,
    ) {
        self.prose_findings = Some(ProseFindings {
            document: self.doc.identity(),
            version: self.doc.version(),
            diagnostics: diagnostics.clone(),
            fixes: fixes.clone(),
        });
        self.extra_diagnostics = diagnostics;
        self.quick_fixes = fixes;
        self.semantic_version = Some(self.doc.version());
    }

    fn built_in_diagnostics(&mut self) -> Vec<Diagnostic> {
        use rustpy_settings::inspection_profiles::HighlightingLevel;
        if self.inspection_level == HighlightingLevel::None {
            if data_syntax::supports(self.language) {
                self.quick_fixes.clear();
            }
            return Vec::new();
        }
        if self.language == Language::Cython {
            return rustpy_semantic::cython::diagnostics(&self.doc.text());
        }
        if self.inspection_level == HighlightingLevel::Syntax
            && !data_syntax::supports(self.language)
        {
            return syntax_support::diagnostics(self.syntax());
        }
        if data_syntax::supports(self.language) {
            let mut diagnostics = self.data_syntax.diagnostics(
                &self.doc,
                self.language,
                self.path.as_deref(),
                &self.framework_inputs,
            );
            if self.inspection_level == HighlightingLevel::Syntax {
                diagnostics.retain(|d| {
                    d.code.ends_with("-syntax")
                        || d.code.starts_with("data-syntax")
                        || matches!(
                            d.code.as_str(),
                            "ShellSyntax"
                                | "JavaScriptSyntax"
                                | "HtmlSyntax"
                                | "CssSyntax"
                                | "SqlDialect"
                        )
                });
                self.quick_fixes.clear();
            } else {
                self.quick_fixes = self.data_syntax.fixes.clone();
                if let Some(prose) = self.prose_findings.as_ref().filter(|prose| {
                    prose.document == self.doc.identity()
                        && prose.version == self.doc.version()
                        && self.semantic_version == Some(prose.version)
                }) {
                    diagnostics.extend(prose.diagnostics.iter().cloned());
                    self.quick_fixes.extend(prose.fixes.iter().cloned());
                }
            }
            if let Some((version, external)) = &self.external_diagnostics {
                if *version == self.doc.version() {
                    diagnostics.extend(external.iter().cloned());
                }
            }
            return diagnostics;
        }
        let semantic_current = self.semantic_version == Some(self.doc.version())
            && self.prose_findings.as_ref().is_none_or(|prose| {
                prose.document == self.doc.identity() && prose.version == self.doc.version()
            });
        let mut diagnostics = if semantic_current {
            self.extra_diagnostics.clone()
        } else if self.refresh_frame_syntax() {
            syntax_support::diagnostics(self.syntax())
        } else {
            // A large file's reparse is in flight: the previous tree's errors,
            // moved through the edits since (an edited error waits for it).
            let (parsed, parse, _) = self.syntax.as_ref().unwrap();
            let steps = self.doc.changes_since(*parsed).unwrap_or_default();
            syntax_support::diagnostics(parse)
                .into_iter()
                .filter_map(|mut d| {
                    d.range = map_range_through_edits(d.range, &steps)?;
                    Some(d)
                })
                .collect()
        };
        if let Some((version, external)) = &self.external_diagnostics {
            if *version == self.doc.version() {
                diagnostics.extend(external.iter().cloned());
            }
        }
        diagnostics
    }

    pub fn structure(&mut self) -> Vec<StructureItem> {
        if self.language != Language::Python && self.language != Language::Cython {
            if usize::from(self.doc.len()) > 2 * 1024 * 1024 || self.is_large_file() {
                return Vec::new();
            }
            if let Some(path) = &self.path {
                return super::data_documents::outline(&self.doc.text(), path);
            }
        }
        self.ensure_syntax();
        let (_, parse, _) = self.syntax.as_ref().unwrap();
        let doc = &self.doc;
        syntax_support::structure(parse, &|off| doc.line_of(off))
    }

    /// The definitions around the caret (from the per-frame tree, so a large
    /// file's may lag a keystroke behind; ranges are clamped to the text).
    pub fn breadcrumbs(&mut self) -> Vec<(String, TextRange)> {
        let head = self.primary().head();
        let len = self.doc.len();
        syntax_support::breadcrumbs(self.frame_syntax(), head.min(len))
            .into_iter()
            .map(|(name, range)| {
                (
                    name,
                    TextRange::new(range.start().min(len), range.end().min(len)),
                )
            })
            .collect()
    }

    /// Whether `region` is currently folded.
    pub fn is_folded(&self, region: &FoldRegion) -> bool {
        self.fold_markers.iter().any(|(id, kind)| matches!(self.doc.marker(*id), Ok(Some(MarkerPosition::Point(anchor))) if Self::fold_anchor_matches(*kind, anchor, region)))
    }

    /// Toggles the fold marker of `line`: unfolds the regions starting there
    /// when one is folded, else folds the outermost one.
    pub fn toggle_fold_at_line(&mut self, line: usize) {
        self.ensure_syntax();
        let on_line: Vec<FoldRegion> = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .filter(|f| f.first_line == line)
            .cloned()
            .collect();
        let Some(outermost) = on_line.first().cloned() else {
            return;
        };
        if on_line.iter().any(|f| self.is_folded(f)) {
            for f in &on_line {
                self.unfold_start(f.range.start());
            }
        } else {
            self.fold(&outermost);
        }
    }

    /// Innermost region containing `line` (optionally only unfolded ones).
    fn innermost_region_at(&mut self, line: usize, unfolded_only: bool) -> Option<FoldRegion> {
        self.ensure_syntax();
        let folded = &self.folded;
        self.syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .filter(|f| {
                f.first_line <= line
                    && line <= f.last_line
                    && !(unfolded_only && folded.contains(&f.range.start()))
            })
            .max_by_key(|f| (f.first_line, std::cmp::Reverse(f.last_line)))
            .cloned()
    }

    /// Folds/unfolds the innermost region containing the primary caret.
    pub fn toggle_fold_at_caret(&mut self) {
        let line = self.doc.line_of(self.primary().head());
        if let Some(r) = self.innermost_region_at(line, false) {
            self.toggle_fold_at_line(r.first_line);
        }
    }

    /// *Collapse* (Ctrl+Minus): folds the innermost expanded region containing
    /// the caret, so repeated use collapses outward.
    pub fn collapse_region_at_caret(&mut self) {
        let line = self.doc.line_of(self.primary().head());
        if let Some(r) = self.innermost_region_at(line, true) {
            self.fold(&r);
        }
    }

    /// *Expand* (Ctrl+Equals): unfolds the folded region whose header holds the caret.
    pub fn expand_region_at_caret(&mut self) {
        let line = self.doc.line_of(self.primary().head());
        self.ensure_syntax();
        let target = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .find(|f| f.first_line == line && self.folded.contains(&f.range.start()))
            .map(|f| f.range.start());
        if let Some(start) = target {
            self.unfold_start(start);
        }
    }

    /// Folds `region`; carets inside it move to the end of its header line so
    /// the fold is not undone by revealing the caret.
    fn fold(&mut self, region: &FoldRegion) {
        self.add_fold_start(region.range.start());
        self.move_carets_out_of_folds();
    }

    fn move_carets_out_of_folds(&mut self) {
        let hidden = self.hidden_line_ranges();
        if hidden.is_empty() {
            return;
        }
        let mut moved = false;
        let carets: Vec<Caret> = self
            .carets
            .iter()
            .map(|k| {
                let line = self.doc.line_of(k.head());
                match hidden.iter().find(|(a, b)| *a <= line && line <= *b) {
                    Some((a, _)) => {
                        moved = true;
                        Caret::at(self.doc.line_range(a - 1).end())
                    }
                    None => *k,
                }
            })
            .collect();
        if moved {
            self.set_carets(carets);
        }
    }

    pub fn expand_all_folds(&mut self) {
        for id in self.fold_markers.keys() {
            let _ = self.doc.remove_marker(*id);
        }
        if self.doc.transaction_depth() == 0 {
            self.fold_markers.clear();
        }
        self.folded.clear();
    }

    pub fn collapse_all_folds(&mut self) {
        self.ensure_syntax();
        let starts: Vec<TextSize> = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .filter(|f| f.kind != super::syntax_support::FoldKind::Brackets)
            .map(|f| f.range.start())
            .collect();
        for start in starts {
            self.add_fold_start(start);
        }
        self.move_carets_out_of_folds();
    }

    /// Applies Settings › Code Folding "collapse imports/docstrings by default"
    /// once per opened file.
    pub fn apply_default_folds(&mut self, imports: bool, docstrings: bool) {
        if self.fold_document == self.doc.identity() && self.default_folds_applied {
            return;
        }
        self.ensure_syntax();
        if self.default_folds_applied {
            return;
        }
        self.default_folds_applied = true;
        if !imports && !docstrings {
            return;
        }
        use super::syntax_support::FoldKind;
        let starts: Vec<TextSize> = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .filter(|f| {
                (imports && f.kind == FoldKind::Imports)
                    || (docstrings && f.kind == FoldKind::Docstring)
            })
            .map(|f| f.range.start())
            .collect();
        if !starts.is_empty() {
            for start in starts {
                self.add_fold_start(start);
            }
            self.move_carets_out_of_folds();
        }
    }

    /// Unfolds every region containing `offset` so the caret is visible.
    pub fn reveal_offset(&mut self, offset: TextSize) {
        let line = self.doc.line_of(offset);
        let (regions, folded) = self.frame_folds();
        if !regions.iter().any(|f| {
            f.first_line < line && line <= f.last_line && folded.contains(&f.range.start())
        }) {
            return;
        }
        // Unfolding edits fold markers: against the current tree.
        self.ensure_syntax();
        let to_unfold: Vec<TextSize> = self
            .syntax
            .as_ref()
            .unwrap()
            .2
            .iter()
            .filter(|f| {
                f.first_line < line && line <= f.last_line && self.folded.contains(&f.range.start())
            })
            .map(|f| f.range.start())
            .collect();
        for s in to_unfold {
            self.unfold_start(s);
        }
    }

    /// Lines hidden by folds, as `(first_hidden, last_hidden)` inclusive ranges, sorted & merged.
    pub fn hidden_line_ranges(&mut self) -> Vec<(usize, usize)> {
        let (regions, folded) = self.frame_folds();
        let mut ranges: Vec<(usize, usize)> = regions
            .iter()
            .filter(|f| folded.contains(&f.range.start()))
            .map(|f| (f.first_line + 1, f.last_line))
            .collect();
        ranges.sort();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for r in ranges {
            match merged.last_mut() {
                Some(last) if r.0 <= last.1 + 1 => last.1 = last.1.max(r.1),
                _ => merged.push(r),
            }
        }
        merged
    }

    /// Publish a validated, successfully written save as one staged history
    /// unit. Carets follow each intermediate edit coordinate space.
    pub fn commit_prepared_save(
        &mut self,
        plan: rustpy_text::PreparedSavePlan,
    ) -> Result<(), rustpy_text::PreparedSaveError> {
        let before = self.selections();
        let mut carets = self.carets.clone();
        let mut closers = self.auto_closers.clone();
        for edits in plan.stages() {
            carets = carets
                .into_iter()
                .map(|caret| {
                    Caret::new(
                        map_offset(edits, caret.sel.anchor, false),
                        map_offset(edits, caret.sel.head, false),
                    )
                })
                .collect();
            closers = closers
                .into_iter()
                .filter(|&offset| {
                    !edits
                        .iter()
                        .any(|edit| edit.range.start() <= offset && offset < edit.range.end())
                })
                .map(|offset| map_offset(edits, offset, true))
                .collect();
        }
        let after: Vec<_> = carets.iter().map(|caret| caret.sel).collect();
        self.doc.commit_prepared_save(plan, &before, &after)?;
        self.set_carets(carets);
        self.auto_closers = closers;
        self.last_op = LastOp::Other;
        self.redetect_indent();
        Ok(())
    }

    /// Applies quick-fix style edits `(range, text)` as one undo step.
    pub fn apply_text_edits(&mut self, edits: Vec<(TextRange, String)>, label: &str) -> bool {
        if edits.is_empty() {
            return false;
        }
        let mut sorted: Vec<Edit> = edits
            .into_iter()
            .map(|(r, t)| Edit::replace(self.doc.clamp_range(r), t))
            .collect();
        // `map_offset` walks the edits in document order: callers may list them
        // in any order (a completion at the caret, then its import at the top).
        sorted.sort_by_key(|e| e.range.start());
        let carets: Vec<Caret> = self
            .carets
            .iter()
            .map(|k| {
                Caret::new(
                    map_offset(&sorted, k.sel.anchor, false),
                    map_offset(&sorted, k.sel.head, false),
                )
            })
            .collect();
        self.last_op = LastOp::Other;
        let token = match self.begin_transaction(label) {
            Ok(token) => token,
            Err(error) => {
                if error == rustpy_text::HistoryError::ReadOnly {
                    self.read_only_refused = true;
                }
                return false;
            }
        };
        if self.apply(sorted, carets, EditGroup::Separate, Some(label)) {
            self.commit_transaction(token)
                .expect("the command owns the innermost transaction");
            true
        } else {
            self.rollback_transaction(token)
                .expect("the rejected command owns its transaction");
            false
        }
    }

    // ----- line helpers -----------------------------------------------------

    /// Zero-based line numbers touched by any caret or selection, sorted & deduped.
    pub fn selected_lines(&self) -> Vec<usize> {
        let mut lines = Vec::new();
        for c in &self.carets {
            let r = c.range();
            let first = self.doc.line_of(r.start());
            let mut last = self.doc.line_of(r.end());
            // A selection ending at column 0 does not include that line.
            if last > first && r.end() == self.doc.line_range(last).start() && c.has_selection() {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Contiguous runs of selected lines as `(first, last)` inclusive.
    pub fn selected_line_blocks(&self) -> Vec<(usize, usize)> {
        let lines = self.selected_lines();
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for l in lines {
            match blocks.last_mut() {
                Some((_, last)) if *last + 1 == l => *last = l,
                _ => blocks.push((l, l)),
            }
        }
        blocks
    }

    /// [`EditorState::selected_line_blocks`] with each block extended over the
    /// collapsed folds whose header it contains (a folded body moves, is
    /// duplicated or deleted with its header).
    pub fn selected_line_blocks_with_folds(&mut self) -> Vec<(usize, usize)> {
        let hidden = self.hidden_line_ranges();
        let mut blocks = self.selected_line_blocks();
        for b in &mut blocks {
            for &(first, last) in &hidden {
                if first > b.0 && first <= b.1 + 1 && last > b.1 {
                    b.1 = last;
                }
            }
        }
        blocks.dedup_by(|next, prev| {
            next.0 <= prev.1 + 1 && {
                prev.1 = prev.1.max(next.1);
                true
            }
        });
        blocks
    }

    pub fn file_name(&self) -> String {
        self.title()
    }
}

/// Maps an offset from pre-edit to post-edit coordinates for sorted,
/// non-overlapping `edits`. Offsets inside a replaced range map to the end of
/// the replacement when `after` is true, else to its start.
pub fn map_offset(edits: &[Edit], off: TextSize, after: bool) -> TextSize {
    let mut delta: i64 = 0;
    for e in edits {
        if off < e.range.start() {
            break;
        }
        let base = u32::from(e.range.start()) as i64 + delta;
        if e.range.is_empty() {
            if off == e.range.start() {
                // Insertion exactly at `off`: stay before it unless `after`.
                return TextSize::new(
                    (base + if after { e.text.len() as i64 } else { 0 }).max(0) as u32
                );
            }
        } else if off < e.range.end() {
            // Strictly inside a replaced range.
            return TextSize::new(
                (base + if after { e.text.len() as i64 } else { 0 }).max(0) as u32
            );
        } else if off == e.range.end() {
            // Right after a replaced range: follows the replacement text.
            return TextSize::new((base + e.text.len() as i64).max(0) as u32);
        }
        delta += e.text.len() as i64 - u32::from(e.range.len()) as i64;
    }
    TextSize::new((u32::from(off) as i64 + delta).max(0) as u32)
}

#[cfg(test)]
mod map_tests {
    use super::*;

    fn ts(n: u32) -> TextSize {
        TextSize::new(n)
    }

    #[test]
    fn maps_offsets_through_edits() {
        let edits = vec![
            Edit::insert(ts(1), "XX"),
            Edit::replace(TextRange::new(ts(4), ts(6)), "Y"),
            Edit::delete(TextRange::new(ts(8), ts(9))),
        ];
        assert_eq!(map_offset(&edits, ts(0), false), ts(0));
        assert_eq!(map_offset(&edits, ts(1), false), ts(1));
        assert_eq!(map_offset(&edits, ts(1), true), ts(3));
        assert_eq!(map_offset(&edits, ts(2), false), ts(4));
        assert_eq!(map_offset(&edits, ts(5), false), ts(6));
        assert_eq!(map_offset(&edits, ts(6), false), ts(7));
        assert_eq!(map_offset(&edits, ts(7), false), ts(8));
        assert_eq!(map_offset(&edits, ts(9), false), ts(9));
        assert_eq!(map_offset(&edits, ts(12), true), ts(12));
    }

    #[test]
    fn edits_listed_out_of_order_keep_the_caret_after_the_completion() {
        // Auto-import completion: `Use` → `User` at the caret, plus an import
        // inserted at the top, listed in that order.
        let src = "import os\n\nUse";
        let mut e = EditorState::from_text(src, Language::Python);
        e.set_carets(vec![Caret::at(ts(src.len() as u32))]);
        let at = |s: &str| ts(src.find(s).unwrap() as u32);
        let edits = vec![
            (
                TextRange::new(at("Use"), ts(src.len() as u32)),
                "User".to_string(),
            ),
            (
                TextRange::empty(at("\n\nUse")),
                "\nfrom app.models import User".to_string(),
            ),
        ];
        assert!(e.apply_text_edits(edits, "Complete with import"));
        let text = e.doc.text();
        assert_eq!(text, "import os\nfrom app.models import User\n\nUser");
        assert_eq!(
            e.primary().head(),
            ts(text.len() as u32),
            "the caret stays after the completed name"
        );
    }
}

/// Replaces the parts of `spans` inside `range` with `overlay` (sorted, non-overlapping result).
fn overlay_spans(spans: Vec<Span>, range: TextRange, overlay: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() + overlay.len());
    for s in spans {
        if s.range.end() <= range.start() || s.range.start() >= range.end() {
            out.push(s);
            continue;
        }
        if s.range.start() < range.start() {
            out.push(Span {
                range: TextRange::new(s.range.start(), range.start()),
                tag: s.tag,
            });
        }
        // Inside the injected range: keep the base tag only for gaps between overlay tokens.
        let mut cursor = s.range.start().max(range.start());
        let inner_end = s.range.end().min(range.end());
        let start_cursor = cursor;
        for o in overlay
            .iter()
            .filter(|o| o.range.start() >= start_cursor && o.range.end() <= inner_end)
        {
            if o.range.start() < cursor {
                continue;
            }
            if o.range.start() > cursor {
                out.push(Span {
                    range: TextRange::new(cursor, o.range.start()),
                    tag: s.tag,
                });
            }
            out.push(*o);
            cursor = o.range.end();
        }
        if cursor < inner_end {
            out.push(Span {
                range: TextRange::new(cursor, inner_end),
                tag: s.tag,
            });
        }
        if s.range.end() > range.end() {
            out.push(Span {
                range: TextRange::new(range.end(), s.range.end()),
                tag: s.tag,
            });
        }
    }
    out.sort_by_key(|s| s.range.start());
    out
}

/// Cython-only keywords and C types layered over Python highlighting.
fn cython_keyword_spans(text: &str) -> Vec<Span> {
    const KEYWORDS: &[&str] = &[
        "cdef",
        "cpdef",
        "ctypedef",
        "cimport",
        "nogil",
        "gil",
        "inline",
        "extern",
        "struct",
        "union",
        "enum",
        "readonly",
        "public",
        "api",
        "packed",
        "fused",
        "include",
        "DEF",
        "IF",
        "ELIF",
        "ELSE",
        "noexcept",
        "sizeof",
        "NULL",
        "cppclass",
        "namespace",
        "const",
        "volatile",
    ];
    const TYPES: &[&str] = &[
        "int",
        "long",
        "short",
        "char",
        "double",
        "float",
        "bint",
        "size_t",
        "Py_ssize_t",
        "unsigned",
        "signed",
        "void",
        "object",
        "list",
        "dict",
        "str",
        "bytes",
        "complex",
        "Py_UCS4",
        "Py_UNICODE",
        "Py_hash_t",
        "ssize_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
    ];
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            in_comment = false;
            i += 1;
            continue;
        }
        if c == b'#' {
            in_comment = true;
        }
        if in_comment || !(c.is_ascii_alphabetic() || c == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let word = &text[start..i];
        let tag = if KEYWORDS.contains(&word) {
            Some(HighlightTag::Keyword)
        } else if TYPES.contains(&word) && word != "object" {
            Some(HighlightTag::Builtin)
        } else {
            None
        };
        if let Some(tag) = tag {
            out.push(Span {
                range: TextRange::new(TextSize::new(start as u32), TextSize::new(i as u32)),
                tag,
            });
        }
    }
    out
}

/// Recolors bracket tokens by nesting depth (outside strings and comments).
fn with_rainbow_brackets(spans: Vec<Span>, text: &str) -> Vec<Span> {
    let tags = [
        HighlightTag::Rainbow1,
        HighlightTag::Rainbow2,
        HighlightTag::Rainbow3,
        HighlightTag::Rainbow4,
        HighlightTag::Rainbow5,
        HighlightTag::Rainbow6,
    ];
    // Offsets covered by strings/comments are skipped.
    let mut skip: Vec<(usize, usize)> = spans
        .iter()
        .filter(|s| {
            matches!(
                s.tag,
                HighlightTag::String
                    | HighlightTag::Bytes
                    | HighlightTag::FString
                    | HighlightTag::FStringBrace
                    | HighlightTag::EscapeSequence
                    | HighlightTag::Docstring
                    | HighlightTag::Comment
                    | HighlightTag::TodoComment
            )
        })
        .map(|s| (usize::from(s.range.start()), usize::from(s.range.end())))
        .collect();
    skip.sort();
    let mut depth: usize = 0;
    let mut overlay = Vec::new();
    let mut k = 0;
    for (i, b) in text.bytes().enumerate() {
        while k < skip.len() && skip[k].1 <= i {
            k += 1;
        }
        if k < skip.len() && skip[k].0 <= i && i < skip[k].1 {
            continue;
        }
        let tag = match b {
            b'(' | b'[' | b'{' => {
                let t = tags[depth % tags.len()];
                depth += 1;
                t
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                tags[depth % tags.len()]
            }
            _ => continue,
        };
        overlay.push(Span {
            range: TextRange::new(TextSize::new(i as u32), TextSize::new(i as u32 + 1)),
            tag,
        });
    }
    apply_overlay(spans, overlay)
}

/// Highlighting of the languages injected into a Python file's strings and
/// of its doctests (`temporary`: the current temporary injection).
fn injected_spans(
    source: &str,
    parse: &Parse,
    temporary: Option<&super::language_injections::Temporary>,
    preferences: &rustpy_settings::editor_pages::Preferences,
    path: Option<&Path>,
) -> Vec<Span> {
    let baseline_regex = rustpy_regex::injections(source, parse);
    let mut spans = vec![];
    for injection in
        super::language_injections::discover_in(source, parse, temporary, preferences, path)
    {
        let explicit_regex =
            temporary.is_some_and(|temp| temp.injection().literal == injection.literal);
        if injection.language == super::language_injections::Language::Regex
            && !explicit_regex
            && baseline_regex
                .iter()
                .any(|old| old.string_range == injection.literal)
        {
            continue;
        } // Existing regex overlay keeps its original styling.
        if injection.language == super::language_injections::Language::FileReference {
            spans.push(Span {
                range: injection.content,
                tag: HighlightTag::Link,
            });
        } else {
            spans.extend(super::language_injections::highlights(source, &injection));
        }
    }
    for test in rustpy_syntax::doctest::discover(source, parse) {
        for line in test.lines {
            let code = &source[usize::from(line.code.start())..usize::from(line.code.end())];
            if !test.raw && !super::language_injections::valid_decoding(code, false) {
                continue;
            }
            let (decoded, map) = super::language_injections::decoded_map(code, test.raw);
            spans.extend(
                rustpy_highlight::highlight_python(&decoded)
                    .into_iter()
                    .filter_map(|span| {
                        let from = usize::from(span.range.start());
                        let to = usize::from(span.range.end());
                        if from >= to || to > map.len() {
                            return None;
                        }
                        let start = line.code.start() + TextSize::new(map[from].0 as u32);
                        let end = line.code.start() + TextSize::new(map[to - 1].1 as u32);
                        (start < end && end <= line.code.end()).then_some(Span {
                            range: rustpy_core::TextRange::new(start, end),
                            tag: span.tag,
                        })
                    }),
            );
        }
    }
    spans
}

/// Built-in Python highlighting: tokens and syntax roles, with the regex
/// patterns inside `re.*` calls (a language injection) highlighted too.
fn python_syntax_spans(text: &str, parse: &Parse) -> Vec<Span> {
    let mut spans = rustpy_highlight::highlight_python_with_parse(text, parse);
    // Language injections: regex patterns inside `re.*` calls.
    for inj in rustpy_regex::injections(text, parse) {
        let (vs, ve) = (
            usize::from(inj.value_range.start()),
            usize::from(inj.value_range.end()),
        );
        if vs >= ve || ve > text.len() {
            continue;
        }
        let raw = &text[vs..ve];
        let tokens = rustpy_regex::tokenize(raw, inj.verbose);
        let overlay: Vec<Span> = tokens
            .iter()
            .filter_map(|t| {
                use rustpy_regex::RegexTokenKind as K;
                let tag = match t.kind {
                    K::Escape => HighlightTag::Decorator,
                    K::CharClass | K::ClassRange => HighlightTag::Builtin,
                    K::Quantifier => HighlightTag::Number,
                    K::GroupOpen | K::GroupClose | K::GroupName | K::Alternation | K::Anchor => {
                        HighlightTag::Keyword
                    }
                    K::Flag | K::Comment => HighlightTag::Comment,
                    K::Dot => HighlightTag::Operator,
                    K::Literal | K::Error => return None,
                };
                Some(Span {
                    range: TextRange::new(
                        TextSize::new((vs + usize::from(t.range.start())) as u32),
                        TextSize::new((vs + usize::from(t.range.end())) as u32),
                    ),
                    tag,
                })
            })
            .collect();
        if !overlay.is_empty() {
            spans = overlay_spans(spans, inj.value_range, overlay);
        }
    }
    spans
}

/// `base` (sorted, disjoint) with the spans of `extra` that overlap none of it.
fn fill_gaps(base: Vec<Span>, extra: Vec<Span>) -> Vec<Span> {
    if extra.is_empty() {
        return base;
    }
    let free: Vec<Span> = extra
        .into_iter()
        .filter(|span| {
            let i = base.partition_point(|b| b.range.end() <= span.range.start());
            base.get(i)
                .is_none_or(|b| b.range.start() >= span.range.end())
        })
        .collect();
    let mut out = base;
    out.extend(free);
    out.sort_by_key(|span| span.range.start());
    out
}

/// A background reparse's result: the tree, its fold regions and the text.
type ParsedText = (Parse, Vec<FoldRegion>, String);

/// `text` parsed incrementally from `base` (a tree and the text it is of).
pub(crate) fn reparsed(base: (Parse, String), text: String) -> (Parse, String) {
    let (old, old_text) = base;
    if old_text == text {
        return (old, text);
    }
    let parse = rustpy_syntax::reparse::reparse_text(&old, &old_text, &text).0;
    (parse, text)
}

/// [`reparsed`] with the fold regions of the new tree (off the UI thread).
fn parse_with_folds(old: &Parse, old_text: &str, text: String) -> ParsedText {
    let parse = if old_text == text {
        old.clone()
    } else {
        rustpy_syntax::reparse::reparse_text(old, old_text, &text).0
    };
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let line_of = |offset: TextSize| {
        starts
            .partition_point(|&start| start <= usize::from(offset))
            .saturating_sub(1)
    };
    let folds = syntax_support::fold_regions(&parse, &line_of);
    (parse, folds, text)
}

/// `range` (of an older version) moved through the edit `steps` made since,
/// like [`map_spans_through_edits`]: `None` once an edit overlapped or
/// touched it at an end.
fn map_range_through_edits(
    range: TextRange,
    steps: &[Vec<rustpy_text::Edit>],
) -> Option<TextRange> {
    let span = Span {
        range,
        tag: HighlightTag::Comment,
    };
    map_spans_through_edits(&[span], steps)
        .first()
        .map(|span| span.range)
}

/// Documents at least this large get their highlighting (built-in, rainbow
/// and injected languages) computed off the UI thread.
pub const BACKGROUND_SEMANTICS_BYTES: usize = 48 * 1024;

/// Highlight spans of a large document computed off the UI thread, one run
/// at a time (edits made meanwhile are covered by the next run).
#[derive(Debug, Default)]
struct BackgroundSpans {
    /// The newest finished result: (document, version, spans sorted by start).
    done: Option<(DocumentId, u64, Arc<Vec<Span>>)>,
    /// The run in flight: (document, version, run).
    job: Option<(DocumentId, u64, rustpy_core::TaskHandle<Vec<Span>>)>,
}

impl BackgroundSpans {
    /// Takes the result of a finished run: true when there was one.
    fn poll(&mut self) -> bool {
        let Some(result) = self
            .job
            .as_mut()
            .and_then(|(_, _, handle)| handle.try_result())
        else {
            return false;
        };
        let Some((document, version, _)) = self.job.take() else {
            return false;
        };
        match result {
            Ok(mut spans) => {
                spans.sort_by_key(|span| span.range.start());
                self.done = Some((document, version, Arc::new(spans)));
                true
            }
            Err(_) => false,
        }
    }

    /// The spans of `doc` as it is now, and whether they are its own: the
    /// result for this version once it is in, else the newest result moved
    /// through the edits made since (see [`map_spans_through_edits`]). Starts the run `start` prepares
    /// (on the UI thread; the closure it returns runs in the background) when
    /// this version has no result and no run is in flight.
    fn spans_for<F>(
        &mut self,
        doc: &Document,
        title: &str,
        start: impl FnOnce() -> F,
    ) -> (Vec<Span>, bool)
    where
        F: FnOnce() -> Vec<Span> + Send + 'static,
    {
        let (document, version) = (doc.identity(), doc.version());
        self.poll();
        let current = self
            .done
            .as_ref()
            .is_some_and(|(d, v, _)| *d == document && *v == version);
        if !current && self.job.is_none() {
            let run = start();
            let handle = rustpy_core::TaskManager::global()
                .task(title)
                .hidden()
                .spawn(move |_| Ok(run()));
            self.job = Some((document, version, handle));
        }
        match &self.done {
            Some((d, v, spans)) if *d == document => {
                if *v == version {
                    (spans.to_vec(), true)
                } else {
                    let mapped = doc
                        .changes_since(*v)
                        .map(|steps| map_spans_through_edits(spans, &steps))
                        .unwrap_or_default();
                    (mapped, false)
                }
            }
            _ => (Vec::new(), false),
        }
    }
}

/// `spans` (sorted, of an older version) moved through the edit `steps`
/// made since (oldest first, each in the coordinates before it): spans after
/// an edit shift by its length change, a span with edits strictly inside it
/// (typing within a string or a comment) grows or shrinks with them, and a
/// span an edit overlaps or touches at an end is dropped (its token changed).
fn map_spans_through_edits(spans: &[Span], steps: &[Vec<rustpy_text::Edit>]) -> Vec<Span> {
    let change =
        |edit: &rustpy_text::Edit| edit.text.len() as i64 - i64::from(u32::from(edit.range.len()));
    let shift = |at: TextSize, by: i64| TextSize::new((i64::from(u32::from(at)) + by) as u32);
    let mut out: Vec<Span> = spans.to_vec();
    for step in steps {
        let mut edits: Vec<&rustpy_text::Edit> = step.iter().collect();
        edits.sort_by_key(|e| e.range.start());
        let mut next = Vec::with_capacity(out.len());
        let mut e = 0;
        let mut delta: i64 = 0;
        'spans: for span in out {
            while e < edits.len() && edits[e].range.end() < span.range.start() {
                delta += change(edits[e]);
                e += 1;
            }
            let mut inner = 0;
            for edit in edits[e..]
                .iter()
                .take_while(|edit| edit.range.start() <= span.range.end())
            {
                if edit.range.start() > span.range.start() && edit.range.end() < span.range.end() {
                    inner += change(edit);
                } else {
                    continue 'spans;
                }
            }
            next.push(Span {
                range: TextRange::new(
                    shift(span.range.start(), delta),
                    shift(span.range.end(), delta + inner),
                ),
                tag: span.tag,
            });
        }
        out = next;
    }
    out
}

/// Layers `overlay` (non-overlapping spans) over sorted, non-overlapping `base`
/// spans: base spans are cut where an overlay covers them, and overlays in
/// uncolored gaps are inserted as well.
fn apply_overlay(base: Vec<Span>, mut overlay: Vec<Span>) -> Vec<Span> {
    if overlay.is_empty() {
        return base;
    }
    overlay.sort_by_key(|s| s.range.start());
    let mut out = Vec::with_capacity(base.len() + overlay.len() * 2);
    let mut j = 0;
    for s in base {
        while j < overlay.len() && overlay[j].range.end() <= s.range.start() {
            j += 1;
        }
        let mut cursor = s.range.start();
        let mut k = j;
        while k < overlay.len() && overlay[k].range.start() < s.range.end() {
            let o = overlay[k].range;
            if o.start() > cursor {
                out.push(Span {
                    range: TextRange::new(cursor, o.start()),
                    tag: s.tag,
                });
            }
            cursor = cursor.max(o.end());
            k += 1;
        }
        if cursor < s.range.end() {
            out.push(Span {
                range: TextRange::new(cursor, s.range.end()),
                tag: s.tag,
            });
        }
    }
    out.extend(overlay);
    out.sort_by_key(|s| s.range.start());
    out
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    fn sp(a: u32, b: u32, tag: HighlightTag) -> Span {
        Span {
            range: TextRange::new(TextSize::new(a), TextSize::new(b)),
            tag,
        }
    }

    #[test]
    fn overlay_fills_gaps_and_splits_base() {
        let base = vec![
            sp(0, 4, HighlightTag::Keyword),
            sp(10, 20, HighlightTag::String),
        ];
        let over = vec![
            sp(5, 6, HighlightTag::Rainbow1),
            sp(12, 13, HighlightTag::Rainbow2),
            sp(19, 22, HighlightTag::Rainbow3),
        ];
        let out = apply_overlay(base, over);
        let got: Vec<(u32, u32, HighlightTag)> = out
            .iter()
            .map(|s| (u32::from(s.range.start()), u32::from(s.range.end()), s.tag))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 4, HighlightTag::Keyword),
                (5, 6, HighlightTag::Rainbow1),
                (10, 12, HighlightTag::String),
                (12, 13, HighlightTag::Rainbow2),
                (13, 19, HighlightTag::String),
                (19, 22, HighlightTag::Rainbow3)
            ]
        );
    }

    #[test]
    fn rainbow_brackets_skip_strings() {
        let text = "f(a[1], '(')";
        let base = vec![sp(8, 11, HighlightTag::String)];
        let out = with_rainbow_brackets(base, text);
        let brackets: Vec<(u32, HighlightTag)> = out
            .iter()
            .filter(|s| s.tag != HighlightTag::String)
            .map(|s| (u32::from(s.range.start()), s.tag))
            .collect();
        assert_eq!(
            brackets,
            vec![
                (1, HighlightTag::Rainbow1),
                (3, HighlightTag::Rainbow2),
                (5, HighlightTag::Rainbow2),
                (11, HighlightTag::Rainbow1)
            ]
        );
    }
}

#[cfg(test)]
mod background_highlight_tests {
    use super::*;

    fn sp(a: u32, b: u32) -> Span {
        Span {
            range: TextRange::new(TextSize::new(a), TextSize::new(b)),
            tag: HighlightTag::Rainbow1,
        }
    }

    fn ranges(spans: &[Span]) -> Vec<(u32, u32)> {
        spans
            .iter()
            .map(|s| (u32::from(s.range.start()), u32::from(s.range.end())))
            .collect()
    }

    #[test]
    fn spans_move_through_edits_and_spans_an_edit_touches_are_dropped() {
        let spans = [sp(0, 3), sp(5, 8), sp(10, 12), sp(20, 25)];
        let steps = vec![
            // Typing right after the second span extends that identifier.
            vec![rustpy_text::Edit::insert(TextSize::new(8), "xy")],
            // A deletion (in the new coordinates) before the last span.
            vec![rustpy_text::Edit::delete(TextRange::new(
                TextSize::new(15),
                TextSize::new(18),
            ))],
        ];
        assert_eq!(
            ranges(&map_spans_through_edits(&spans, &steps)),
            vec![(0, 3), (12, 14), (19, 24)]
        );
        // Several edits of one step (multiple carets) shift by their sum.
        let step = vec![vec![
            rustpy_text::Edit::insert(TextSize::new(20), "!"),
            rustpy_text::Edit::insert(TextSize::new(4), "ab"),
        ]];
        assert_eq!(
            ranges(&map_spans_through_edits(&spans, &step)),
            vec![(0, 3), (7, 10), (12, 14)]
        );
        // Typing inside a span (a string) resizes it.
        let inside = vec![vec![rustpy_text::Edit::insert(TextSize::new(22), "abc")]];
        assert_eq!(
            ranges(&map_spans_through_edits(&spans, &inside)),
            vec![(0, 3), (5, 8), (10, 12), (20, 28)]
        );
    }

    /// A Python module above [`BACKGROUND_SEMANTICS_BYTES`] with locals.
    fn large_module() -> String {
        let mut out = String::new();
        let mut n = 0;
        while out.len() < BACKGROUND_SEMANTICS_BYTES + 1024 {
            out.push_str(&format!(
                "def function_{n}(value):\n    total = value + 1\n    return total\n\n\n"
            ));
            n += 1;
        }
        out
    }

    fn wait_for_background(state: &mut EditorState) {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while state.rainbow.job.is_some()
            || state.injections.job.is_some()
            || state.python_spans.job.is_some()
        {
            assert!(
                Instant::now() < deadline,
                "background highlighting never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            state.view_highlight_spans(0, 10);
        }
    }

    #[test]
    fn a_large_file_reparses_in_the_background_and_folds_follow_the_edits_meanwhile() {
        let source = large_module();
        let mut state = EditorState::from_text(&source, Language::Python);
        let line = source[..source.find("def function_10(").unwrap()]
            .matches('\n')
            .count();
        state.toggle_fold_at_line(line);
        let hidden = state.hidden_line_ranges();
        assert_eq!(hidden, vec![(line + 1, line + 2)]);
        let before = state.blocking_reparses;
        // Two lines typed at the top: the fold moves down with its text at
        // once, though the reparse runs off the UI thread.
        state.set_carets(vec![Caret::at(TextSize::new(0))]);
        state.insert_text("# a\n# b\n", &EditOptions::default());
        assert!(!state.refresh_frame_syntax(), "the reparse is in flight");
        assert_eq!(state.hidden_line_ranges(), vec![(line + 3, line + 4)]);
        assert!(state
            .frame_fold_regions()
            .iter()
            .any(|f| f.first_line == line + 2));
        assert_eq!(
            state.blocking_reparses, before,
            "no reparse on the UI thread"
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while !state.refresh_frame_syntax() {
            assert!(
                Instant::now() < deadline,
                "the background reparse never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The new tree keeps the fold (its marker was never dropped).
        assert_eq!(state.hidden_line_ranges(), vec![(line + 3, line + 4)]);
        assert_eq!(state.blocking_reparses, before);
        // An editing command still gets the exact tree of this version.
        assert_eq!(state.syntax().syntax_node().text().len(), state.doc.len());
    }

    #[test]
    fn a_large_file_classifies_the_caret_by_lexing_its_line_only() {
        use rustpy_semantic::completion_ctx::CompletionContext;
        let mut source = large_module();
        source.push_str("# note here\ntext = '''first\nsecond\n");
        let mut state = EditorState::from_text(&source, Language::Python);
        // In a multi-line string the line alone would read as code.
        let end = state.doc.len();
        assert_eq!(state.lex_restart(end), 0);
        assert_eq!(
            state.completion_context_at(end),
            CompletionContext::StringLiteral
        );
        // In code the lexing starts at the line.
        let at = TextSize::new(source.find("    total = value").unwrap() as u32 + 9);
        assert!(state.lex_restart(at) > 0);
        assert_eq!(state.completion_context_at(at), CompletionContext::Code);
        let comment = TextSize::new(source.find("# note").unwrap() as u32 + 6);
        assert!(state.lex_restart(comment) > 0);
        assert_eq!(
            state.completion_context_at(comment),
            CompletionContext::Comment
        );
    }

    #[test]
    fn a_large_file_gets_rainbow_spans_from_the_background_and_keeps_them_while_typing() {
        let source = large_module();
        let mut state = EditorState::from_text(&source, Language::Python);
        state.semantic_highlighting = true;
        let total = TextSize::new(source.find("total = value").unwrap() as u32);
        // The first request starts the run; the rainbow arrives with it.
        state.view_highlight_spans(0, 10);
        wait_for_background(&mut state);
        let (spans, _) = state.view_highlight_spans(0, 10);
        assert_eq!(
            rustpy_highlight::python::tag_at(&spans, total).map(|tag| matches!(
                tag,
                HighlightTag::Rainbow1
                    | HighlightTag::Rainbow2
                    | HighlightTag::Rainbow3
                    | HighlightTag::Rainbow4
                    | HighlightTag::Rainbow5
                    | HighlightTag::Rainbow6
            )),
            Some(true),
            "a local of a large file is rainbow-colored"
        );
        let before = rustpy_highlight::python::tag_at(&spans, total);
        // Typing at the top: the previous result, moved by the edit, stays
        // until the new one is in (no frame computes the scope model).
        state.set_carets(vec![Caret::at(TextSize::new(0))]);
        state.insert_text("# note\n", &EditOptions::default());
        let moved = total + TextSize::new(7);
        let (spans, _) = state.view_highlight_spans(0, 10);
        assert_eq!(rustpy_highlight::python::tag_at(&spans, moved), before);
        wait_for_background(&mut state);
        let (spans, _) = state.view_highlight_spans(0, 10);
        assert_eq!(rustpy_highlight::python::tag_at(&spans, moved), before);
    }
}

#[cfg(test)]
#[path = "fold_marker_tests.rs"]
mod fold_marker_tests;

#[cfg(test)]
mod doctest_injection_tests {
    use super::*;
    #[test]
    fn actual_editor_overlay_colors_only_source_owned_example_code() {
        let source="def f():\n    r\"\"\"Examples.\n    >>> for value in [1, 2]:\n    ...     print(value)\n    expected output\n    \"\"\"\nnot_doc = '>>> for fake in []:'\n";
        let mut state = EditorState::from_text(source, Language::Python);
        let spans = state.highlight_spans().to_vec();
        let code = TextSize::new(source.find("for value").unwrap() as u32);
        assert_eq!(
            rustpy_highlight::python::tag_at(&spans, code),
            Some(rustpy_highlight::HighlightTag::Keyword)
        );
        let output = TextSize::new(source.find("expected output").unwrap() as u32);
        assert_eq!(
            rustpy_highlight::python::tag_at(&spans, output),
            Some(rustpy_highlight::HighlightTag::Docstring)
        );
        let fake = TextSize::new(source.find("for fake").unwrap() as u32);
        assert_ne!(
            rustpy_highlight::python::tag_at(&spans, fake),
            Some(rustpy_highlight::HighlightTag::Keyword)
        );
        assert_eq!(state.doc.text().as_str(), source);
    }
}

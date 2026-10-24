//! RustPy IDE — settings schema and persistence (roadmap §5, §0.3).
//!
//! IDE-level settings live in the user's config directory; per-project state
//! lives in `<project>/.rustpy/workspace.json` (the `.idea` equivalent), and
//! the shareable project structure in `<project>/.rustpy/project.json`.

pub mod accessibility;
pub mod branches;
pub mod bundle;
pub mod chrome;
pub mod code_style_schemes;
pub mod concurrency;
pub mod copyright;
pub mod coverage;
pub mod debug_log;
pub mod debugger_workspace;
pub mod diagnostics;
pub mod dialogs;
pub mod diff_merge;
pub mod documentation;
pub mod editor_pages;
pub mod editorconfig;
pub mod external_tools;
pub mod file_watchers;
pub mod github;
pub mod help;
pub mod inspection_profiles;
pub mod jupyter;
pub mod local_history;
pub mod macro_shortcuts;
pub mod markdown;
pub mod menus;
pub mod native_ui;
pub mod navigation_history;
pub mod navigation_search;
pub mod network;
pub mod notifications;
pub mod other_vcs;
pub mod path_variables;
pub mod persist;
pub mod plugins;
pub mod profiler;
pub mod project_directory;
pub mod python_console;
pub mod python_style;
pub mod python_testing;
pub mod run_console;
pub mod save_actions;
pub mod save_synchronization;
pub mod scopes;
pub mod settings_sync;
pub mod sphinx;
pub mod style_inspections;
pub mod tab_policy;
pub mod tasks;
pub mod tasks_workspace;
pub mod terminal;
pub mod testing;
pub mod todo;
pub mod tool_windows;
pub mod trust;
pub mod ui_themes;
pub mod vcs_operations;
pub mod web_browsers;
pub mod window_session_store;
pub mod workspace_store;
pub use workspace_store::{
    WorkspaceBreakpoints, WorkspaceError, WorkspaceLegacyRoots, WorkspacePatch, WorkspaceSession,
    WorkspaceSessionLease, WorkspaceSnapshot, WorkspaceStore,
};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use network::{
    CertificateSettings, PasswordSettings, PasswordStorage, ProxyKind, ProxyMode, ProxySettings,
};
pub use persist::LoadError;

pub const APP_DIR_NAME: &str = "rustpy";
pub const PROJECT_DIR_NAME: &str = ".rustpy";

/// Appearance & Behavior → Appearance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// UI theme: "Dark", "Light", "High Contrast", or a `custom:<file>.json` identifier.
    pub theme: String,
    pub ui_font_size: f32,
    /// UI font family (an installed font); empty: the bundled default.
    pub ui_font_family: String,
    pub show_tool_window_bars: bool,
    pub show_status_bar: bool,
    pub show_navigation_bar: bool,
    pub show_main_toolbar: bool,
    pub compact_mode: bool,
    /// Secondary descriptions in Project and Structure trees; identity/navigation stay unchanged.
    pub show_tree_details: bool,
    pub show_memory_indicator: bool,
    /// Editor color schemes last used with a dark and with a light theme, so a theme
    /// switch (Quick Switch Scheme) brings a custom scheme back (empty = builtin).
    pub dark_scheme: String,
    pub light_scheme: String,
    /// UI language: a locale code such as `zh-CN`, `ja` or `de`, or empty for the
    /// system default (detected from the environment only: GNU `LANGUAGE`, then `LC_ALL` /
    /// `LC_MESSAGES` / `LANG`; `LANGUAGE` counts only when one of those is set and is not
    /// `C`; English when none of them names a translated language).
    pub language: String,
    /// Empty follows the UI locale; regional variants such as en-GB are retained.
    pub date_locale: String,
    pub date_iso: bool,
    pub clock_12_hour: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "Dark".into(),
            ui_font_size: 13.0,
            show_tool_window_bars: true,
            show_status_bar: true,
            show_navigation_bar: true,
            show_main_toolbar: true,
            compact_mode: false,
            show_tree_details: true,
            show_memory_indicator: false,
            dark_scheme: String::new(),
            light_scheme: String::new(),
            language: String::new(),
            date_locale: String::new(),
            date_iso: false,
            clock_12_hour: false,
            ui_font_family: String::new(),
        }
    }
}

/// AI Assistant (Anthropic Messages API).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// `anthropic` (legacy default) or `openai-compatible`.
    pub provider: String,
    pub openai_model: String,
    pub openai_base_url: String,
    #[serde(skip)]
    pub openai_api_key: String,
    pub remember_openai_api_key: bool,
    /// API key for this session (empty = `ANTHROPIC_API_KEY`). Never written to
    /// settings.json: it is kept in the password safe when `remember_api_key` is set.
    #[serde(skip)]
    pub api_key: String,
    /// A plain-text key found in settings.json (written by older builds). It is
    /// moved into the password safe at startup and then cleared.
    #[serde(rename = "api_key", skip_serializing_if = "String::is_empty")]
    pub legacy_api_key: String,
    /// Keep the key in the password safe; otherwise it is forgotten on exit.
    pub remember_api_key: bool,
    /// Model id; empty = the default (Claude Opus 5.5).
    pub model: String,
    /// 0 = default.
    pub max_tokens: u32,
    /// Empty = https://api.anthropic.com.
    pub base_url: String,
    pub refusal_fallback: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            openai_model: String::new(),
            openai_base_url: "https://api.openai.com/v1".into(),
            openai_api_key: String::new(),
            remember_openai_api_key: true,
            api_key: String::new(),
            legacy_api_key: String::new(),
            remember_api_key: true,
            model: String::new(),
            max_tokens: 0,
            base_url: String::new(),
            refusal_fallback: false,
        }
    }
}

/// Shortcut changes of a keymap preset: `(action id, shortcuts)`; an empty
/// list unbinds the action.
pub type ShortcutOverrides = Vec<(String, Vec<String>)>;

/// The insertion caret's appearance. Overwrite mode always uses a block.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaretShape {
    #[default]
    Thin,
    Block,
    Underscore,
}

/// Editor → General / Appearance / Font / Code Style basics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub preferences: editor_pages::Preferences,
    pub font_family: String,
    pub font_size: f32,
    pub line_spacing: f32,
    pub enable_ligatures: bool,
    pub color_scheme: String,
    pub tab_size: usize,
    pub indent_size: usize,
    pub use_tab_character: bool,
    /// Code Style › "Detect and use existing file indents for editing".
    pub detect_indents: bool,
    /// Apply .editorconfig properties to editing, formatting and saving.
    pub editorconfig_enabled: bool,
    pub show_line_numbers: bool,
    pub relative_line_numbers: bool,
    pub show_whitespaces: bool,
    pub show_leading_whitespaces: bool,
    pub show_inner_whitespaces: bool,
    pub show_trailing_whitespaces: bool,
    pub show_line_separators: bool,
    pub show_method_separators: bool,
    pub show_css_color_preview: bool,
    pub show_indent_guides: bool,
    pub highlight_current_line: bool,
    pub right_margin: usize,
    /// Insert safe physical line breaks at the margin while typing.
    pub wrap_on_typing: bool,
    /// Navigate highlighted errors with F2 without including warnings.
    pub error_navigation_errors_only: bool,
    /// Ctrl+B / pointer declaration navigation follows a matching .py when available.
    pub prefer_source_navigation: bool,
    /// Show shared Quick Documentation after a continuous pointer hover.
    pub documentation_on_hover: bool,
    pub documentation_delay_ms: u64,
    pub show_right_margin: bool,
    pub visual_guides: Vec<usize>,
    pub soft_wrap: bool,
    pub inlay_parameter_hints: bool,
    pub inlay_type_hints: bool,
    pub inlay_return_hints: bool,
    pub code_vision_usages: bool,
    pub code_vision_author: bool,
    pub caret_blinking: bool,
    pub caret_blink_period_ms: u64,
    /// Legacy preference: true continues to force a block insertion caret.
    pub block_caret: bool,
    pub caret_shape: CaretShape,
    /// Thin-caret width / underscore thickness, in logical pixels (1..=10).
    pub caret_width: u8,
    pub smooth_scrolling: bool,
    pub scroll_beyond_end: bool,
    pub highlight_matching_brace: bool,
    pub highlight_usages_on_caret_move: bool,
    pub strip_trailing_spaces_on_save: StripTrailing,
    /// Preserve trailing spaces on every caret head line when stripping on save.
    pub keep_trailing_spaces_on_caret_line: bool,
    pub ensure_newline_at_eof: bool,
    pub auto_indent: bool,
    pub auto_close_brackets: bool,
    pub auto_close_quotes: bool,
    pub surround_selection_on_typing: bool,
    pub smart_home_end: bool,
    pub smart_backspace: bool,
    pub camel_humps_words: bool,
    /// Smart Keys › Reformat on paste.
    pub reformat_on_paste: ReformatOnPaste,
    /// Smart Keys › Python: insert `self` (or `cls`) when typing `(` of a method.
    pub insert_self_in_methods: bool,
    /// Smart Keys: Tab jumps past an auto-inserted closing bracket or quote.
    pub jump_outside_brackets_with_tab: bool,
    /// Smart Keys › Python: typing `"""` inserts the closing triple quotes.
    pub auto_close_triple_quotes: bool,
    /// Smart Keys › Enter: Enter inside `"""|"""` under a `def` inserts a docstring stub.
    pub insert_docstring_stub: bool,
    /// Add editable type placeholders to generated structured docstrings.
    pub insert_type_placeholders_in_docstring_stub: bool,
    /// Include a return-type field independently of parameter placeholders.
    pub insert_rtype_in_docstring_stub: bool,
    pub show_intention_bulb: bool,
    pub show_intention_preview: bool,
    pub show_breadcrumbs: bool,
    /// Place editor breadcrumbs above the document instead of below it.
    pub breadcrumbs_at_top: bool,
    /// Permit ordinary caret navigation and clicks beyond physical line ends.
    pub allow_caret_after_end_of_line: bool,
    /// Keep legacy rectangular selection beyond short rows unless disabled.
    pub allow_caret_after_end_of_line_in_block_selection: bool,
    pub show_sticky_lines: bool,
    pub sticky_lines_limit: usize,
    /// Color (), [] and {} by nesting depth.
    pub rainbow_brackets: bool,
    pub show_inlay_hints: bool,
    pub code_folding: bool,
    pub fold_imports_by_default: bool,
    pub fold_docstrings_by_default: bool,
    pub mouse_wheel_zoom: bool,
    pub tab_limit: usize,
    pub tabs: tab_policy::TabPolicySettings,
    pub mark_modified_tabs_with_asterisk: bool,
    pub open_in_preview_tab: bool,
    pub autoscroll_from_source: bool,
    pub autoscroll_to_source: bool,
    pub structure: StructureSettings,
    pub project_view: ProjectViewSettings,
    /// Appearance › Render documentation comments: docstrings show as formatted text.
    pub render_doc_comments: bool,
    /// Editor › Reader Mode.
    pub reader_mode: ReaderModeSettings,
    pub unicode_bidi: bool,
    /// Auto, LTR or RTL display direction. Source bytes are never reordered.
    pub unicode_base_direction: String,
}

/// Project tool-window presentation; changes never alter project membership/import roots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectViewSettings {
    pub view: String,
    pub flatten_packages: bool,
    pub compact_packages: bool,
    pub show_excluded: bool,
    pub visibility_icons: bool,
    pub sort_by_type: bool,
    pub folders_first: bool,
    pub show_libraries: bool,
    pub single_click: bool,
    pub always_select_opened: bool,
    pub focus_editor_on_escape: bool,
    /// Semicolon-separated parent suffix=child suffix rules, e.g. .py=.pyi,.pyc.
    pub nesting_rules: String,
}
impl Default for ProjectViewSettings {
    fn default() -> Self {
        Self {
            view: "Project".into(),
            flatten_packages: false,
            compact_packages: false,
            show_excluded: true,
            visibility_icons: true,
            sort_by_type: false,
            folders_first: true,
            show_libraries: true,
            single_click: true,
            always_select_opened: false,
            focus_editor_on_escape: true,
            nesting_rules: ".py=.pyi,.pyc;.js=.js.map;.ts=.ts.map".into(),
        }
    }
}

/// Shared structure popup/tool-window preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureSettings {
    pub show_fields: bool,
    pub show_inherited: bool,
    pub show_imports: bool,
    pub show_dunder: bool,
    pub sort_alphabetically: bool,
    pub sort_visibility: bool,
    pub group_by_kind: bool,
    pub autoscroll_from_source: bool,
    pub autoscroll_to_source: bool,
    pub show_members_in_project: bool,
}
impl Default for StructureSettings {
    fn default() -> Self {
        Self {
            show_fields: true,
            show_inherited: false,
            show_imports: false,
            show_dunder: true,
            sort_alphabetically: false,
            sort_visibility: false,
            group_by_kind: false,
            autoscroll_from_source: true,
            autoscroll_to_source: false,
            show_members_in_project: false,
        }
    }
}

/// Editor › Reader Mode: how library and read-only files are shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderModeSettings {
    /// Protect library source files from editing until explicitly unlocked.
    pub protect_library_sources: bool,
    /// Open library and read-only files in reader mode.
    pub enabled: bool,
    /// Show documentation comments rendered.
    pub render_docs: bool,
    /// Show warnings and weak warnings (errors are always shown).
    pub show_warnings: bool,
    /// Increase the line height.
    pub increase_line_height: bool,
    pub show_line_numbers: bool,
    pub show_inlay_hints: bool,
    pub ligatures: bool,
}

impl Default for ReaderModeSettings {
    fn default() -> Self {
        Self {
            protect_library_sources: true,
            enabled: true,
            render_docs: true,
            show_warnings: false,
            increase_line_height: false,
            show_line_numbers: true,
            show_inlay_hints: true,
            ligatures: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StripTrailing {
    None,
    #[default]
    ModifiedLines,
    All,
}

/// What happens to the indentation of pasted code.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReformatOnPaste {
    /// Insert the text unchanged.
    None,
    /// Shift the pasted block to the caret's indentation (Python files).
    #[default]
    IndentBlock,
    /// Shift indentation, then safely format the pasted Python replacement.
    Reformat,
}

impl EditorSettings {
    pub fn editorconfig_for(&self, path: &Path) -> editorconfig::EditorConfigProps {
        if self.editorconfig_enabled && !path.starts_with("<typeshed>") {
            editorconfig::resolve_cached(path)
        } else {
            editorconfig::EditorConfigProps::default()
        }
    }
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            preferences: Default::default(),
            font_family: "Hack".into(),
            font_size: 13.0,
            line_spacing: 1.2,
            enable_ligatures: false,
            color_scheme: "Dark".into(),
            tab_size: 4,
            indent_size: 4,
            use_tab_character: false,
            detect_indents: true,
            editorconfig_enabled: true,
            show_line_numbers: true,
            relative_line_numbers: false,
            show_whitespaces: false,
            show_leading_whitespaces: true,
            show_inner_whitespaces: true,
            show_trailing_whitespaces: true,
            show_line_separators: false,
            show_method_separators: false,
            show_css_color_preview: true,
            show_indent_guides: true,
            highlight_current_line: true,
            right_margin: 120,
            wrap_on_typing: false,
            error_navigation_errors_only: false,
            prefer_source_navigation: true,
            documentation_on_hover: true,
            documentation_delay_ms: 600,
            show_right_margin: true,
            visual_guides: Vec::new(),
            soft_wrap: false,
            inlay_parameter_hints: true,
            inlay_type_hints: true,
            inlay_return_hints: false,
            code_vision_usages: false,
            code_vision_author: false,
            caret_blinking: true,
            caret_blink_period_ms: 500,
            block_caret: false,
            caret_shape: CaretShape::Thin,
            caret_width: 2,
            smooth_scrolling: true,
            scroll_beyond_end: true,
            highlight_matching_brace: true,
            highlight_usages_on_caret_move: true,
            strip_trailing_spaces_on_save: StripTrailing::ModifiedLines,
            keep_trailing_spaces_on_caret_line: true,
            ensure_newline_at_eof: true,
            auto_indent: true,
            auto_close_brackets: true,
            auto_close_quotes: true,
            surround_selection_on_typing: true,
            smart_home_end: true,
            smart_backspace: true,
            camel_humps_words: false,
            reformat_on_paste: ReformatOnPaste::IndentBlock,
            insert_self_in_methods: true,
            jump_outside_brackets_with_tab: true,
            auto_close_triple_quotes: true,
            insert_docstring_stub: true,
            insert_type_placeholders_in_docstring_stub: false,
            insert_rtype_in_docstring_stub: false,
            show_intention_bulb: true,
            show_intention_preview: true,
            show_breadcrumbs: true,
            breadcrumbs_at_top: false,
            allow_caret_after_end_of_line: false,
            allow_caret_after_end_of_line_in_block_selection: true,
            show_sticky_lines: true,
            sticky_lines_limit: 5,
            rainbow_brackets: false,
            show_inlay_hints: true,
            code_folding: true,
            fold_imports_by_default: false,
            fold_docstrings_by_default: false,
            mouse_wheel_zoom: true,
            tab_limit: 10,
            tabs: Default::default(),
            mark_modified_tabs_with_asterisk: true,
            open_in_preview_tab: false,
            autoscroll_from_source: false,
            autoscroll_to_source: false,
            structure: StructureSettings::default(),
            project_view: ProjectViewSettings::default(),
            render_doc_comments: false,
            reader_mode: ReaderModeSettings::default(),
            unicode_bidi: true,
            unicode_base_direction: "Auto".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectionSettings {
    pub style: style_inspections::StyleInspections,
    pub profiles: inspection_profiles::Profiles,
    pub pep8: bool,
    pub unused_imports: bool,
    pub unused_variables: bool,
    pub unresolved_references: bool,
    pub type_checker: bool,
    pub security: bool,
    pub compatibility: bool,
    pub code_smells: bool,
    pub naming: bool,
    pub missing_docstrings: bool,
    /// Probable bugs: unbound locals, shadowed outer names, protected member
    /// access, attributes defined outside `__init__`, abstract classes,
    /// coroutines that are not awaited.
    pub probable_bugs: bool,
    /// Exact diagnostic inspection codes disabled in the editor profile.
    pub disabled_inspections: Vec<String>,
    /// Grammar checks in comments, docstrings, Markdown and commit messages.
    pub grammar: bool,
    /// Supported grammar locale key; unknown values are retained and reported.
    pub grammar_language: String,
    /// Disabled built-in grammar category keys.
    pub grammar_disabled_categories: Vec<String>,
    /// Spelling (typos in identifiers, comments, strings, Markdown, plain text).
    pub spelling: bool,
}

impl Default for InspectionSettings {
    fn default() -> Self {
        Self {
            style: style_inspections::StyleInspections::default(),
            profiles: inspection_profiles::Profiles::default(),
            pep8: true,
            unused_imports: true,
            unused_variables: true,
            unresolved_references: true,
            type_checker: true,
            security: true,
            compatibility: true,
            code_smells: true,
            naming: true,
            missing_docstrings: false,
            probable_bugs: true,
            disabled_inspections: Vec::new(),
            grammar: true,
            grammar_language: "en-US".into(),
            grammar_disabled_categories: Vec::new(),
            spelling: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeStyleSettings {
    /// "preserve" | "double" | "single".
    pub quote_style: String,
    pub magic_trailing_comma: bool,
    pub blank_lines_top_level: usize,
    pub blank_lines_in_class: usize,
    /// "builtin" | "black" | "ruff" | "autopep8" | "yapf".
    pub formatter: String,
    pub format_on_save: bool,
    pub optimize_imports_on_save: bool,
    pub optimize_imports_on_commit: bool,
    pub optimize_imports_on_fly: bool,
    pub preview_optimize_imports: bool,
    pub add_unambiguous_imports_on_fly: bool,
    pub show_import_popup: bool,
    /// Whole-file save formatting; false retains the legacy active-selection policy.
    pub format_whole_file_on_save: bool,
    pub rearrange_on_save: bool,
    pub cleanup_on_save: bool,
    pub show_reformat_dialog: bool,
    pub reformat_file_masks: String,
    /// Extra arguments for the external formatter.
    pub formatter_args: String,
    /// Per-tool stdin runner configuration; empty executable uses the project Python -m.
    pub external_formatters: Vec<ExternalFormatterSettings>,
    pub use_black_default: bool,
    pub python: python_style::PythonStyle,
    pub languages: std::collections::BTreeMap<String, code_style_schemes::LanguageStyle>,
    /// all | changed | vcs (HEAD-relative current document ranges).
    pub reformat_scope: String,
    pub formatter_markers: bool,
    pub formatter_off: String,
    pub formatter_on: String,
    pub do_not_format: Vec<String>,
    pub do_not_format_scopes: Vec<String>,
    pub keep_line_breaks: bool,
    pub keep_simple_blocks: bool,
    pub keep_first_column_comments: bool,
}

impl Default for CodeStyleSettings {
    fn default() -> Self {
        Self {
            quote_style: "preserve".into(),
            magic_trailing_comma: true,
            blank_lines_top_level: 2,
            blank_lines_in_class: 1,
            formatter: "builtin".into(),
            format_on_save: false,
            optimize_imports_on_save: false,
            optimize_imports_on_commit: false,
            optimize_imports_on_fly: false,
            preview_optimize_imports: false,
            add_unambiguous_imports_on_fly: false,
            show_import_popup: true,
            format_whole_file_on_save: false,
            rearrange_on_save: false,
            cleanup_on_save: false,
            show_reformat_dialog: false,
            reformat_file_masks: "*.py;*.pyi;*.pyw;*.md;*.markdown".into(),
            formatter_args: String::new(),
            external_formatters: ["black", "ruff", "isort", "autopep8", "yapf"]
                .into_iter()
                .map(ExternalFormatterSettings::for_tool)
                .collect(),
            use_black_default: false,
            python: Default::default(),
            languages: Default::default(),
            reformat_scope: "all".into(),
            formatter_markers: false,
            formatter_off: "@formatter:off".into(),
            formatter_on: "@formatter:on".into(),
            do_not_format: Vec::new(),
            do_not_format_scopes: Vec::new(),
            keep_line_breaks: false,
            keep_simple_blocks: false,
            keep_first_column_comments: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalFormatterSettings {
    pub tool: String,
    pub executable: String,
    pub arguments: String,
    /// Comma/semicolon separated filename or project-relative glob patterns.
    pub file_masks: String,
    pub on_reformat: bool,
    pub on_save: bool,
}
impl Default for ExternalFormatterSettings {
    fn default() -> Self {
        Self::for_tool("black")
    }
}
impl ExternalFormatterSettings {
    pub fn for_tool(tool: &str) -> Self {
        Self {
            tool: tool.into(),
            executable: String::new(),
            arguments: String::new(),
            file_masks: "*.py; *.pyi".into(),
            on_reformat: true,
            on_save: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefactoringSettings {
    pub rename_in_place: bool,
    pub rename_variables_in_place: bool,
    pub preview_refactoring: bool,
    pub search_in_comments: bool,
    pub prefer_from_imports: bool,
    pub confirm_undo: bool,
}

impl Default for RefactoringSettings {
    fn default() -> Self {
        Self {
            rename_in_place: true,
            rename_variables_in_place: true,
            preview_refactoring: false,
            search_in_comments: true,
            prefer_from_imports: true,
            confirm_undo: true,
        }
    }
}

/// Settings › Tools › External Tools entry. Macros: `$FilePath$`, `$FileDir$`, `$FileName$`,
/// `$FileNameWithoutExtension$`, `$ProjectFileDir$`, `$LineNumber$`, `$PyInterpreterDirectory$`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalTool {
    pub name: String,
    pub group: String,
    pub program: String,
    pub arguments: String,
    pub working_dir: String,
    pub show_console: bool,
    /// Keyboard shortcut that runs the tool ("Ctrl+Alt+B"); empty: none.
    pub shortcut: String,
    pub options: external_tools::Options,
}

/// Settings › Editor › TextMate Bundles entry: a TextMate bundle folder
/// (`.tmbundle`), a VS Code extension folder (`package.json` + `syntaxes/`), a
/// folder of either, or a single `.tmLanguage` / `.tmLanguage.json` grammar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextMateBundle {
    pub path: PathBuf,
    pub enabled: bool,
}

impl Default for TextMateBundle {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            enabled: true,
        }
    }
}

/// Settings › Tools › Python External Documentation entry: the page External
/// Documentation (Shift+F1) opens for symbols of `module` and its submodules.
/// Placeholders: `{module.name}`, `{module.basename}`, `{element.name}`,
/// `{element.qname}`, `{class.name}`, `{function.name}`, `{package}`, `{python.version}`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalDocUrl {
    pub module: String,
    pub url: String,
}

pub use file_watchers::FileWatcher;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionCaseMatch {
    #[default]
    None,
    FirstLetter,
    All,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletionPrefs {
    pub match_case: CompletionCaseMatch,
    pub sort_lexicographically: bool,
    pub show_documentation: bool,
    pub show_full_signatures: bool,
    pub preselect_first: bool,
    pub lookup_item_height: f32,
    pub show_parameter_info: bool,
    pub insert_by_context_characters: bool,
    pub show_as_you_type: bool,
    pub case_sensitive: bool,
    pub match_middle: bool,
    pub hide_used: bool,
    pub auto_popup_delay_ms: u64,
    pub insert_parentheses: bool,
}

impl Default for CompletionPrefs {
    fn default() -> Self {
        Self {
            match_case: CompletionCaseMatch::None,
            sort_lexicographically: false,
            show_documentation: false,
            show_full_signatures: false,
            preselect_first: true,
            lookup_item_height: 22.0,
            show_parameter_info: true,
            insert_by_context_characters: true,
            show_as_you_type: true,
            case_sensitive: false,
            match_middle: true,
            hide_used: false,
            auto_popup_delay_ms: 0,
            insert_parentheses: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyrightSettings {
    pub enabled: bool,
    /// Template with `$today.year`, `$project`, `$user`, `$file` variables.
    pub text: String,
    pub add_to_new_files: bool,
}

impl Default for CopyrightSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            text: "Copyright (c) $today.year $user. All rights reserved.".into(),
            add_to_new_files: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub preferences: terminal::Preferences,
    /// Shell executable; empty = system default.
    pub shell: String,
    /// Put the project interpreter's virtualenv on `PATH` in new terminal tabs
    /// (on by default). Stored under a new key: earlier builds
    /// saved this option as an unused `activate_virtualenv: false`.
    #[serde(rename = "activate_venv")]
    pub activate_virtualenv: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            preferences: terminal::Preferences::default(),
            shell: String::new(),
            activate_virtualenv: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsSettings {
    pub preferences: VcsPreferences,
    pub branches: branches::BranchPreferences,
    /// Path to the git executable; empty = `git` on PATH.
    pub git_executable: String,
    pub confirm_before_push: bool,
    /// Update Project / Pull: `"merge"` or `"rebase"`.
    pub update_method: String,
    /// Stash uncommitted changes around Update Project and restore them afterwards.
    pub update_autostash: bool,
    /// Update Project runs with the saved options instead of asking first.
    pub update_dont_ask: bool,
}

impl Default for VcsSettings {
    fn default() -> Self {
        Self {
            preferences: VcsPreferences::default(),
            branches: branches::BranchPreferences::default(),
            git_executable: String::new(),
            confirm_before_push: false,
            update_method: "merge".into(),
            update_autostash: true,
            update_dont_ask: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsPreferences {
    pub autocrlf: String,
    pub credential_helper: bool,
    pub ssh_executable: String,
    /// `native` invokes OpenSSH; `builtin` uses libgit2/libssh2 and IDE credential callbacks.
    pub ssh_transport: String,
    pub hg_executable: String,
    pub svn_executable: String,
    pub p4_executable: String,
    /// Optional TFVC adapter; explicit machine-local Visual Studio tf.exe path.
    pub tf_executable: String,
    pub ssh_key: String,
    pub protected_branches: String,
    pub warn_detached: bool,
    pub warn_crlf: bool,
    pub fix_crlf: bool,
    pub sign_off: bool,
    pub sign_commits: bool,
    pub signing_key: String,
    pub signing_format: String,
    pub staging_area: bool,
    pub non_modal_commit: bool,
    pub show_status_branch: bool,
    pub synchronize_branches: bool,
    pub auto_update_rejected_push: bool,
    pub check_incoming: bool,
    pub cherry_pick_commit: bool,
    pub shallow_depth: u32,
    pub update_path_filter: String,
    pub github_client_id: String,
}
impl Default for VcsPreferences {
    fn default() -> Self {
        Self {
            autocrlf: "inherit".into(),
            credential_helper: true,
            ssh_executable: String::new(),
            ssh_transport: "native".into(),
            hg_executable: String::new(),
            svn_executable: String::new(),
            p4_executable: String::new(),
            tf_executable: String::new(),
            ssh_key: String::new(),
            protected_branches: "main,master".into(),
            warn_detached: true,
            warn_crlf: true,
            fix_crlf: false,
            sign_off: false,
            sign_commits: false,
            signing_key: String::new(),
            signing_format: "openpgp".into(),
            staging_area: false,
            non_modal_commit: true,
            show_status_branch: true,
            synchronize_branches: false,
            auto_update_rejected_push: false,
            check_incoming: false,
            cherry_pick_commit: true,
            shallow_depth: 0,
            update_path_filter: String::new(),
            github_client_id: String::new(),
        }
    }
}

/// Appearance & Behavior › System Settings › Updates, plus the update bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// "Check IDE updates for": look for a new release on startup, at most once a day.
    pub check_automatically: bool,
    /// Stage verified updates in the background; installation still needs Restart.
    pub download_automatically: bool,
    /// Release channel: "release" (Stable Releases) or "eap" (Early Access Program).
    pub channel: String,
    /// Where releases come from: a GitHub `owner/name`, a repository URL or a Releases API
    /// URL (GitHub Enterprise too); empty = the default repository.
    pub repository: String,
    /// Unix time (seconds) of the last check the release server answered; 0 = never.
    pub last_check: i64,
    /// Versions the user chose to skip ("Ignore This Update").
    pub ignored_versions: Vec<String>,
    /// The IDE version that last started; What's New opens when a newer one starts.
    /// Empty until the first start.
    pub last_seen_version: String,
    /// "Show What's New after an IDE update".
    pub show_whats_new: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_automatically: true,
            download_automatically: false,
            channel: "release".into(),
            repository: String::new(),
            last_check: 0,
            ignored_versions: Vec::new(),
            last_seen_version: String::new(),
            show_whats_new: true,
        }
    }
}

/// Top-level IDE settings (stored once per user).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub native_ui: native_ui::Settings,
    pub menus: menus::Settings,
    pub accessibility: accessibility::AccessibilitySettings,
    pub web_development: web_development::WebDevelopment,
    pub diagnostics: diagnostics::Settings,
    pub chrome: chrome::Settings,
    pub tool_window_layouts: tool_windows::Layouts,
    pub scientific: scientific::ScientificSettings,
    pub shared_indexes: shared_indexes::SharedIndexesSettings,
    pub platform: platform::PlatformSettings,
    pub data_files: data_files::DataFileSettings,
    pub vcs_operations: vcs_operations::Settings,
    pub appearance: AppearanceSettings,
    pub editor: EditorSettings,
    pub keymap: String,
    /// User keymap name and immutable bundled/plugin parent.
    pub custom_keymaps: Vec<(String, String)>,
    /// Named Quick Lists: name, action IDs, keyboard shortcut.
    pub keymap_quick_lists: Vec<(String, Vec<String>, String)>,
    /// Keymap overrides of the selected preset (`keymap`): `(action id, shortcuts)`;
    /// an empty list unbinds the action.
    pub custom_shortcuts: Vec<(String, Vec<String>)>,
    /// The overrides of the other presets, `(preset, overrides)`: each preset keeps
    /// its own changes, which come back when it is selected again.
    pub keymap_overrides: Vec<(String, ShortcutOverrides)>,
    pub inspections: InspectionSettings,
    pub terminal: TerminalSettings,
    pub python_console: python_console::PythonConsoleSettings,
    pub jupyter: jupyter::Settings,
    pub coverage: coverage::CoverageSettings,
    pub run_console: run_console::Settings,
    pub python_testing: python_testing::PythonTestingSettings,
    pub profiler: profiler::ProfilerSettings,
    /// Opt-in clipboard stack analysis when the application regains focus.
    pub detect_clipboard_stack_traces: bool,
    pub save_synchronization: save_synchronization::SaveSynchronizationSettings,
    pub save_actions: save_actions::Settings,
    pub vcs: VcsSettings,
    pub local_history: local_history::LocalHistorySettings,
    pub diff_merge: diff_merge::DiffMergeSettings,
    pub other_vcs: other_vcs::OtherVcsSettings,
    /// Appearance & Behavior › System Settings › Updates.
    pub updates: UpdateSettings,
    /// Recorded editor macros: `(name, steps)` where a step is `action:<id>` or `text:<text>`.
    pub macros: Vec<(String, Vec<String>)>,
    /// Named macro shortcuts, edited alongside the macro rather than the last-recorded action.
    pub macro_shortcuts: Vec<macro_shortcuts::MacroShortcut>,
    pub code_style: CodeStyleSettings,
    pub code_style_schemes: code_style_schemes::Schemes,
    pub copyright: CopyrightSettings,
    pub copyright_profiles: copyright::CopyrightProfilesSettings,
    pub todo: todo::TodoSettings,
    pub completion: CompletionPrefs,
    pub navigation_search: navigation_search::NavigationSearch,
    pub navigation_history: navigation_history::NavigationHistorySettings,
    pub scopes: scopes::ScopeSettings,
    pub disabled_intentions: Vec<String>,
    pub refactoring: RefactoringSettings,
    pub external_tools: Vec<ExternalTool>,
    pub file_watchers: Vec<FileWatcher>,
    pub markdown: markdown::Settings,
    /// Settings › Editor › TextMate Bundles: grammars for languages without a
    /// built-in highlighter, in priority order (later bundles win).
    pub textmate_bundles: Vec<TextMateBundle>,
    /// Settings › Plugins › Marketplace: URL of the plugin catalog (JSON);
    /// empty = the catalog bundled with the IDE.
    pub plugin_catalog_url: String,
    pub plugins: plugins::Settings,
    /// Browser command for "Open in Browser" (empty = system default).
    pub browser: String,
    pub web_browsers: web_browsers::Settings,
    /// External Documentation URL patterns that override the built-in ones.
    pub external_doc_urls: Vec<ExternalDocUrl>,
    /// Semantic highlighting of local variables and parameters ("rainbow").
    pub semantic_highlighting: bool,
    /// Action ids shown by the Quick List popup.
    pub quick_list: Vec<String>,
    /// Completed Learn IDE Features lessons.
    pub learned_lessons: Vec<String>,
    /// Docker CLI used by the Services tool window (empty = `docker` on PATH).
    pub docker_path: String,
    /// Run Python scripts/modules under the type collector (feeds code insight).
    pub collect_runtime_types: bool,
    /// Custom Python stub directories, in resolution priority order.
    pub python_stub_roots: Vec<PathBuf>,
    /// Settings › Build, Execution, Deployment › Python Debugger.
    pub debugger: DebuggerSettings,
    /// Settings › Tools › AI Assistant.
    pub ai: AiSettings,
    /// Appearance & Behavior › System Settings › HTTP Proxy.
    pub http_proxy: ProxySettings,
    /// Tools › Server Certificates.
    pub certificates: CertificateSettings,
    /// Appearance & Behavior › System Settings › Passwords.
    pub passwords: PasswordSettings,
    /// Docstring format: "auto" | "plain" | "rest" | "epytext" | "google" | "numpy".
    pub docstring_format: String,
    pub documentation: documentation::DocumentationSettings,
    pub sphinx: sphinx::SphinxSettings,
    pub testing: testing::TestingSettings,
    pub concurrency: concurrency::ConcurrencySettings,
    pub github: github::GitHubSettings,
    pub recent_projects: Vec<PathBuf>,
    /// Machine-local creation default; never included in portable Settings Sync.
    pub default_project_directory: Option<PathBuf>,
    pub reopen_last_project: bool,
    pub confirm_exit: bool,
    pub show_tips_on_startup: bool,
    /// Project roots the user trusted (Trust Project). Trust is stored only here,
    /// never inside the project, so a repository cannot mark itself trusted.
    pub trusted_projects: Vec<PathBuf>,
    /// Trusted Locations: every project under one of these directories is trusted.
    pub trusted_locations: Vec<PathBuf>,
    /// Where File › Open, Recent Projects, Get from VCS and dropped folders open a
    /// project while another one is open: "ask" (also when empty), "current" or "new".
    pub open_project_in: String,
    /// File › Power Save Mode: no background code analysis, no completion
    /// popup while typing, no semantic highlighting.
    pub power_save_mode: bool,
    pub notifications: NotificationSettings,
    /// Appearance & Behavior › Path Variables.
    pub path_variables: path_variables::PathVariables,
}

/// Appearance & Behavior › Notifications (see `notifications`).
pub use notifications::{
    GroupNotificationSettings, NotificationDisplay, NotificationPresentation, NotificationSettings,
};

/// Value of `Settings::open_project_in`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpenProjectIn {
    Ask,
    CurrentWindow,
    NewWindow,
}

impl Settings {
    pub fn config_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("RUSTPY_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_DIR_NAME)
    }

    /// Caches and other state that can be rebuilt (the system directory):
    /// Local History. `RUSTPY_SYSTEM_DIR` (set by `rustpy --system-dir`), otherwise
    /// the configuration directory.
    pub fn system_dir() -> PathBuf {
        match std::env::var_os("RUSTPY_SYSTEM_DIR").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => Self::config_dir(),
        }
    }

    /// Where `rustpy.log` is written: `RUSTPY_LOG_DIR` (set by `rustpy --log-dir`),
    /// otherwise `log/` in the system directory.
    pub fn log_dir() -> PathBuf {
        match std::env::var_os("RUSTPY_LOG_DIR").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => Self::system_dir().join("log"),
        }
    }

    pub fn settings_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    /// Loads settings, falling back to defaults (merging unknown/missing fields).
    ///
    /// A file that cannot be parsed is first copied to `settings.json.invalid-<time>`;
    /// [`Settings::take_load_error`] then reports it so the UI can tell the user.
    pub fn load() -> Settings {
        let settings: Settings = persist::load_global(&Self::settings_path());
        path_variables::install(&settings.path_variables);
        settings
    }

    /// The problem found by the last [`Settings::load`], once (`None` when the file was fine).
    pub fn take_load_error() -> Option<LoadError> {
        persist::take_load_error()
    }

    pub(crate) fn default_with_flags() -> Settings {
        Settings {
            keymap: "Default".into(),
            custom_keymaps: Vec::new(),
            keymap_quick_lists: Vec::new(),
            reopen_last_project: true,
            confirm_exit: true,
            show_tips_on_startup: false,
            ..Default::default()
        }
    }
    pub fn shipped_defaults() -> Settings {
        Self::default_with_flags()
    }

    /// Writes `settings.json` atomically. When another RustPy process changed the
    /// file since this one loaded or saved it, the two are merged: values this
    /// process changed win, every other value keeps the file's version, and the
    /// recent and trusted project lists are combined (see [`persist`]).
    pub fn save(&self) -> anyhow::Result<()> {
        persist::save_global(&Self::settings_path(), self)?;
        path_variables::install(&self.path_variables);
        Ok(())
    }

    pub fn add_recent_project(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.recent_projects.retain(|p| p != &path);
        self.recent_projects.insert(0, path);
        self.recent_projects.truncate(25);
    }

    /// Whether the project at `root` is trusted: listed in `trusted_projects`, or
    /// inside one of the absolute `trusted_locations`. Nothing stored in the
    /// project itself counts.
    pub fn is_project_trusted(&self, root: &Path) -> bool {
        trust::is_trusted(root, &self.trusted_projects, &self.trusted_locations)
    }

    /// Records only an existing, canonical directory selected by the user.
    pub fn trust_project(&mut self, root: &Path) -> bool {
        let Ok(root) = trust::canonical_directory(root) else {
            return false;
        };
        if self.trusted_projects.contains(&root) {
            return false;
        }
        self.trusted_projects.push(root);
        true
    }

    /// The selected keymap preset ("Default" when unset).
    pub fn keymap_name(&self) -> &str {
        if self.keymap.is_empty() {
            "Default"
        } else {
            &self.keymap
        }
    }

    /// Selects keymap preset `name`: the shortcut changes of the current preset
    /// are put aside, and `name`'s own changes (if any) become `custom_shortcuts`.
    pub fn switch_keymap(&mut self, name: &str) {
        let current = self.keymap_name().to_string();
        if current == name {
            return;
        }
        self.keymap_overrides.retain(|(p, _)| *p != current);
        let mine = std::mem::take(&mut self.custom_shortcuts);
        if !mine.is_empty() {
            self.keymap_overrides.push((current, mine));
        }
        if let Some(i) = self.keymap_overrides.iter().position(|(p, _)| p == name) {
            self.custom_shortcuts = self.keymap_overrides.remove(i).1;
        }
        self.keymap = name.to_string();
    }

    /// Whether preset `name` has shortcut changes.
    pub fn keymap_modified(&self, name: &str) -> bool {
        if name == self.keymap_name() {
            !self.custom_shortcuts.is_empty()
        } else {
            self.keymap_overrides
                .iter()
                .any(|(p, o)| p == name && !o.is_empty())
        }
    }

    pub fn open_project_in(&self) -> OpenProjectIn {
        match self.open_project_in.as_str() {
            "current" => OpenProjectIn::CurrentWindow,
            "new" => OpenProjectIn::NewWindow,
            _ => OpenProjectIn::Ask,
        }
    }

    pub fn set_open_project_in(&mut self, v: OpenProjectIn) {
        self.open_project_in = match v {
            OpenProjectIn::Ask => "ask",
            OpenProjectIn::CurrentWindow => "current",
            OpenProjectIn::NewWindow => "new",
        }
        .into();
    }
}

/// `RUSTPY_TRUST_ALL` (any value) trusts every project, as headless smoke tests need.
pub fn trust_all_from_env() -> bool {
    std::env::var_os("RUSTPY_TRUST_ALL").is_some()
}

/// `RUSTPY_DISABLE_PLUGINS` (any value, set by `rustpy --disable-plugins`): start
/// without user-installed extensions (TextMate bundles); also accepted as the
/// `disableNonBundledPlugins` launcher argument.
pub fn plugins_disabled() -> bool {
    std::env::var_os("RUSTPY_DISABLE_PLUGINS").is_some()
}

/// Per-project workspace state (`.rustpy/workspace.json`).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub open_files: Vec<OpenFileState>,
    /// Per-file codec overrides, retained independently of whether a tab is open.
    /// Codec keys are opaque here; readers validate them against supported encodings.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub file_encodings: Vec<FileEncodingOverride>,
    pub active_file: Option<PathBuf>,
    /// Optional two-group editor layout. Older flat sessions omit this field.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_editor_layout"
    )]
    pub editor_layout: Option<EditorLayout>,
    pub expanded_dirs: Vec<PathBuf>,
    pub tool_windows: ToolWindowLayout,
    pub interpreter: Option<PathBuf>,
    /// Line breakpoints per file (1-based lines).
    pub breakpoints: Vec<(PathBuf, Vec<usize>)>,
    pub watches: Vec<String>,
    pub bookmarks: Vec<BookmarkState>,
    /// Named bookmark lists, including intentionally empty lists.
    pub bookmark_lists: Vec<String>,
    pub recent_files: Vec<PathBuf>,
    /// Older builds: directories marked as Sources Root. The project structure now
    /// lives in the shareable `project.json` (owned by `rustpy-project`); these are
    /// only read to migrate it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_roots: Vec<PathBuf>,
    /// Older builds: excluded directories (see `source_roots`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded_dirs: Vec<PathBuf>,
    /// Run configurations started when the project opens (Startup Tasks); they
    /// only start once the project is trusted. Older files also have a `trusted`
    /// flag here: it is ignored, because trust lives in the IDE settings.
    pub startup_tasks: Vec<String>,
    /// Line breakpoints with their properties. `breakpoints` above keeps the plain
    /// lines for older versions and is used when this list is empty.
    pub line_breakpoints: Vec<LineBreakpointState>,
    /// Python exception breakpoints (View Breakpoints › Python Exception Breakpoints).
    pub exception_breakpoints: ExceptionBreakpointSettings,
    /// Mute Breakpoints.
    pub breakpoints_muted: bool,
    /// Absolute project root when the file was written. Paths inside the
    /// project are stored relative to it, so a copied or moved project
    /// restores its own files rather than the original folder's.
    pub root: Option<PathBuf>,
}

/// Settings › Build, Execution, Deployment › Python Debugger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebuggerSettings {
    pub workspace: debugger_workspace::DebuggerWorkspaceSettings,
    /// Do not step into library scripts (debugpy `justMyCode`): Step Into skips
    /// the standard library and site-packages, and exceptions raised there are
    /// ignored. Breakpoints set in library files before a session still work.
    pub just_my_code: bool,
    /// Show the return values of functions after stepping out of them.
    pub show_return_values: bool,
    /// gevent compatible debugging (`GEVENT_SUPPORT=True`).
    pub gevent_compatible: bool,
    /// Show variable values at the end of executed lines while suspended.
    pub inline_values: bool,
}

impl Default for DebuggerSettings {
    fn default() -> Self {
        Self {
            workspace: Default::default(),
            just_my_code: true,
            show_return_values: true,
            gevent_compatible: false,
            inline_values: true,
        }
    }
}

/// A line breakpoint as stored in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LineBreakpointState {
    pub metadata: debugger_workspace::BreakpointMetadata,
    pub path: PathBuf,
    /// 1-based line.
    pub line: usize,
    pub enabled: bool,
    /// Suspend the program when hit (otherwise the breakpoint only logs).
    pub suspend: bool,
    /// Python expression that must be true for the breakpoint to fire.
    pub condition: String,
    /// "Evaluate and log" message; `{expr}` parts are evaluated.
    pub log_message: String,
    /// Log a "Breakpoint reached" message on every hit.
    pub log_hit: bool,
    /// Remove once hit.
    pub temporary: bool,
    /// Hit count condition such as `5`, `>= 3` or `% 2`.
    pub hit_condition: String,
}

impl Default for LineBreakpointState {
    fn default() -> Self {
        Self {
            metadata: Default::default(),
            path: PathBuf::new(),
            line: 1,
            enabled: true,
            suspend: true,
            condition: String::new(),
            log_message: String::new(),
            log_hit: false,
            temporary: false,
            hit_condition: String::new(),
        }
    }
}

/// Python exception breakpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExceptionBreakpointSettings {
    /// Any exception: break when raised.
    pub on_raise: bool,
    /// Any exception: break when it terminates the program.
    pub on_terminate: bool,
    /// Any exception: break when user code does not handle it.
    pub user_unhandled: bool,
    pub classes: Vec<ExceptionClassState>,
}

impl Default for ExceptionBreakpointSettings {
    fn default() -> Self {
        Self {
            on_raise: false,
            on_terminate: true,
            user_unhandled: false,
            classes: Vec::new(),
        }
    }
}

/// An exception breakpoint for one class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExceptionClassState {
    pub name: String,
    pub enabled: bool,
    pub on_raise: bool,
    pub on_terminate: bool,
}

impl Default for ExceptionClassState {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            on_raise: false,
            on_terminate: true,
        }
    }
}

/// Bookmark target: only Line bookmarks participate in document line anchors.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkKind {
    #[default]
    Line,
    File,
    Folder,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BookmarkState {
    pub kind: BookmarkKind,
    /// Empty in older workspaces: the default list.
    pub list: String,
    pub path: PathBuf,
    /// 1-based line.
    pub line: usize,
    pub mnemonic: Option<char>,
    pub description: String,
}

/// A persisted per-file encoding choice. Unknown codec keys are retained so
/// consumers can report unsupported values instead of silently choosing a codec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEncodingOverride {
    pub path: PathBuf,
    pub encoding: String,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenFileState {
    pub path: PathBuf,
    pub caret_line: u32,
    pub caret_column: u32,
    pub scroll_line: u32,
    pub pinned: bool,
}

/// Physical editor placement; these names describe the visible panes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorSplit {
    SideBySide,
    Stacked,
}

/// One file's group membership, keyed by its path rather than a runtime tab ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorGroupMember {
    pub path: PathBuf,
    pub group: u8,
}

/// Outer window origin and inner client size in logical screen coordinates.
/// Negative positions support monitors
/// left of or above the primary display. Invalid bounds are discarded on load.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogicalScreenBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl LogicalScreenBounds {
    fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() <= 1_000_000.0
            && self.y.abs() <= 1_000_000.0
            && self.width.is_finite()
            && self.height.is_finite()
            && (64.0..=16_384.0).contains(&self.width)
            && (64.0..=16_384.0).contains(&self.height)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DetachedEditorState {
    pub path: PathBuf,
    pub bounds: Option<LogicalScreenBounds>,
}

/// Independent position of one editor instance. Multiple groups may share one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorInstanceState {
    pub file: OpenFileState,
    pub group: u8,
}

/// Optional session placement. Membership is reconciled in open-file order,
/// and an unusable split becomes one group without dropping valid files.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EditorLayout {
    pub instances: Vec<EditorInstanceState>,
    pub active_group: u8,
    pub detached_group: u8,
    pub groups: Vec<EditorGroupMember>,
    pub split: Option<EditorSplit>,
    pub split_fraction: f32,
    pub group_active: [Option<PathBuf>; 2],
    pub detached: Option<DetachedEditorState>,
}
impl Default for EditorLayout {
    fn default() -> Self {
        Self {
            instances: Vec::new(),
            active_group: 0,
            detached_group: 0,
            groups: Vec::new(),
            split: None,
            split_fraction: 0.5,
            group_active: [None, None],
            detached: None,
        }
    }
}
impl EditorLayout {
    /// Normalize metadata only; opening/decoding files remains the caller's job.
    /// Missing group records mean group zero; the first duplicate record wins.
    pub fn normalize(&mut self, open_files: &[OpenFileState], active_file: Option<&Path>) {
        if !self.split_fraction.is_finite() || !(0.1..=0.9).contains(&self.split_fraction) {
            self.split_fraction = 0.5;
        }
        self.active_group = u8::from(self.active_group == 1);
        self.detached_group = u8::from(self.detached_group == 1);
        let mut seen = std::collections::HashSet::new();
        self.instances.retain_mut(|instance| {
            instance.group = u8::from(instance.group == 1);
            let path = rustpy_core::vfs::normalize_path(&instance.file.path);
            open_files
                .iter()
                .any(|file| rustpy_core::vfs::normalize_path(&file.path) == path)
                && seen.insert((path, instance.group))
        });
        let mut groups = Vec::new();
        for file in open_files {
            if groups.iter().any(|member: &EditorGroupMember| {
                rustpy_core::vfs::normalize_path(&member.path)
                    == rustpy_core::vfs::normalize_path(&file.path)
            }) {
                continue;
            }
            let group = self
                .groups
                .iter()
                .find(|member| {
                    rustpy_core::vfs::normalize_path(&member.path)
                        == rustpy_core::vfs::normalize_path(&file.path)
                })
                .map(|member| if member.group == 1 { 1 } else { 0 })
                .unwrap_or(0);
            groups.push(EditorGroupMember {
                path: file.path.clone(),
                group,
            });
        }
        if self.split.is_none()
            || !(groups.iter().any(|member| member.group == 0)
                || self.instances.iter().any(|instance| instance.group == 0))
            || !(groups.iter().any(|member| member.group == 1)
                || self.instances.iter().any(|instance| instance.group == 1))
        {
            self.split = None;
            for member in &mut groups {
                member.group = 0;
            }
            for instance in &mut self.instances {
                instance.group = 0;
            }
            let mut paths = std::collections::HashSet::new();
            self.instances.retain(|instance| {
                paths.insert(rustpy_core::vfs::normalize_path(&instance.file.path))
            });
            self.active_group = 0;
            self.detached_group = 0;
        }
        for group in 0..2 {
            let find = |path: &Path| {
                self.instances
                    .iter()
                    .find(|instance| {
                        instance.group as usize == group
                            && rustpy_core::vfs::normalize_path(&instance.file.path)
                                == rustpy_core::vfs::normalize_path(path)
                    })
                    .map(|instance| instance.file.path.clone())
                    .or_else(|| {
                        groups
                            .iter()
                            .find(|member| {
                                member.group as usize == group
                                    && rustpy_core::vfs::normalize_path(&member.path)
                                        == rustpy_core::vfs::normalize_path(path)
                            })
                            .map(|member| member.path.clone())
                    })
            };
            self.group_active[group] = self.group_active[group]
                .as_deref()
                .and_then(find)
                .or_else(|| active_file.and_then(find))
                .or_else(|| {
                    groups
                        .iter()
                        .find(|member| member.group as usize == group)
                        .map(|member| member.path.clone())
                });
        }
        if let Some(detached) = &mut self.detached {
            if let Some(member) = groups.iter().find(|member| {
                rustpy_core::vfs::normalize_path(&member.path)
                    == rustpy_core::vfs::normalize_path(&detached.path)
            }) {
                detached.path = member.path.clone();
                if !self.instances.iter().any(|instance| {
                    instance.file.path == detached.path && instance.group == self.detached_group
                }) {
                    self.detached_group = member.group;
                }
                if detached.bounds.is_some_and(|bounds| !bounds.is_valid()) {
                    detached.bounds = None;
                }
            } else {
                self.detached = None;
            }
        }
        self.groups = groups;
    }
}
impl<'de> Deserialize<'de> for EditorLayout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut layout = Self {
            instances: value
                .get("instances")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default(),
            active_group: u8::from(
                value
                    .get("active_group")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1),
            ),
            detached_group: u8::from(
                value
                    .get("detached_group")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1),
            ),
            ..Self::default()
        };
        let path =
            |value: &serde_json::Value| serde_json::from_value::<PathBuf>(value.clone()).ok();
        if let Some(groups) = value.get("groups").and_then(serde_json::Value::as_array) {
            layout.groups = groups
                .iter()
                .filter_map(|member| {
                    Some(EditorGroupMember {
                        path: path(member.get("path")?)?,
                        group: if member.get("group").and_then(serde_json::Value::as_u64) == Some(1)
                        {
                            1
                        } else {
                            0
                        },
                    })
                })
                .collect();
        }
        layout.split = value
            .get("split")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        layout.split_fraction = value
            .get("split_fraction")
            .and_then(serde_json::Value::as_f64)
            .map(|n| n as f32)
            .filter(|n| n.is_finite() && (0.1..=0.9).contains(n))
            .unwrap_or(0.5);
        if let Some(active) = value
            .get("group_active")
            .and_then(serde_json::Value::as_array)
        {
            for (index, value) in active.iter().take(2).enumerate() {
                layout.group_active[index] = path(value);
            }
        }
        layout.detached = value.get("detached").and_then(|value| {
            let path = path(value.get("path")?)?;
            let bounds = value
                .get("bounds")
                .and_then(|value| serde_json::from_value::<LogicalScreenBounds>(value.clone()).ok())
                .filter(|bounds| bounds.is_valid());
            Some(DetachedEditorState { path, bounds })
        });
        Ok(layout)
    }
}
/// Invalid layout fields must not invalidate unrelated, valid flat session data.
pub(crate) fn deserialize_editor_layout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<EditorLayout>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    if !value.is_object() {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

/// Which tool windows a project shows, restored when it opens again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolWindowLayout {
    pub framework: tool_windows::Framework,
    /// The left tool window (`left_tool`) is shown.
    pub project_visible: bool,
    /// Width of the left tool window.
    pub project_width: f32,
    /// The left tool window shows Structure (also see `left_tool`).
    pub structure_visible: bool,
    pub bottom_visible: bool,
    pub bottom_height: f32,
    /// Title of the bottom tool window ("Terminal", "Run", "Git", …).
    pub bottom_tab: String,
    /// Left tool window: "Project", "Structure" or "Bookmarks".
    pub left_tool: String,
    /// The bottom tool window floats in its own OS window.
    pub bottom_floating: bool,
    pub bottom_maximized: bool,
    /// The tool window used last (View › Jump to Last Tool Window, F12).
    pub last_tool: String,
}

impl Default for ToolWindowLayout {
    fn default() -> Self {
        Self {
            framework: Default::default(),
            project_visible: true,
            project_width: 260.0,
            structure_visible: false,
            bottom_visible: false,
            bottom_height: 300.0,
            bottom_tab: "Terminal".into(),
            left_tool: "Project".into(),
            bottom_floating: false,
            bottom_maximized: false,
            last_tool: String::new(),
        }
    }
}

impl Workspace {
    pub fn path_for(project: &Path) -> PathBuf {
        project.join(PROJECT_DIR_NAME).join("workspace.json")
    }

    pub fn load(project: &Path) -> Workspace {
        match WorkspaceStore::new(project).load() {
            Ok(snapshot) => snapshot.workspace().clone(),
            Err(error) => {
                log::warn!("cannot load workspace: {error}");
                Self::default()
            }
        }
    }

    /// Checked loading; unlike the compatibility loader, errors are not defaults.
    pub fn try_load(project: &Path) -> Result<Workspace, WorkspaceError> {
        WorkspaceStore::new(project)
            .load()
            .map(|snapshot| snapshot.workspace().clone())
    }

    pub(crate) fn resolve_paths(&mut self, project: &Path) {
        let saved_root = self.root.take();
        self.map_paths(|p| {
            if p.is_relative() {
                return Some(project.join(p));
            }
            match &saved_root {
                // Written from another location (a copy or a move): paths under
                // the old root belong to this project.
                Some(old) => Some(
                    p.strip_prefix(old)
                        .map(|rel| project.join(rel))
                        .unwrap_or_else(|_| p.to_path_buf()),
                ),
                // Older files stored absolute paths only: anything outside this
                // project may be another copy's file, so it is not restored.
                None => p.starts_with(project).then(|| p.to_path_buf()),
            }
        });
        self.normalize_editor_layout();
    }

    pub(crate) fn normalize_editor_layout(&mut self) {
        if let Some(layout) = &mut self.editor_layout {
            layout.normalize(&self.open_files, self.active_file.as_deref());
            self.active_file = self
                .active_file
                .as_deref()
                .and_then(|path| {
                    self.open_files
                        .iter()
                        .find(|file| {
                            rustpy_core::vfs::normalize_path(&file.path)
                                == rustpy_core::vfs::normalize_path(path)
                        })
                        .map(|file| file.path.clone())
                })
                .or_else(|| layout.group_active[0].clone())
                .or_else(|| layout.group_active[1].clone());
        }
    }

    /// Checked atomic compatibility replacement. For concurrent edits use
    /// WorkspaceStore patches; this legacy API has no caller baseline.
    pub fn save(&self, project: &Path) -> anyhow::Result<()> {
        WorkspaceStore::new(project)
            .replace_compat(self)
            .map_err(anyhow::Error::msg)
    }

    /// Applies `f` to every stored path; entries mapped to `None` are dropped.
    pub(crate) fn map_paths(&mut self, f: impl Fn(&Path) -> Option<PathBuf>) {
        self.open_files = std::mem::take(&mut self.open_files)
            .into_iter()
            .filter_map(|mut o| {
                f(&o.path).map(|p| {
                    o.path = p;
                    o
                })
            })
            .collect();
        self.file_encodings = std::mem::take(&mut self.file_encodings)
            .into_iter()
            .filter_map(|mut entry| {
                f(&entry.path).map(|path| {
                    entry.path = path;
                    entry
                })
            })
            .collect();
        self.active_file = self.active_file.as_deref().and_then(&f);
        self.expanded_dirs = self.expanded_dirs.iter().filter_map(|p| f(p)).collect();
        self.interpreter = self
            .interpreter
            .as_deref()
            .and_then(|p| f(p).or_else(|| Some(p.to_path_buf())));
        self.breakpoints = std::mem::take(&mut self.breakpoints)
            .into_iter()
            .filter_map(|(p, l)| f(&p).map(|p| (p, l)))
            .collect();
        self.line_breakpoints = std::mem::take(&mut self.line_breakpoints)
            .into_iter()
            .filter_map(|mut b| {
                f(&b.path).map(|p| {
                    b.path = p;
                    b
                })
            })
            .collect();
        self.bookmarks = std::mem::take(&mut self.bookmarks)
            .into_iter()
            .filter_map(|mut b| {
                f(&b.path).map(|p| {
                    b.path = p;
                    b
                })
            })
            .collect();
        self.recent_files = self.recent_files.iter().filter_map(|p| f(p)).collect();
        self.source_roots = self.source_roots.iter().filter_map(|p| f(p)).collect();
        self.excluded_dirs = self.excluded_dirs.iter().filter_map(|p| f(p)).collect();
        if let Some(layout) = &mut self.editor_layout {
            layout.instances = std::mem::take(&mut layout.instances)
                .into_iter()
                .filter_map(|mut instance| {
                    f(&instance.file.path).map(|path| {
                        instance.file.path = path;
                        instance
                    })
                })
                .collect();
            layout.groups = std::mem::take(&mut layout.groups)
                .into_iter()
                .filter_map(|mut member| {
                    f(&member.path).map(|path| {
                        member.path = path;
                        member
                    })
                })
                .collect();
            for active in &mut layout.group_active {
                *active = active.as_deref().and_then(&f);
            }
            layout.detached = layout.detached.take().and_then(|mut detached| {
                f(&detached.path).map(|path| {
                    detached.path = path;
                    detached
                })
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_inlay_code_vision_flags_preserve_legacy_and_round_trip() {
        let legacy: EditorSettings = serde_json::from_str("{}").unwrap();
        assert!(
            !legacy.inlay_return_hints && !legacy.code_vision_usages && !legacy.code_vision_author
        );
        for flags in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            let settings = EditorSettings {
                inlay_return_hints: flags.0,
                code_vision_usages: flags.1,
                code_vision_author: flags.2,
                ..Default::default()
            };
            let restored: EditorSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(
                (
                    restored.inlay_return_hints,
                    restored.code_vision_usages,
                    restored.code_vision_author
                ),
                flags
            );
        }
    }

    #[test]
    fn each_keymap_preset_keeps_its_own_changes() {
        let mut s = Settings::default();
        assert_eq!(s.keymap_name(), "Default");
        s.custom_shortcuts = vec![("SaveDocument".into(), vec!["Ctrl+Alt+S".into()])];
        s.switch_keymap("VS Code");
        assert_eq!(s.keymap, "VS Code");
        assert!(
            s.custom_shortcuts.is_empty(),
            "Default's change does not follow into VS Code"
        );
        assert!(s.keymap_modified("Default") && !s.keymap_modified("VS Code"));
        s.custom_shortcuts = vec![("GotoFile".into(), vec!["Ctrl+E".into()])];
        s.switch_keymap("Default");
        assert_eq!(
            s.custom_shortcuts,
            vec![("SaveDocument".to_string(), vec!["Ctrl+Alt+S".to_string()])]
        );
        s.switch_keymap("VS Code");
        assert_eq!(
            s.custom_shortcuts,
            vec![("GotoFile".to_string(), vec!["Ctrl+E".to_string()])]
        );
        // Selecting the current preset changes nothing; a preset without changes stores nothing.
        s.switch_keymap("VS Code");
        assert_eq!(s.custom_shortcuts.len(), 1);
        s.custom_shortcuts.clear();
        s.switch_keymap("Eclipse");
        assert!(
            s.keymap_overrides.iter().all(|(p, _)| p != "VS Code"),
            "{:?}",
            s.keymap_overrides
        );
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.keymap_overrides, s.keymap_overrides);
    }

    #[test]
    fn settings_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rustpy-settings-test-{}", std::process::id()));
        std::env::set_var("RUSTPY_CONFIG_DIR", &dir);
        let mut s = Settings::load();
        assert_eq!(s.editor.tab_size, 4);
        s.editor.tab_size = 2;
        s.add_recent_project(Path::new("/tmp/p"));
        s.save().unwrap();
        let loaded = Settings::load();
        assert_eq!(loaded.editor.tab_size, 2);
        assert_eq!(loaded.recent_projects, vec![PathBuf::from("/tmp/p")]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn workspace_paths_follow_a_copied_project() {
        let base = std::env::temp_dir().join(format!("rustpy-ws-copy-{}", std::process::id()));
        let (a, b) = (base.join("projA"), base.join("projB"));
        std::fs::create_dir_all(&a).unwrap();
        let ws = Workspace {
            open_files: vec![
                OpenFileState {
                    path: a.join("main.py"),
                    ..Default::default()
                },
                OpenFileState {
                    path: PathBuf::from("/usr/lib/python3/os.py"),
                    ..Default::default()
                },
            ],
            active_file: Some(a.join("main.py")),
            bookmarks: vec![BookmarkState {
                path: a.join("pkg/util.py"),
                line: 3,
                ..Default::default()
            }],
            recent_files: vec![a.join("main.py")],
            breakpoints: vec![(a.join("main.py"), vec![2])],
            ..Default::default()
        };
        ws.save(&a).unwrap();
        let text = std::fs::read_to_string(Workspace::path_for(&a)).unwrap();
        assert!(text.contains("\"main.py\""), "stored relative: {text}");
        // Copy the project: its files, not projA's, are restored.
        std::fs::create_dir_all(b.join(".rustpy")).unwrap();
        std::fs::copy(Workspace::path_for(&a), Workspace::path_for(&b)).unwrap();
        let loaded = Workspace::load(&b);
        assert_eq!(loaded.open_files[0].path, b.join("main.py"));
        assert_eq!(
            loaded.open_files[1].path,
            PathBuf::from("/usr/lib/python3/os.py"),
            "files outside the project keep their path"
        );
        assert_eq!(loaded.active_file, Some(b.join("main.py")));
        assert_eq!(loaded.bookmarks[0].path, b.join("pkg/util.py"));
        assert_eq!(loaded.recent_files, vec![b.join("main.py")]);
        assert_eq!(loaded.breakpoints[0].0, b.join("main.py"));
        // An old file with absolute paths only: entries of another folder are dropped.
        std::fs::write(
            Workspace::path_for(&b),
            serde_json::to_string(&Workspace {
                open_files: vec![
                    OpenFileState {
                        path: a.join("main.py"),
                        ..Default::default()
                    },
                    OpenFileState {
                        path: b.join("x.py"),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let legacy = Workspace::load(&b);
        assert_eq!(
            legacy
                .open_files
                .iter()
                .map(|o| o.path.clone())
                .collect::<Vec<_>>(),
            vec![b.join("x.py")]
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn workspace_cannot_mark_a_project_trusted() {
        // Older workspace.json files (or a hostile repository) may carry `trusted: true`.
        let ws: Workspace =
            serde_json::from_str(r#"{"trusted": true, "startup_tasks": ["serve"]}"#).unwrap();
        assert_eq!(ws.startup_tasks, vec!["serve".to_string()]);
        let written = serde_json::to_string(&ws).unwrap();
        assert!(
            !written.contains("trusted"),
            "trust is never written into the project: {written}"
        );
        let s = Settings::default();
        assert!(!s.is_project_trusted(Path::new("/work/repo")));
    }

    #[test]
    fn trusted_projects_and_locations() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let mut settings = Settings::default();
        assert!(settings.trust_project(&project));
        assert!(!settings.trust_project(&project));
        assert!(settings.is_project_trusted(&project));
        settings.trusted_projects.clear();
        settings.trusted_locations.push(root.clone());
        assert!(settings.is_project_trusted(&project));
        assert!(!settings.is_project_trusted(&root.join("missing")));
        assert!(!settings.trust_project(&root.join("missing")));
    }

    #[test]
    fn open_project_in_setting() {
        let mut s = Settings::default();
        assert_eq!(s.open_project_in(), OpenProjectIn::Ask);
        s.set_open_project_in(OpenProjectIn::NewWindow);
        assert_eq!(s.open_project_in, "new");
        assert_eq!(s.open_project_in(), OpenProjectIn::NewWindow);
        s.set_open_project_in(OpenProjectIn::CurrentWindow);
        assert_eq!(s.open_project_in(), OpenProjectIn::CurrentWindow);
    }

    #[test]
    fn ui_language_round_trips_and_defaults_to_the_system_language() {
        assert_eq!(
            Settings::default().appearance.language,
            "",
            "empty = system default"
        );
        // Files written before the setting existed keep the other appearance values.
        let old: Settings =
            serde_json::from_str(r#"{"appearance": {"theme": "Light", "ui_font_size": 15}}"#)
                .unwrap();
        assert_eq!(old.appearance.language, "");
        assert_eq!(old.appearance.theme, "Light");
        let mut s = Settings::default();
        s.appearance.language = "zh-CN".into();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""language":"zh-CN""#), "{json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.appearance.language, "zh-CN");
        assert_eq!(back.appearance, s.appearance);
    }

    #[test]
    fn ui_language_survives_a_save_and_load() {
        let dir =
            std::env::temp_dir().join(format!("rustpy-settings-language-{}", std::process::id()));
        let path = dir.join("settings.json");
        let (mut s, mut base, _) = persist::load_from(&path);
        s.appearance.language = "ja".into();
        persist::save_to(&path, &s, &mut base).unwrap();
        let (loaded, _, err) = persist::load_from(&path);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(loaded.appearance.language, "ja");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn debugger_settings_and_breakpoints_default_sensibly() {
        let s: Settings =
            serde_json::from_str(r#"{"debugger": {"gevent_compatible": true}}"#).unwrap();
        assert!(
            s.debugger.just_my_code
                && s.debugger.show_return_values
                && s.debugger.inline_values
                && s.debugger.gevent_compatible
        );
        assert!(
            Settings::default().debugger.just_my_code,
            "Do not step into library scripts is on by default"
        );
        // Older workspaces only have plain breakpoint lines.
        let ws: Workspace =
            serde_json::from_str(r#"{"breakpoints": [["/p/a.py", [3, 7]]]}"#).unwrap();
        assert_eq!(ws.breakpoints, vec![(PathBuf::from("/p/a.py"), vec![3, 7])]);
        assert!(ws.line_breakpoints.is_empty());
        assert!(
            ws.exception_breakpoints.on_terminate
                && !ws.exception_breakpoints.on_raise
                && !ws.breakpoints_muted
        );
        let ws: Workspace = serde_json::from_str(r#"{"line_breakpoints": [{"path": "/p/a.py", "line": 4, "condition": "x > 1"}], "exception_breakpoints": {"classes": [{"name": "ValueError"}]}}"#).unwrap();
        let bp = &ws.line_breakpoints[0];
        assert!(
            bp.enabled && bp.suspend && !bp.temporary,
            "missing flags keep their defaults: {bp:?}"
        );
        assert_eq!((bp.line, bp.condition.as_str()), (4, "x > 1"));
        let class = &ws.exception_breakpoints.classes[0];
        assert!(class.enabled && class.on_terminate && !class.on_raise);
    }

    #[test]
    fn line_breakpoints_follow_a_copied_project() {
        let base = std::env::temp_dir().join(format!("rustpy-ws-bp-copy-{}", std::process::id()));
        let (a, b) = (base.join("projA"), base.join("projB"));
        std::fs::create_dir_all(&a).unwrap();
        let library = PathBuf::from("/usr/lib/python3/json/decoder.py");
        let ws = Workspace {
            line_breakpoints: vec![
                LineBreakpointState {
                    path: a.join("main.py"),
                    line: 4,
                    condition: "x > 1".into(),
                    ..Default::default()
                },
                LineBreakpointState {
                    path: library.clone(),
                    line: 9,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        ws.save(&a).unwrap();
        let text = std::fs::read_to_string(Workspace::path_for(&a)).unwrap();
        assert!(text.contains("\"main.py\""), "stored relative: {text}");
        std::fs::create_dir_all(b.join(".rustpy")).unwrap();
        std::fs::copy(Workspace::path_for(&a), Workspace::path_for(&b)).unwrap();
        let loaded = Workspace::load(&b);
        assert_eq!(
            (
                loaded.line_breakpoints[0].path.clone(),
                loaded.line_breakpoints[0].condition.as_str()
            ),
            (b.join("main.py"), "x > 1")
        );
        assert_eq!(
            loaded.line_breakpoints[1].path, library,
            "files outside the project keep their path"
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn library_source_protection_defaults_safely_and_persists_independently_of_reader_mode() {
        let old: Settings =
            serde_json::from_str(r#"{"editor":{"reader_mode":{"enabled":false}}}"#).unwrap();
        assert!(old.editor.reader_mode.protect_library_sources);
        assert!(!old.editor.reader_mode.enabled);
        let mut settings = Settings::default();
        settings.editor.reader_mode.protect_library_sources = false;
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.editor.reader_mode.protect_library_sources);
        assert!(restored.editor.reader_mode.enabled);
    }

    #[test]
    fn textmate_bundles_roundtrip_and_default_to_enabled() {
        let mut s = Settings::default();
        s.textmate_bundles.push(TextMateBundle {
            path: PathBuf::from("/b/rust"),
            enabled: false,
        });
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.textmate_bundles, s.textmate_bundles);
        // An entry without `enabled` (hand-written settings) is enabled.
        let s: Settings =
            serde_json::from_str(r#"{"textmate_bundles": [{"path": "/b/go"}]}"#).unwrap();
        assert_eq!(
            s.textmate_bundles,
            vec![TextMateBundle {
                path: PathBuf::from("/b/go"),
                enabled: true
            }]
        );
        // Older settings files have no list.
        assert!(serde_json::from_str::<Settings>("{}")
            .unwrap()
            .textmate_bundles
            .is_empty());
    }

    #[test]
    fn provider_preferences_roundtrip_without_credentials() {
        let mut settings = Settings::default();
        settings.ai.provider = "openai-compatible".into();
        settings.ai.openai_model = "configured-model".into();
        settings.ai.openai_api_key = "do-not-serialize".into();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("do-not-serialize"));
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.ai.openai_model, "configured-model");
        assert_eq!(restored.ai.provider, "openai-compatible");
        assert!(restored.ai.openai_api_key.is_empty());
        let legacy: Settings = serde_json::from_str(r#"{"ai":{"model":"legacy-model"}}"#).unwrap();
        assert_eq!(legacy.ai.provider, "anthropic");
        assert_eq!(legacy.ai.model, "legacy-model");
    }

    #[test]
    fn ai_key_is_never_written_back_to_settings_json() {
        // Older builds stored the key in plain text under `ai.api_key`.
        let s: Settings =
            serde_json::from_str(r#"{"ai": {"api_key": "sk-old", "model": "m"}}"#).unwrap();
        assert_eq!(s.ai.legacy_api_key, "sk-old");
        assert_eq!(
            s.ai.api_key, "",
            "the session key comes from the password safe"
        );
        assert!(s.ai.remember_api_key);
        // Until it is moved into the password safe the legacy key is kept (no data loss) ...
        assert!(serde_json::to_string(&s).unwrap().contains("sk-old"));
        // ... afterwards neither it nor the session key reach the file.
        let mut moved = s.clone();
        moved.ai.legacy_api_key.clear();
        moved.ai.api_key = "sk-session".into();
        let json = serde_json::to_string(&moved).unwrap();
        assert!(!json.contains("sk-"), "{json}");
        assert!(!json.contains(r#""api_key""#), "{json}");
        let defaults: Settings = serde_json::from_str("{}").unwrap();
        assert!(defaults.ai.remember_api_key);
        assert_eq!(defaults.http_proxy.mode, ProxyMode::AutoDetect);
    }

    #[test]
    fn update_settings_default_to_daily_stable_checks() {
        // Settings files from before the Updates page get the defaults.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.updates.check_automatically && s.updates.show_whats_new);
        assert_eq!(s.updates.channel, "release");
        assert_eq!(
            (
                s.updates.last_check,
                s.updates.last_seen_version.as_str(),
                s.updates.repository.as_str()
            ),
            (0, "", "")
        );
        let s: Settings = serde_json::from_str(
            r#"{"updates": {"channel": "eap", "ignored_versions": ["0.2.0"]}}"#,
        )
        .unwrap();
        assert_eq!(s.updates.channel, "eap");
        assert_eq!(s.updates.ignored_versions, vec!["0.2.0".to_string()]);
        assert!(s.updates.check_automatically);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.updates, s.updates);
    }

    #[test]
    fn tool_window_layout_and_notifications_default_for_old_files() {
        let ws: Workspace = serde_json::from_str(r#"{"tool_windows": {"project_visible": false, "bottom_tab": "Git", "bottom_visible": true}}"#).unwrap();
        assert!(!ws.tool_windows.project_visible);
        assert_eq!(ws.tool_windows.left_tool, "Project");
        assert_eq!(ws.tool_windows.bottom_tab, "Git");
        assert!(!ws.tool_windows.bottom_floating && ws.tool_windows.last_tool.is_empty());
        let s: Settings = serde_json::from_str(r#"{"confirm_exit": true}"#).unwrap();
        assert_eq!(s.notifications, NotificationSettings::default());
        let s: Settings =
            serde_json::from_str(r#"{"notifications": {"disabled_groups": ["Git"]}}"#).unwrap();
        assert_eq!(s.notifications.silenced_groups(), vec!["Git".to_string()]);
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_defaulted() {
        let s: Settings =
            serde_json::from_str(r#"{"editor": {"font_size": 15, "bogus": 1}, "future": true}"#)
                .unwrap();
        assert_eq!(s.editor.font_size, 15.0);
        assert_eq!(s.editor.tab_size, 4);
    }

    #[test]
    fn plugin_catalog_url_defaults_to_the_bundled_catalog_and_roundtrips() {
        // Settings files from before plugins: empty = the catalog bundled with the IDE.
        assert_eq!(
            serde_json::from_str::<Settings>("{}")
                .unwrap()
                .plugin_catalog_url,
            ""
        );
        let s = Settings {
            plugin_catalog_url: "https://example.com/catalog.json".into(),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.plugin_catalog_url, s.plugin_catalog_url);
    }

    #[test]
    fn terminal_activates_virtualenv_by_default() {
        assert!(Settings::default().terminal.activate_virtualenv);
        // Older builds saved the (then unused) option as `false` under its old key.
        let s: Settings = serde_json::from_str(
            r#"{"terminal": {"shell": "/bin/zsh", "activate_virtualenv": false}}"#,
        )
        .unwrap();
        assert!(s.terminal.activate_virtualenv);
        assert_eq!(s.terminal.shell, "/bin/zsh");
        let s: Settings =
            serde_json::from_str(r#"{"terminal": {"activate_venv": false}}"#).unwrap();
        assert!(!s.terminal.activate_virtualenv);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""activate_venv":false"#), "{json}");
    }

    #[test]
    fn vcs_update_options_default_for_older_settings_files() {
        let s: Settings =
            serde_json::from_str(r#"{"vcs": {"git_executable": "/usr/bin/git"}}"#).unwrap();
        assert_eq!(s.vcs.git_executable, "/usr/bin/git");
        assert_eq!(
            (
                s.vcs.update_method.as_str(),
                s.vcs.update_autostash,
                s.vcs.update_dont_ask
            ),
            ("merge", true, false)
        );
        let s: Settings = serde_json::from_str(
            r#"{"vcs": {"update_method": "rebase", "update_dont_ask": true}}"#,
        )
        .unwrap();
        assert_eq!(
            (
                s.vcs.update_method.as_str(),
                s.vcs.update_autostash,
                s.vcs.update_dont_ask
            ),
            ("rebase", true, true)
        );
    }
}

#[cfg(test)]
mod caret_settings_tests {
    use super::*;

    #[test]
    fn old_block_preference_and_default_settings_survive_new_fields() {
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.editor.caret_shape, CaretShape::Thin);
        assert_eq!(empty.editor.caret_width, 2);
        assert!(!empty.editor.block_caret);
        let old: Settings =
            serde_json::from_str(r#"{"editor":{"block_caret":true,"caret_blinking":false}}"#)
                .unwrap();
        assert!(old.editor.block_caret);
        assert!(!old.editor.caret_blinking);
        assert_eq!(old.editor.caret_shape, CaretShape::Thin);
        assert_eq!(old.editor.caret_width, 2);
    }

    #[test]
    fn every_shape_and_width_roundtrips_without_discarding_legacy_preference() {
        for shape in [CaretShape::Thin, CaretShape::Block, CaretShape::Underscore] {
            let settings = Settings {
                editor: EditorSettings {
                    caret_shape: shape,
                    caret_width: 5,
                    block_caret: true,
                    ..EditorSettings::default()
                },
                ..Settings::default()
            };
            let encoded = serde_json::to_string(&settings).unwrap();
            let decoded: Settings = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, settings);
            assert!(encoded.contains(r#""block_caret":true"#));
        }
    }

    #[test]
    fn numeric_width_is_not_an_unchecked_floating_point_preference() {
        let width: Settings = serde_json::from_str(r#"{"editor":{"caret_width":255}}"#).unwrap();
        assert_eq!(width.editor.caret_width, 255);
        // The renderer clamps representable out-of-range values without losing
        // the other settings; invalid types retain normal checked-load errors.
        for invalid in ["-1", "256", "1.5", "null", r#""wide""#] {
            assert!(serde_json::from_str::<Settings>(&format!(
                r#"{{"editor":{{"caret_width":{invalid}}}}}"#
            ))
            .is_err());
        }
    }
}

#[cfg(test)]
mod smart_paste_settings_tests {
    use super::*;
    #[test]
    fn return_type_docstring_option_roundtrips_independently_of_parameter_types() {
        let legacy: EditorSettings = serde_json::from_str("{}").unwrap();
        assert!(!legacy.insert_rtype_in_docstring_stub);
        let settings = EditorSettings {
            insert_rtype_in_docstring_stub: true,
            insert_type_placeholders_in_docstring_stub: false,
            ..Default::default()
        };
        let decoded: EditorSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(decoded.insert_rtype_in_docstring_stub);
        assert!(!decoded.insert_type_placeholders_in_docstring_stub);
    }
    #[test]
    fn legacy_defaults_and_all_paste_modes_roundtrip() {
        let legacy: EditorSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.reformat_on_paste, ReformatOnPaste::IndentBlock);
        assert!(!legacy.insert_type_placeholders_in_docstring_stub);
        for mode in [
            ReformatOnPaste::None,
            ReformatOnPaste::IndentBlock,
            ReformatOnPaste::Reformat,
        ] {
            let settings = EditorSettings {
                reformat_on_paste: mode,
                insert_type_placeholders_in_docstring_stub: true,
                ..Default::default()
            };
            let decoded: EditorSettings =
                serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
            assert_eq!(decoded.reformat_on_paste, mode);
            assert!(decoded.insert_type_placeholders_in_docstring_stub);
        }
        for (json, expected) in [
            ("\"None\"", ReformatOnPaste::None),
            ("\"IndentBlock\"", ReformatOnPaste::IndentBlock),
        ] {
            assert_eq!(
                serde_json::from_str::<ReformatOnPaste>(json).unwrap(),
                expected
            );
        }
    }
}

#[cfg(test)]
mod external_formatter_settings_tests {
    use super::*;
    #[test]
    fn legacy_style_settings_gain_all_tools_and_per_tool_configuration_roundtrips() {
        let legacy: CodeStyleSettings =
            serde_json::from_str(r#"{"formatter":"ruff","formatter_args":"--line-length 88"}"#)
                .unwrap();
        assert_eq!(legacy.external_formatters.len(), 5);
        assert!(!legacy.use_black_default);
        let mut current = legacy;
        current.use_black_default = true;
        let config = current
            .external_formatters
            .iter_mut()
            .find(|c| c.tool == "isort")
            .unwrap();
        config.executable = "/selected environment/bin/isort".into();
        config.arguments = "--profile black".into();
        config.file_masks = "src/*.py; *.pyi".into();
        config.on_save = false;
        assert_eq!(
            serde_json::from_str::<CodeStyleSettings>(&serde_json::to_string(&current).unwrap())
                .unwrap(),
            current
        );
    }
}

pub mod data_files;
#[cfg(test)]
#[path = "keymap_workspace_fixtures.rs"]
mod keymap_workspace_fixtures;
pub mod platform;
pub mod scientific;
pub mod shared_indexes;

pub mod web_development;

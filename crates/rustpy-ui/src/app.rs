//! The IDE application: window layout, menus, tabs, tool windows, dialogs.

#[cfg(test)]
#[path = "app/project_tab_discovery_transition_tests.rs"]
mod project_tab_discovery_transition_tests;

#[cfg(test)]
#[path = "app/project_tab_global_task_transition_tests.rs"]
mod project_tab_global_task_transition_tests;

#[cfg(test)]
#[path = "app/project_tab_global_task_appearance_tests.rs"]
mod project_tab_global_task_appearance_tests;

#[cfg(test)]
#[path = "app/project_tab_unlock_discriminator.rs"]
mod project_tab_unlock_discriminator;

use crate::a11y::DialogWindow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_analysis::inspections::{self, InspectionOptions, QuickFix};
use rustpy_core::{TextRange, TextSize};
use rustpy_project::{Interpreter, RunConfig, RunConfigKind, RunConfigStore};
use rustpy_refactor::refactor::{self, Refactoring};
use rustpy_run::RunSession;
use rustpy_semantic::features::{self, CompletionItem, CompletionKind, HoverInfo, SignatureHelp};
use rustpy_semantic::usages::{self, Usage};
use rustpy_semantic::Analysis;
use rustpy_syntax::ast::AstNode;

mod a11y_ui;
#[cfg(test)]
mod accessibility_connected_tests;
mod accessibility_workspace;
mod analysis_jobs;
mod background;
mod basic_completion_ui;
#[cfg(test)]
mod binary_view_connected_tests;
mod bookmark_lists_ui;
#[cfg(test)]
mod bookmark_shortcuts_connected_tests;
mod bottom_dock;
mod branch_workspace;
mod breadcrumbs_ui;
mod choice_popup;
mod chrome_ui;
mod cli_requests;
mod clipboard_shortcuts;
mod code_insight;
mod code_style_ui;
mod commit_workspace;
mod completion_ui;
mod completion_worker;
mod concurrency_ui;
mod configuration_types_ui;
#[cfg(test)]
mod console_accessibility_tests;
mod console_exec;
mod conversion_ui;
mod copy_reference_paths;
mod copyright_ui;
mod coverage_ui;
mod csv_table;
#[cfg(test)]
mod csv_table_connected_tests;
mod csv_table_ui;
mod data_file_completion;
mod data_files_ui;
mod database_workspace;
mod debug_log_ui;
mod debug_sessions;
#[cfg(test)]
mod debug_tree_accessibility_tests;
mod debugger_ui;
#[cfg(test)]
mod declaration_navigation_tests;
mod deployment_host;
mod diagnostics;
mod diagnostics_workspace;
mod diagrams_workspace;
mod dialog_workspace;
mod diff_assets;
mod diff_workspace;
mod directory_comparison_ui;
pub(crate) mod directory_diff;
mod docker_services;
mod doctest_ui;
mod document_markers;
mod documentation_preferences;
mod editor_accessibility_ui;
mod editor_actions;
mod editor_splits;
mod editor_title;
mod editor_title_ui;
#[cfg(test)]
mod editor_transform_tests;
#[cfg(test)]
mod editorconfig_style_connected_tests;
mod editorconfig_ui;
mod emacs_editing;
#[cfg(test)]
mod emacs_editing_tests;
mod emmet_ui;
mod encoding_ui;
#[cfg(test)]
mod endpoint_package_accessibility_tests;
#[cfg(test)]
mod error_description_connected_tests;
mod error_description_ui;
mod extract_ui;
mod file_comparisons;
mod file_permissions;
#[cfg(test)]
mod file_permissions_tests;
mod file_results_ui;
mod file_sync;
mod file_templates_ui;
mod find_bar;
mod font_ui;
mod formatter_tools;
#[cfg(all(test, target_os = "linux"))]
mod framework_shell_acceptance_tests;
#[cfg(test)]
mod frameworks_connected_tests;
mod frameworks_ui;
mod generate_methods_ui;
#[cfg(test)]
mod git_accessibility_tests;
mod git_ops;
mod git_workflows;
mod github_ui;
mod gitignore_templates_ui;
mod grammar_policy;
mod gutter_actions;
mod gutter_actions_ui;
mod gutter_context;
mod hidden_source_actions;
mod hierarchy_ui;
mod http_workspace;
mod i18n_workspace;
#[cfg(test)]
mod idle_repaint_tests;
mod image_grid;
#[cfg(test)]
mod implementation_navigation_tests;
mod import_workflows;
mod indent_status;
#[cfg(test)]
mod indentation_preferences_tests;
mod index_ui;
mod inlay_code_vision;
mod inlay_code_vision_ui;
mod inline_ui;
mod inplace_rename;
mod inspection_infrastructure;
mod install_package;
mod intention_actions;
mod intention_preferences;
mod intentions_accessibility;
mod interpreter_management;
mod jupyter_workspace;
mod keymap_workspace;
#[cfg(test)]
mod language_injections_connected_tests;
mod language_injections_ui;
mod layout;
mod local_history;
mod local_history_workflows;
mod macro_ui;
mod manage_py_console;
#[cfg(test)]
mod markdown_preview_connected_tests;
mod markdown_ui;
mod member_refactoring_ui;
mod menu_customization;
#[cfg(test)]
mod menu_tests;
mod move_ui;
mod native_project_windows;
mod native_skeleton_ui;
mod native_ui;
mod navigation;
mod navigation_bar;
mod navigation_history;
#[cfg(test)]
mod navigation_scroll_tests;
mod netsec_ui;
mod notify_ui;
#[cfg(test)]
mod ordinary_controls_accessibility_tests;
mod other_vcs_ui;
mod paragraph_ui;
mod param_info;
#[cfg(test)]
mod parameter_info_connected_tests;
mod parameter_info_ui;
mod path_variables_ui;
mod plugin_runtime;
mod plugin_scripting;
mod plugin_vim;
mod plugins_ui;
mod profiler_ui;
mod project_dialogs;
mod project_index;
mod project_line_separators;
mod project_navigation_ui;
mod project_new_ui;
mod project_open;
#[cfg(test)]
mod project_preview_regression_tests;
mod project_problems;
mod project_setup;
mod project_structure;
mod project_view_features;
mod project_windows;
mod push_dialog;
#[cfg(test)]
mod python_console_accessibility_tests;
mod python_stubs;
mod python_style_ui;
#[cfg(test)]
mod quick_documentation_connected_tests;
mod quick_documentation_ui;
mod refactor_ui;
mod refactoring_infrastructure;
mod refactoring_preferences;
mod reformat_workflows;
mod related_navigation;
mod remotes_dialog;
mod rename_framework_ui;
mod rename_ui;
mod rst_workspace;
mod run_configuration_sessions;
mod run_configuration_workflows;
mod run_console;
mod run_launch;
#[cfg(all(test, target_os = "linux"))]
mod run_workflow_acceptance_tests;
mod safe_delete_ui;
mod save_actions_workflows;
mod save_pipeline;
#[cfg(test)]
mod save_pipeline_tests;
mod save_synchronization;
mod save_ui;
mod scheme_editor;
mod scientific_ui;
mod scope_ui;
#[cfg(test)]
mod scratch_todo_connected_tests;
mod scratches_ui;
mod search_everywhere_gesture;
#[cfg(test)]
mod search_everywhere_gesture_ownership_tests;
#[cfg(test)]
mod search_everywhere_gesture_tests;
mod search_navigation;
#[cfg(test)]
mod service_profile_accessibility_tests;
mod services_workspace;
mod session_ui;
mod settings_sync;
mod settings_ui;
mod shared_index_workspace;
mod shell_lifecycle;
mod signature_ui;
#[cfg(test)]
mod smart_completion_rendered_keyboard_tests;
#[cfg(test)]
mod smartkeys_tests;
mod sphinx_ui;
mod stack_trace_ui;
mod statement_completion;
mod status_chrome;
mod string_paste_prompt;
mod structure_ui;
#[cfg(test)]
mod stub_edit_connected_tests;
#[cfg(test)]
mod svg_preview_connected_tests;
mod svg_preview_ui;
mod template_catalog_ui;
#[cfg(test)]
mod terminal_accessibility_tests;
mod terminal_workspace;
mod test_frameworks;
mod testing_ui;
mod todo_settings_ui;
#[cfg(test)]
mod tool_tree_accessibility_tests;
mod tool_window_accessibility;
mod tool_window_lists;
mod usage_results_ui;
mod vcs_mapping_ui;
mod vcs_operations;
mod virtual_insertions;
#[cfg(test)]
mod virtual_space_connected_tests;
mod web_languages;
mod window_operations;
mod zen_mode;

mod ai_inline;
mod collaboration_ui;
mod detached_tab_drag;
mod editor_settings_workspace;
mod external_tool_sessions;
mod file_watchers;
mod help_workspace;
#[cfg(test)]
mod live_templates_connected_tests;
mod platform_notices;
mod remote_development;
#[cfg(test)]
mod source_inspection;
mod startup_restore;
mod structural_ui;
mod tab_policy;
mod tab_policy_settings;
mod tab_presentation;
#[cfg(test)]
mod tab_state_tests;
mod tab_strip;
mod tasks_ui;
mod tasks_workspace_ui;
mod textmate_bundles;
mod tool_stripes;
mod tool_window_contents;
mod tool_window_right_contents;
mod tool_window_tabs;
mod tool_windows;
mod tools_ui;
#[cfg(test)]
mod ui_snapshot_tests;
mod undo_history;
mod updates_ui;
mod vcs_gutter;
mod web_preview_ui;
pub mod window_lifecycle;
mod workspace_ui;

use find_bar::FindBar;
use local_history::{DiffDialog, LocalHistoryState, MarkdownMode};
use project_dialogs::{CloneState, FileBrowser, NewProjectState};

use crate::concurrency_panel::{ConcurrencyEvent, ConcurrencyPanel};
use crate::coverage_panel::{CoverageEvent, CoveragePanel};
use crate::db_panel::{DbEvent, DbPanel};
use crate::debug_panel::{DebugEvent, DebugPanel};
use crate::diagram_panel::{DiagramEvent, DiagramPanel};
use crate::git_panel::{GitEvent, GitPanel};
use crate::hierarchy_panel::{HierarchyEvent, HierarchyKind, HierarchyPanel};
use crate::http_panel::{HttpEvent, HttpPanel};
use crate::packages_panel::{PackagesEvent, PackagesPanel};
use crate::profiler_panel::{ProfilerEvent, ProfilerPanel};
use crate::run_panel::{RunPanel, RunPanelEvent};
use crate::search_ui::{EverywhereTab, FindInFiles, NavEvent, SearchEverywhere};
use crate::services_panel::{ProcessRow, ServicesEvent, ServicesInput, ServicesPanel};
use crate::terminal_panel::TerminalPanel;
use crate::test_panel::{TestPanel, TestPanelEvent};
use rustpy_debug::DebugSession;
use rustpy_highlight::ColorScheme;
use rustpy_semantic::hierarchy;
use rustpy_settings::{Settings, Workspace};
mod live_template_management;
use rustpy_templates::{self as templates, ExpandContext, LiveTemplate, TemplateContext};
use rustpy_text::Document;
use rustpy_vcs::GitRepo;

use crate::a11y::Accessible as _;
use crate::actions::{Action, KeyChord, Keymap};
use crate::editor::view::{self, EditorView, ViewConfig, ViewEvent};
use crate::editor::{EditOptions, EditorState, Language};
use crate::project_tree::{ProjectTree, TreeAction, TreeKey};
use crate::theme::{rgb, UiTheme};

pub struct Tab {
    pub state: EditorState,
    pub view: EditorView,
    pub pinned: bool,
    /// Editor group (0 = left/top, 1 = right/bottom) when the editor is split.
    pub group: u8,
    /// Preview tab (single-click in the project view): replaced by the next preview.
    pub preview: bool,
    /// Non-text viewer: decoded image bytes (texture created lazily) and its texture.
    pub image: Option<ImageTab>,
    pub svg: Option<crate::editor::svg_preview::SvgPane>,
    /// Jupyter notebook editor (the document holds the `.ipynb` JSON).
    pub notebook: Option<Box<crate::notebook_view::NotebookTab>>,
}

impl Tab {
    /// A notebook can have pending model edits before its JSON document syncs.
    fn has_unsaved_changes(&self) -> bool {
        self.state.doc.is_modified()
            || self
                .notebook
                .as_ref()
                .is_some_and(|notebook| notebook.is_dirty())
    }
}

#[derive(Clone)]
pub struct ImageTab {
    pub rgba: Vec<u8>,
    pub size: [usize; 2],
    pub texture: Option<egui::TextureHandle>,
    pub zoom: f32,
    pub checkerboard: bool,
    pub pixel_grid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SplitDir {
    /// Groups side by side ("Split Right").
    Horizontal,
    /// Groups stacked ("Split Down").
    Vertical,
}

pub struct Project {
    pub root: PathBuf,
    pub tree: ProjectTree,
    pub analysis: Arc<Analysis>,
    pub run_configs: RunConfigStore,
    pub interpreters: Vec<Interpreter>,
    pub interpreter: Option<Interpreter>,
    interpreter_rx: Option<crossbeam_channel::Receiver<Vec<Interpreter>>>,
    pub git: Option<GitRepo>,
    /// Last VCS status refresh time and the branch name shown in the status bar.
    pub git_refreshed: f64,
    pub git_branch: Option<String>,
}

/// Cyclic completion stays attached to its exact editor, caret and edit.
struct HippieCompletionState {
    view: egui::Id,
    caret: TextSize,
    start: TextSize,
    candidates: Vec<String>,
    next: usize,
    version: u64,
}

/// Code completion popup state.
#[derive(Default)]
struct CompletionPopup {
    visible: bool,
    items: Vec<CompletionItem>,
    /// Parallel to `items`: auto-import `(module, name)` for second-press items.
    imports: Vec<Option<(String, String)>>,
    selected: usize,
    /// Start offset of the identifier being completed.
    prefix_start: TextSize,
    doc_version: u64,
    /// Caret when the lookup was (re)computed: moving it without typing closes the lookup.
    caret: TextSize,
    explicit: bool,
    /// Where the lookup was drawn last frame (a click in it must not close it).
    rect: Option<Rect>,
}

/// `(path, document version, per-line annotations)` for the Annotate gutter.
type BlameCache = (PathBuf, u64, Vec<(String, u8, String)>);

/// Active live-template editing session (tab stops).
struct TemplateSession {
    tab_id: egui::Id,
    /// Variables in tab order with every occurrence range (absolute).
    vars: Vec<(String, Vec<TextRange>)>,
    idx: usize,
    end: TextSize,
    version: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChoiceKind {
    Surround,
    LiveTemplate,
    PasteHistory,
    CopyPath,
    Bookmarks,
    Generate,
    /// Choose the expression to extract.
    Extract(ExtractKind),
    /// Replace this occurrence only / all occurrences of the expression at the range.
    ExtractOccurrences(ExtractKind, TextRange),
    RefactorThis,
    /// Choose among several declarations / implementations.
    Locations,
    SelectIn,
    /// Show Usages, Go to Test, Go to Type Declaration (rows in `navigation::NavState`).
    Nav,
    QuickList,
    SwitchTask,
    LoadContext,
    /// Window › Layouts › Restore Layout… / Delete Layout… (rows are layout names).
    RestoreLayout,
    DeleteLayout,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MemberRefactoring {
    PullUp,
    PushDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExtractKind {
    Variable,
    Constant,
    Field,
    Parameter,
}

impl ChoiceKind {
    fn centered(self) -> bool {
        matches!(
            self,
            ChoiceKind::Bookmarks
                | ChoiceKind::PasteHistory
                | ChoiceKind::SwitchTask
                | ChoiceKind::LoadContext
                | ChoiceKind::RestoreLayout
                | ChoiceKind::DeleteLayout
                | ChoiceKind::QuickList
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Bookmark {
    path: PathBuf,
    /// 1-based line.
    line: usize,
    mnemonic: Option<char>,
    description: String,
    kind: rustpy_settings::BookmarkKind,
    list: String,
}

/// Generic list popup at the caret or centred (Surround With, Insert Live
/// Template, Bookmarks, …): see `choice_popup`.
#[derive(Clone)]
struct ChoicePopup {
    kind: ChoiceKind,
    title: String,
    /// `(key, description)` rows.
    items: Vec<(String, String)>,
    /// Index into `items`.
    selected: usize,
    /// Speed search typed while the popup is open (hides rows it does not match).
    filter: String,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum BottomTool {
    WebPreview,
    Terminal,
    Run,
    Debug,
    Tests,
    Packages,
    Hierarchy,
    Services,
    Coverage,
    Profiler,
    Concurrency,
    Branches,
    PullRequests,
    HttpClient,
    Database,
    Diagram,
    Endpoints,
    Problems,
    Todo,
    PythonConsole,
    SciView,
    Git,
    OtherVcs,
    Find,
    Notifications,
    Documentation,
    ManagePy,
}

impl BottomTool {
    /// The tool window title in the UI language.
    fn title(self) -> &'static str {
        crate::i18n::tr_static(self.stable_name())
    }
    fn stable_name(self) -> &'static str {
        match self {
            BottomTool::WebPreview => "Web Preview",
            BottomTool::Documentation => "Documentation",
            BottomTool::Notifications => "Notifications",
            BottomTool::Terminal => "Terminal",
            BottomTool::Run => "Run",
            BottomTool::Problems => "Problems",
            BottomTool::Todo => "TODO",
            BottomTool::PythonConsole => "Python Console",
            BottomTool::SciView => "Plots / SciView",
            BottomTool::Git => "Git",
            BottomTool::OtherVcs => "Mercurial / SVN / Perforce",
            BottomTool::Find => "Find",
            BottomTool::Debug => "Debug",
            BottomTool::Tests => "Tests",
            BottomTool::Packages => "Python Packages",
            BottomTool::Hierarchy => "Hierarchy",
            BottomTool::Services => "Services",
            BottomTool::Coverage => "Coverage",
            BottomTool::Profiler => "Profiler",
            BottomTool::Concurrency => "Concurrency Diagram",
            BottomTool::Branches => "Branches",
            BottomTool::PullRequests => "Pull Requests",
            BottomTool::HttpClient => "HTTP Client",
            BottomTool::Database => "Database",
            BottomTool::Diagram => "Diagram",
            BottomTool::Endpoints => "Endpoints",
            BottomTool::ManagePy => "manage.py",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum LeftTool {
    Project,
    Structure,
    Bookmarks,
}

enum Dialog {
    Frameworks,
    WebLanguages,
    OpenWebUrl,
    DataFiles,
    GitHubModal,
    None,
    GoToLine(String),
    MakeWritable {
        document: rustpy_text::DocumentId,
        request: file_permissions::WritableRequest,
    },
    FileStructure {
        query: String,
        selected: usize,
    },
    /// Refactor › Rename… (when the rename cannot be done in place).
    Rename(Box<rename_ui::RenameDialogState>),
    Inline(Box<inline_ui::InlineState>),
    Conversion(Box<conversion_ui::ConversionState>),
    /// Refactoring Preview of a rename.
    RenamePreview(Box<rename_ui::RenamePreviewState>),
    FrameworkRename(Box<rename_framework_ui::FrameworkRenameState>),
    RefactoringPreview(Box<refactoring_infrastructure::PreviewState>),
    UndoWorkspaceRefactoring(usize),
    EditorConfig(Box<editorconfig_ui::PreviewState>),
    EvaluateExpression(String),
    Breakpoints(Box<crate::breakpoints::BreakpointsDialog>),
    /// The popup of one breakpoint (right-click it, or Ctrl+Shift+F8 on its line).
    EditBreakpoint(Box<crate::breakpoints::BreakpointPopup>),
    QuickDefinition {
        title: String,
        path: PathBuf,
        line: usize,
        text: String,
    },
    Extract(Box<extract_ui::ExtractState>),
    ChangeSignature(Box<signature_ui::ChangeSignatureState>),
    /// Safe Delete found usages: "Delete Anyway" or cancel.
    SafeDelete(Box<safe_delete_ui::SafeDeleteState>),
    BookmarkDescription {
        index: usize,
        text: String,
    },
    RunConfigurations,
    InterpreterPicker,
    Branches,
    UpdateProject(git_workflows::UpdateDialog),
    /// Git › Push….
    Push(Box<push_dialog::PushState>),
    /// Git › Manage Remotes….
    Remotes(Box<remotes_dialog::RemotesState>),
    ConfirmClose {
        tab: usize,
        then_exit: bool,
    },
    /// New file with a template kind in a directory.
    NewFileKind {
        dir: PathBuf,
        kind: &'static str,
        name: String,
    },
    RenamePath {
        path: PathBuf,
        name: String,
    },
    KeymapReference,
    HelpTopics,
    ExternalChange(PathBuf),
    EncodingReload(encoding_ui::EncodingReloadRequest),
    TrustProject(PathBuf),
    /// Captured member selection and physical destinations.
    Members(Box<member_refactoring_ui::MemberState>),
    Reformat(Box<reformat_workflows::ReformatState>),
    Move(Box<move_ui::MoveState>),
    /// Very large clipboard paste awaiting confirmation.
    ConfirmPaste(String),
    StringPaste(string_paste_prompt::Prompt),
    OpenTask(Box<tasks_ui::OpenTaskState>),
    /// File › Manage IDE Settings: 0 = export, 1 = import, 2 = restore defaults.
    ManageSettings {
        mode: u8,
        path: String,
        result: Option<Result<String, String>>,
    },
    SaveContext(String),
    /// Window › Layouts › Save Current Layout As… (the name being typed).
    SaveLayout(String),
    /// SciView data table.
    DataView {
        title: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        total: usize,
        offset: usize,
        filter: String,
    },
    WslDistributions {
        root: PathBuf,
        python: String,
        job: rustpy_project::interpreter_remote::WslRegistrationJob,
    },
    InterpreterProbe {
        root: PathBuf,
        job: rustpy_project::interpreter_remote::DiscoveryJob,
    },
    RemoteInterpreter {
        kind: String,
        target: String,
        python: String,
        /// Remote directory where the project is visible (path mapping).
        remote_root: String,
        error: Option<String>,
    },
    RenameModule {
        path: PathBuf,
        name: String,
    },
    CheckRegex {
        pattern: String,
        flags: String,
        sample: String,
        result: Option<rustpy_regex::MatchResult>,
        error: Option<String>,
        rx: Option<crossbeam_channel::Receiver<anyhow::Result<rustpy_regex::MatchResult>>>,
    },
    Macros {
        selected: usize,
        new_name: String,
    },
    Structural(Box<structural_ui::StructuralState>),
    TipOfTheDay(usize),
    About,
    Settings,
    OpenPath(Box<FileBrowser>),
    NewProject(Box<NewProjectState>),
    CloneRepo(Box<CloneState>),
    /// "This Window / New Window" or "discard unsaved changes" before a project opens.
    OpenProject(Box<project_open::OpenProjectState>),
    WorkspacePersistence(workspace_ui::WorkspaceTransition),
    /// A message with OK; `then` is the dialog it interrupted.
    Notice {
        title: String,
        text: String,
        then: Option<Box<Dialog>>,
    },
    /// Quick Switch Scheme (Ctrl+`): theme and editor color scheme.
    QuickSwitch(Box<settings_ui::QuickSwitchState>),
    LocalHistory(Box<LocalHistoryState>),
    UndoHistory(undo_history::UndoHistoryState),
    ConfirmRefactoringUndo(refactoring_preferences::UndoRequest),
    /// Local History of a folder, including deleted files.
    FolderHistory(Box<local_history::FolderHistoryState>),
    Diff(Box<DiffDialog>),
    ResolveConflict {
        path: PathBuf,
        ours: String,
        theirs: String,
        base: String,
        diff: Box<crate::diff_view::SideBySide>,
    },
    CompareBranch {
        branch: String,
        diffs: Vec<rustpy_vcs::FileDiff>,
        selected: usize,
    },
    CompareFilePick(Box<(FileBrowser, file_comparisons::ComparisonSource)>),
    DirectoryComparePick(Box<(FileBrowser, PathBuf)>),
    DirectoryComparison,
    /// External Tools: the value of `$Prompt$` before the tool runs.
    ToolPrompt(Box<tools_ui::ToolPromptState>),
    /// New › <file template>: the name of the new file.
    NewFromTemplate(Box<file_templates_ui::NewFromTemplateState>),
    /// New Scratch File language picker: (filter, selected row).
    NewScratch(String, usize),
    /// Generate .gitignore from bundled templates.
    GitignoreTemplates(Box<gitignore_templates_ui::GitignoreTemplatesState>),
}

struct StartupTerminalDispatch {
    cwd: PathBuf,
    env: Vec<(String, String)>,
    command: String,
}

pub struct IdeApp {
    native_ui: native_ui::State,
    native_project_binding: Option<crate::launch::native_project_windows::Binding>,
    project_windows: project_windows::State,
    settings: Settings,
    theme: UiTheme,
    scheme: ColorScheme,
    keymap: Keymap,
    keymap_workspace: keymap_workspace::KeymapWorkspace,
    project: Option<Project>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    closed_tabs: Vec<tab_strip::ClosedTab>,
    detached_tab_drag: detached_tab_drag::DetachedTabDrag,
    tab_policy: tab_policy::TabPolicy,
    zoom: f32,
    find: FindBar,
    dialog: Dialog,
    bottom: Option<BottomTool>,
    left_tool: LeftTool,
    completion: CompletionPopup,
    completion_ui: completion_ui::CompletionUiState,
    basic_completion_ui: basic_completion_ui::BasicCompletionUi,
    import_completion: Option<index_ui::ImportCompletionJob>,
    completion_worker: completion_worker::CompletionWorker,
    import_workflows: import_workflows::State,
    documentation: quick_documentation_ui::DocumentationUi,
    scientific: scientific_ui::State,
    shared_index_download: shared_index_workspace::State,
    emmet_ui: emmet_ui::EmmetUi,
    editor_accessibility: editor_accessibility_ui::EditorAccessibility,
    intentions_accessibility: intentions_accessibility::IntentionsAccessibility,
    accessibility_workspace: accessibility_workspace::AccessibilityWorkspace,
    menu_customization: menu_customization::MenuCustomization,
    quick_doc: Option<HoverInfo>,
    run_panel: RunPanel,
    terminal_panel: TerminalPanel,
    terminal_workspace: terminal_workspace::TerminalWorkspace,
    git_panel: GitPanel,
    vcs_workspace: vcs_mapping_ui::VcsWorkspace,
    other_vcs_workspace: other_vcs_ui::OtherVcsWorkspace,
    github: github_ui::State,
    py_console: crate::console_workspace::ConsoleWorkspace,
    plugin_runtime: plugin_runtime::State,
    find_in_files: FindInFiles,
    everywhere: SearchEverywhere,
    search_everywhere_gesture: search_everywhere_gesture::SearchEverywhereGesture,
    /// Alt+Enter popup: fixes for the caret position.
    injection_ui: language_injections_ui::InjectionUi,
    injected_debug_sources: Vec<language_injections_ui::DebugSource>,
    intention_menu: intention_actions::IntentionMenu,
    intentions: Option<(Vec<QuickFix>, usize)>,
    /// Editor (view id), document version and caret the open intentions were computed for.
    intentions_at: Option<(egui::Id, u64, TextSize)>,
    /// Ctrl+F1 problem description popup.
    error_description: error_description_ui::ErrorDescriptionUi,
    /// Ctrl+P parameter info popup.
    param_info: Option<SignatureHelp>,
    parameter_info_ui: parameter_info_ui::ParameterInfoUi,
    /// Editor, document version and caret the parameter info was computed for.
    param_info_at: Option<(egui::Id, u64, TextSize)>,
    /// Editor, document version and caret Quick Documentation was opened at:
    /// typing, moving the caret or switching editors closes the popup.
    quick_doc_at: Option<(egui::Id, u64, TextSize)>,
    /// Find Usages results shown in the Find tool window.
    usages: Vec<Usage>,
    usages_title: String,
    file_results: file_results_ui::FileResults,
    usage_ui: usage_results_ui::UsageUi,
    /// Line and exception breakpoints.
    breakpoints: crate::breakpoints::BreakpointStore,
    debug: Option<DebugSession>,
    debug_sessions: Vec<(DebugSession, DebugPanel)>,
    debug_breakpoints_snapshot: crate::breakpoints::BreakpointStore,
    debug_panel: DebugPanel,
    test_panel: TestPanel,
    testing_ui: testing_ui::State,
    packages_panel: PackagesPanel,
    hierarchy_panel: HierarchyPanel,
    services_panel: ServicesPanel,
    coverage_panel: CoveragePanel,
    profiler_panel: ProfilerPanel,
    concurrency_panel: ConcurrencyPanel,
    http_panel: HttpPanel,
    db_panel: DbPanel,
    diagram_panel: DiagramPanel,
    split: Option<SplitDir>,
    split_fraction: f32,
    editor_layout_generation: u64,
    editor_layout_seed_pending: bool,
    detached_bounds: Option<rustpy_settings::LogicalScreenBounds>,
    detached_bounds_pending: bool,
    settings_ui: settings_ui::SettingsUi,
    dialog_workspace: dialog_workspace::State,
    shortcuts_applied: bool,
    /// Remembered active tab (view id) per editor group.
    group_active: [Option<egui::Id>; 2],
    /// `(path, 1-based line)` of the current execution point.
    exec_point: Option<(PathBuf, usize)>,
    exec_point_column: Option<usize>,
    debug_was_stopped: bool,
    templates: Vec<LiveTemplate>,
    live_template_management: live_template_management::LiveTemplateManagement,
    template_session: Option<TemplateSession>,
    choice: Option<ChoicePopup>,
    clipboard_history: Vec<String>,
    clipboard_payload: Option<crate::editor::commands::clipboard_payload::ClipboardPayload>,
    emacs_editing: crate::editor::emacs::EmacsEditing,
    emacs_owner: Option<(rustpy_text::DocumentId, egui::Id)>,
    emacs_frame_interrupted: bool,
    pending_copy: Option<String>,
    recent_files: Vec<PathBuf>,
    bookmarks: Vec<Bookmark>,
    bookmark_lists: bookmark_lists_ui::BookmarkListsUi,
    /// Per-document anchors for breakpoints, bookmarks and sticky highlights.
    document_markers: document_markers::MarkerState,
    /// Alt+/ hippie completion: (prefix start, candidates, next index, doc version).
    generate_methods: generate_methods_ui::GenerateMethodsUi,
    hippie: Option<HippieCompletionState>,
    context_info: Option<(String, f64)>,
    /// Expression candidates for the extract popup.
    extract_candidates: Vec<TextRange>,
    location_choices: Vec<features::Location>,
    structure_ui: structure_ui::State,
    web_languages: web_languages::State,
    data_files: data_files_ui::State,
    frameworks: frameworks_ui::State,
    manage_py: manage_py_console::State,
    pending_select_in: Option<usize>,
    /// Dialog to open after the current one closes (set from inside a dialog closure).
    pending_dialog: Option<Dialog>,
    /// File-system watcher for the project root.
    fs_watcher: Option<notify::RecommendedWatcher>,
    fs_events: Option<crossbeam_channel::Receiver<notify::Result<notify::Event>>>,
    fs_dirty_since: Option<f64>,
    /// Paths changed on disk that still need a decision (modified in the editor too).
    external_changes: Vec<PathBuf>,
    /// Raw byte revisions of open files as last loaded, saved or acknowledged.
    disk_revisions: std::collections::HashMap<PathBuf, Option<rustpy_core::vfs::ContentRevision>>,
    file_permissions:
        std::collections::HashMap<rustpy_text::DocumentId, file_permissions::FilePermissionState>,
    permission_viewers: std::collections::HashSet<rustpy_text::DocumentId>,
    csv_routes: std::collections::HashMap<egui::Id, csv_table_ui::CsvUi>,
    svg_jobs: crate::editor::svg_preview::SvgJobs,
    directory_comparison: Option<Box<directory_diff::DirectoryDiff>>,
    native_skeleton_ui: native_skeleton_ui::NativeSkeletonUi,
    interpreter_management: interpreter_management::State,
    run_workflows: run_configuration_workflows::State,
    measured_sessions: run_configuration_sessions::State,
    run_console_state: run_console::State,
    local_history_runtime: local_history_workflows::Runtime,
    directory_diff_domain: rustpy_core::CancellationToken,
    directory_diff_draining: Vec<directory_diff::DirectoryJob>,
    directory_comparison_suspended: bool,
    permissions_window_focused: bool,
    save_synchronization: save_synchronization::SaveSynchronization,
    observed_revisions:
        std::collections::HashMap<PathBuf, Option<rustpy_core::vfs::ContentRevision>>,
    vfs: Arc<rustpy_core::vfs::Vfs>,
    fs_overflow: Arc<std::sync::atomic::AtomicBool>,
    file_session: u64,
    /// Explicit per-file decoding choices, including unknown keys for round-trip compatibility.
    encoding_overrides: std::collections::HashMap<PathBuf, String>,
    encoding_generation: u64,
    workspace_client: Option<workspace_ui::WorkspaceClient>,
    coordinated_quit: Option<window_lifecycle::WindowQuit>,
    coordinated_quit_committed: bool,
    /// Image of the last ordinary frame, painted behind the Quit prompt.
    quit_backdrop: Option<egui::TextureHandle>,
    quit_backdrop_wanted: bool,
    /// File > Exit waits for this frame to pass so the frame after it, still
    /// ordinary, is captured as the Quit backdrop.
    shell_quit_after_frame: Option<u64>,
    shell_managed_window: bool,
    shell_global_quit_requested: bool,
    shell_defer_committed_close: bool,
    external_watches: std::collections::HashSet<PathBuf>,
    /// Macro recording: steps collected since Start.
    macro_recording: Option<Vec<String>>,
    window_title: String,
    /// Source file for the Copy File dialog.
    copy_source: Option<PathBuf>,
    /// Tab shown in a separate OS window (Window → Detach Tab).
    detached: Option<usize>,
    /// Safe mode: the project is not trusted, so nothing is executed.
    safe_mode: bool,
    project_trust_identity: Option<rustpy_settings::trust::DirectoryIdentity>,
    trust_probe: project_open::TrustProbe,
    /// Scripted input for headless smoke tests.
    pub input_player: Option<crate::input_script::InputPlayer>,
    clipboard_shortcuts: clipboard_shortcuts::ClipboardShortcuts,
    /// Help › Learn IDE Features.
    learn: crate::learn_panel::LearnPanel,
    /// Help › Check for Updates…, the daily update check and What's New.
    updates: updates_ui::UpdatesUi,
    help_workspace: help_workspace::State,
    platform_notices: platform_notices::State,
    settings_sync: settings_sync::State,
    remote_development: remote_development::State,
    endpoints_panel: crate::endpoints_panel::EndpointsPanel,
    deploy: crate::deploy_ui::DeployUi,
    ai_panel: crate::ai_panel::AiPanel,
    collaboration: collaboration_ui::State,
    ai_inline: ai_inline::State,
    rt_checked: f64,
    rt_mtime: Option<std::time::SystemTime>,
    rt_analysis: usize,
    /// Interpreter docstrings and the External Documentation link.
    docs: code_insight::DocsState,
    /// Last time a notebook model was written into its document.
    notebook_synced: f64,
    /// Cached step definitions: (time, definitions).
    step_defs_cache: Option<(f64, Vec<rustpy_gherkin::StepDef>)>,
    /// Active in-place rename (Shift+F6 in place, or a refactoring's new name).
    inplace_rename: Option<inplace_rename::InplaceRename>,
    refactoring_groups: Vec<refactoring_infrastructure::UndoGroup>,
    /// How often each completion was accepted (ranks the popup).
    completion_stats: std::collections::HashMap<String, u32>,
    /// Messages from background jobs (shown in the status bar when they finish).
    background_status: Vec<std::sync::mpsc::Receiver<String>>,
    /// Persistent symbol indexes of the project and its interpreter's libraries.
    symbol_indexes: crate::symbol_index::SymbolIndexes,
    /// Actions refused or queued while the indexes are built (dumb mode).
    dumb_mode: crate::dumb_mode::DumbMode,
    learn_seen_version: Option<u64>,
    /// Tasks & Contexts of the open project (loaded lazily).
    tasks: Option<rustpy_settings::tasks::TaskStore>,
    task_workspace: tasks_workspace_ui::Workspace,
    task_choice_ids: Vec<String>,
    task_time_acc: f64,
    last_task_tick: f64,
    last_user_input: f64,
    /// Bottom tool window shown in its own OS window.
    floating_bottom: bool,
    bottom_maximized: bool,
    watcher_runtime: file_watchers::Runtime,
    web_preview: web_preview_ui::Workspace,
    pending_tool: Option<rustpy_settings::ExternalTool>,
    external_tool_sessions: external_tool_sessions::Workspace,
    startup_tasks_done: bool,
    macro_playing: bool,
    /// Blame annotations for the active tab: `(path, doc version, lines)`.
    blame: Option<BlameCache>,
    annotate_enabled: bool,
    /// Action to run on the next frame (from popups that cannot call `handle_action`).
    pending_action: Option<Action>,
    project_new_generation: u64,
    markdown_mode: MarkdownMode,
    markdown_workspace: markdown_ui::Workspace,
    /// New Project setup in progress (environment, requirements, Django project).
    pending_venv: Option<Box<project_setup::ProjectSetup>>,
    tool_lists: tool_window_lists::State,
    todo_items: project_index::TodoList,
    todo_colors: Vec<[u8; 3]>,
    todo_refreshed: f64,
    /// Gutter change markers for the active tab: (path, doc version, changes).
    line_changes: Option<(PathBuf, u64, Vec<rustpy_vcs::LineChange>)>,
    next_session_id: u64,
    /// Navigation history, Recent Files / Switcher / Recent Locations popups, Find window results.
    nav: navigation::NavState,
    scope_ui: scope_ui::ScopeUi,
    project_navigation: project_navigation_ui::ProjectNavigationUi,
    project_view: project_view_features::ProjectViewUi,
    last_edit_time: f64,
    /// When the active document editor's framework inputs were last refreshed.
    document_inputs_time: f64,
    status: Option<(String, f64)>,
    focus_editor: bool,
    next_tab_id: u64,
    exit_requested: bool,
    /// Headless smoke test: capture a screenshot to this path after a few frames, then exit.
    pub screenshot_path: Option<PathBuf>,
    /// Smoke test: run the opened file on startup.
    pub run_on_start: bool,
    /// Smoke test: open the terminal and type this command.
    pub terminal_on_start: Option<String>,
    startup_terminal_session: Option<u64>,
    /// Smoke test: open this bottom tool window on start ("git", "run", …).
    pub tool_on_start: Option<String>,
    /// Smoke test: move the caret to `line:col` (1-based) on start.
    pub goto_on_start: Option<(usize, usize)>,
    /// Smoke test: run these action ids (in order, one per frame) on start.
    pub actions_on_start: Vec<String>,
    /// Wall-clock time of the first frame (smoke-test scheduling).
    start_time: Option<f64>,
    last_smoke_action: f64,
    screenshot_requested: bool,
    frame_count: u64,
    /// Background tasks, code analysis scheduling, Power Save Mode, freeze detector.
    bg: background::BackgroundState,
    diagnostic_export: diagnostics::DiagnosticExport,
    diagnostics_workspace: diagnostics_workspace::State,
    debug_log_settings: debug_log_ui::State,
    /// Balloons and the Notifications tool window.
    notifications: crate::notifications::NotificationCenter,
    /// Tool-window layout, view modes and the exit question.
    layout: layout::LayoutState,
    /// Project file index (excluded folders, content roots) and background scans.
    project_index: project_index::ProjectIndex,
    code_vision: inlay_code_vision_ui::Runtime,
    gutter_actions: gutter_actions_ui::Runtime,
    stack_trace_state: stack_trace_ui::State,
    diff_workspace: diff_workspace::Workspace,
}

impl IdeApp {
    pub fn new(settings: Settings, open: Option<PathBuf>) -> Self {
        Self::new_with_startup(settings, open, true)
    }

    /// Construct a Welcome window without opening a recent project or cwd.
    pub(crate) fn new_welcome(settings: Settings) -> Self {
        Self::new_with_startup(settings, None, false)
    }

    fn new_with_startup(settings: Settings, open: Option<PathBuf>, resolve_startup: bool) -> Self {
        let theme = UiTheme::for_name(&settings.appearance.theme);
        let scheme = settings_ui::scheme_by_name(&settings.editor.color_scheme)
            .unwrap_or_else(ColorScheme::dark);
        let keymap = Keymap::preset(&settings.keymap);
        debug_log_ui::apply_saved(&settings);
        let mut documentation = quick_documentation_ui::DocumentationUi::default();
        documentation.font_size = f32::from(settings.documentation.font_size.clamp(10, 24));
        documentation.auto_update = settings.documentation.auto_update;
        let mut app = Self {
            native_ui: Default::default(),
            native_project_binding: None,
            project_windows: Default::default(),
            settings,
            theme,
            scheme,
            keymap,
            keymap_workspace: Default::default(),
            project: None,
            tabs: Vec::new(),
            active: None,
            closed_tabs: Vec::new(),
            detached_tab_drag: Default::default(),
            tab_policy: Default::default(),
            zoom: 0.0,
            find: FindBar::default(),
            dialog: Dialog::None,
            bottom: None,
            left_tool: LeftTool::Project,
            completion: CompletionPopup::default(),
            completion_ui: Default::default(),
            basic_completion_ui: Default::default(),
            import_completion: None,
            completion_worker: Default::default(),
            import_workflows: Default::default(),
            documentation,
            scientific: Default::default(),
            shared_index_download: Default::default(),
            emmet_ui: Default::default(),
            editor_accessibility: Default::default(),
            intentions_accessibility: Default::default(),
            accessibility_workspace: Default::default(),
            menu_customization: Default::default(),
            quick_doc: None,
            run_panel: RunPanel::default(),
            terminal_panel: TerminalPanel::default(),
            terminal_workspace: Default::default(),
            git_panel: GitPanel::default(),
            vcs_workspace: Default::default(),
            other_vcs_workspace: Default::default(),
            github: github_ui::State::default(),
            py_console: crate::console_workspace::ConsoleWorkspace::default(),
            plugin_runtime: plugin_runtime::State::default(),
            find_in_files: FindInFiles::default(),
            everywhere: SearchEverywhere::default(),
            search_everywhere_gesture: Default::default(),
            injection_ui: Default::default(),
            injected_debug_sources: Vec::new(),
            intention_menu: Default::default(),
            intentions: None,
            intentions_at: None,
            error_description: Default::default(),
            param_info: None,
            parameter_info_ui: Default::default(),
            param_info_at: None,
            quick_doc_at: None,
            usages: Vec::new(),
            usages_title: String::new(),
            file_results: Default::default(),
            usage_ui: Default::default(),
            breakpoints: Default::default(),
            debug: None,
            debug_sessions: Vec::new(),
            debug_breakpoints_snapshot: Default::default(),
            debug_panel: DebugPanel::default(),
            test_panel: TestPanel::default(),
            testing_ui: testing_ui::State::default(),
            packages_panel: PackagesPanel::default(),
            hierarchy_panel: HierarchyPanel::default(),
            services_panel: ServicesPanel::default(),
            coverage_panel: CoveragePanel::default(),
            profiler_panel: ProfilerPanel::default(),
            concurrency_panel: ConcurrencyPanel::default(),
            http_panel: HttpPanel::default(),
            db_panel: DbPanel::default(),
            diagram_panel: DiagramPanel::default(),
            split: None,
            split_fraction: 0.5,
            editor_layout_generation: 0,
            editor_layout_seed_pending: true,
            detached_bounds: None,
            detached_bounds_pending: false,
            group_active: [None, None],
            settings_ui: settings_ui::SettingsUi::default(),
            dialog_workspace: Default::default(),
            shortcuts_applied: false,
            exec_point: None,
            exec_point_column: None,
            debug_was_stopped: false,
            templates: load_templates(),
            live_template_management: live_template_management::LiveTemplateManagement::load(),
            template_session: None,
            choice: None,
            clipboard_history: Vec::new(),
            clipboard_payload: None,
            emacs_editing: Default::default(),
            emacs_owner: None,
            emacs_frame_interrupted: false,
            pending_copy: None,
            recent_files: Vec::new(),
            bookmarks: Vec::new(),
            bookmark_lists: Default::default(),
            document_markers: Default::default(),
            generate_methods: Default::default(),
            hippie: None,
            context_info: None,
            extract_candidates: Vec::new(),
            location_choices: Vec::new(),
            structure_ui: structure_ui::State::default(),
            web_languages: Default::default(),
            data_files: Default::default(),
            frameworks: Default::default(),
            manage_py: Default::default(),
            pending_select_in: None,
            pending_dialog: None,
            fs_watcher: None,
            fs_events: None,
            fs_dirty_since: None,
            external_changes: Vec::new(),
            disk_revisions: Default::default(),
            file_permissions: Default::default(),
            permission_viewers: Default::default(),
            csv_routes: Default::default(),
            svg_jobs: crate::editor::svg_preview::SvgJobs::bundled(),
            directory_comparison: None,
            native_skeleton_ui: native_skeleton_ui::NativeSkeletonUi::default(),
            interpreter_management: interpreter_management::State::default(),
            run_workflows: run_configuration_workflows::State::default(),
            measured_sessions: run_configuration_sessions::State::default(),
            run_console_state: run_console::State::default(),
            local_history_runtime: local_history_workflows::Runtime::default(),
            directory_diff_domain: rustpy_core::CancellationToken::new(),
            directory_diff_draining: Vec::new(),
            directory_comparison_suspended: false,
            permissions_window_focused: false,
            save_synchronization: Default::default(),
            observed_revisions: Default::default(),
            vfs: Arc::new(rustpy_core::vfs::Vfs::new()),
            fs_overflow: Default::default(),
            file_session: 0,
            encoding_overrides: Default::default(),
            encoding_generation: 0,
            workspace_client: None,
            coordinated_quit: None,
            coordinated_quit_committed: false,
            quit_backdrop: None,
            quit_backdrop_wanted: false,
            shell_quit_after_frame: None,
            shell_managed_window: false,
            shell_global_quit_requested: false,
            shell_defer_committed_close: false,
            external_watches: Default::default(),
            macro_recording: None,
            macro_playing: false,
            window_title: String::new(),
            copy_source: None,
            detached: None,
            safe_mode: false,
            project_trust_identity: None,
            trust_probe: Default::default(),
            floating_bottom: false,
            bottom_maximized: false,
            watcher_runtime: file_watchers::Runtime::default(),
            web_preview: web_preview_ui::Workspace::default(),
            pending_tool: None,
            external_tool_sessions: Default::default(),
            startup_tasks_done: false,
            input_player: None,
            clipboard_shortcuts: Default::default(),
            deploy: Default::default(),
            ai_panel: Default::default(),
            collaboration: Default::default(),
            ai_inline: Default::default(),
            endpoints_panel: Default::default(),
            background_status: Vec::new(),
            step_defs_cache: None,
            notebook_synced: 0.0,
            inplace_rename: None,
            refactoring_groups: Vec::new(),
            completion_stats: load_completion_stats(),
            rt_checked: -10.0,
            rt_mtime: None,
            rt_analysis: 0,
            learn: Default::default(),
            learn_seen_version: None,
            tasks: None,
            task_workspace: tasks_workspace_ui::Workspace::default(),
            task_choice_ids: Vec::new(),
            task_time_acc: 0.0,
            last_task_tick: 0.0,
            last_user_input: 0.0,
            blame: None,
            annotate_enabled: false,
            pending_action: None,
            project_new_generation: u64::MAX,
            pending_venv: None,
            markdown_mode: MarkdownMode::default(),
            markdown_workspace: markdown_ui::Workspace::default(),
            tool_lists: Default::default(),
            todo_items: Vec::new(),
            todo_colors: Vec::new(),
            todo_refreshed: -100.0,
            line_changes: None,
            next_session_id: 1,
            last_edit_time: 0.0,
            document_inputs_time: f64::NEG_INFINITY,
            status: None,
            focus_editor: true,
            next_tab_id: 1,
            exit_requested: false,
            screenshot_path: None,
            run_on_start: false,
            terminal_on_start: None,
            startup_terminal_session: None,
            tool_on_start: None,
            goto_on_start: None,
            actions_on_start: Vec::new(),
            start_time: None,
            last_smoke_action: 0.0,
            screenshot_requested: false,
            frame_count: 0,
            nav: Default::default(),
            scope_ui: Default::default(),
            project_navigation: Default::default(),
            project_view: Default::default(),
            symbol_indexes: Default::default(),
            dumb_mode: Default::default(),
            updates: Default::default(),
            help_workspace: Default::default(),
            platform_notices: Default::default(),
            settings_sync: Default::default(),
            remote_development: Default::default(),
            docs: Default::default(),
            bg: Default::default(),
            diagnostic_export: Default::default(),
            diagnostics_workspace: Default::default(),
            debug_log_settings: Default::default(),
            notifications: Default::default(),
            layout: Default::default(),
            project_index: Default::default(),
            code_vision: Default::default(),
            gutter_actions: Default::default(),
            stack_trace_state: Default::default(),
            diff_workspace: Default::default(),
        };
        if resolve_startup {
            match open {
                Some(p) if p.is_dir() => app.open_project(&p),
                Some(p) if p.is_file() => {
                    let root = p
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("."));
                    app.open_project(&root);
                    app.open_file(&p);
                }
                _ => {
                    let last = app.settings.recent_projects.first().cloned();
                    match last {
                        Some(p) if app.settings.reopen_last_project && p.is_dir() => {
                            app.open_project(&p)
                        }
                        _ => {
                            if let Some(cwd) = std::env::current_dir()
                                .ok()
                                .filter(|d| project_dialogs::looks_like_project(d))
                            {
                                app.open_project(&cwd);
                            }
                        }
                    }
                }
            }
        }
        app
    }

    // ----- project & files ---------------------------------------------------

    pub fn open_project(&mut self, root: &Path) {
        self.open_project_prepared(root, None);
    }

    fn open_project_prepared(
        &mut self,
        root: &Path,
        prepared: Option<startup_restore::PreparedStartupProject>,
    ) {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        // Unsaved edits are saved first; edits that cannot be saved stop the switch.
        if !self.save_before_switching(&root) {
            return;
        }
        if !self.workspace_transition(workspace_ui::WorkspaceTransition::Switch(root.clone())) {
            return;
        }
        self.restore_dialog_project_base();
        if let Err(error) = self.settings.load_project_code_style(&root) {
            self.set_status(format!("Cannot load project code style: {error}"));
        }
        if let Err(error) = self.settings.inspections.profiles.load_project(&root) {
            self.set_status(format!("Cannot load project inspection profiles: {error}"));
        }
        if let Err(error) = self.settings.scopes.load_project(&root) {
            self.set_status(format!("Project scopes: {error}"));
        }
        self.load_dialog_project_preferences(&root);
        self.reset_documentation();
        self.py_console.reset();
        self.cancel_startup_terminal();
        self.forget_all_tabs();
        self.cancel_project_tasks();
        let analysis = Arc::new(Analysis::for_project(&root));
        analysis.set_analyze_doc_comments(
            self.settings
                .documentation
                .effective(Some(&root), &self.settings.docstring_format)
                .analyze_doc_comments,
        );
        let run_configs = RunConfigStore::load(&root);
        // Trust lives in the IDE settings only and is decided before anything runs:
        // discovery probes (executes) the project's own venvs only once it is trusted.
        self.project_trust_identity =
            rustpy_settings::trust::DirectoryIdentity::capture(&root).ok();
        let trusted = self.project_trust_identity.is_some()
            && (rustpy_settings::trust_all_from_env() || self.settings.is_project_trusted(&root));
        self.safe_mode = !trusted;
        let (ws, prepared_model) = match prepared {
            Some(prepared) => (
                self.install_workspace_client(prepared.workspace),
                Some((prepared.model, prepared.ignored_roots)),
            ),
            None => (self.begin_workspace(&root), None),
        };
        self.encoding_generation = self.encoding_generation.wrapping_add(1);
        self.encoding_overrides = ws
            .file_encodings
            .iter()
            .map(|entry| {
                (
                    rustpy_core::vfs::normalize_path(&entry.path),
                    entry.encoding.clone(),
                )
            })
            .collect();
        let preferred = ws.interpreter.clone();
        let rx =
            rustpy_project::interpreter::discovery_receiver(Some(root.clone()), trusted, preferred);
        let git = GitRepo::discover(&root);
        self.db_panel.database_project_changed(&root);
        self.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis,
            run_configs,
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: Some(rx),
            git,
            git_refreshed: -100.0,
            git_branch: None,
        });
        self.refresh_project_new_catalog();
        self.settings.add_recent_project(&root);
        let _ = self.settings.save();
        self.start_fs_watcher(&root);
        if let Some(p) = &mut self.project {
            for d in &ws.expanded_dirs {
                p.tree.expanded.insert(d.clone());
            }
            p.tree.expanded.insert(root.clone());
        }
        self.breakpoints = crate::breakpoints::BreakpointStore::from_workspace(&ws);
        self.bookmarks = ws
            .bookmarks
            .iter()
            .map(|b| Bookmark {
                path: b.path.clone(),
                line: b.line,
                mnemonic: b.mnemonic,
                description: b.description.clone(),
                kind: b.kind,
                list: b.list.clone(),
            })
            .collect();
        self.restore_bookmark_lists(ws.bookmark_lists.clone());
        if let Some((model, ignored)) = prepared_model {
            self.install_prepared_project_structure(&root, model, &ignored);
        } else {
            self.load_project_structure(&root);
        }
        self.apply_interpreter_paths();
        self.recent_files = ws.recent_files.clone();
        self.tasks = Some(rustpy_settings::tasks::TaskStore::load(&root));
        self.safe_mode = !trusted || self.project_trust_identity.is_none();
        self.startup_tasks_done = false;
        if !trusted {
            self.dialog = Dialog::TrustProject(root.clone());
        }
        for f in &ws.open_files {
            if f.path.is_file() {
                self.restore_editor_session(f);
            }
        }
        if !ws.recent_files.is_empty() {
            // Reopening the tabs moved each to the front: keep the saved most-recently-used order.
            self.recent_files = ws.recent_files.clone();
        }
        self.restore_editor_layout(ws.editor_layout.as_ref());
        if let Some(active) = ws.active_file {
            // The legacy active_file identifies a document, not one of its split instances.
            // Keep the instance selected by the restored layout when it owns that document.
            if let Some(i) = self
                .active
                .filter(|&i| {
                    self.tabs
                        .get(i)
                        .is_some_and(|t| t.state.path.as_deref() == Some(active.as_path()))
                })
                .or_else(|| {
                    self.tabs
                        .iter()
                        .position(|t| t.state.path.as_deref() == Some(active.as_path()))
                })
            {
                self.active = Some(i);
            }
        }
        self.start_indexing();
        let has_saved_layout = self
            .project
            .as_ref()
            .is_some_and(|project| rustpy_settings::Workspace::path_for(&project.root).is_file());
        let startup_layout =
            if self.settings.tool_window_layouts.restore_on_startup && has_saved_layout {
                ws.tool_windows.clone()
            } else {
                self.settings
                    .tool_window_layouts
                    .default_layout
                    .clone()
                    .unwrap_or_default()
            };
        self.restore_tool_window_layout(&startup_layout);
        self.rebuild_file_index();
        self.acknowledge_loaded_workspace_roots();
        self.report_workspace_error();
    }

    fn record_recent(&mut self, path: &Path) {
        if path.starts_with("<typeshed>") {
            return;
        }
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(50);
    }

    /// Closes any project so the Welcome screen shows (smoke tests).
    pub fn show_welcome(&mut self) {
        self.close_project();
    }

    pub fn open_file(&mut self, path: &Path) {
        let normalized = if path.starts_with("<typeshed>") {
            path.to_path_buf()
        } else {
            rustpy_core::vfs::normalize_path(path)
        };
        let path = normalized.as_path();
        if self.open_associated_editor_file(path, false, "") {
            return;
        }
        self.record_recent(path);
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            self.open_data_archive(path);
            return;
        }
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["db", "sqlite", "sqlite3", "duckdb"].contains(&e.to_ascii_lowercase().as_str())
        }) {
            if let Some(project) = &self.project {
                let root = project.root.clone();
                self.db_panel.register_database_file(&root, path);
                self.bottom = Some(BottomTool::Database);
                return;
            }
        }
        let preferred_group = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.group);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|tab| {
                tab.state.path.as_deref() == Some(path) && Some(tab.group) == preferred_group
            })
            .or_else(|| {
                self.tabs
                    .iter()
                    .position(|tab| tab.state.path.as_deref() == Some(path))
            })
        {
            self.active = Some(i);
            self.focus_editor = true;
            self.refresh_file_permissions(i);
            return;
        }
        // Only when the file is loaded: re-activating a tab must not hide a change made on disk since.

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "ipynb" {
            let loaded = self
                .file_vfs()
                .read(path, self.normal_editor_read_limits())
                .map_err(|e| e.to_string())
                .and_then(|snapshot| {
                    let text = std::str::from_utf8(&snapshot.bytes)
                        .map_err(|e| e.to_string())?
                        .to_owned();
                    let model = text
                        .parse::<rustpy_notebook::Notebook>()
                        .map_err(|e| e.to_string())?;
                    Ok((
                        crate::notebook_view::NotebookTab::from_notebook(model),
                        text,
                        snapshot.revision,
                    ))
                });
            match loaded {
                Ok((nb, text, revision)) => {
                    self.disk_revisions
                        .insert(path.to_path_buf(), Some(revision));
                    let mut state = EditorState::from_text(&text, Language::Json);
                    state.path = Some(path.to_path_buf());
                    self.push_tab(state);
                    if let Some(t) = self.tabs.last_mut() {
                        t.notebook = Some(Box::new(nb));
                    }
                    self.record_recent(path);
                    return;
                }
                Err(e) => self.set_status(format!(
                    "Cannot open the notebook as a notebook ({e}); showing JSON"
                )),
            }
        }
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif"
        ) {
            match self
                .file_vfs()
                .read(path, self.normal_editor_read_limits())
                .ok()
                .and_then(|s| image::load_from_memory(&s.bytes).ok())
            {
                Some(img) => {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let mut state = EditorState::from_text("", Language::Other);
                    state.path = Some(path.to_path_buf());
                    state.doc.set_read_only(true);
                    self.push_tab(state);
                    if let Some(t) = self.tabs.last_mut() {
                        t.image = Some(ImageTab {
                            rgba: rgba.into_raw(),
                            size,
                            texture: None,
                            zoom: self.settings.editor.preferences.image_zoom.clamp(0.1, 8.0),
                            checkerboard: self.settings.editor.preferences.image_checkerboard,
                            pixel_grid: self.settings.editor.preferences.image_grid,
                        });
                    }
                    return;
                }
                None => self.set_status(format!("Cannot decode image {}", path.display())),
            }
        }
        if self.open_large_file(path) {
            return;
        }
        // The binary probe and the hex view read only the part they show.
        if let Some((bytes, size)) = background::read_head(path, 4096 * 16) {
            let probe = &bytes[..bytes.len().min(8192)];
            if !self
                .encoding_overrides
                .contains_key(&rustpy_core::vfs::normalize_path(path))
                && probe.contains(&0u8)
                && !probe.starts_with(&[0xff, 0xfe])
                && !probe.starts_with(&[0xfe, 0xff])
                && !matches!(ext.as_str(), "py" | "txt" | "md")
            {
                // Binary file: hex dump viewer (read-only).
                let mut out = String::new();
                for (i, chunk) in bytes.chunks(16).take(4096).enumerate() {
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                    let ascii: String = chunk
                        .iter()
                        .map(|b| {
                            if b.is_ascii_graphic() || *b == b' ' {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    out.push_str(&format!(
                        "{:08x}  {:<47}  |{}|\n",
                        i * 16,
                        hex.join(" "),
                        ascii
                    ));
                }
                if size > 4096 * 16 {
                    out.push_str(&format!("… {} more bytes\n", size - 4096 * 16));
                }
                let mut state = EditorState::from_text(&out, Language::PlainText);
                state.path = Some(path.to_path_buf());
                state.doc.set_read_only(true);
                self.permission_viewers.insert(state.doc.identity());
                self.push_tab(state);
                self.set_status(format!("{} is a binary file (hex view)", path.display()));
                return;
            }
        }
        if path.starts_with("<typeshed>") {
            self.open_virtual(path);
            return;
        }
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path.as_path()))
        {
            self.active = Some(i);
            self.focus_editor = true;
            return;
        }
        match self
            .file_vfs()
            .read(&path, self.normal_editor_read_limits())
        {
            Ok(snapshot) => {
                let bytes = snapshot.bytes;
                let selected = match self.encoding_override(&path) {
                    Ok(selected) => selected,
                    Err(error) => {
                        self.set_status(format!("Cannot open {}: {error}", path.display()));
                        return;
                    }
                };
                let doc = match Document::from_bytes_with_policy(
                    &bytes,
                    rustpy_text::SourcePolicy::for_path(&path),
                    selected,
                ) {
                    Ok(doc) => doc,
                    Err(error) => {
                        self.set_status(format!("Cannot open {}: {error}", path.display()));
                        return;
                    }
                };
                if self.open_associated_editor_file(&path, false, &doc.line_text(0)) {
                    return;
                }
                self.disk_revisions
                    .insert(path.clone(), Some(snapshot.revision));
                // Python library sources open read-only, so in reader mode (Settings › Editor › Reader Mode).
                let (search_paths, roots) = self
                    .project
                    .as_ref()
                    .map(|p| {
                        (
                            p.analysis
                                .resolver
                                .search_paths
                                .iter()
                                .chain(&p.analysis.resolver.stub_roots)
                                .cloned()
                                .collect::<Vec<_>>(),
                            p.analysis.resolver.roots.as_slice(),
                        )
                    })
                    .unwrap_or((Vec::new(), &[]));
                let mut state = EditorState::new(doc, Some(path.clone()));
                crate::editor::doc_render::lock_library_file(
                    &mut state,
                    &search_paths,
                    roots,
                    self.settings.editor.reader_mode.protect_library_sources,
                );
                self.attach_textmate_grammar(&mut state);
                self.push_tab(state);
                if let Some(index) = self.active {
                    self.bind_file_permissions(index);
                }
            }
            Err(e) => self.set_status(format!("Cannot open {}: {e}", path.display())),
        }
    }

    /// Periodically refreshes VCS status for the project tree and gutter.
    fn refresh_git(&mut self, now: f64) {
        if !self.builtin_plugin_enabled("git") {
            if let Some(project) = self.project.as_mut() {
                project.tree.vcs_status.clear();
                project.git_branch = None;
            }
            self.line_changes = None;
            return;
        }
        let Some(p) = &mut self.project else { return };
        let Some(git) = &p.git else { return };
        if now - p.git_refreshed > 2.0 {
            p.tree.vcs_status = git.status_map();
            for root in &self.vcs_workspace.mappings.roots {
                if root != git.root() {
                    if let Some(repo) = GitRepo::discover(root) {
                        p.tree.vcs_status.extend(repo.status_map());
                    }
                }
            }
            p.git_branch = git.current_branch();
            p.tree.vcs_status.retain(|path, _| {
                self.vcs_workspace
                    .mappings
                    .provider_for(path)
                    .is_none_or(|(_, provider)| provider == rustpy_vcs::provider::Provider::Git)
            });
            p.git_refreshed = now;
            self.line_changes = None;
        }
        // Gutter markers for the active tab.
        if let Some(i) = self.active {
            let tab = &self.tabs[i];
            if let Some(path) = &tab.state.path {
                let version = tab.state.doc.version();
                let stale = self
                    .line_changes
                    .as_ref()
                    .map(|(p, v, _)| p != path || *v != version)
                    .unwrap_or(true);
                if stale {
                    let routed = self
                        .vcs_workspace
                        .mappings
                        .root_for(path)
                        .filter(|root| root.as_path() != git.root())
                        .and_then(|root| GitRepo::discover(root));
                    let changes = if self.vcs_workspace.mappings.provider_for(path).is_some_and(
                        |(_, provider)| provider != rustpy_vcs::provider::Provider::Git,
                    ) {
                        Vec::new()
                    } else {
                        routed
                            .as_ref()
                            .unwrap_or(git)
                            .line_changes(path, &tab.state.doc.text())
                    };
                    self.line_changes = Some((path.clone(), version, changes));
                }
            }
        }
    }

    /// Reloads editors whose files changed on disk (after rollback/pull/checkout).
    fn reload_changed_files(&mut self) {
        self.emacs_editing.interrupt();
        self.queue_open_file_reconciliation();
        self.reconcile_external_changes();
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
            p.tree.invalidate_all();
        }
    }

    fn interpreter_discovery_pending(&self) -> bool {
        self.project
            .as_ref()
            .is_some_and(|p| p.interpreter_rx.is_some())
    }

    fn cancel_startup_terminal(&mut self) {
        self.terminal_on_start = None;
        self.startup_terminal_session = None;
    }

    fn take_startup_terminal(&mut self, elapsed: f64) -> Option<StartupTerminalDispatch> {
        self.terminal_on_start.as_ref()?;
        let session = *self
            .startup_terminal_session
            .get_or_insert(self.file_session);
        if session != self.file_session {
            self.cancel_startup_terminal();
            return None;
        }
        if elapsed <= 1.0 || self.interpreter_discovery_pending() {
            return None;
        }
        let cwd = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let env = crate::terminal_panel::terminal_env(
            self.settings.terminal.activate_virtualenv,
            self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
        );
        self.startup_terminal_session = None;
        Some(StartupTerminalDispatch {
            cwd,
            env,
            command: self.terminal_on_start.take()?,
        })
    }

    /// Receives interpreter discovery results and applies the workspace choice.
    fn poll_interpreters(&mut self) {
        let ws = self.effective_workspace();
        let Some(p) = &mut self.project else { return };
        let Some(rx) = &p.interpreter_rx else { return };
        let found = match rx.try_recv() {
            Ok(found) => found,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Vec::new(),
        };
        {
            p.interpreters = found;
            let preferred = p
                .tree
                .model()
                .sdk
                .or(ws.interpreter)
                .and_then(|path| p.interpreters.iter().find(|i| i.path == path).cloned());
            p.interpreter = preferred
                .or_else(|| {
                    p.interpreters
                        .iter()
                        .find(|i| i.kind.contains("venv"))
                        .cloned()
                })
                .or_else(|| p.interpreters.first().cloned());
            p.interpreter_rx = None;
            self.apply_interpreter_paths();
        }
    }

    /// Updates module resolution: the project's import roots (content, Sources and
    /// Test Sources roots) and the selected interpreter's search paths.
    fn apply_interpreter_paths(&mut self) {
        self.cancel_native_skeletons();
        let Some(p) = &self.project else { return };
        let roots = p.tree.import_roots();
        // Stub `sys.version_info` / `sys.platform` branches follow the interpreter;
        // source and content roots apply even without one.
        let model = p.tree.model();
        let registry = rustpy_project::environment_manager::Registry::load(
            &rustpy_project::environment_manager::registry_path(),
        );
        let interpreters: Vec<_> = p
            .interpreters
            .iter()
            .cloned()
            .map(|mut interpreter| {
                registry.apply_paths(&mut interpreter);
                interpreter
            })
            .collect();
        let current = p.interpreter.clone().map(|mut interpreter| {
            registry.apply_paths(&mut interpreter);
            interpreter
        });
        let selected = model
            .sdk
            .as_deref()
            .and_then(|sdk| {
                interpreters
                    .iter()
                    .find(|interpreter| interpreter.path == sdk)
            })
            .or_else(|| {
                if model.sdk.is_none() {
                    current.as_ref()
                } else {
                    None
                }
            });
        let mut resolver = code_insight::resolver_for(roots.clone(), selected)
            .with_stub_roots(self.settings.python_stub_roots.iter().cloned())
            .with_excluded(p.tree.excluded.iter().cloned());
        if let Some(level) = model.language_level {
            resolver.python_version = level;
        }
        for module in &model.modules {
            let sdk = module.sdk.as_deref().or(model.sdk.as_deref());
            let interpreter = sdk
                .and_then(|sdk| {
                    interpreters
                        .iter()
                        .find(|interpreter| interpreter.path == sdk)
                })
                .or_else(|| if sdk.is_none() { selected } else { None });
            let mut module_roots = vec![module.content_root.clone()];
            module_roots.extend(
                roots
                    .iter()
                    .filter(|root| {
                        root.starts_with(&module.content_root) && **root != module.content_root
                    })
                    .cloned(),
            );
            module_roots.extend(
                roots
                    .iter()
                    .filter(|root| !root.starts_with(&module.content_root))
                    .cloned(),
            );
            let mut module_resolver = code_insight::resolver_for(module_roots, interpreter)
                .with_stub_roots(self.settings.python_stub_roots.iter().cloned())
                .with_excluded(p.tree.excluded.iter().cloned());
            if let Some(level) = module.language_level.or(model.language_level) {
                module_resolver.python_version = level;
            }
            resolver = resolver.with_module(module.content_root.clone(), module_resolver);
        }
        let analysis = Analysis::new(resolver);
        // Re-sync open files into the new database.
        analysis.set_analyze_doc_comments(self.documentation_preferences().analyze_doc_comments);
        let analysis = Arc::new(analysis);
        for tab in &mut self.tabs {
            tab.state.synced_version = None;
            tab.state.semantic_version = None;
            crate::editor::doc_render::lock_library_file(
                &mut tab.state,
                &analysis
                    .resolver
                    .search_paths
                    .iter()
                    .chain(&analysis.resolver.stub_roots)
                    .cloned()
                    .collect::<Vec<_>>(),
                &analysis.resolver.roots,
                self.settings.editor.reader_mode.protect_library_sources,
            );
        }
        if let Some(p) = &mut self.project {
            p.tree.show_python_stubs = true;
            p.tree.python_stub_roots = self.settings.python_stub_roots.clone();
            p.tree.invalidate_all();
            p.analysis = analysis;
        }
        for index in 0..self.tabs.len() {
            self.refresh_file_permissions(index);
        }
    }

    fn select_interpreter(&mut self, mut i: Interpreter) {
        self.remember_managed_interpreter(&mut i);
        let path = i.path.clone();
        if let Some(project) = &self.project {
            let mut model = project.tree.model();
            model.sdk = Some(path.clone());
            if !self.apply_project_model(model) {
                return;
            }
        }
        if let Some(p) = &mut self.project {
            p.interpreter = Some(i);
        }
        self.apply_interpreter_paths();
        self.record_interpreter_choice(&path);
        self.save_workspace();
    }

    /// Runs a configuration, opening the Run tool window.
    /// Runs tests through the Test Runner tool window (pytest when available, unittest otherwise).
    fn run_tests(&mut self, targets: Vec<String>, keywords: &str) {
        if self.project_safe_mode() {
            self.notify_safe_mode("Running tests");
            return;
        }
        if !self.save_all() {
            return;
        }
        let Some(p) = &self.project else { return };
        let Some(interp) = p.interpreter.as_ref().map(|i| i.path.clone()) else {
            self.notify_no_interpreter();
            return;
        };
        let root = p.root.clone();
        let framework = if targets.iter().any(|target| {
            target
                .split("::")
                .next()
                .unwrap_or(target)
                .contains(".feature")
        }) {
            if self.settings.python_testing.runner == "lettuce" {
                rustpy_test::Framework::Lettuce
            } else {
                rustpy_test::Framework::Behave
            }
        } else {
            self.configured_test_framework().unwrap_or_else(|| {
                rustpy_test::detect_framework_for(&root, &targets, Some(&interp))
            })
        };
        let mut args = Vec::new();
        if !keywords.trim().is_empty() && framework == rustpy_test::Framework::Pytest {
            args.push("-k".to_string());
            args.push(keywords.trim().to_string());
        }
        let mut options = self.default_test_options();
        self.configure_test_tools(framework, &targets, &mut args, &mut options);
        self.apply_test_preferences(framework, &mut args, &mut options);
        self.checkpoint_execution_history("Before test run");
        self.begin_measurement(run_configuration_sessions::Kind::Tests, "Tests");
        match self
            .test_panel
            .start_with(framework, &interp, &root, targets, args, options)
        {
            Ok(()) => self.bottom = Some(BottomTool::Tests),
            Err(e) => {
                self.bottom = Some(BottomTool::Tests);
                self.set_status(e);
            }
        }
    }

    /// PYTHONPATH for runs: content roots plus Sources and Test Sources roots (when
    /// the configuration asks for them).
    fn pythonpath_for(&self, cfg: &RunConfig) -> Option<String> {
        let p = self.project.as_ref()?;
        let inherited = cfg
            .workflow
            .inherit_environment
            .then(|| std::env::var("PYTHONPATH").ok())
            .flatten();
        rustpy_project::env::pythonpath_for(
            &p.root,
            &p.tree.model(),
            cfg.add_content_roots,
            cfg.add_source_roots,
            inherited.as_deref(),
        )
    }

    /// Saves and runs a configuration: pytest / unittest in the Test Runner,
    /// anything else in the Run tool window (see `run_launch.rs`).
    fn run_config(&mut self, cfg: RunConfig) {
        self.launch_config(cfg, crate::run_context::RunMode::Run);
    }

    /// Runs the context configuration (Ctrl+Shift+F10) or the selected configuration (Shift+F10).
    fn run_action(&mut self, context: bool) {
        self.launch_selected_or_context(context, crate::run_context::RunMode::Run);
    }

    fn toggle_breakpoint(&mut self, path: &Path, line0: usize) {
        self.reconcile_document_markers();
        self.breakpoints.toggle(path, line0 + 1);
        self.reconcile_document_markers();
        self.sync_breakpoints(path);
        self.save_workspace();
    }

    fn breakpoint_lines(&self, path: Option<&Path>) -> Vec<(usize, view::BpGlyph)> {
        path.map(|p| self.breakpoints.glyphs(p, self.debug.as_ref()))
            .unwrap_or_default()
    }

    /// Shift+F9: debug the selected configuration (or the context of the
    /// current file); Ctrl+Shift+F9 (`context`): the test or script at the caret.
    fn debug_action(&mut self, context: bool) {
        self.launch_selected_or_context(context, crate::run_context::RunMode::Debug);
    }

    /// Saves a configuration and starts a debug session for it (see `run_launch.rs`).
    fn debug_config(&mut self, cfg: RunConfig) {
        self.launch_config(cfg, crate::run_context::RunMode::Debug);
    }

    /// Polls the debug session and moves the editor to the execution point.
    fn debug_transport_pending(&self) -> Vec<(u64, String)> {
        let mut pending = self
            .debug
            .iter()
            .chain(self.debug_sessions.iter().map(|(session, _)| session))
            .flat_map(DebugSession::transport_cleanup_ids)
            .map(|id| (id, format!("Debugger transport {id} cleanup")))
            .collect::<Vec<_>>();
        pending.extend(rustpy_debug::dap::poll_retiring_transports());
        pending.sort_by_key(|(id, _)| *id);
        pending.dedup_by_key(|(id, _)| *id);
        pending
    }
    fn poll_debug_transport_retirement(&mut self, ctx: &egui::Context) -> bool {
        for session in self
            .debug
            .iter_mut()
            .chain(self.debug_sessions.iter_mut().map(|(session, _)| session))
        {
            session.poll();
        }
        let pending = !self.debug_transport_pending().is_empty();
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        pending
    }

    fn poll_debug(&mut self, ctx: &egui::Context) {
        self.debug_tick();
        let Some(d) = &mut self.debug else { return };
        let changed = d.poll();
        if d.is_active() || !d.transport_cleanup_ids().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if !changed {
            return;
        }
        let stopped = d.is_stopped();
        let frame = d.current_frame().cloned();
        let column_encoding = d.column_encoding();
        if stopped {
            if let Some(f) = frame {
                if let Some(path) = f.path.clone() {
                    if self.settings.debugger.workspace.jump_to_source
                        && self.debug_frame_location_changed(&path, f.line, f.column)
                    {
                        self.navigate_debug_frame(&path, f.line, f.column, column_encoding);
                    }
                }
            }
            if !self.debug_was_stopped {
                self.bottom = Some(BottomTool::Debug);
            }
        } else {
            self.exec_point = None;
            self.exec_point_column = None;
        }
        self.debug_was_stopped = stopped;
    }

    /// Lines of the active editor that show a ▶ gutter icon.
    fn run_lines(&mut self, i: usize) -> Vec<usize> {
        let tab = &mut self.tabs[i];
        if tab
            .state
            .path
            .as_deref()
            .and_then(rustpy_deploy::targets::document_kind)
            .is_some()
        {
            return tab
                .state
                .doc
                .text()
                .lines()
                .position(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
                .into_iter()
                .collect();
        }
        if matches!(tab.state.language, Language::Shell | Language::Dockerfile) {
            return vec![0];
        }
        if tab.state.language == Language::Requirements {
            if tab.state.is_large_file() || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024 {
                return Vec::new();
            }
            return rustpy_packages::parse_requirements(&tab.state.doc.text())
                .into_iter()
                .map(|requirement| requirement.line.saturating_sub(1))
                .collect();
        }
        if tab.state.language == Language::Http {
            let text = tab.state.doc.text();
            return rustpy_http::parse(&text)
                .iter()
                .map(|r| r.line.saturating_sub(1))
                .collect();
        }
        if tab.state.language == Language::Gherkin {
            let f = rustpy_gherkin::parse(&tab.state.doc.text());
            let mut lines: Vec<usize> = Vec::new();
            if f.line > 0 {
                lines.push(f.line - 1);
            }
            lines.extend(f.scenarios.iter().map(|s| s.line.saturating_sub(1)));
            return lines;
        }
        if tab.state.language != Language::Python
            || tab
                .state
                .path
                .as_ref()
                .is_some_and(|p| p.starts_with("<typeshed>"))
        {
            return Vec::new();
        }
        // Tests (methods, nested classes, TestCase subclasses), main guard, doctests.
        self.python_run_lines(i)
    }

    /// Opens a read-only tab for a bundled stub (`<typeshed>/os/path.pyi`).
    fn open_virtual(&mut self, path: &Path) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path))
        {
            self.active = Some(i);
            self.focus_editor = true;
            return;
        }
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return;
        };
        let Some(info) = analysis.file_for_path(path) else {
            self.set_status(format!("Cannot open {}", path.display()));
            return;
        };
        let mut doc = Document::new(&info.text);
        doc.set_read_only(true);
        let mut state = EditorState::new(doc, Some(path.to_path_buf()));
        state.language = Language::Python;
        self.push_tab(state);
    }

    /// Pushes open editor contents to the analysis database and refreshes
    /// semantic diagnostics for the active tab (debounced after edits).
    fn sync_analysis(&mut self, now: f64) {
        for tab in &mut self.tabs {
            tab.state.inspection_level = tab
                .state
                .path
                .as_ref()
                .map(|path| self.settings.inspections.profiles.level(path))
                .unwrap_or_default();
        }
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return;
        };
        self.release_closed_editors(&analysis);
        // Text pushed in the background has landed: the database has it now.
        if let Some((path, document, version)) = self.bg.text_sync.poll() {
            if let Some(tab) = self.tabs.iter_mut().find(|t| {
                t.state.path.as_deref() == Some(path.as_path())
                    && t.state.doc.identity() == document
            }) {
                if tab
                    .state
                    .synced_version
                    .is_some_and(|synced| synced > version)
                {
                    // Newer text was pushed synchronously meanwhile and the
                    // older push overwrote it: push the current text again.
                    tab.state.synced_version = None;
                } else {
                    tab.state.synced_version = Some(version);
                }
            }
        }
        // Template/data editors are not analysed below, but their document
        // inspections need the project's inputs.
        if let Some(i) = self.active {
            if self.tabs[i].state.framework_inputs.root.is_none()
                || !(0.0..0.5).contains(&(now - self.document_inputs_time))
            {
                self.document_inputs_time = now;
                self.refresh_document_inputs(i);
            }
        }
        for tab in &mut self.tabs {
            let Some(path) = tab.state.path.clone() else {
                continue;
            };
            if remote_development::is_remote_mirror(&path) {
                continue;
            }
            // Large-file mode: nothing is analysed (code insight syncs on demand).
            if tab.state.language != Language::Python
                || path.starts_with("<typeshed>")
                || tab.state.is_large_file()
                || tab.state.doc.is_read_only()
            {
                continue;
            }
            let v = tab.state.doc.version();
            if tab.state.synced_version != Some(v) {
                let document = tab.state.doc.identity();
                if self.bg.text_sync.observe(&path, document, v) {
                    self.last_edit_time = now;
                    // A background run for the previous text is obsolete.
                    self.bg.analysis.cancel_obsolete(&path, v);
                }
                // Large files are reparsed off the UI thread (one push at a
                // time; the newest text follows when it lands).
                if usize::from(tab.state.doc.len()) < analysis_jobs::BACKGROUND_SYNC_BYTES {
                    self.bg
                        .text_sync
                        .push_now(&analysis, &path, &tab.state.doc.text());
                    tab.state.synced_version = Some(v);
                } else {
                    let doc = &tab.state.doc;
                    self.bg
                        .text_sync
                        .push_in_background(&analysis, &path, document, v, || doc.text());
                }
            }
        }
        // Power Save Mode: no background analysis at all.
        if !self.background_features().code_analysis {
            return;
        }
        let Some(i) = self.active else { return };
        let grammar_settings = self.settings.inspections.clone();
        if self.remote_bound(i) {
            return;
        }
        if grammar_settings.grammar
            && !grammar_settings
                .disabled_inspections
                .iter()
                .any(|code| code == "Grammar")
        {
            if let Err(error) = grammar_policy::configured(&grammar_settings) {
                self.set_status(error.to_string());
            }
        }
        let spelling_on = self.settings.inspections.spelling
            && !self
                .settings
                .inspections
                .disabled_inspections
                .iter()
                .any(|code| code == "SpellCheckingInspection");
        if self.tabs[i].state.language == Language::Gherkin {
            let v = self.tabs[i].state.doc.version();
            if self.tabs[i].state.semantic_version != Some(v)
                && now - self.last_edit_time
                    > self.settings.editor.preferences.reparse_delay_ms.min(5000) as f64 / 1000.0
            {
                self.gherkin_diagnostics(i);
            }
            return;
        }
        let tab = &mut self.tabs[i];
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        // Prose files: grammar only.
        if matches!(tab.state.language, Language::Markdown | Language::PlainText)
            && !tab.state.is_large_file()
        {
            let v = tab.state.doc.version();
            if tab.state.semantic_version != Some(v)
                && now - self.last_edit_time
                    > self.settings.editor.preferences.reparse_delay_ms.min(5000) as f64 / 1000.0
            {
                let text = tab.state.doc.text();
                let context = if tab.state.language == Language::Markdown {
                    rustpy_grammar::policy::Context::Markdown
                } else {
                    rustpy_grammar::policy::Context::Prose
                };
                let issues =
                    grammar_policy::check(&grammar_settings, &text, context).unwrap_or_default(); // Invalid preferences are explicitly reported above.
                let (mut diags, mut fixes) = grammar_diagnostics(&issues);
                if spelling_on {
                    let typos = rustpy_spell::check_plain_text(
                        &text,
                        &spell_dictionary_for(
                            self.project.as_ref().map(|project| project.root.as_path()),
                        ),
                    );
                    let (d, f) = spelling_diagnostics(&typos);
                    diags.extend(d);
                    fixes.extend(f);
                }
                tab.state.set_prose_diagnostics(diags, fixes);
            }
            return;
        }
        if tab.state.language != Language::Python || path.starts_with("<typeshed>") {
            return;
        }
        // Inspections, grammar, spelling and inlay hints run in the background.
        self.schedule_analysis(i, &analysis, now);
        let tab = &mut self.tabs[i];
        let v = tab.state.doc.version();
        // Highlight usages of the identifier under the caret (debounced by caret change).
        if self.settings.editor.highlight_usages_on_caret_move && !tab.state.is_large_file() {
            let caret = tab.state.primary().head();
            let key = (v, caret);
            if let Some((found, at, ranges)) = self.bg.usage_highlights.poll() {
                if found == path && at == key && tab.state.usage_highlight_caret == Some(key) {
                    tab.state.usage_highlights = ranges;
                }
            }
            if tab.state.usage_highlight_caret != Some(key) && now - self.last_edit_time > 0.2 {
                if tab.state.has_selection() {
                    self.bg.usage_highlights.cancel();
                    tab.state.usage_highlight_caret = Some(key);
                    tab.state.usage_highlights = Vec::new();
                } else if usize::from(tab.state.doc.len()) < analysis_jobs::BACKGROUND_SYNC_BYTES {
                    tab.state.usage_highlight_caret = Some(key);
                    tab.state.usage_highlights = analysis
                        .file_for_path(&path)
                        .map(|file| {
                            usages::usages_in_file(&analysis, &file, caret)
                                .into_iter()
                                .map(|u| (u.range, u.is_write))
                                .collect()
                        })
                        .unwrap_or_default();
                } else if tab.state.synced_version == Some(v) {
                    // Large file: resolved in the background once the
                    // database has this text; the previous highlights stay
                    // until then.
                    tab.state.usage_highlight_caret = Some(key);
                    if let Some(file) = analysis.file_for_path(&path) {
                        self.bg.usage_highlights.start(&analysis, file, key);
                    }
                }
            }
        }
    }

    /// Records a macro step while recording (ignores the recording actions themselves).
    fn macro_record_action(&mut self, action: Action) {
        if self.macro_playing {
            return;
        }
        if let Some(steps) = &mut self.macro_recording {
            if !matches!(
                action,
                Action::StartStopMacroRecording | Action::PlaybackLastMacro | Action::EditMacros
            ) {
                steps.push(format!("action:{}", action.id()));
            }
        }
    }

    fn macro_record_text(&mut self, text: &str) {
        if self.macro_playing || text.is_empty() {
            return;
        }
        if let Some(steps) = &mut self.macro_recording {
            if let Some(last) = steps.last_mut() {
                if let Some(prev) = last.strip_prefix("text:") {
                    *last = format!("text:{prev}{text}");
                    return;
                }
            }
            steps.push(format!("text:{text}"));
        }
    }

    fn play_macro(&mut self, steps: Vec<String>, ctx: &egui::Context) {
        if let Err(error) = macro_ui::validate_steps(&steps) {
            self.set_status(error);
            return;
        }
        self.macro_playing = true;
        let opts = self.edit_options();
        for step in steps {
            if let Some(id) = step.strip_prefix("action:") {
                if let Some(a) = Keymap::action_by_id(id) {
                    if !self.dispatch_editor_action(a) {
                        self.handle_action(a, ctx);
                    }
                }
            } else if let Some(text) = step.strip_prefix("text:") {
                self.emacs_editing.interrupt();
                if let Some(index) = self.active {
                    self.refresh_file_permissions(index);
                }
                if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                    tab.state.insert_text(text, &opts);
                }
            }
        }
        self.macro_playing = false;
    }

    /// Runs editor-level actions (caret moves, edits) directly on the active editor state.
    fn dispatch_editor_action(&mut self, action: Action) -> bool {
        if action == Action::Undo && self.request_workspace_refactoring_undo() {
            return true;
        }
        if !Self::is_emacs_action(action) {
            self.emacs_editing.interrupt();
        }
        if let Some(index) = self.active {
            self.refresh_file_permissions(index);
        }
        let opts = self.edit_options();
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return false;
        };
        crate::editor::view::handle_editor_action(action, &mut tab.state, &opts)
    }

    /// Recomputes blame annotations for the active tab when enabled and stale.
    fn update_blame(&mut self, i: usize) {
        if !self.annotate_enabled {
            self.blame = None;
            return;
        }
        let Some(tab) = self.tabs.get(i) else { return };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let version = tab.state.doc.version();
        if self
            .blame
            .as_ref()
            .is_some_and(|(p, v, _)| *p == path && *v == version)
        {
            return;
        }
        let Some(git) = self
            .vcs_workspace
            .mappings
            .root_for(&path)
            .and_then(|root| GitRepo::discover(root))
        else {
            return;
        };
        let lines = git
            .blame_contents_with_options(
                &path,
                tab.state.doc.text().as_bytes(),
                self.settings.vcs_operations.blame_ignore_whitespace,
            )
            .unwrap_or_default();
        let out =
            vcs_operations::format_blame_annotations(&git, lines, &self.settings.vcs_operations);
        self.blame = Some((path, version, out));
    }

    /// Loads `.rustpy/runtime_types.json` into the analysis when it changes
    /// (or when the analysis database was rebuilt).
    fn sync_runtime_types(&mut self, now: f64) {
        if now - self.rt_checked < 1.0 {
            return;
        }
        self.rt_checked = now;
        let Some(p) = &self.project else { return };
        let path = rustpy_semantic::runtime_types::RuntimeTypes::path_for(&p.root);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let ptr = Arc::as_ptr(&p.analysis) as usize;
        if mtime == self.rt_mtime && ptr == self.rt_analysis {
            return;
        }
        self.rt_mtime = mtime;
        self.rt_analysis = ptr;
        let rt = rustpy_semantic::runtime_types::RuntimeTypes::load(&p.root);
        let changed = !rt.is_empty() || !p.analysis.runtime_types().is_empty();
        let n = rt.functions.len();
        p.analysis.set_runtime_types(rt);
        if changed {
            for tab in &mut self.tabs {
                tab.state.semantic_version = None;
            }
            log::info!("run-time types: {n} function(s) loaded");
        }
    }

    fn ai_config(&self) -> rustpy_ai::AiConfig {
        let s = &self.settings.ai;
        let mut c = rustpy_ai::AiConfig {
            worker_owner: self.ai_panel.worker_owner(),
            ..Default::default()
        };
        if !s.api_key.trim().is_empty() {
            c.api_key = Some(s.api_key.trim().to_string());
        }
        if !s.model.trim().is_empty() {
            c.model = s.model.trim().to_string();
        }
        if s.max_tokens > 0 {
            c.max_tokens = s.max_tokens;
        }
        if !s.base_url.trim().is_empty() {
            c.base_url = s.base_url.trim().to_string();
        }
        c.refusal_fallback = s.refusal_fallback;
        if s.provider == "openai-compatible" {
            c.provider = rustpy_ai::Provider::OpenAiCompatible;
            c.model = s.openai_model.clone();
            c.base_url = s.openai_base_url.clone();
            c.api_key =
                (!s.openai_api_key.trim().is_empty()).then(|| s.openai_api_key.trim().to_owned());
            c.max_tokens = s.max_tokens;
        }
        c
    }

    /// Selected text, else the whole document, with file name and language.
    fn ai_editor_context(&self) -> Option<(String, String, String, bool)> {
        let i = self.active?;
        let st = &self.tabs[i].state;
        let name = st
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| st.title());
        let lang = st.language_label().to_lowercase();
        if st.has_selection() {
            Some((st.doc.slice(st.primary().range()), name, lang, true))
        } else {
            Some((st.doc.text(), name, lang, false))
        }
    }

    /// Source of the function or class enclosing the caret (for docstrings).
    fn enclosing_def_source(&mut self) -> Option<String> {
        let i = self.active?;
        let caret = self.tabs[i].state.primary().head();
        let text = self.tabs[i].state.doc.text();
        let parse = self.tabs[i].state.syntax().clone();
        let node = parse.syntax_node();
        let tok = crate::editor::syntax_support::token_at(&node, caret).right_biased()?;
        let def = tok.parent_ancestors().find(|n| {
            matches!(
                n.kind(),
                rustpy_syntax::SyntaxKind::FUNC_DEF | rustpy_syntax::SyntaxKind::CLASS_DEF
            )
        })?;
        let r = def.text_range();
        Some(text[usize::from(r.start())..usize::from(r.end())].to_string())
    }

    fn ai_action(&mut self, action: Action) {
        let cfg = self.ai_config();
        let Some((code, name, lang, selection)) = self.ai_editor_context() else {
            self.set_status("Open a file to use AI actions");
            return;
        };
        let scope = if selection { "selection" } else { "file" };
        let (req, label) = match action {
            Action::AiExplainCode => (
                rustpy_ai::explain_code(&code, &lang, &name),
                format!("Explain Code ({name}, {scope})"),
            ),
            Action::AiSuggestRefactoring => (
                rustpy_ai::suggest_refactoring(&code),
                format!("Suggest Refactoring ({name}, {scope})"),
            ),
            Action::AiWriteTests => (
                rustpy_ai::write_tests(&code, "pytest"),
                format!("Generate Unit Tests ({name}, {scope})"),
            ),
            Action::AiGenerateDocstring => {
                let src = self.enclosing_def_source().unwrap_or(code);
                let style = match self.docstring_format() {
                    rustpy_semantic::docstrings::DocstringFormat::Google => "google",
                    rustpy_semantic::docstrings::DocstringFormat::NumPy => "numpy",
                    _ => "rest",
                };
                (
                    rustpy_ai::generate_docstring(&src, style),
                    format!("Generate Documentation ({name})"),
                )
            }
            Action::AiFixError => {
                let Some(i) = self.active else { return };
                let line = self.tabs[i]
                    .state
                    .doc
                    .line_of(self.tabs[i].state.primary().head());
                let diags: Vec<String> = self.tabs[i]
                    .state
                    .diagnostics()
                    .into_iter()
                    .filter(|d| {
                        self.tabs[i].state.doc.line_of(d.range.start()) == line
                            && d.severity >= rustpy_core::Severity::Warning
                    })
                    .map(|d| format!("line {}: {}", line + 1, d.message))
                    .collect();
                let error = if diags.is_empty() {
                    self.run_panel.last_error_text().unwrap_or_default()
                } else {
                    diags.join("\n")
                };
                if error.trim().is_empty() {
                    self.set_status("No error at the caret or in the last run to fix");
                    return;
                }
                (
                    rustpy_ai::fix_error(&code, &error),
                    format!("Fix with AI ({name})"),
                )
            }
            _ => return,
        };
        self.ai_panel.redirect = None;
        self.ai_panel.start(&cfg, req, String::new(), Some(label));
    }

    /// behave / pytest-bdd step definitions of the project (cached for a few seconds).
    fn step_definitions(&mut self) -> Vec<rustpy_gherkin::StepDef> {
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let now = EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64();
        if let Some((at, defs)) = &self.step_defs_cache {
            if now - at < 5.0 {
                return defs.clone();
            }
        }
        let mut defs = Vec::new();
        for p in self
            .project_files()
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "py"))
            .take(5000)
        {
            let open = self
                .tabs
                .iter()
                .find(|t| t.state.path.as_deref() == Some(p.as_path()))
                .map(|t| t.state.doc.text());
            let Some(text) = open.or_else(|| std::fs::read_to_string(&p).ok()) else {
                continue;
            };
            if rustpy_gherkin::looks_like_step_module(&text) {
                defs.extend(rustpy_gherkin::scan_step_definitions(&text, &p));
            }
        }
        self.step_defs_cache = Some((now, defs.clone()));
        defs
    }

    /// Ctrl+B on a Gherkin step: jump to its step definition.
    fn goto_step_definition(&mut self) -> bool {
        let Some(i) = self.active else { return false };
        if self.tabs[i].state.language != Language::Gherkin {
            return false;
        }
        let text = self.tabs[i].state.doc.text();
        let caret = usize::from(self.tabs[i].state.primary().head());
        let feature = rustpy_gherkin::parse(&text);
        let defs = self.step_definitions();
        let hit = feature
            .background
            .iter()
            .flat_map(|b| b.steps.iter().map(move |s| (None, s)))
            .chain(
                feature
                    .scenarios
                    .iter()
                    .flat_map(|sc| sc.steps.iter().map(move |s| (Some(sc), s))),
            )
            .find(|(_, s)| s.range.start <= caret && caret <= s.range.end);
        let Some((scenario, step)) = hit else {
            self.set_status("Place the caret on a step");
            return true;
        };
        let found: Vec<rustpy_gherkin::StepDef> = match scenario {
            Some(sc) => rustpy_gherkin::definitions_for_step(sc, step, &defs)
                .into_iter()
                .cloned()
                .collect(),
            None => rustpy_gherkin::find_step_definitions(step, &defs)
                .into_iter()
                .cloned()
                .collect(),
        };
        match found.first() {
            Some(d) => {
                let (path, line) = (d.path.clone(), d.line.saturating_sub(1));
                self.goto_file_line(&path, line);
            }
            None => self.set_status(format!(
                "No step definition matches \"{}\" (Alt+Enter creates one)",
                step.text
            )),
        }
        true
    }

    /// Undefined steps of a `.feature` file as weak warnings with a create fix.
    fn gherkin_diagnostics(&mut self, i: usize) {
        let text = self.tabs[i].state.doc.text();
        let feature = rustpy_gherkin::parse(&text);
        let defs = self.step_definitions();
        let mut diags = Vec::new();
        let mut fixes = Vec::new();
        for step in rustpy_gherkin::unresolved_steps(&feature, &defs) {
            let range = TextRange::new(
                TextSize::new(step.text_range.start as u32),
                TextSize::new(step.text_range.end as u32),
            );
            diags.push(rustpy_core::Diagnostic {
                range,
                severity: rustpy_core::Severity::WeakWarning,
                message: format!("Undefined step reference: {}", step.text),
                code: "BehaveUndefinedStep".into(),
            });
            fixes.push((
                range,
                vec![rustpy_analysis::inspections::QuickFix {
                    title: format!("Create step definition: {} {}", step.keyword, step.text),
                    edits: Vec::new(),
                }],
            ));
        }
        for d in &feature.diagnostics {
            let range = TextRange::new(
                TextSize::new(d.range.start as u32),
                TextSize::new(d.range.end as u32),
            );
            diags.push(rustpy_core::Diagnostic {
                range,
                severity: rustpy_core::Severity::Error,
                message: d.message.clone(),
                code: "GherkinSyntax".into(),
            });
        }
        let st = &mut self.tabs[i].state;
        st.extra_diagnostics = diags;
        st.quick_fixes = fixes;
        st.semantic_version = Some(st.doc.version());
    }

    /// Appends a behave stub for `step_line` ("Given I have 3 apples") to the steps module.
    fn create_step_definition(&mut self, step_line: &str) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        let feature =
            rustpy_gherkin::parse(&format!("Feature: f\n  Scenario: s\n    {step_line}\n"));
        let Some(step) = feature
            .scenarios
            .first()
            .and_then(|s| s.steps.first())
            .cloned()
        else {
            return;
        };
        let defs = self.step_definitions();
        let target = defs
            .first()
            .map(|d| d.path.clone())
            .unwrap_or_else(|| root.join("features/steps/steps.py"));
        let stub = rustpy_gherkin::step_definition_stub(&step, rustpy_gherkin::Matcher::Parse);
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        let mut text = if existing.trim().is_empty() {
            "from behave import given, when, then, step".to_string()
        } else {
            existing.trim_end().to_string()
        };
        // Two blank lines before a top-level definition (PEP 8).
        text.push_str("\n\n\n");
        text.push_str(&stub);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let result = git_workflows::file_change(&target, "Creating step definition", || {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &text)
        });
        match result {
            Ok(()) => {
                self.step_defs_cache = None;
                if let Some(t) = self
                    .tabs
                    .iter()
                    .position(|t| t.state.path.as_deref() == Some(target.as_path()))
                {
                    self.reload_tab_from_disk(t);
                }
                let line = text.lines().count().saturating_sub(stub.lines().count());
                self.goto_file_line(&target, line);
                self.set_status(format!("Step definition created in {}", target.display()));
            }
            Err(e) => self.set_status(format!("Cannot write {}: {e}", target.display())),
        }
    }

    /// Re-scans the project's Python files for HTTP routes.
    fn scan_endpoints(&mut self) {
        let files: Vec<PathBuf> = self
            .project_files()
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "py"))
            .take(5000)
            .collect();
        let sources: Vec<(PathBuf, String)> = files
            .iter()
            .filter_map(|p| {
                let open = self
                    .tabs
                    .iter()
                    .find(|t| t.state.path.as_deref() == Some(p.as_path()))
                    .map(|t| t.state.doc.text());
                open.or_else(|| std::fs::read_to_string(p).ok())
                    .map(|t| (p.clone(), t))
            })
            .collect();
        self.endpoints_panel.files_scanned = sources.len();
        self.endpoints_panel.endpoints = rustpy_semantic::endpoints::scan_project(&sources);
        self.endpoints_panel.scanned = true;
    }

    /// Opens `path` with the caret at the start of 0-based `line`.
    fn goto_file_line(&mut self, path: &Path, line: usize) {
        self.mark_nav_jump();
        self.open_file(path);
        if let Some(tab) = self
            .opened_editor_index(path)
            .and_then(|i| self.tabs.get_mut(i))
        {
            tab.view.cancel_session_restore();
            tab.state
                .move_primary_to(rustpy_core::Position::new(line as u32, 0));
            tab.state.scroll_to_caret = true;
        }
        self.focus_editor = true;
    }

    /// Project files without folders marked Excluded, plus attached content
    /// roots (Find Usages, Rename, hierarchies, …; see `project_index`).
    fn project_files(&self) -> Vec<PathBuf> {
        self.scoped_project_files()
            .into_iter()
            .filter(|path| {
                !path.components().any(|part| {
                    rustpy_settings::editor_pages::matches(
                        &self.settings.editor.preferences.ignored_patterns,
                        Path::new(part.as_os_str()),
                    )
                })
            })
            .collect()
    }

    /// Navigates to a single location, or lists several in the Find tool window.
    fn navigate_locations(&mut self, locs: Vec<features::Location>, empty_msg: &str) {
        match locs.len() {
            0 => self.set_status(empty_msg),
            1 => self.navigate_to(&locs[0].path.clone(), locs[0].range),
            _ => {
                // Lines from the editor or analysis text: stubs are not on disk, unsaved text differs.
                let items: Vec<(String, String)> = locs
                    .iter()
                    .map(|l| {
                        (
                            l.path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            format!(
                                "{}:{}",
                                self.display_path(&l.path),
                                self.location_line(&l.path, l.range.start())
                            ),
                        )
                    })
                    .collect();
                self.extract_candidates.clear();
                self.location_choices = locs;
                self.choice = Some(ChoicePopup::new(
                    ChoiceKind::Locations,
                    "Choose Declaration".into(),
                    items,
                ));
            }
        }
    }

    /// Pushes the active document's text into the analysis immediately (for chained refactorings).
    fn sync_analysis_now(&mut self) {
        if self.active.is_some_and(|i| self.remote_bound(i)) {
            return;
        }
        self.apply_documentation_policy();
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return;
        };
        if self
            .active
            .is_some_and(|i| self.is_large_file_placeholder(i))
        {
            return;
        }
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        if tab.state.language == Language::Python && !path.starts_with("<typeshed>") {
            self.bg
                .text_sync
                .push_now(&analysis, &path, &tab.state.doc.text());
            tab.state.synced_version = Some(tab.state.doc.version());
        }
    }

    fn analysis_file(&self, i: usize) -> Option<(Arc<Analysis>, Arc<rustpy_semantic::FileInfo>)> {
        if self.remote_bound(i) {
            return None;
        }
        self.apply_documentation_policy();
        let analysis = self.project.as_ref()?.analysis.clone();
        // A large-file placeholder holds no text (not loaded): nothing to analyse.
        if self.is_large_file_placeholder(i) {
            return None;
        }
        let tab = self.tabs.get(i)?;
        let path = tab.state.path.clone()?;
        // The editor can be a keystroke ahead of the analysis copy (it is synced
        // once per frame); features run right after an edit must see the new text.
        if tab.state.language == Language::Python
            && tab.state.synced_version != Some(tab.state.doc.version())
            && !path.starts_with("<typeshed>")
        {
            let text = tab.state.doc.text();
            // A background push of older text must not land after this one.
            self.bg.text_sync.finish(&path);
            if analysis
                .file_for_path(&path)
                .is_none_or(|f| *f.text != *text)
            {
                analysis.set_file_text(&path, &text);
            }
        }
        let file = analysis.file_for_path(&path)?;
        Some((analysis, file))
    }

    /// Records the current position and jumps to `path:range`.
    fn navigate_to(&mut self, path: &Path, range: TextRange) {
        self.mark_nav_jump();
        self.open_file(path);
        let normalized = if path.starts_with("<typeshed>") {
            path.to_path_buf()
        } else {
            rustpy_core::vfs::normalize_path(path)
        };
        if let Some(tab) = self
            .active
            .and_then(|i| self.tabs.get_mut(i))
            .filter(|tab| tab.state.path.as_deref() == Some(normalized.as_path()))
        {
            tab.view.cancel_session_restore();
            let start = tab.state.doc.clamp(range.start());
            let end = tab.state.doc.clamp(range.end());
            tab.state.reveal_offset(start);
            tab.state
                .set_carets(vec![crate::editor::Caret::new(start, end)]);
            tab.state.last_op = crate::editor::state::LastOp::Other;
        }
        self.focus_editor = true;
    }

    fn go_to_declaration(&mut self, offset: TextSize) {
        if self.template_url_declaration(offset) {
            return;
        }
        if self.jinja_macro_navigation(offset) {
            return;
        }
        if self.template_link_navigation(offset) {
            return;
        }
        if self.web_navigation(offset) {
            return;
        }
        let Some(i) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let locs = features::definition_at_with_options(
            &analysis,
            &file,
            offset,
            self.settings.editor.prefer_source_navigation,
        );
        // Already on the declaration: "Go to Declaration or Usages" shows its usages.
        if locs
            .iter()
            .any(|l| l.path == file.path && l.range.contains_inclusive(offset))
        {
            self.show_usages_at(offset);
            return;
        }
        self.navigate_locations(locs, "Cannot find declaration to go to");
    }

    fn open_completion(&mut self, explicit: bool) {
        crate::performance_trace::completion_requested();
        if self.open_remote_completion(explicit) {
            return;
        }
        if self.web_completion(explicit) {
            return;
        }
        if self.open_plugin_completion(explicit) {
            return;
        }
        if self.open_deployment_completion(explicit) {
            return;
        }
        if self.open_markdown_completion(explicit) {
            return;
        }
        if self.open_data_completion(explicit) {
            return;
        }
        if self.framework_document_completion(explicit) {
            return;
        }
        if self.open_doctest_completion(explicit) {
            return;
        }
        if self.open_rst_completion(explicit) {
            return;
        }
        if self.open_editorconfig_completion(explicit) {
            return;
        }
        if self.open_basic_completion(explicit) {
            return;
        }
        if self.open_prose_completion(explicit) {
            return;
        }
        if self.open_path_completion(explicit) {
            return;
        }
        self.completion_ui.due = None;

        self.import_completion = None;
        let Some(i) = self.active else { return };
        // The installed/language-specific providers above own non-Python
        // documents even when they have no matching suggestions. Their absence
        // must not reinterpret HTML/CSS/JS as Python and offer Python imports.
        if self.tabs[i].state.language != Language::Python {
            // No provider claimed this caret: drop the previous lookup's
            // items too, so no other document or context inherits them.
            self.completion.visible = false;
            self.completion.items.clear();
            self.completion.imports.clear();
            return;
        }
        let background = !explicit && self.completes_in_background(i);
        let analysis_file = if background {
            None
        } else {
            let Some(found) = self.analysis_file(i) else {
                return;
            };
            Some(found)
        };
        let tab = &self.tabs[i];
        let caret = tab.state.primary().head();
        let text = tab.state.doc.text();
        let off = usize::from(caret).min(text.len());
        let before = &text[..off];
        let prefix_start = before
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map(|i| {
                i + before[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1)
            })
            .unwrap_or(0);
        // Second press of Ctrl+Space (also when the first press found nothing
        // and the lookup stayed closed), as of the request a background result
        // answers.
        let second_press = self.completion_worker.take_resume().unwrap_or_else(|| {
            self.completion.explicit
                && (self.completion.visible
                    || (self.completion.doc_version == tab.state.doc.version()
                        && self.completion.caret == caret))
        });
        let (mut items, analysis_file) = match analysis_file {
            Some((analysis, file)) => {
                let Some(items) = self.python_completion_candidates(
                    &analysis,
                    &file,
                    caret,
                    prefix_start,
                    explicit,
                    second_press,
                    false,
                ) else {
                    // Computing off the UI thread: the lookup shows its loading row
                    // meanwhile, so the previous lookup's items cannot be committed.
                    self.show_loading_lookup(TextSize::new(prefix_start as u32), explicit);
                    return;
                };
                (items, Some((analysis, file)))
            }
            None => {
                // A large file while typing: the worker analyses its text.
                let Some(lookup) = self.large_file_lookup(i, prefix_start) else {
                    self.show_loading_lookup(TextSize::new(prefix_start as u32), explicit);
                    return;
                };
                let completion_worker::Found::Items(items) = &lookup.found else {
                    return;
                };
                // The import lookup needs a file of exactly this text.
                let analysis = self.project.as_ref().map(|p| p.analysis.clone());
                let file = lookup
                    .file
                    .clone()
                    .filter(|file| *file.text == *text)
                    .or_else(|| {
                        let path = self.tabs[i].state.path.as_deref()?;
                        analysis
                            .as_ref()?
                            .file_for_path(path)
                            .filter(|file| *file.text == *text)
                    });
                (items.clone(), analysis.zip(file))
            }
        };
        // Live/postfix templates and the import lookup apply in code only.
        let in_code = !before[prefix_start..].is_empty()
            && self.tabs[i].state.completion_context_at(caret).is_code();
        let tab = &self.tabs[i];
        let prefix = &before[prefix_start..];
        // Live and postfix templates only apply in code (not comments, strings or new names).
        if !prefix.is_empty() && in_code {
            if prefix_start > 0 && before.as_bytes()[prefix_start - 1] == b'.' {
                if templates::expression_before_dot(&text, prefix_start - 1).is_some() {
                    for t in templates::matching_postfix(prefix).into_iter().filter(|t| {
                        self.settings.editor.preferences.postfix_enabled
                            && !self
                                .settings
                                .editor
                                .preferences
                                .postfix_disabled
                                .iter()
                                .any(|key| key == t.key)
                    }) {
                        items.push(CompletionItem {
                            label: t.key.into(),
                            kind: CompletionKind::Postfix,
                            detail: t.description.to_string(),
                            insert: t.key.to_string(),
                            caret_offset: None,
                            priority: 200,
                        });
                    }
                }
            } else {
                let ctx = self.template_context(i);
                for t in self.templates.iter().filter(|t| {
                    t.abbreviation.starts_with(prefix) && t.applies_in(ctx) && !t.is_surround()
                }) {
                    items.push(CompletionItem {
                        label: t.abbreviation.clone().into(),
                        kind: CompletionKind::Template,
                        detail: t.description.clone(),
                        insert: t.abbreviation.clone(),
                        caret_offset: None,
                        priority: 150,
                    });
                }
            }
        }
        let prefs = &self.settings.completion;
        let cs = rustpy_semantic::completion2::CompletionSettings {
            show_as_you_type: prefs.show_as_you_type,
            case_sensitive: prefs.case_sensitive,
            match_middle: prefs.match_middle,
            hide_used: prefs.hide_used,
        };
        let items = if prefix.is_empty() {
            items
        } else {
            rustpy_semantic::completion2::filter_items(items, prefix, &cs)
        };
        let mut items = self.order_completion_items(items, prefix);
        // Second press of Ctrl+Space: add importable names from the project and the libraries.
        let mut imports: Vec<Option<(String, String)>> = vec![None; items.len()];
        self.append_plugin_completions(i, prefix, &mut items, &mut imports);
        let import_scope =
            index_ui::import_completion_scope(explicit, second_press, items.is_empty(), prefix);
        if std::env::var("RUSTPY_DEBUG_COMPLETION").is_ok() {
            log::info!(
                "debug-completion: open explicit={explicit} prefix={prefix:?} prefix_start={prefix_start} caret={caret:?} first={:?}",
                items
                    .iter()
                    .take(3)
                    .map(|i| (i.label.to_string(), i.insert.clone()))
                    .collect::<Vec<_>>()
            );
        }
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            items,
            imports,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: TextSize::new(prefix_start as u32),
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            rect: None,
            explicit,
        };
        if let (Some(with_project), Some((analysis, file))) = (import_scope, analysis_file.clone())
        {
            self.start_import_completion(analysis, file, caret, with_project, false);
        } else if let Some((analysis, file)) = analysis_file.filter(|_| {
            import_scope.is_none()
                && self.settings.code_style.show_import_popup
                && (if explicit {
                    !prefix.is_empty()
                } else {
                    prefix.chars().count() >= 3
                })
                && in_code
        }) {
            self.start_import_completion(analysis, file, caret, true, true);
        } else if explicit && !self.completion.visible {
            self.set_status("No suggestions");
        }
    }

    /// Whether the lookup may open by itself while typing: Settings › Code
    /// Completion › "Show suggestions as you type" is on and the caret is in
    /// code (not in a comment, string, number or a name being defined, nor
    /// in a name being renamed in place).
    fn completion_auto_popup_allowed(&mut self, i: usize) -> bool {
        if self.tabs.get(i).is_none()
            || !self.background_features().completion_popup
            || self.inplace_rename_in(i)
        {
            return false;
        }
        // Token-level checks only (a large file lexes the caret's line).
        let state = &mut self.tabs[i].state;
        let caret = state.primary().head();
        state.completion_context_at(caret).allows_auto_popup()
            || doctest_ui::is_doctest_prompt_at(state, caret)
    }

    /// Ctrl+Shift+Space: completion ranked by the expected type at the caret.
    fn open_smart_completion(&mut self) {
        let Some(i) = self.active else { return };
        if self.tabs[i].state.language != Language::Python {
            self.open_completion(true);
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let tab = &self.tabs[i];
        let caret = tab.state.primary().head();
        let text = tab.state.doc.text();
        let off = usize::from(caret).min(text.len());
        let before = &text[..off];
        let prefix_start = before
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map(|i| {
                i + before[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1)
            })
            .unwrap_or(0);
        let Some(items) = self.python_completion_candidates(
            &analysis,
            &file,
            caret,
            prefix_start,
            true,
            false,
            true,
        ) else {
            self.show_loading_lookup(TextSize::new(prefix_start as u32), true);
            return;
        };
        let tab = &self.tabs[i];
        let items = self.order_completion_items(items, &text[prefix_start..off]);
        let n = items.len();
        self.completion = CompletionPopup {
            visible: n > 0,
            items,
            imports: vec![None; n],
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: TextSize::new(prefix_start as u32),
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            rect: None,
            explicit: true,
        };
        if n == 0 {
            self.set_status("No smart suggestions here");
        }
    }

    /// Inserts the selected lookup item. `replace` (Tab) also replaces the rest
    /// of the identifier after the caret.
    fn accept_completion(&mut self, replace: bool) {
        if self.accept_web_completion() {
            return;
        }
        if self.non_text_source_hidden() {
            self.completion.visible = false;
            self.import_completion = None;
            return;
        }
        let Some(item) = self.completion.items.get(self.completion.selected).cloned() else {
            return;
        };
        self.remember_completion(&item.label);
        if self.accept_virtual_completion(&item, replace) {
            return;
        }
        // In a name being renamed in place only the item's name goes in (no import, template or parentheses).
        let naming = self.active.is_some_and(|i| self.inplace_rename_in(i));
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let caret = tab.state.primary().head();
        let start = self.completion.prefix_start.min(caret);
        let mut end = caret;
        let segment = self.completion_ui.paths;
        if replace {
            end = TextSize::new(rustpy_semantic::basic_completion::replacement_end(
                &tab.state.doc.text(),
                usize::from(caret),
                segment,
            ) as u32);
        }
        let range = TextRange::new(start, end);
        // "Insert parentheses when completing callables"; no second `(` before an existing one.
        let (mut insert, mut insert_caret) = features::insert_text_for(
            &item,
            self.settings.completion.insert_parentheses,
            tab.state.doc.char_at(end),
        );
        if let Some(Some((module, name))) = self
            .completion
            .imports
            .get(self.completion.selected)
            .cloned()
            .filter(|_| !naming)
        {
            // Auto-import: insert the symbol and the import statement together.
            self.completion.visible = false;
            self.import_completion = None;
            if let Some(i) = self.active {
                if let Some((_, file)) = self.analysis_file(i) {
                    let qualified =
                        !self.settings.editor.preferences.prefer_from_imports && !name.is_empty();
                    if qualified {
                        let shift = module.len() + 1;
                        insert = format!("{module}.{insert}");
                        insert_caret = insert_caret.map(|offset| offset + shift);
                    }
                    let (irange, itext) = rustpy_semantic::completion2::import_edit_for(
                        &file,
                        &module,
                        if qualified { "" } else { &name },
                    );
                    let import_shift = if irange.end() <= start {
                        itext.len() as i64 - usize::from(irange.len()) as i64
                    } else {
                        0
                    };
                    let desired_caret = (i64::from(u32::from(start))
                        + import_shift
                        + insert_caret.unwrap_or(insert.len()) as i64)
                        .max(0) as u32;
                    let mut edits = vec![(range, insert.clone())];
                    if !itext.is_empty() {
                        edits.push((irange, itext));
                    }
                    self.tabs[i]
                        .state
                        .apply_text_edits(edits, "Complete with import");
                    let target = self.tabs[i].state.doc.clamp(TextSize::new(desired_caret));
                    self.tabs[i]
                        .state
                        .set_carets(vec![crate::editor::Caret::at(target)]);
                    if self.settings.completion.show_parameter_info && insert.contains('(') {
                        self.update_param_info(false);
                    }
                    let label = if name.is_empty() {
                        format!("import {module}")
                    } else {
                        format!("from {module} import {name}")
                    };
                    self.set_status(format!("Added {label}"));
                }
            }
            return;
        }
        if item.kind == CompletionKind::Template && !naming {
            self.completion.visible = false;
            self.import_completion = None;
            if let Some(t) = self
                .templates
                .iter()
                .find(|t| t.abbreviation == item.label.as_str())
                .cloned()
            {
                self.expand_template(&t, range, None);
            }
            return;
        }
        if item.kind == CompletionKind::Postfix && !naming {
            self.completion.visible = false;
            self.import_completion = None;
            self.apply_postfix(&item.insert, usize::from(start).saturating_sub(1));
            return;
        }
        if std::env::var("RUSTPY_DEBUG_COMPLETION").is_ok() {
            log::info!(
                "debug-completion: accept {:?} insert={:?} range={:?} caret={:?} text_before={:?}",
                item.label,
                item.insert,
                range,
                caret,
                tab.state.doc.slice(TextRange::new(TextSize::new(0), caret))
            );
        }
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        if segment {
            let position = start + TextSize::new(insert_caret.unwrap_or(insert.len()) as u32);
            tab.state.apply(
                vec![rustpy_text::Edit::replace(range, insert)],
                vec![crate::editor::Caret::at(position)],
                rustpy_text::EditGroup::Separate,
                Some("Complete"),
            );
            self.completion.visible = false;
            self.import_completion = None;
            return;
        }
        // Every caret typing the same prefix gets the item (several carets, an in-place rename).
        let parens = self.settings.completion.insert_parentheses;
        let (edits, carets) = tab.state.completion_edits(start, replace, |next| {
            if naming {
                (item.label.to_string(), None)
            } else {
                features::insert_text_for(&item, parens, next)
            }
        });
        tab.state.apply(
            edits,
            carets,
            rustpy_text::EditGroup::Separate,
            Some("Complete"),
        );
        tab.state.last_op = crate::editor::state::LastOp::Other;
        self.completion.visible = false;
        self.import_completion = None;
        if self.settings.completion.show_parameter_info && insert.contains('(') {
            self.update_param_info(false);
        }
    }

    /// Statement context when only whitespace precedes the word at the caret on its line.
    fn template_context(&self, i: usize) -> TemplateContext {
        let tab = &self.tabs[i];
        let language = match tab.state.language {
            Language::Python => "python",
            Language::Cython => "cython",
            Language::Html => "html",
            Language::JavaScript => "javascript",
            Language::Template => "django",
            _ => "other",
        };
        templates::management::context_at(
            language,
            &tab.state.doc.text(),
            usize::from(tab.state.primary().head()),
        )
    }

    fn expand_context(
        &mut self,
        i: usize,
        at: TextSize,
        selection: Option<String>,
    ) -> ExpandContext {
        let clipboard = self.clipboard_text();
        let tab = &mut self.tabs[i];
        let line = tab.state.doc.line_of(at);
        let text = tab.state.doc.line_text(line);
        let indent: String = text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let file_name = tab
            .state
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.py".into());
        let (class_name, function_name) = {
            let crumbs = crate::editor::syntax_support::breadcrumbs(tab.state.syntax(), at);
            let class_name = crumbs
                .iter()
                .rev()
                .find(|(c, _)| !c.ends_with(')'))
                .map(|(c, _)| c.clone());
            let function_name = crumbs
                .iter()
                .rev()
                .find(|(c, _)| c.ends_with(')'))
                .map(|(c, _)| c.trim_end_matches("()").to_string());
            (class_name, function_name)
        };
        ExpandContext {
            indent,
            selection,
            file_name,
            class_name,
            function_name,
            user: std::env::var("USER").unwrap_or_else(|_| "user".into()),
            clipboard,
            line_number: line + 1,
        }
    }

    /// Replaces `replace` with the expanded template and starts a tab-stop session.
    fn expand_template(&mut self, t: &LiveTemplate, replace: TextRange, selection: Option<String>) {
        let Some(i) = self.active else { return };
        let ctx = self.expand_context(i, replace.start(), selection);
        let mut inputs = self.live_template_inputs(i, replace.start());
        // complete() is queried at its inserted stop, which may follow a member-access dot.
        inputs.completion.clear();
        let choices = templates::management::expand_with_inputs(t, &ctx, &inputs).choices;
        self.reset_live_template_choices();
        let tab = &mut self.tabs[i];
        let placement = if tab.state.has_virtual_padding() {
            let mut placement = None;
            let success = tab
                .state
                .try_edit_in_virtual_space("Live template", |state| {
                    let replace = state.primary().range();
                    let mut ctx = ctx.clone();
                    ctx.indent = state
                        .doc
                        .line_text(state.doc.line_of(replace.start()))
                        .chars()
                        .take_while(|c| *c == ' ' || *c == '\t')
                        .collect();
                    let exp = templates::management::expand_with_inputs(t, &ctx, &inputs).expansion;
                    placement = virtual_insertions::apply_expansion(state, &exp, replace);
                    placement.is_some()
                });
            if success != Some(true) {
                return;
            }
            placement
        } else {
            virtual_insertions::apply_expansion(
                &mut tab.state,
                &templates::management::expand_with_inputs(t, &ctx, &inputs).expansion,
                replace,
            )
        };
        let Some((vars, end)) = placement else {
            return;
        };
        tab.state.last_op = crate::editor::state::LastOp::Other;
        self.live_template_management.choice_tab = Some(tab.view.id);
        self.live_template_management.choices = choices;
        self.live_template_management.dynamic_variables = t
            .variables
            .iter()
            .filter(|variable| {
                matches!(
                    variable.expression.trim(),
                    "complete()" | "pyIterableVariable()"
                )
            })
            .map(|variable| {
                (
                    variable.name.clone(),
                    variable.expression.trim().to_string(),
                )
            })
            .collect();
        self.template_session = if vars.is_empty() {
            None
        } else {
            Some(TemplateSession {
                tab_id: tab.view.id,
                vars,
                idx: 0,
                end,
                version: tab.state.doc.version(),
            })
        };
        self.focus_editor = true;
        self.refresh_live_template_choices();
    }

    fn apply_postfix(&mut self, key: &str, dot_off: usize) {
        if !self.settings.editor.preferences.postfix_enabled
            || self
                .settings
                .editor
                .preferences
                .postfix_disabled
                .iter()
                .any(|disabled| disabled == key)
        {
            return;
        }
        let Some(i) = self.active else { return };
        let tab = &mut self.tabs[i];
        let text = tab.state.doc.text();
        let line = tab.state.doc.line_of(TextSize::new(dot_off as u32));
        let lt = tab.state.doc.line_text(line);
        let indent: String = lt.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        if let Some(r) = templates::apply_postfix(&text, dot_off, key, &indent) {
            let caret = r.edit_range.start() + TextSize::new(r.caret as u32);
            tab.state.apply(
                vec![rustpy_text::Edit::replace(r.edit_range, r.text.clone())],
                vec![crate::editor::Caret::at(caret)],
                rustpy_text::EditGroup::Separate,
                Some("Postfix completion"),
            );
            tab.state.last_op = crate::editor::state::LastOp::Other;
        }
    }

    /// Applies file operations from multi-file refactorings (edits to open tabs or disk, create/move/delete).
    fn apply_file_ops(&mut self, ops: Vec<rustpy_refactor::refactor2::FileOp>, title: &str) {
        if self.settings.refactoring.preview_refactoring {
            self.preview_file_ops(ops, title);
        } else {
            self.apply_file_ops_failures(ops, title);
        }
    }

    /// Applies a complete plan, rolling earlier changes back if any operation fails.
    fn apply_file_ops_failures(
        &mut self,
        ops: Vec<rustpy_refactor::refactor2::FileOp>,
        title: &str,
    ) -> Vec<String> {
        let changes = ops
            .iter()
            .filter_map(|operation| match operation {
                rustpy_refactor::refactor2::FileOp::Create(path, _) => Some((path.clone(), false)),
                rustpy_refactor::refactor2::FileOp::Delete(path) => Some((path.clone(), true)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let failures = self.apply_file_ops_atomically(ops, title);
        if failures.is_empty() {
            for (path, removed) in changes {
                if removed {
                    self.vcs_file_deleted(&path);
                } else {
                    self.vcs_file_created(&path);
                }
            }
        }
        failures
    }

    fn apply_multi(&mut self, r: Result<rustpy_refactor::refactor2::MultiFileRefactoring, String>) {
        match r {
            Ok(m) => {
                let ops = m
                    .changes
                    .into_iter()
                    .map(|(p, e)| rustpy_refactor::refactor2::FileOp::Edit(p, e))
                    .collect();
                let title = m.title.clone();
                self.apply_file_ops(ops, &title);
            }
            Err(e) => self.set_status(format!("Cannot refactor: {e}")),
        }
    }

    /// Applies a refactoring result: edits, then an in-place rename session on the new name.
    fn apply_refactoring(&mut self, r: Result<Refactoring, String>) {
        if self.settings.refactoring.preview_refactoring {
            match r {
                Ok(refactoring) => self.preview_single_refactoring(refactoring),
                Err(error) => self.set_status(format!("Cannot refactor: {error}")),
            }
        } else {
            self.apply_refactoring_now(r);
        }
    }

    fn apply_refactoring_now(&mut self, r: Result<Refactoring, String>) {
        if self.non_text_source_hidden() {
            self.set_status("Choose Source to apply text changes");
            return;
        }
        match r {
            Ok(r) => {
                let title = r.title.clone();
                if let Some(i) = self.active {
                    self.snapshot(i, &format!("Before {title}"));
                }
                if let Some(i) = self.active.filter(|i| *i < self.tabs.len()) {
                    self.finish_inplace_rename();
                    let transaction = self.tabs[i].state.begin_transaction(&title).ok();
                    self.tabs[i].state.apply_text_edits(r.edits.clone(), &title);
                    // The new name is renamed in place; Enter finishes.
                    match r.rename_ranges.first() {
                        Some(first) => {
                            let name = self.tabs[i].state.doc.slice(*first);
                            self.start_inplace_rename_transaction(
                                i,
                                r.rename_ranges.clone(),
                                first.start(),
                                inplace_rename::InplaceKind::Introduced(title),
                                rustpy_refactor::rename::RenameCheck::identifier_only(&name),
                                transaction,
                            );
                        }
                        None => {
                            if let Some(token) = transaction {
                                let _ = self.tabs[i].state.commit_transaction(token);
                            }
                            self.tabs[i].state.doc.mark_refactoring();
                            self.set_status(title);
                        }
                    }
                }
                self.focus_editor = true;
            }
            Err(e) => self.set_status(format!("Cannot refactor: {e}")),
        }
    }

    fn run_extract(&mut self, kind: ExtractKind, range: TextRange) {
        // Asks "this occurrence / all occurrences" first when the expression repeats.
        self.run_extract_with(kind, range, None);
    }

    /// Extract Variable/Constant/Field/Parameter: uses the selection, or offers the expressions around the caret.
    fn start_extract(&mut self, kind: ExtractKind) {
        let Some(i) = self.active else { return };
        let Some((_, file)) = self.analysis_file(i) else {
            self.set_status("Refactorings are available for Python files in a project");
            return;
        };
        let k = self.tabs[i].state.primary();
        if k.has_selection() {
            self.run_extract(kind, k.range());
            return;
        }
        let cands = refactor::expression_candidates(&file, k.head());
        match cands.len() {
            0 => self.set_status("Cannot find an expression to extract at the caret"),
            1 => self.run_extract(kind, cands[0]),
            _ => {
                let text = self.tabs[i].state.doc.text();
                let items: Vec<(String, String)> = cands
                    .iter()
                    .enumerate()
                    .map(|(n, r)| {
                        let mut t =
                            text[usize::from(r.start())..usize::from(r.end())].replace('\n', " ");
                        if t.chars().count() > 70 {
                            t = t.chars().take(67).collect();
                            t.push('…');
                        }
                        (format!("{}", n + 1), t)
                    })
                    .collect();
                self.extract_candidates = cands;
                self.choice = Some(ChoicePopup::new(
                    ChoiceKind::Extract(kind),
                    "Select the expression to extract".into(),
                    items,
                ));
            }
        }
    }

    fn open_refactor_this(&mut self) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let has_sel = tab.state.primary().has_selection();
        let mut items: Vec<(String, String)> = vec![
            ("Rename…".into(), "Shift+F6".into()),
            ("Extract Variable…".into(), "Ctrl+Alt+V".into()),
            ("Extract Constant…".into(), "Ctrl+Alt+C".into()),
            ("Extract Field…".into(), "Ctrl+Alt+F".into()),
            ("Extract Parameter…".into(), "Ctrl+Alt+P".into()),
        ];
        if has_sel {
            items.push(("Extract Method…".into(), "Ctrl+Alt+M".into()));
        }
        items.push(("Inline…".into(), "Ctrl+Alt+N".into()));
        items.push(("Convert…".into(), String::new()));
        items.push(("Change Signature…".into(), "Ctrl+F6".into()));
        items.push(("Move…".into(), "F6".into()));
        items.push(("Safe Delete…".into(), "Alt+Delete".into()));
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::RefactorThis,
            "Refactor This".into(),
            items,
        ));
    }

    /// Code → Update Copyright: apply the configured profile for this file.
    fn update_copyright(&mut self) {
        self.update_copyright_profile();
    }

    /// Image viewer tab: zoom, checkerboard background, size info.
    fn image_viewer(&mut self, ui: &mut egui::Ui, i: usize) {
        if ui.button("Open externally").clicked() {
            if let Some(path) = self.tabs[i].state.path.clone() {
                self.open_associated_editor_file(&path, true, "");
            }
        }
        let theme = self.theme.clone();
        let ctx = ui.ctx().clone();
        let Some(img) = self.tabs[i].image.as_mut() else {
            return;
        };
        if img.texture.is_none() {
            let color = egui::ColorImage::from_rgba_unmultiplied(img.size, &img.rgba);
            img.texture =
                Some(ctx.load_texture(format!("image-{i}"), color, egui::TextureOptions::LINEAR));
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} × {} px", img.size[0], img.size[1]))
                    .color(theme.text_dim),
            );
            if ui.button("−").clicked() {
                img.zoom = (img.zoom / 1.25).max(0.05);
            }
            ui.label(format!("{:.0}%", img.zoom * 100.0));
            if ui.button("+").clicked() {
                img.zoom = (img.zoom * 1.25).min(32.0);
            }
            if ui.button("1:1").clicked() {
                img.zoom = 1.0;
            }
            if ui.button("Fit").clicked() {
                let avail = ui.available_size();
                let fit = (avail.x.max(1.0) / img.size[0].max(1) as f32)
                    .min(600.0 / img.size[1].max(1) as f32);
                img.zoom = fit.clamp(0.05, 8.0);
            }
            ui.checkbox(&mut img.checkerboard, "Chessboard");
            ui.checkbox(&mut img.pixel_grid, "Pixel grid")
                .on_hover_text("Show source-pixel boundaries at 400% zoom and above.");
        });
        ui.separator();
        let zoom = img.zoom;
        let checker = img.checkerboard;
        let grid = img.pixel_grid;
        let pixels = img.size;
        let size = egui::Vec2::new(img.size[0] as f32 * zoom, img.size[1] as f32 * zoom);
        let tex = img.texture.clone().unwrap();
        egui::ScrollArea::both()
            .id_salt(("image_view", i))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(size.max(ui.available_size()), Sense::hover());
                let img_rect = Rect::from_min_size(rect.min, size);
                if checker {
                    let cell = 12.0;
                    let painter = ui.painter().with_clip_rect(img_rect);
                    let (c1, c2) = (Color32::from_gray(0x66), Color32::from_gray(0x99));
                    let mut y = img_rect.top();
                    let mut row = 0;
                    while y < img_rect.bottom() {
                        let mut x = img_rect.left();
                        let mut col = 0;
                        while x < img_rect.right() {
                            painter.rect_filled(
                                Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell)),
                                0.0,
                                if (row + col) % 2 == 0 { c1 } else { c2 },
                            );
                            x += cell;
                            col += 1;
                        }
                        y += cell;
                        row += 1;
                    }
                }
                ui.painter().image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                if grid {
                    image_grid::paint(ui, img_rect, pixels, zoom);
                }
            });
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if ui.input(|i| i.modifiers.ctrl) && wheel != 0.0 {
            if let Some(img) = self.tabs[i].image.as_mut() {
                img.zoom = (img.zoom * if wheel > 0.0 { 1.1 } else { 0.9 }).clamp(0.05, 32.0);
            }
        }
    }

    /// Detached editor tab rendered in its own OS window.
    fn detached_window(&mut self, ctx: &egui::Context) {
        self.reconcile_document_markers();
        let Some(i) = self.detached else { return };
        if i >= self.tabs.len() {
            self.detached = None;
            return;
        }
        let title = format!("{} – RustPy", self.tabs[i].state.title());
        let theme = self.theme.clone();
        let opts = self.edit_options_for(i);
        let font_size = self.font_size();
        let keymap = self.keymap.clone();
        let tab_path = self.tabs[i].state.path.clone();
        let bp_lines = self.breakpoint_lines(tab_path.as_deref());
        let bm_lines = self.bookmark_lines(tab_path.as_deref());
        let run_lines = self.run_lines(i);
        let search_highlights = self.sticky_highlights_for(i);
        let mut reattach = false;
        let mut return_drop = None;
        let vp_id = self.detached_viewport_id(i);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([900.0, 600.0]);
        if let Some(bounds) = self.detached_bounds {
            builder = builder.with_inner_size([bounds.width, bounds.height]);
            if self.detached_bounds_pending {
                builder = builder.with_position([bounds.x, bounds.y]);
            }
        }
        self.detached_bounds_pending = false;
        ctx.show_viewport_immediate(vp_id, builder, |ctx, class| {
            if ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Text(_)
                            | egui::Event::Paste(_)
                            | egui::Event::Key { pressed: true, .. }
                            | egui::Event::PointerButton { pressed: true, .. }
                    )
                })
            }) {
                self.refresh_file_permissions(i);
            }
            if class != egui::ViewportClass::EmbeddedWindow {
                let (inner, outer) =
                    ctx.input(|input| (input.viewport().inner_rect, input.viewport().outer_rect));
                self.update_detached_bounds(inner, outer);
            }
            if ctx.input(|i| i.viewport().close_requested()) {
                reattach = true;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(rgb(self.scheme.editor.background)))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        return_drop = self.detached_drag_header(ui, i).1;
                        if ui.small_button("Reattach").clicked() {
                            reattach = true;
                        }
                    });
                    self.data_files_toolbar(ui, i);
                    let controls_available = self.non_text_controls_available(ui.ctx())
                        && !self.detached_tab_drag.is_dragging();
                    if ui
                        .add_enabled_ui(controls_available, |ui| self.csv_table_area(ui, i))
                        .inner
                    {
                        self.retire_non_text_focus(i, ui.ctx(), controls_available);
                        return;
                    }
                    if ui
                        .add_enabled_ui(controls_available, |ui| self.svg_controls(ui, i))
                        .inner
                    {
                        self.retire_non_text_focus(i, ui.ctx(), controls_available);
                        self.svg_preview_view(ui, i);
                        return;
                    }
                    self.bind_context_run_owner(ui.ctx(), i);
                    let (gutter_markers, gutter_quick_fixes) = self.gutter_view_inputs(i);
                    let keyboard =
                        self.editor_keyboard_for(i) && !self.detached_tab_drag.is_dragging();
                    let clipboard_events = {
                        let scheme = &self.scheme;
                        let settings = &self.settings.editor;
                        let tab = &mut self.tabs[i];
                        tab.view.clipboard_payload = self.clipboard_payload.clone();
                        let cfg = ViewConfig {
                            scheme,
                            theme: &theme,
                            settings,
                            opts: &opts,
                            keymap: &keymap,
                            font_size,
                            search_highlights: &search_highlights,
                            request_focus: false,
                            run_lines: &run_lines,
                            line_changes: &[],
                            breakpoints: &bp_lines,
                            exec_line: None,
                            inline_values: &[],
                            bookmarks: &bm_lines,
                            gutter_markers: &gutter_markers,
                            gutter_quick_fixes: &gutter_quick_fixes,
                            code_vision: self.code_vision.display_for(&tab.state),
                            annotations: None,
                            coverage: &[],
                            keyboard,
                        };
                        view::show(ui, &mut tab.state, &mut tab.view, &cfg)
                    };
                    self.record_auxiliary_editor_events(i, &clipboard_events);
                    self.string_paste_events(i, &clipboard_events);
                    for event in &clipboard_events {
                        self.code_vision_editor_event(i, event);
                    }
                });
        });
        self.reconcile_document_markers();
        if let Some(drop) = return_drop {
            self.apply_detached_return(drop, ctx);
        } else if reattach {
            self.reattach_editor();
        }
    }

    /// Moving the editor back changes its parent ScrollArea identity. Restore
    /// the detached viewport position before the root group first renders it.
    fn reattach_editor(&mut self) {
        let Some(i) = self.detached.take() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(i) else {
            return;
        };
        tab.view.reanchor_session_scroll(&tab.state);
        self.active = Some(i);
        self.focus_editor = true;
    }

    /// Sends the HTTP request whose block contains `line` of tab `i`.
    fn run_http_at(&mut self, i: usize, line: usize) {
        if self.project_safe_mode() {
            self.notify_safe_mode("HTTP requests and scripts");
            return;
        }
        self.http_panel.trusted = true;
        let tab = &self.tabs[i];
        if tab.state.language != Language::Http {
            self.set_status("Run HTTP Request works in .http files");
            return;
        }
        let text = tab.state.doc.text();
        let path = tab
            .state
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("untitled.http"));
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        self.http_panel.set_file(&path, &text);
        if self.http_panel.run_at_line(&text, line, &root).is_none() && self.http_panel.is_running()
        {
            self.bottom = Some(BottomTool::HttpClient);
        } else {
            self.set_status("No request at this line");
        }
    }

    fn docstring_format(&self) -> rustpy_semantic::docstrings::DocstringFormat {
        use rustpy_semantic::docstrings::DocstringFormat as F;
        let preferences = self.documentation_preferences();
        match preferences.format_key() {
            "plain" => F::Plain,
            "rest" => F::ReStructuredText,
            "epytext" => F::Epytext,
            "google" => F::Google,
            "numpy" => F::NumPy,
            _ => self.detect_project_docstring_format(),
        }
    }

    /// Code → Generate Docstring: inserts a stub for the function at the caret (or completes a missing parameter list).
    fn generate_docstring(&mut self) {
        let Some(i) = self.active else { return };
        self.sync_analysis_now();
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        let fmt = self.docstring_format();
        let options = rustpy_semantic::docstrings::StubOptions {
            type_placeholders: self
                .settings
                .editor
                .insert_type_placeholders_in_docstring_stub,
            return_type: self.settings.editor.insert_rtype_in_docstring_stub,
        };
        let result = rustpy_semantic::docstrings::generate_with_options(
            &analysis, &file, caret, fmt, options,
        )
        .or_else(|| {
            rustpy_semantic::docstrings::update_with_options(&analysis, &file, caret, fmt, options)
        });
        match result {
            Some((range, text)) => {
                let start = range.start();
                let tab = &mut self.tabs[i];
                tab.state.apply(vec![rustpy_text::Edit::replace(range, text.clone())], vec![crate::editor::Caret::at(start + TextSize::new(3.min(text.len()) as u32))], rustpy_text::EditGroup::Separate, Some("Generate docstring"));
                tab.state.last_op = crate::editor::state::LastOp::Other;
                self.set_status(format!("Docstring stub inserted ({})", fmt.label()));
            }
            None => self.set_status("Place the caret inside a function without a docstring (or with missing parameters)"),
        }
    }

    /// Values of the `$FilePath$`-style macros of External Tools / File Watchers.
    /// Expands `$FilePath$`-style macros (run configuration parameters and working directory).
    /// Path variables (`$USER_HOME$`, `$PROJECT_DIR$`, Settings › Path Variables) expand too.
    fn expand_macros(&self, text: &str, file: Option<&Path>) -> String {
        // Path variables first, so file names containing `$…$` are not expanded again.
        let text = rustpy_settings::path_variables::active()
            .expand(text, self.project.as_ref().map(|p| p.root.as_path()));
        crate::plugin_host::expand_macros_verbatim(&text, &self.macro_values(file))
    }

    fn macro_values(&self, file: Option<&Path>) -> Vec<(&'static str, String)> {
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let line = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.state.doc.line_of(t.state.primary().head()) + 1)
            .unwrap_or(1);
        let interp_dir = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref())
            .and_then(|i| i.path.parent().map(Path::to_path_buf));
        crate::plugin_host::tool_macro_values(file, &root, line, interp_dir.as_deref())
    }

    /// Code → Silent Code Cleanup publishes one prepared, policy-checked edit.
    fn code_cleanup(&mut self) {
        self.silent_cleanup();
    }

    /// Ctrl+Alt+L: PEP 8 / Black-style reformat of the selection or the whole file.
    fn reformat_code(&mut self) {
        self.reconcile_document_markers();
        let Some(index) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        if !self.refresh_file_permissions(index) {
            return;
        }
        let tab = &self.tabs[index];
        if tab.state.language != Language::Python {
            self.data_format_active();
            return;
        }
        if self.selected_formatter() != "builtin" {
            if tab.state.primary().has_selection() {
                self.request_reformat(true);
                return;
            }
            if self.external_format_enabled(index, false) {
                let tool = self.selected_formatter().to_owned();
                self.start_external_formatter(&tool, false);
            } else {
                self.set_status("External formatter disabled for this reformat or file mask");
            }
            return;
        }
        match self.prepare_format_edits(index, &tab.state.doc, &tab.state.carets) {
            Ok(edits) if edits.is_empty() => self.set_status("Already formatted"),
            Ok(edits) => {
                let original = self.tabs[index].state.doc.text();
                let tuples: Vec<_> = edits
                    .iter()
                    .map(|edit| (edit.range, edit.text.clone()))
                    .collect();
                let count = rustpy_semantic::edit_helpers::try_apply_edits(&original, &tuples)
                    .map(|formatted| {
                        crate::editor::save_actions::changed_lines(&original, &formatted).len()
                    })
                    .unwrap_or(0);
                self.tabs[index].state.apply_text_edits(
                    edits
                        .into_iter()
                        .map(|edit| (edit.range, edit.text))
                        .collect(),
                    "Reformat Code",
                );
                self.set_status(format!("{count} lines reformatted"));
                self.editor_format_notification("Code reformatted");
            }
            Err(error) => self.set_status(format!("Cannot reformat: {error}")),
        }
    }

    /// Tab with a template abbreviation before the caret → expand it. Returns true when handled.
    /// Tab in HTML / CSS / template files: expand the Emmet abbreviation before the caret.
    fn try_expand_emmet(&mut self) -> bool {
        self.expand_configured_emmet()
    }

    fn try_expand_abbreviation(&mut self) -> bool {
        let Some(i) = self.active else { return false };
        let state = &self.tabs[i].state;
        let position = state.virtual_caret(0);
        if position.anchor != state.doc.offset_to_position(state.primary().sel.anchor)
            || position.head != state.doc.offset_to_position(state.primary().head())
        {
            return false;
        }
        let tab = &self.tabs[i];
        let k = tab.state.primary();
        if k.has_selection() {
            return false;
        }
        let caret = k.head();
        let line = tab.state.doc.line_of(caret);
        let lr = tab.state.doc.line_range(line);
        let before = tab.state.doc.slice(TextRange::new(lr.start(), caret));
        let word_start = before
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map(|p| p + 1)
            .unwrap_or(0);
        let word = &before[word_start..];
        if word.is_empty() {
            return false;
        }
        let ctx = self.template_context(i);
        let Some(t) = self
            .templates
            .iter()
            .find(|t| t.abbreviation == word && t.applies_in(ctx) && !t.is_surround())
            .cloned()
        else {
            return false;
        };
        let range = TextRange::new(lr.start() + TextSize::new(word_start as u32), caret);
        let before = self.tabs[i].state.doc.version();
        self.expand_template(&t, range, None);
        self.tabs[i].state.doc.version() != before
    }

    /// Re-syncs the session's ranges with document edits; drops the session when a stop was destroyed.
    fn sync_template_session(&mut self) {
        let Some(sess) = &mut self.template_session else {
            return;
        };
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            self.template_session = None;
            return;
        };
        if tab.view.id != sess.tab_id {
            self.template_session = None;
            return;
        }
        let Some(changes) = tab.state.doc.changes_since(sess.version) else {
            self.template_session = None;
            return;
        };
        for edits in changes {
            let mut ok = true;
            let active_var = sess.idx;
            for (vi, (_, ranges)) in sess.vars.iter_mut().enumerate() {
                for (ri, r) in ranges.iter_mut().enumerate() {
                    match remap_range(*r, &edits, vi == active_var && ri == 0) {
                        Some(n) => *r = n,
                        None => ok = false,
                    }
                }
            }
            match remap_range(TextRange::new(sess.end, sess.end), &edits, false) {
                Some(n) => sess.end = n.start(),
                None => ok = false,
            }
            if !ok {
                self.template_session = None;
                return;
            }
        }
        sess.version = tab.state.doc.version();
    }

    /// Tab inside a template session: mirror the current variable and move to the next stop.
    /// Whether the caret is inside the live-template variable being edited.
    fn caret_in_template_variable(&self) -> bool {
        let (Some(sess), Some(i)) = (&self.template_session, self.active) else {
            return false;
        };
        let Some(tab) = self.tabs.get(i).filter(|t| t.view.id == sess.tab_id) else {
            return false;
        };
        let caret = tab.state.primary().head();
        sess.vars
            .get(sess.idx)
            .is_some_and(|(_, ranges)| ranges.iter().any(|r| r.contains_inclusive(caret)))
    }

    fn next_template_stop(&mut self) {
        self.sync_template_session();
        let Some(sess) = &mut self.template_session else {
            return;
        };
        let Some(i) = self.active else { return };
        let tab = &mut self.tabs[i];
        // Mirror the first occurrence's text into the others.
        if let Some((_, ranges)) = sess.vars.get(sess.idx) {
            if ranges.len() > 1 {
                let value = tab.state.doc.slice(ranges[0]);
                let edits: Vec<rustpy_text::Edit> = ranges[1..]
                    .iter()
                    .filter(|r| tab.state.doc.slice(**r) != value)
                    .map(|r| rustpy_text::Edit::replace(*r, value.clone()))
                    .collect();
                if !edits.is_empty() {
                    let caret = tab.state.primary();
                    tab.state.apply(
                        edits.clone(),
                        vec![caret],
                        rustpy_text::EditGroup::Separate,
                        Some("Live template"),
                    );
                    for (_, rs) in sess.vars.iter_mut() {
                        for r in rs.iter_mut() {
                            if let Some(n) = remap_range(*r, &edits, false) {
                                *r = n;
                            }
                        }
                    }
                    if let Some(n) = remap_range(TextRange::new(sess.end, sess.end), &edits, false)
                    {
                        sess.end = n.start();
                    }
                    sess.version = tab.state.doc.version();
                }
            }
        }
        sess.idx += 1;
        if let Some((_, ranges)) = sess.vars.get(sess.idx) {
            let r = ranges[0];
            tab.state
                .set_carets(vec![crate::editor::Caret::new(r.start(), r.end())]);
        } else {
            let end = sess.end.min(tab.state.doc.len());
            tab.state.set_carets(vec![crate::editor::Caret::at(end)]);
            self.template_session = None;
        }
        self.refresh_live_template_choices();
    }

    fn open_surround_with(&mut self) {
        let Some(i) = self.active else { return };
        let tab = &mut self.tabs[i];
        let k = tab.state.primary();
        if !k.has_selection() {
            let line = tab.state.doc.line_of(k.head());
            let lr = tab.state.doc.line_range(line);
            let text = tab.state.doc.slice(lr);
            let lead = text.len() - text.trim_start().len();
            tab.state.set_carets(vec![crate::editor::Caret::new(
                lr.start() + TextSize::new(lead as u32),
                lr.end(),
            )]);
        }
        let items = self
            .available_surround_templates()
            .iter()
            .map(|t| (t.abbreviation.clone(), t.description.clone()))
            .collect();
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::Surround,
            "Surround With".into(),
            items,
        ));
    }

    fn open_insert_live_template(&mut self) {
        let Some(i) = self.active else { return };
        let ctx = self.template_context(i);
        let mut items: Vec<(String, String)> = self
            .templates
            .iter()
            .filter(|t| t.applies_in(ctx) && !t.is_surround())
            .map(|t| (t.abbreviation.clone(), t.description.clone()))
            .collect();
        items.sort();
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::LiveTemplate,
            "Insert Live Template".into(),
            items,
        ));
    }

    fn apply_choice(&mut self, idx: usize) {
        if let Some(kind @ (ChoiceKind::SwitchTask | ChoiceKind::LoadContext)) =
            self.choice.as_ref().map(|c| c.kind)
        {
            self.choice = None;
            self.apply_task_choice(kind, idx);
            return;
        }
        let Some(choice) = self.choice.take() else {
            return;
        };
        let Some((key, _)) = choice.items.get(idx) else {
            return;
        };
        // Bookmarks, Quick List, Copy Path, … also work with no editor open.
        let active = self.active;
        let is_bookmark_choice = matches!(choice.kind, ChoiceKind::Bookmarks);
        match choice.kind {
            ChoiceKind::Surround => {
                let Some(i) = active else { return };
                let tab = &self.tabs[i];
                let doc = &tab.state.doc;
                let mut range = tab.state.primary().range();
                let mut selection = doc.slice(range);
                // Whole lines: the template starts at the first line's indentation and
                // the lines are passed dedented, so they nest one level deeper
                // instead of keeping (and doubling) their own indentation.
                let first = doc.line_of(range.start());
                let mut last = doc.line_of(range.end());
                if last > first && range.end() == doc.line_range(last).start() {
                    last -= 1;
                }
                let first_text = doc.line_text(first);
                let indent_len = first_text.len() - first_text.trim_start().len();
                let whole_lines = range.start()
                    <= doc.line_range(first).start() + TextSize::new(indent_len as u32)
                    && (range.end() >= doc.line_range(last).end()
                        || last < doc.line_of(range.end()));
                if whole_lines && !first_text.trim().is_empty() {
                    range = TextRange::new(
                        doc.line_range(first).start() + TextSize::new(indent_len as u32),
                        doc.line_range(last).end(),
                    );
                    selection = (first..=last)
                        .map(|l| {
                            let t = doc.line_text(l);
                            let ws = t.len() - t.trim_start().len();
                            t[ws.min(indent_len)..].to_string()
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                if let Some(t) = self
                    .available_surround_templates()
                    .into_iter()
                    .find(|t| &t.abbreviation == key)
                {
                    self.expand_template(&t, range, Some(selection));
                }
            }
            ChoiceKind::LiveTemplate => {
                let Some(i) = active else { return };
                let caret = self.tabs[i].state.primary().head();
                if let Some(t) = self
                    .templates
                    .iter()
                    .find(|t| &t.abbreviation == key)
                    .cloned()
                {
                    self.expand_template(&t, TextRange::new(caret, caret), None);
                }
            }
            ChoiceKind::PasteHistory => {
                let Some(i) = active else { return };
                if let Some(text) = self.clipboard_history.get(idx).cloned() {
                    let opts = self.edit_options();
                    self.tabs[i]
                        .state
                        .paste_payload(&text, self.clipboard_payload.as_ref(), &opts);
                    // Move the pasted entry to the front of the history.
                    self.clipboard_history.remove(idx);
                    self.clipboard_history.insert(0, text);
                }
            }
            ChoiceKind::CopyPath => {
                let value = choice
                    .items
                    .get(idx)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                self.set_status(format!("Copied: {value}"));
                self.pending_copy = Some(value);
            }
            ChoiceKind::SelectIn => {
                self.pending_select_in = Some(idx);
            }
            ChoiceKind::Bookmarks => {
                if let Some(bookmark) = self.bookmarks.get(idx).cloned() {
                    self.open_bookmark_target(&bookmark);
                }
            }
            ChoiceKind::Generate => self.generate_member(key.clone()),
            ChoiceKind::Extract(kind) => {
                if let Some(r) = self.extract_candidates.get(idx).copied() {
                    self.run_extract(kind, r);
                }
            }
            ChoiceKind::ExtractOccurrences(kind, range) => {
                self.clear_sticky_highlights();
                self.run_extract_with(kind, range, Some(idx == 1));
            }
            ChoiceKind::Locations => {
                if let Some(l) = self.location_choices.get(idx).cloned() {
                    self.navigate_to(&l.path, l.range);
                }
            }
            ChoiceKind::Nav => self.apply_nav_choice(idx),
            ChoiceKind::QuickList => {
                if let Some(a) = self
                    .settings
                    .quick_list
                    .get(idx)
                    .and_then(|id| Keymap::action_by_id(id))
                {
                    self.pending_action = Some(a);
                }
            }
            ChoiceKind::SwitchTask | ChoiceKind::LoadContext => {}
            kind @ (ChoiceKind::RestoreLayout | ChoiceKind::DeleteLayout) => {
                self.apply_layout_choice(kind, key)
            }
            ChoiceKind::RefactorThis => {
                let action = match key.as_str() {
                    "Rename…" => Action::Rename,
                    "Extract Variable…" => Action::ExtractVariable,
                    "Extract Constant…" => Action::ExtractConstant,
                    "Extract Field…" => Action::ExtractField,
                    "Extract Parameter…" => Action::ExtractParameter,
                    "Extract Method…" => Action::ExtractMethod,
                    "Inline…" => Action::Inline,
                    "Convert…" => Action::ConvertRefactoring,
                    "Change Signature…" => Action::ChangeSignature,
                    "Move…" => Action::MoveRefactoring,
                    _ => Action::SafeDelete,
                };
                self.pending_action = Some(action);
            }
        }
        if !is_bookmark_choice {
            self.focus_editor = true;
        }
    }

    fn toggle_bookmark(&mut self, mnemonic: Option<char>) {
        self.reconcile_document_markers();
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let line = tab.state.doc.line_of(tab.state.primary().head()) + 1;
        if let Some(pos) = self.bookmarks.iter().position(|b| {
            b.kind == rustpy_settings::BookmarkKind::Line && b.path == path && b.line == line
        }) {
            self.bookmarks.remove(pos);
            self.set_status("Bookmark removed");
        } else {
            if let Some(m) = mnemonic {
                self.bookmarks.retain(|b| b.mnemonic != Some(m));
            }
            self.bookmarks.push(Bookmark {
                path,
                line,
                mnemonic,
                description: String::new(),
                kind: rustpy_settings::BookmarkKind::Line,
                list: self.bookmark_lists.selected_name(),
            });
            self.set_status("Bookmark added (F11 toggles, Shift+F11 lists)");
        }
        self.reconcile_document_markers();
        self.save_workspace();
    }

    fn bookmark_lines(&self, path: Option<&Path>) -> Vec<(usize, Option<char>)> {
        let Some(path) = path else { return Vec::new() };
        self.bookmarks
            .iter()
            .filter(|b| b.kind == rustpy_settings::BookmarkKind::Line && b.path == path)
            .map(|b| (b.line.saturating_sub(1), b.mnemonic))
            .collect()
    }

    fn open_bookmarks_popup(&mut self) {
        if self.bookmarks.is_empty() {
            self.set_status("No bookmarks (F11 toggles a bookmark on the current line)");
            return;
        }
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let items: Vec<(String, String)> = self
            .bookmarks
            .iter()
            .map(|b| {
                (
                    b.mnemonic
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "•".into()),
                    format!(
                        "[{}] {}:{}  {}",
                        bookmark_lists_ui::list_name(b),
                        b.path.strip_prefix(&root).unwrap_or(&b.path).display(),
                        b.line,
                        b.description
                    ),
                )
            })
            .collect();
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::Bookmarks,
            "Bookmarks".into(),
            items,
        ));
    }

    fn cycle_bookmark(&mut self, next: bool) {
        if self.bookmarks.is_empty() {
            self.set_status("No bookmarks");
            return;
        }
        let cur = self.active.and_then(|i| self.tabs.get(i)).and_then(|t| {
            t.state
                .path
                .clone()
                .map(|p| (p, t.state.doc.line_of(t.state.primary().head()) + 1))
        });
        let mut sorted = self.bookmarks.clone();
        sorted.sort_by_key(|b| (b.path.clone(), b.line));
        let idx = match &cur {
            Some((p, l)) => {
                let pos = sorted
                    .iter()
                    .position(|b| (&b.path, b.line) > (p, *l))
                    .unwrap_or(0);
                if next {
                    pos
                } else {
                    sorted
                        .iter()
                        .rposition(|b| (&b.path, b.line) < (p, *l))
                        .unwrap_or(sorted.len() - 1)
                }
            }
            None => 0,
        };
        let bookmark = sorted[idx].clone();
        self.open_bookmark_target(&bookmark);
    }

    fn bookmarks_view(&mut self, ui: &mut egui::Ui) {
        self.bookmark_lists_view(ui);
    }

    /// Ctrl+Shift+Delete: replace the enclosing compound statement with its body.
    fn unwrap_statement(&mut self) {
        use rustpy_syntax::SyntaxKind;
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let caret = tab.state.primary().head();
        let root = tab.state.syntax().syntax_node();
        let at = crate::editor::syntax_support::token_at(&root, caret);
        let Some(tok) = at.clone().left_biased().or_else(|| at.right_biased()) else {
            return;
        };
        let Some(stmt) = tok.parent_ancestors().find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::IF_STMT
                    | SyntaxKind::FOR_STMT
                    | SyntaxKind::WHILE_STMT
                    | SyntaxKind::WITH_STMT
                    | SyntaxKind::TRY_STMT
            )
        }) else {
            self.set_status("No enclosing if/for/while/with/try statement to unwrap");
            return;
        };
        // Pick the block that contains the caret (or the first block).
        let blocks: Vec<_> = stmt
            .children()
            .filter(|n| n.kind() == SyntaxKind::BLOCK)
            .chain(
                stmt.children()
                    .flat_map(|c| c.children())
                    .filter(|n| n.kind() == SyntaxKind::BLOCK),
            )
            .collect();
        let Some(block) = blocks
            .iter()
            .find(|b| b.text_range().contains_inclusive(caret))
            .or(blocks.first())
            .cloned()
        else {
            return;
        };
        let stmt_line = tab.state.doc.line_of(stmt.text_range().start());
        let stmt_lr = tab.state.doc.line_range(stmt_line);
        let stmt_indent = tab
            .state
            .doc
            .slice(stmt_lr)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let block_text = tab.state.doc.slice(block.text_range());
        let mut lines: Vec<&str> = block_text.lines().collect();
        while lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        let body_indent = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(stmt_indent);
        let shift = body_indent.saturating_sub(stmt_indent);
        let new_text: String = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    String::new()
                } else {
                    l.chars().skip(shift).collect()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let stmt_end_line = tab
            .state
            .doc
            .line_of(stmt.text_range().end().max(stmt.text_range().start()));
        let end_lr = tab.state.doc.line_range(stmt_end_line);
        let range = TextRange::new(stmt_lr.start(), end_lr.end().max(stmt.text_range().end()));
        let new_caret = stmt_lr.start()
            + TextSize::new(new_text.len().min(usize::from(caret - stmt_lr.start())) as u32);
        tab.state.apply(
            vec![rustpy_text::Edit::replace(range, new_text)],
            vec![crate::editor::Caret::at(new_caret)],
            rustpy_text::EditGroup::Separate,
            Some("Unwrap"),
        );
        tab.state.last_op = crate::editor::state::LastOp::Other;
    }

    /// Ctrl+Shift+I: source peek of the symbol's definition.
    fn quick_definition(&mut self) {
        let Some(i) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        let locs = features::definition_at(&analysis, &file, caret);
        let Some(loc) = locs.first() else {
            self.set_status("Cannot find declaration to show");
            return;
        };
        let source = if let Some(t) = self
            .tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(loc.path.as_path()))
        {
            t.state.doc.text()
        } else if loc.path.starts_with("<typeshed>") {
            analysis
                .file_for_path(&loc.path)
                .map(|f| f.text.to_string())
                .unwrap_or_default()
        } else {
            std::fs::read_to_string(&loc.path).unwrap_or_default()
        };
        let off = usize::from(loc.range.start()).min(source.len());
        let line = source[..off].matches('\n').count();
        let lines: Vec<&str> = source.lines().collect();
        let start_line = line;
        // Include decorators / the whole def until the indentation returns to the def level (max 40 lines).
        let base_indent = lines
            .get(line)
            .map(|l| l.len() - l.trim_start().len())
            .unwrap_or(0);
        let mut end_line = line + 1;
        while end_line < lines.len() && end_line - start_line < 40 {
            let l = lines[end_line];
            if !l.trim().is_empty() && l.len() - l.trim_start().len() <= base_indent {
                break;
            }
            end_line += 1;
        }
        let text = lines[start_line..end_line.min(lines.len())].join("\n");
        let title = format!(
            "{}:{}",
            loc.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            line + 1
        );
        self.dialog = Dialog::QuickDefinition {
            title,
            path: loc.path.clone(),
            line: line + 1,
            text,
        };
    }

    fn highlight_usages_sticky(&mut self) {
        let Some(i) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        let usages = rustpy_semantic::usages::usages_in_file(&analysis, &file, caret);
        if usages.is_empty() {
            self.set_status("No usages found at the caret");
            self.clear_sticky_highlights();
        } else {
            self.set_status(format!(
                "{} usages highlighted (Esc to clear)",
                usages.len()
            ));
            self.set_sticky_highlights(i, usages.into_iter().map(|u| u.range).collect());
        }
    }

    /// Alt+/: cyclic expand word from the document.
    fn hippie_complete(&mut self) {
        let Some(i) = self.active else { return };
        let tab = &mut self.tabs[i];
        let caret = tab.state.primary().head();
        let version = tab.state.doc.version();
        let text = tab.state.doc.text();
        let (start, cands, idx) = match self.hippie.take() {
            Some(state)
                if state.view == tab.view.id
                    && state.caret == caret
                    && state.version == version
                    && !state.candidates.is_empty() =>
            {
                (state.start, state.candidates, state.next)
            }
            _ => {
                let off = usize::from(caret).min(text.len());
                let before = &text[..off];
                let start = before
                    .rfind(|c: char| !rustpy_core::is_ident_continue(c))
                    .map(|p| p + before[p..].chars().next().map_or(0, char::len_utf8))
                    .unwrap_or(0);
                let prefix = before[start..].to_string();
                if prefix.is_empty() {
                    self.set_status("Nothing to expand");
                    return;
                }
                // Candidates: words before the caret (nearest first), then after.
                let mut cands: Vec<String> = Vec::new();
                let word_re = |s: &str| -> Vec<(usize, String)> {
                    let mut out = Vec::new();
                    let mut cur = String::new();
                    let mut cur_start = 0;
                    for (bi, c) in s.char_indices() {
                        if rustpy_core::is_ident_continue(c) {
                            if cur.is_empty() {
                                cur_start = bi;
                            }
                            cur.push(c);
                        } else if !cur.is_empty() {
                            out.push((cur_start, std::mem::take(&mut cur)));
                        }
                    }
                    if !cur.is_empty() {
                        out.push((cur_start, cur));
                    }
                    out
                };
                let words = word_re(&text);
                let mut before_words: Vec<String> = words
                    .iter()
                    .filter(|(s, w)| {
                        *s + w.len() <= start && w.starts_with(&prefix) && *w != prefix
                    })
                    .map(|(_, w)| w.clone())
                    .collect();
                before_words.reverse();
                let after_words: Vec<String> = words
                    .iter()
                    .filter(|(s, w)| *s >= off && w.starts_with(&prefix) && *w != prefix)
                    .map(|(_, w)| w.clone())
                    .collect();
                for w in before_words.into_iter().chain(after_words) {
                    if !cands.contains(&w) {
                        cands.push(w);
                    }
                }
                cands.push(prefix.clone());
                (TextSize::new(start as u32), cands, 0)
            }
        };
        let word = cands[idx % cands.len()].clone();
        let range = TextRange::new(start, caret);
        let new_caret = start + TextSize::new(word.len() as u32);
        tab.state.apply(
            vec![rustpy_text::Edit::replace(range, word)],
            vec![crate::editor::Caret::at(new_caret)],
            rustpy_text::EditGroup::Separate,
            Some("Hippie completion"),
        );
        tab.state.last_op = crate::editor::state::LastOp::Other;
        self.hippie = Some(HippieCompletionState {
            view: tab.view.id,
            caret: new_caret,
            start,
            next: (idx + 1) % cands.len(),
            candidates: cands,
            version: tab.state.doc.version(),
        });
    }

    /// Alt+Insert: choose the kind, then select actual fields and methods.
    fn open_generate(&mut self) {
        if !self.active.is_some_and(|i| i < self.tabs.len()) {
            return;
        }
        let items = vec![
            ("__init__", "Constructor"),
            ("__repr__", "__repr__"),
            ("__str__", "__str__"),
            ("__eq__", "Equality and hash"),
            ("__lt__", "Ordering"),
            ("dunders", "Dunder methods"),
            ("property", "Property"),
            ("override", "Override or implement methods"),
            ("__slots__", "__slots__"),
            ("__post_init__", "Dataclass __post_init__"),
            ("docstring", "Docstring"),
            ("copyright", "Copyright header"),
            ("main", "Main block"),
            ("test", "Test"),
        ]
        .into_iter()
        .map(|(key, label)| (key.to_string(), label.to_string()))
        .collect();
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::Generate,
            "Generate".into(),
            items,
        ));
    }
    fn generate_member(&mut self, key: String) {
        match key.as_str() {
            "docstring" => self.generate_docstring(),
            "copyright" => self.update_copyright(),
            "main" => {
                let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
                    return;
                };
                if self.tabs[i].state.doc.is_read_only() {
                    self.set_status("Cannot generate in a read-only document");
                    return;
                }
                match rustpy_refactor::generation::main_guard(&self.tabs[i].state.doc.text()) {
                    Ok(edits) => {
                        if self.tabs[i]
                            .state
                            .apply_text_edits(edits, "Generate main block")
                        {
                            self.focus_editor = true;
                        }
                    }
                    Err(error) => self.set_status(error),
                }
            }
            "test" => {
                if let Some(path) = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|tab| tab.state.path.clone())
                {
                    self.open_generate_test_picker(path);
                } else {
                    self.set_status("Save the source file before creating its test");
                }
            }
            _ => self.open_generate_members(&key),
        }
    }

    /// Alt+Enter: collect quick fixes for problems at the caret (or on its line).
    fn open_intentions(&mut self) {
        let Some(i) = self.active else { return };
        // Quick fixes need the analysis of this very text: it may still be running.
        if self.defer_intentions_until_analyzed(i) {
            return;
        }
        let tab = &self.tabs[i];
        let caret = tab.state.primary().head();
        let line = tab.state.doc.line_of(caret);
        let mut fixes: Vec<QuickFix> = Vec::new();
        // Problems under the caret win; otherwise offer the ones on the caret's line.
        let quick_fixes = analysis_jobs::current_quick_fixes(&tab.state);
        let at_caret = quick_fixes
            .iter()
            .any(|(range, _)| range.contains_inclusive(caret));
        for (range, fx) in quick_fixes {
            let same_line = tab.state.doc.line_of(range.start()) == line;
            if range.contains_inclusive(caret) || (!at_caret && same_line) {
                for f in fx {
                    if !fixes.iter().any(|x| x.title == f.title) {
                        fixes.push(f.clone());
                    }
                }
            }
        }
        let quick_titles = fixes.iter().map(|fix| fix.title.clone()).collect();
        if let Some((analysis, file)) = self.analysis_file(i) {
            if rustpy_semantic::docstrings::generate(
                &analysis,
                &file,
                caret,
                self.docstring_format(),
            )
            .is_some()
            {
                fixes.push(QuickFix {
                    title: "Insert documentation string stub".into(),
                    edits: Vec::new(),
                });
            }
            for it in rustpy_analysis::intentions::intentions_at(&analysis, &file, caret) {
                if !fixes.iter().any(|f| f.title == it.title) {
                    fixes.push(QuickFix {
                        title: it.title,
                        edits: it.edits,
                    });
                }
            }
            let edits = rustpy_format::imports::optimize_imports_with_style(
                &file,
                &inspections::unused_import_ranges_with(&analysis, &file),
                &self.settings.code_style.python.imports,
            );
            if !edits.is_empty() && !fixes.iter().any(|f| f.title == "Optimize imports") {
                fixes.push(QuickFix {
                    title: "Optimize imports".into(),
                    edits,
                });
            }
        }
        fixes.extend(self.injected_intentions());
        fixes.retain(|f| !self.settings.disabled_intentions.contains(&f.title));
        self.append_fix_all_intentions(&mut fixes);
        self.prepare_intention_management(&mut fixes, &quick_titles);
        if fixes.is_empty() {
            self.set_status("No context actions available at this location");
        } else {
            self.intentions = Some((fixes, 0));
            self.intentions_at = self
                .active
                .and_then(|i| self.tabs.get(i))
                .map(|t| (t.view.id, t.state.doc.version(), t.state.primary().head()));
        }
    }

    fn apply_intention(&mut self, idx: usize) {
        let Some((fixes, _)) = self.intentions.take() else {
            return;
        };
        // Only for the editor state the fixes were computed for.
        let at = self.intentions_at.take();
        if !self.intention_context_current()
            || at.is_none()
            || at
                != self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| (t.view.id, t.state.doc.version(), t.state.primary().head()))
        {
            return;
        }
        let Some(fix) = fixes.get(idx) else { return };
        if self.apply_intention_management(idx) {
            return;
        }
        if self.injected_intention_action(&fix.title) {
            return;
        }
        if let Some(word) = fix
            .title
            .strip_prefix("Save '")
            .and_then(|r| r.strip_suffix("' to dictionary"))
        {
            let word = word.to_string();
            match rustpy_spell::add_user_word(
                self.project
                    .as_ref()
                    .map(|project| project.root.join(".rustpy/dictionaries/project.txt"))
                    .unwrap_or_else(user_dictionary_path),
                &word,
            ) {
                Ok(_) => {
                    // Rebuild the dictionary with the new word and re-check open files.
                    static_reset_dictionary();
                    for t in &mut self.tabs {
                        t.state.semantic_version = None;
                    }
                    self.set_status(format!("'{word}' added to the dictionary"));
                }
                Err(e) => self.set_status(format!("Cannot update the dictionary: {e}")),
            }
            self.focus_editor = true;
            return;
        }
        if let Some(step) = fix.title.strip_prefix("Create step definition: ") {
            let step = step.to_string();
            self.create_step_definition(&step);
            return;
        }
        if fix.title == "Insert documentation string stub" {
            self.generate_docstring();
            self.focus_editor = true;
            return;
        }
        if fix.title == "Create package requirements file" {
            self.add_package_requirement(None);
            return;
        }
        if let Some(distribution) = fix
            .title
            .strip_prefix("Add package requirement '")
            .and_then(|value| value.strip_suffix('\''))
        {
            let distribution = distribution.to_string();
            self.add_package_requirement(Some(&distribution));
            return;
        }
        if let Some(name) = fix
            .title
            .strip_prefix("Create template '")
            .and_then(|name| name.strip_suffix('\''))
        {
            let name = name.to_owned();
            self.framework_create_template(&name);
            return;
        }
        if let Some(dist) = inspections::install_package_target(&fix.title) {
            let dist = dist.to_string();
            self.install_package(&dist);
            return;
        }
        if fix.title == inspections::RENAME_ELEMENT_FIX {
            self.rename_element_fix();
            return;
        }
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            tab.state.apply_text_edits(fix.edits.clone(), &fix.title);
        }
        self.set_status(fix.title.clone());
        self.focus_editor = true;
    }

    /// Ctrl+P / auto: parameter info for the call around the caret.
    fn update_param_info(&mut self, explicit: bool) {
        self.update_parameter_info(explicit);
    }

    fn selected_text_seed(&self) -> Option<String> {
        let tab = self.active.and_then(|i| self.tabs.get(i))?;
        let k = tab.state.primary();
        if k.has_selection() {
            Some(tab.state.doc.slice(k.range()))
        } else {
            tab.state
                .doc
                .word_range_at(k.head())
                .map(|r| tab.state.doc.slice(r))
        }
    }

    fn open_everywhere(&mut self, tab: EverywhereTab) {
        self.refresh_unwatched_file_index();
        // Wall-clock seconds (an idle UI renders few frames).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        if let Some(p) = &self.project {
            if matches!(
                tab,
                EverywhereTab::All | EverywhereTab::Classes | EverywhereTab::Symbols
            ) {
                // The project index lists the symbols; until it is built (or
                // after it was deleted), a line scan of the project does.
                if self.everywhere.index.project.is_none() {
                    self.everywhere.rescan_symbols(&p.root, now, 5.0);
                }
                // Open editors' current text wins: unsaved functions are found too.
                let root = p.root.clone();
                let unsaved = self
                    .tabs
                    .iter()
                    .filter(|t| t.has_unsaved_changes())
                    .filter_map(|t| {
                        t.state
                            .path
                            .as_ref()
                            .filter(|p| {
                                p.extension().and_then(|e| e.to_str()) == Some("py")
                                    && p.starts_with(&root)
                            })
                            .map(|p| (p.clone(), t.state.doc.text()))
                    });
                self.everywhere.set_unsaved(unsaved);
            }
        }
        self.prepare_search_navigation();
        self.everywhere.open(tab);
    }

    fn handle_nav_event(&mut self, ev: NavEvent, ctx: &egui::Context) {
        match ev {
            NavEvent::SearchMemory(memory) => {
                self.settings.navigation_search = memory;
                if let Err(error) = self.settings.save() {
                    self.set_status(format!("Cannot save search preferences: {error}"));
                }
            }
            NavEvent::SearchDestination(destination) => {
                self.handle_search_destination(destination, ctx)
            }
            NavEvent::OpenFile { path, line, column } => {
                if path.is_dir() {
                    if let Some(project) = &mut self.project {
                        project.tree.reveal(&path);
                        project.tree.selected = Some(path);
                    }
                    self.left_tool = LeftTool::Project;
                    self.layout.left_visible = true;
                    return;
                }
                self.mark_nav_jump();
                self.open_file(&path);
                if let (Some(l), Some(tab)) = (line, self.active.and_then(|i| self.tabs.get_mut(i)))
                {
                    let col = column.unwrap_or(0);
                    let off = tab
                        .state
                        .doc
                        .position_to_offset(rustpy_core::Position::new(l as u32, 0));
                    // `column` is a byte column from search results: convert via the line text.
                    let line_text = tab.state.doc.line_text(l);
                    let char_col = line_text
                        .get(..col.min(line_text.len()))
                        .map(|s| s.chars().count())
                        .unwrap_or(0);
                    let target = tab
                        .state
                        .doc
                        .position_to_offset(rustpy_core::Position::new(l as u32, char_col as u32));
                    let _ = off;
                    tab.state.reveal_offset(target);
                    tab.state.set_carets(vec![crate::editor::Caret::at(target)]);
                    tab.state.last_op = crate::editor::state::LastOp::Other;
                }
                self.focus_editor = true;
            }
            NavEvent::RunAction(a) => {
                if view::is_editor_action(a) {
                    let opts = self.edit_options();
                    if let Some(i) = self.active {
                        self.refresh_file_permissions(i);
                        view::handle_editor_action(a, &mut self.tabs[i].state, &opts);
                    }
                    self.learn_observe(a);
                } else {
                    self.handle_action(a, ctx);
                }
                self.focus_editor = true;
            }
            NavEvent::Status(m) => self.set_status(m),
            NavEvent::FilesChanged => {
                self.reload_changed_files();
                self.reload_analysis_from_disk();
                self.find_in_files.last_query.clear();
            }
            NavEvent::ApplyEdits { edits, label } => self.apply_file_edits(edits, &label),
            NavEvent::ShowInFindWindow { title, matches } => self.pin_find_results(title, matches),
            NavEvent::ShowFileSearch { search, new_tab } => self.show_file_search(*search, new_tab),
        }
    }

    fn quick_documentation(&mut self) {
        self.show_quick_documentation();
        if self.documentation.docked {
            self.bottom = Some(BottomTool::Documentation);
            self.quick_doc = None;
        }
    }

    fn push_tab(&mut self, mut state: EditorState) {
        self.emacs_editing.interrupt();
        self.apply_template_folders(&mut state);
        self.observe_tab_policy();
        let previous_editor = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let id = egui::Id::new(("editor", self.next_tab_id));
        self.next_tab_id += 1;
        let limit = self.settings.editor.tab_limit.max(1);
        let group = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.group)
            .unwrap_or(0);
        if let Some(reuse) = self.reusable_normal_tab() {
            self.close_tab(reuse);
        }
        while self.tabs.iter().filter(|t| !t.pinned).count() >= limit {
            if let Some(i) = self.tab_lru_candidate() {
                // Closed like any other tab: Reopen Closed Tab brings it back.
                self.close_tab(i);
            } else {
                break;
            }
        }
        if let Some(p) = &state.path {
            self.everywhere.note_recent(p);
        }
        self.tabs.push(Tab {
            state,
            view: EditorView::new(id),
            pinned: false,
            group,
            preview: false,
            image: None,
            svg: None,
            notebook: None,
        });
        self.reconcile_document_markers();
        self.prune_file_permissions();
        self.active = Some(self.tabs.len() - 1);
        self.focus_editor = true;
        if let (Some(p), Some(path)) = (
            &mut self.project,
            self.tabs.last().and_then(|t| t.state.path.clone()),
        ) {
            p.tree.reveal(&path);
        }
        self.sync_external_file_watches();
        self.order_opened_tab(id, previous_editor);
    }

    /// Writes a notebook's model into its document (marks the tab modified).
    fn sync_notebook_doc(tab: &mut Tab) {
        let Some(nb) = tab.notebook.as_mut() else {
            return;
        };
        if !nb.is_dirty() {
            return;
        }
        nb.set_dirty(false);
        let text = nb.text();
        if text != tab.state.doc.text() {
            let full = TextRange::new(TextSize::new(0), tab.state.doc.len());
            tab.state
                .apply_text_edits(vec![(full, text)], "Edit notebook");
        }
        let version = tab.state.doc.version();
        if let Some(nb) = tab.notebook.as_mut() {
            nb.set_synced_version(Some(version));
        }
    }

    fn notebook_area(&mut self, ui: &mut egui::Ui, i: usize) {
        if !self.builtin_plugin_enabled("jupyter") {
            ui.label("Jupyter is disabled in Plugins. Use Source to edit this notebook as JSON.");
            return;
        }
        let theme = self.theme.clone();
        let scheme = self.scheme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let path = self.tabs[i].state.path.clone().unwrap_or_default();
        let interpreter = self.project.as_ref().and_then(|project| {
            project
                .tree
                .model()
                .sdk_for(&path)
                .map(Path::to_path_buf)
                .or_else(|| {
                    project
                        .interpreter
                        .as_ref()
                        .map(|interpreter| interpreter.path.clone())
                })
        });
        let cwd = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let keys_enabled = self.editor_keyboard()
            && self.component_menu_owner_available(false)
            && !egui::Popup::is_any_open(ui.ctx())
            && self.active == Some(i)
            && !self.tabs[i].state.doc.is_read_only();
        // Undo/redo (or a reload) changed the notebook's document: show it.
        let version = self.tabs[i].state.doc.version();
        let stale = self.tabs[i]
            .notebook
            .as_ref()
            .is_some_and(|nb| !nb.is_dirty() && nb.synced_version().is_some_and(|v| v != version));
        if stale {
            match self.tabs[i]
                .state
                .doc
                .text()
                .parse::<rustpy_notebook::Notebook>()
            {
                Ok(model) => {
                    if let Some(nb) = self.tabs[i].notebook.as_mut() {
                        nb.replace_model(model);
                    }
                }
                Err(e) => log::warn!("notebook document no longer parses: {e}"),
            }
        }
        let notebook_execution_allowed = !self.project_safe_mode();
        let mut persist_trust = None;
        if let Some(nb) = self.tabs[i].notebook.as_mut() {
            nb.set_synced_version(Some(version));
            let key = path.to_string_lossy().into_owned();
            let (trusted, resigned) =
                nb.trust_against(self.settings.jupyter.trusted.get(&key).map(String::as_str));
            if let Some(hash) = resigned {
                // Outputs of the user's own trusted execution keep the notebook
                // signed; persisted once the kernel is idle.
                if let Some(snapshot) = &mut self.settings_ui.snapshot {
                    snapshot
                        .settings
                        .jupyter
                        .trusted
                        .insert(key.clone(), hash.clone());
                }
                self.settings.jupyter.trusted.insert(key.clone(), hash);
            }
            if trusted && nb.take_resign_unsaved() {
                persist_trust = self
                    .settings
                    .jupyter
                    .trusted
                    .get(&key)
                    .cloned()
                    .map(|hash| (key, hash));
            }
            nb.configure_workspace(&self.settings.jupyter, trusted, notebook_execution_allowed);
        }
        if let Some((key, hash)) = persist_trust {
            self.change_setting_now(|settings| {
                settings.jupyter.trusted.insert(key.clone(), hash.clone());
            });
        }
        let notebook_editable = !self.tabs[i].state.doc.is_read_only();
        let events = match self.tabs[i].notebook.as_mut() {
            Some(nb) => {
                nb.keys_enabled = keys_enabled;
                nb.editable = notebook_editable;
                nb.ui(
                    ui,
                    &theme,
                    &scheme,
                    mono,
                    interpreter.as_deref(),
                    &cwd,
                    &path,
                )
            }
            None => Vec::new(),
        };
        for event in events {
            self.notebook_event(i, &path, event);
        }
        let now = ui.input(|inp| inp.time);
        if self.tabs[i].notebook.as_ref().is_some_and(|n| n.is_dirty())
            && now - self.notebook_synced > 0.5
        {
            self.notebook_synced = now;
            Self::sync_notebook_doc(&mut self.tabs[i]);
        }
    }

    fn save_tab(&mut self, i: usize) -> bool {
        if let Some(saved) = self.remote_development_save(i) {
            return saved;
        }
        self.save_tab_prepared(i)
    }

    /// Publish save side effects only after the checked write and prepared
    /// document commit succeeded. Local History retains the pre-save editor.
    fn finish_saved_tab(
        &mut self,
        i: usize,
        path: PathBuf,
        revision: rustpy_core::vfs::ContentRevision,
        pre_save_text: &str,
        disk_changed: bool,
    ) {
        self.snapshot_text(&path, pre_save_text, "Before Save");
        self.note_saved_history(&path);
        self.disk_revisions.insert(path.clone(), Some(revision));
        self.bind_file_permissions(i);
        let written_encoding = self.tabs[i].state.doc.encoding();
        self.publish_encoding_override(&path, written_encoding);
        self.persist_encoding_overrides();
        self.reconcile_document_markers();
        if disk_changed {
            self.framework_model_saved(&path);
            self.web_preview_saved(&path);
            self.web_language_saved(&path);
            let deployment_root = self.project.as_ref().map(|project| project.root.clone());
            self.deploy.note_local_save(&path);
            let remote_edit_saved = !self.project_safe_mode()
                && deployment_root
                    .as_ref()
                    .is_some_and(|root| self.deploy.remote_edit_saved(root, &path));
            let actions = self.save_actions_apply(&path);
            if actions && self.settings.save_actions.run_file_watchers {
                self.run_file_watchers(&path);
            }
            if !self.project_safe_mode() {
                self.test_panel.on_source_change(&path);
            }
            if !self.project_safe_mode()
                && !remote_edit_saved
                && actions
                && self.settings.save_actions.upload
            {
                if let Some(root) = self.project.as_ref().map(|p| p.root.clone()) {
                    self.deploy.on_saved(&root, &path, true);
                }
            }
        }
        if let Some(project) = &mut self.project {
            project.git_refreshed = -100.0;
        }
        self.set_status(format!("Saved {}", path.display()));
    }

    fn save_all(&mut self) -> bool {
        let mut success = true;
        let mut failure_status = None;
        let mut attempted = std::collections::HashSet::new();
        for i in 0..self.tabs.len() {
            if !attempted.insert(self.tabs[i].state.doc.identity()) {
                continue;
            }
            if (self.tabs[i].has_unsaved_changes()
                || self.tabs[i]
                    .state
                    .path
                    .as_ref()
                    .is_some_and(|p| self.disk_revisions.get(p) == Some(&None)))
                && !self.save_tab(i)
            {
                success = false;
                if failure_status.is_none() {
                    failure_status = self.status.clone();
                }
            }
        }
        if failure_status.is_some() {
            self.status = failure_status;
        }
        success
    }

    fn request_close_tab(&mut self, i: usize) {
        self.emacs_editing.interrupt();
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs[i].has_unsaved_changes() && !self.has_other_document_view(i) {
            self.dialog = Dialog::ConfirmClose {
                tab: i,
                then_exit: false,
            };
        } else {
            self.close_tab(i);
        }
    }

    /// Whether the editors may take keyboard input: not while a dialog or a
    /// popup that owns the keyboard is open (typing must not reach the
    /// document behind it).
    fn editor_controls_available(&self) -> bool {
        !(self.bottom == Some(BottomTool::HttpClient) && self.http_panel.owns_keyboard())
            && !self.diff_controls_owned()
            && !self.git_panel.operations_modal_active()
            && !self.git_panel.log_modal_active()
            && !self.git_panel.commit_modal_active()
            && !self.stack_trace_state.active()
            && !self.interpreter_management_active()
            && !self.run_panel.modal_active()
            && !self.run_console_state.modal_active()
            && !self.local_history_runtime.modal_active()
            && self.editor_accessibility.owner.is_none()
            && !self.project_view.modal_active()
            && !self.intention_menu.dialog_active()
            && !self.bg.problems.infrastructure.dialog
            && !self.emmet_ui.active()
            && !self.injection_ui.active()
            && !self.generate_methods.active()
            && matches!(self.dialog, Dialog::None)
            && self.choice.is_none()
            && !self.everywhere.visible
            && !self.nav.popup_visible()
            && !self.git_panel.branch_popup
            && self.layout.exit_prompt.is_none()
            && !self.dialog_workspace.maintenance
    }

    fn editor_keyboard(&self) -> bool {
        self.editor_controls_available() && !self.non_text_source_hidden()
    }

    fn non_text_controls_available(&self, ctx: &egui::Context) -> bool {
        self.editor_controls_available()
            && !self.find_in_files.visible
            && !self.completion.visible
            && self.quick_doc.is_none()
            && self.inplace_rename.is_none()
            && self.intentions.is_none()
            && !self.coordinated_quit_active()
            && !egui::Popup::is_any_open(ctx)
    }

    fn editor_keyboard_for(&self, index: usize) -> bool {
        self.editor_controls_available()
            && !self.csv_table_owns_input_for(index)
            && !self.svg_preview_owns_input_for(index)
    }

    fn retire_non_text_focus(
        &mut self,
        index: usize,
        ctx: &egui::Context,
        controls_available: bool,
    ) {
        if controls_available && self.active == Some(index) {
            self.focus_editor = false;
            let view = &mut self.tabs[index].view;
            ctx.memory_mut(|memory| memory.surrender_focus(view.id));
            view.focused = false;
        }
    }

    fn non_text_source_hidden(&self) -> bool {
        self.csv_table_owns_input()
            || self
                .active
                .is_some_and(|index| self.svg_preview_owns_input_for(index))
    }

    fn parameter_info_keyboard(&self, ui: &egui::Ui) -> bool {
        self.editor_keyboard()
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && self.inplace_rename.is_none()
            && self.intentions.is_none()
            && !self.completion.visible
            && self.quick_doc.is_none()
            && !self.coordinated_quit_active()
            && !egui::Popup::is_any_open(ui.ctx())
            && self
                .active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| ui.memory(|m| m.has_focus(tab.view.id)))
    }

    /// Save per-file encoding choices without changing the editor session.
    /// Ordinary Save does not turn current tabs into startup session records.
    fn persist_encoding_overrides(&mut self) {
        let mut workspace = self.effective_workspace();
        workspace.file_encodings = self
            .encoding_overrides
            .iter()
            .map(|(path, encoding)| rustpy_settings::FileEncodingOverride {
                path: path.clone(),
                encoding: encoding.clone(),
            })
            .collect();
        self.commit_workspace(workspace, false);
    }

    fn save_workspace(&mut self) -> bool {
        let Some(p) = &self.project else { return true };
        let on_disk = self.effective_workspace();
        let legacy_roots = !rustpy_project::project::ProjectModel::exists(&p.root);
        // Choices belong to files, including files that currently have no tab.
        // Retain unknown keys and records written while this session was open.
        let file_encodings: std::collections::BTreeMap<PathBuf, String> = self
            .encoding_overrides
            .iter()
            .map(|(path, encoding)| (path.clone(), encoding.clone()))
            .collect();
        let mut ws = Workspace {
            open_files: self.editor_session_files(),
            active_file: self
                .active
                .and_then(|i| self.tabs.get(i))
                .and_then(|t| t.state.path.clone()),
            editor_layout: Some(self.capture_editor_layout()),
            expanded_dirs: p.tree.expanded.iter().cloned().collect(),
            tool_windows: self.tool_window_layout(),
            // Keep the recorded choice while discovery runs and in safe mode (which
            // skips the project's own venvs); explicit picks go through select_interpreter.
            interpreter: if self.project_safe_mode() || p.interpreter_rx.is_some() {
                on_disk.interpreter
            } else {
                p.interpreter.as_ref().map(|i| i.path.clone())
            },
            // Breakpoints are filled in by `write_to` below.
            breakpoints: Vec::new(),
            watches: self
                .debug
                .as_ref()
                .map(|d| d.watches.iter().map(|w| w.expression.clone()).collect())
                .unwrap_or_else(|| on_disk.watches.clone()),
            bookmarks: self
                .bookmarks
                .iter()
                .map(|b| rustpy_settings::BookmarkState {
                    path: b.path.clone(),
                    line: b.line,
                    mnemonic: b.mnemonic,
                    description: b.description.clone(),
                    kind: b.kind,
                    list: b.list.clone(),
                })
                .collect(),
            bookmark_lists: self.bookmark_lists.persisted_names(),
            // The project structure lives in project.json; listed here only until that file exists.
            source_roots: if legacy_roots {
                p.tree.source_roots.iter().cloned().collect()
            } else {
                Vec::new()
            },
            excluded_dirs: if legacy_roots {
                p.tree.excluded.iter().cloned().collect()
            } else {
                Vec::new()
            },
            startup_tasks: on_disk.startup_tasks,
            recent_files: self.recent_files.clone(),
            file_encodings: file_encodings
                .into_iter()
                .map(|(path, encoding)| rustpy_settings::FileEncodingOverride { path, encoding })
                .collect(),
            root: None,
            // Line/exception breakpoints and mute are filled in by `write_to` below.
            ..Default::default()
        };
        self.breakpoints.write_to(&mut ws);
        self.commit_workspace(ws, true)
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        let message = msg.into();
        self.accessibility_status(&message);
        self.status = Some((message, 0.0));
    }

    /// Edit options of the active editor (see `edit_options_for`).
    fn edit_options(&self) -> EditOptions {
        match self.active {
            Some(i) => self.edit_options_for(i),
            None => self.base_edit_options(),
        }
    }

    fn base_edit_options(&self) -> EditOptions {
        let mut options = EditOptions::from_settings(&self.settings.editor);
        options.confirm_refactoring_undo = self.settings.refactoring.confirm_undo;
        options
    }

    fn font_size(&self) -> f32 {
        let size = self
            .settings
            .editor
            .preferences
            .scheme_fonts
            .get(&self.settings.editor.color_scheme)
            .filter(|font| font.enabled)
            .map_or(self.settings.editor.font_size, |font| font.size);
        (size + self.zoom).clamp(6.0, 60.0)
    }

    // ----- actions -----------------------------------------------------------

    fn handle_action(&mut self, action: Action, ctx: &egui::Context) {
        if self.project_safe_mode()
            && matches!(
                action,
                Action::StepOver
                    | Action::StepInto
                    | Action::StepOut
                    | Action::Resume
                    | Action::EvaluateExpression
            )
        {
            self.notify_safe_mode("Debugger execution and evaluations");
            return;
        }
        if !self.dumb_mode_admits(action) {
            return;
        }
        if self.tool_window_action(action, ctx) {
            return;
        }
        if self.project_view_shortcut(action, ctx) {
            return;
        }
        if self.non_text_source_hidden() && hidden_source_actions::requires_visible_source(action) {
            self.set_status("Choose Source to use text editor actions");
            return;
        }
        // A later explicit action supersedes an unpainted tool-focus request.
        // In particular Hide All / Jump Last must not revive a hidden pane intent.
        if action != Action::FocusToolWindowControls {
            self.cancel_pending_tool_focus();
        }
        self.sync_shared_editors();
        if !Self::is_emacs_action(action) {
            self.emacs_editing.interrupt();
        }
        self.reconcile_document_markers();
        self.bg.freeze.note_action(action.text());
        self.learn_observe(action);
        if self.remote_development_action(action) {
            return;
        }
        if self.dialog_policy_action(action, ctx) {
            return;
        }
        self.macro_record_action(action);
        if self.route_other_vcs_action(action) {
            return;
        }
        if let Some(digit) = action.bookmark_digit() {
            self.go_to_numbered_bookmark(digit);
            return;
        }
        use Action::*;
        match action {
            Action::StretchToolWindowLeft
            | Action::StretchToolWindowRight
            | Action::StretchToolWindowUp
            | Action::StretchToolWindowDown
            | Action::WidenToolWindow
            | Action::HideActiveToolWindow => {}
            SortTabsByFilename => self.sort_tabs_by_filename(),
            FocusToolWindowControls => self.request_tool_controls_focus(ctx),
            IncreaseLeftToolWidth | DecreaseLeftToolWidth => self.resize_tool_window_keyboard(
                tool_window_accessibility::ToolDimension::LeftWidth,
                action == IncreaseLeftToolWidth,
                ctx,
            ),
            IncreaseBottomToolHeight | DecreaseBottomToolHeight => self
                .resize_tool_window_keyboard(
                    tool_window_accessibility::ToolDimension::BottomHeight,
                    action == IncreaseBottomToolHeight,
                    ctx,
                ),
            ToggleToolWindowFloating => self.toggle_tool_window_floating_keyboard(ctx),
            ToggleToolWindowMaximized => self.toggle_tool_window_maximized_keyboard(ctx),
            EditorLineActions => self.open_editor_line_actions(ctx),
            EnlargeFirstSplit | ShrinkFirstSplit | EqualizeSplits => {
                if self.split.is_some() {
                    self.split_fraction = match action {
                        EnlargeFirstSplit => (self.split_fraction + 0.05).clamp(0.1, 0.9),
                        ShrinkFirstSplit => (self.split_fraction - 0.05).clamp(0.1, 0.9),
                        _ => 0.5,
                    };
                    ctx.request_repaint();
                }
            }
            EmacsKillLine
            | EmacsKillWordForward
            | EmacsKillWordBackward
            | EmacsKillRegion
            | EmacsCopyRegion
            | EmacsYank
            | EmacsYankPop
            | EmacsTranspose => {
                self.emacs_action(action, ctx);
            }
            ScrollToCenter => {
                if let Some(tab) = self.active.and_then(|index| self.tabs.get_mut(index)) {
                    tab.view.request_center(&mut tab.state);
                    self.focus_editor = true;
                    ctx.request_repaint();
                }
            }
            ToggleReadOnly => {
                if let Some(index) = self.active {
                    self.toggle_file_read_only(index);
                }
            }
            MakeWritable => {
                if let Some(index) = self.active {
                    self.request_make_writable(index);
                }
            }
            Save => {
                if let Some(i) = self.active {
                    self.save_tab(i);
                }
            }
            SaveAll => {
                self.save_all();
            }
            CloseEditor => {
                if let Some(i) = self.active {
                    self.request_close_tab(i);
                }
            }
            CloseAllEditors => {
                while let Some(i) = self.tabs.iter().position(|t| !t.has_unsaved_changes()) {
                    self.close_tab(i);
                }
                if !self.tabs.is_empty() {
                    self.set_status("Some tabs have unsaved changes");
                }
            }
            CloseOtherEditors => {
                if let Some(a) = self.active {
                    self.close_other_tabs(a);
                }
            }
            CloseTabsToTheLeft | CloseTabsToTheRight => {
                if let Some(a) = self.active {
                    self.close_tabs_beside(a, action == CloseTabsToTheRight);
                }
            }
            CloseAllButPinned => {
                if let Some(g) = self.active.and_then(|a| self.tabs.get(a)).map(|t| t.group) {
                    self.close_group_tabs(g, false);
                }
            }
            PinActiveTab => {
                if let Some(a) = self.active {
                    self.toggle_pin(a);
                }
            }
            ReopenClosedTab => self.reopen_closed_tab(),
            NewFile => self.new_file_from_action(),
            SaveFileAsTemplate => self.save_file_as_template(),
            NewScratchFile => self.dialog = Dialog::NewScratch(String::new(), 0),
            NewScratchBuffer => self.create_scratch_buffer(),
            Exit => {
                if self.shell_managed_window {
                    self.shell_global_quit_requested = true;
                    // eframe captures the frame after this one: keep it ordinary.
                    Self::send_quit_backdrop_capture(ctx);
                    self.shell_quit_after_frame = Some(self.frame_count + 1);
                } else {
                    self.request_exit();
                }
            }
            Settings => self.dialog = Dialog::Settings,
            Plugins => {
                self.settings_ui.page = settings_ui::SettingsPage::Plugins;
                self.dialog = Dialog::Settings;
            }
            About => self.dialog = Dialog::About,
            KeymapReference => self.dialog = Dialog::KeymapReference,
            SearchStructurally => self.dialog = Dialog::Structural(Box::default()),
            ReplaceStructurally => {
                let st = structural_ui::StructuralState {
                    replace_mode: true,
                    ..Default::default()
                };
                self.dialog = Dialog::Structural(Box::new(st));
            }
            HelpTopics => self.dialog = Dialog::HelpTopics,
            RemoteDevelopment => self.remote_development.open = true,
            WelcomeGuide | GettingStarted | StartGuidedTour | ProductivityGuide
            | AccessibilityGuide | ContactSupport | SubmitFeedback | EditCustomProperties
            | OnlineHelp => self.help_action(action, ctx),
            TipOfTheDay => self.dialog = Dialog::TipOfTheDay(0),
            CheckForUpdates => self.check_for_updates(),
            WhatsNew => self.show_whats_new(),
            Find => self.open_find_bar(false),
            Replace => self.open_find_bar(true),
            FindNext => self.find_step(false),
            FindPrevious => self.find_step(true),
            FindWordAtCaret => self.find_word_at_caret(),
            UndoHistory => self.open_undo_history(),
            GoToLine => self.dialog = Dialog::GoToLine(String::new()),
            NextTab => self.cycle_tab(1),
            PreviousTab => self.cycle_tab(-1),
            ShowErrorDescription => self.show_error_description(),
            NextError | PreviousError => self.navigate_highlighted_error(
                action == NextError,
                self.settings.editor.error_navigation_errors_only,
            ),
            FileStructure => {
                self.structure_ui.focus_popup = true;
                self.dialog = Dialog::FileStructure {
                    query: String::new(),
                    selected: 0,
                }
            }
            FindInFiles => {
                let seed = self.selected_text_seed();
                self.find_in_files.open(false, seed);
            }
            ReplaceInFiles => {
                let seed = self.selected_text_seed();
                self.find_in_files.open(true, seed);
            }
            SearchEverywhere => self.open_everywhere(EverywhereTab::All),
            GoToFile => self.open_everywhere(EverywhereTab::Files),
            GoToClass => self.open_everywhere(EverywhereTab::Classes),
            GoToSymbol => self.open_everywhere(EverywhereTab::Symbols),
            FindAction => self.open_everywhere(EverywhereTab::Actions),
            Switcher => self.open_switcher_popup(ctx),
            GoToDeclaration if self.goto_step_definition() => {}
            GoToDeclaration => {
                if let Some(off) = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.state.primary().head())
                {
                    self.go_to_declaration(off);
                }
            }
            GoToSourceOrStub => self.go_to_source_or_stub(),
            GoToTypeDeclaration => self.go_to_type_declaration(),
            GoToTest => self.go_to_test(),
            GoToRelatedSymbol => self.go_to_related_symbol(),
            SelectIn => self.show_select_in(),
            JumpToSource => self.jump_to_source(ctx),
            OpenInRightSplit => {
                if let Some(path) = self
                    .active
                    .and_then(|index| self.tabs.get(index))
                    .and_then(|tab| tab.state.path.clone())
                {
                    self.open_in_right_split(&path);
                }
            }
            ShowUsages => self.show_usages_popup(),
            NavigateBack => self.nav_history_step(true),
            NavigateForward => self.nav_history_step(false),
            CodeCompletion => self.open_completion(true),
            SmartTypeCompletion => self.open_smart_completion(),
            GenerateNativeStub => self.generate_native_stub(),
            GenerateDocstring => self.generate_docstring(),
            QuickDocumentation => {
                if !self.web_hover() {
                    self.quick_documentation();
                }
            }
            ExternalDocumentation => self.external_documentation(),
            ShowIntentionActions => {
                if !self.web_actions() {
                    self.open_intentions();
                }
            }
            ParameterInfo => self.update_param_info(true),
            FindUsages => {
                if !self.web_references() {
                    self.find_usages();
                }
            }
            FindUsagesInFile => {
                self.usage_ui.in_file();
                self.find_usages();
            }
            FindUsagesSettings => self.open_usage_settings(),
            HighlightUsagesInFile => self.highlight_usages_sticky(),
            Rename => {
                if !self.web_rename() {
                    self.rename_at_caret();
                }
            }
            OptimizeImports => self.optimize_imports_action(),
            ToggleFindView => self.toggle_bottom(BottomTool::Find),
            SplitVertically => self.split_shared_editor(SplitDir::Horizontal),
            SplitAndMoveRight => self.split_and_move_editor(SplitDir::Horizontal),
            SplitAndMoveDown => self.split_and_move_editor(SplitDir::Vertical),
            OpenInOppositeGroup => self.open_in_opposite_group(),
            SplitHorizontally => self.split_shared_editor(SplitDir::Vertical),
            Unsplit | UnsplitAll => self.unsplit_editors(),
            ChangeSplitOrientation => {
                if let Some(d) = self.split {
                    self.split = Some(if d == SplitDir::Horizontal {
                        SplitDir::Vertical
                    } else {
                        SplitDir::Horizontal
                    });
                }
            }
            MoveTabToOppositeGroup => self.move_to_opposite_group(),
            NextSplitter | PrevSplitter => {
                if let Some(i) = self.active {
                    let other = self.tabs[i].group ^ 1;
                    if let Some(j) = self.group_active_tab(other) {
                        self.active = Some(j);
                        self.focus_editor = true;
                    }
                }
            }
            ReformatCode => {
                if let Some(i) = self.active {
                    self.snapshot(i, "Before Reformat");
                }
                self.request_reformat(false);
            }
            ReformatFile => self.request_reformat(true),
            RearrangeCode => self.request_rearrange(),
            LocalHistory => self.open_local_history(),
            ReloadFromDisk => {
                if let Some(i) = self.active {
                    self.reload_tab_from_disk(i);
                }
            }
            Synchronize => {
                self.set_status("Synchronizing with the file system");
                self.queue_open_file_reconciliation();
                self.reconcile_external_changes();
                self.reload_analysis_from_disk();
                if let Some(p) = &mut self.project {
                    p.tree.invalidate_all();
                    p.git_refreshed = -100.0;
                }
                self.symbol_indexes
                    .refresh_project(std::time::Duration::ZERO);
                self.symbol_indexes
                    .refresh_library(std::time::Duration::ZERO);
                self.refresh_project_files();
            }
            // Normally routed through `dialog_policy_action`'s dialog.
            InvalidateCaches => self.invalidate_caches_now(),
            StartStopMacroRecording => {
                if let Some(steps) = self.macro_recording.take() {
                    if steps.is_empty() {
                        self.set_status("Macro recording stopped (nothing recorded)");
                    } else {
                        self.finish_macro_recording(steps);
                    }
                } else {
                    self.macro_recording = Some(Vec::new());
                    self.set_status("Macro recording started (Edit → Macros → Start/Stop Macro Recording to finish)");
                }
            }
            PlaybackLastMacro => {
                if let Some((_, steps)) = self.settings.macros.last().cloned() {
                    self.play_macro(steps, ctx);
                } else {
                    self.set_status("No recorded macro");
                }
            }
            EditMacros => {
                self.begin_macro_editor(ctx);
                self.dialog = Dialog::Macros {
                    selected: 0,
                    new_name: String::new(),
                }
            }
            Cut | Copy | Paste => self.clipboard_action(action, ctx),
            PasteSimple => {
                if let Some(text) = self.clipboard_text() {
                    let opts = self.edit_options();
                    if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                        tab.state.insert_text(
                            &text,
                            &EditOptions {
                                auto_indent: false,
                                auto_close_brackets: false,
                                auto_close_quotes: false,
                                surround_selection: false,
                                ..opts
                            },
                        );
                    }
                } else {
                    self.set_status("Clipboard history is empty (copy inside the IDE first)");
                }
            }
            Annotate => {
                self.annotate_enabled = !self.annotate_enabled;
                self.blame = None;
                if self.annotate_enabled && self.project.as_ref().is_some_and(|p| p.git.is_none()) {
                    self.set_status("Not a Git repository");
                    self.annotate_enabled = false;
                }
            }
            ShowFileHistory => {
                if let Some(path) = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|tab| tab.state.path.clone())
                {
                    self.show_operation_history(path);
                } else {
                    self.set_status("Show History needs a file or directory in a Git repository");
                }
            }
            StashChanges => {
                self.git_panel.tab = crate::git_panel::GitTab::Log;
                self.bottom = Some(BottomTool::Git);
                self.set_status("Use the Stash controls at the top of the Git Log tab");
            }
            AddToGitignore => {
                let path = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone());
                if let Some(p) = path {
                    let root = self
                        .vcs_workspace
                        .mappings
                        .root_for(&p)
                        .cloned()
                        .or_else(|| {
                            self.project
                                .as_ref()
                                .and_then(|p| p.git.as_ref())
                                .map(|g| g.root().to_path_buf())
                        })
                        .unwrap_or_else(|| p.parent().unwrap_or(&p).to_path_buf());
                    let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
                    match rustpy_vcs::add_to_gitignore(&root, &rel) {
                        Ok(()) => self.set_status(format!("Added '{rel}' to .gitignore")),
                        Err(e) => self.set_status(e.to_string()),
                    }
                    if let Some(pr) = &mut self.project {
                        pr.git_refreshed = -100.0;
                    }
                }
            }
            WebDevelopment => self.open_web_languages(),
            DataFileTools => self.open_data_files(),
            CopyJsonPath => self.data_copy_json_path(ctx),
            GitHubPullRequests => self.open_github(),
            GitHubAccounts => self.open_settings_page("github"),
            GitHubShareProject => self.handle_github_event(crate::github_panel::Event::NewShare),
            GitHubCreateGist => self.github_gist(None),
            GitHubCreatePullRequest => self.github_new_pull(None),
            GitHubCloneRepo => self.open_github_repositories(true),
            GitHubOpenOnWeb => self.github_open_file(),
            GitHubOpenIssue => self.github_open_issue(),
            GitBranches => self.dialog = Dialog::Branches,
            CompareWithClipboard => self.compare_with_clipboard(ctx),
            CompareFiles => {
                let selected_directory = self
                    .project
                    .as_ref()
                    .filter(|_| ProjectTree::has_focus(ctx) || self.active.is_none())
                    .and_then(|project| project.tree.selected.clone().filter(|path| path.is_dir()));
                self.open_comparison_picker(selected_directory.as_deref());
            }
            ToggleMarkdownPreview | ToggleRstPreview => {
                self.markdown_mode = match self.markdown_mode {
                    MarkdownMode::EditorOnly => MarkdownMode::Split,
                    MarkdownMode::Split => MarkdownMode::PreviewOnly,
                    MarkdownMode::PreviewOnly => MarkdownMode::EditorOnly,
                };
                self.markdown_workspace.initialized = true;
                self.settings.markdown.layout = match self.markdown_mode {
                    MarkdownMode::EditorOnly => rustpy_settings::markdown::Layout::Editor,
                    MarkdownMode::Split => rustpy_settings::markdown::Layout::Split,
                    MarkdownMode::PreviewOnly => rustpy_settings::markdown::Layout::Preview,
                };
            }
            OpenTask => self.open_task_dialog(),
            DeploymentConfiguration
            | DeploymentUpload
            | DeploymentDownload
            | DeploymentSync
            | DeploymentBrowse => {
                if self.project_safe_mode() {
                    self.notify_safe_mode("Deployment connections and transfers");
                    return;
                }
                let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
                    self.set_status("Open a project first");
                    return;
                };
                let target: Vec<PathBuf> = vec![self
                    .active
                    .and_then(|i| self.tabs[i].state.path.clone())
                    .unwrap_or_else(|| root.clone())];
                let err = match action {
                    DeploymentConfiguration => {
                        self.deploy.open_config(&root);
                        None
                    }
                    DeploymentUpload => {
                        if !self.save_all() {
                            return;
                        }
                        self.deploy.choose_transfer(&root, target, false)
                    }
                    DeploymentDownload => self.deploy.choose_transfer(&root, target, true),
                    DeploymentSync => {
                        if !self.save_all() {
                            return;
                        }
                        self.deploy.open_sync(&root)
                    }
                    _ => self.deploy.open_browser(&root),
                };
                if let Some(e) = err {
                    self.set_status(e);
                }
            }
            ToggleAiAssistant => {
                if self.ai_panel.visible {
                    self.ai_panel.visible = false;
                } else {
                    self.ai_panel.focus();
                }
            }
            Collaborate => self.collaboration.visible = true,
            AiInlineCompletion => self.request_ai_inline(),
            AiExplainCode | AiSuggestRefactoring | AiWriteTests | AiGenerateDocstring
            | AiFixError => self.ai_action(action),
            CopyAsRichText | CopyAsPlainText => {
                let Some(i) = self.active else { return };
                let plain = self.tabs[i].state.copy_text();
                if plain.is_empty() {
                    return;
                }
                if action == CopyAsPlainText {
                    ctx.copy_text(plain);
                    self.set_status("Copied as plain text");
                    return;
                }
                let (start, end) = {
                    let st = &self.tabs[i].state;
                    if st.has_selection() {
                        let r = st.primary().range();
                        (usize::from(r.start()), usize::from(r.end()))
                    } else {
                        let lines = st.selected_lines();
                        let first = st.doc.line_range(*lines.first().unwrap_or(&0)).start();
                        let last = st
                            .doc
                            .line_range_with_newline(*lines.last().unwrap_or(&0))
                            .end();
                        (usize::from(first), usize::from(last))
                    }
                };
                let text = self.tabs[i].state.doc.text();
                let spans = self.tabs[i].state.highlight_spans().to_vec();
                let html = crate::editor::rich_copy::to_html(
                    &text,
                    start,
                    end,
                    &spans,
                    &self.scheme,
                    &self.settings.editor.font_family,
                    self.settings.editor.font_size,
                );
                match crate::editor::rich_copy::set_clipboard_html(&html, &text[start..end]) {
                    Ok(()) => self.set_status("Copied as rich text (HTML with syntax colors)"),
                    Err(e) => {
                        ctx.copy_text(plain);
                        self.set_status(format!(
                            "Rich text clipboard unavailable ({e}); copied plain text"
                        ));
                    }
                }
            }
            LearnIdeFeatures => {
                self.learn.visible = !self.learn.visible;
                if self.learn.completed.is_empty() {
                    self.learn.completed = self.settings.learned_lessons.iter().cloned().collect();
                }
            }
            SettingsSync | SettingsRepository => {
                self.open_settings_sync(action == Action::SettingsRepository)
            }
            ExportSettings | ImportSettings | RestoreDefaultSettings => {
                let home = dirs::home_dir().unwrap_or_default();
                let mode = match action {
                    ExportSettings => 0,
                    ImportSettings => 1,
                    _ => 2,
                };
                let path = if mode == 2 {
                    String::new()
                } else {
                    home.join("rustpy-settings.zip").display().to_string()
                };
                self.dialog = Dialog::ManageSettings {
                    mode,
                    path,
                    result: None,
                };
            }
            SwitchTask => self.open_switch_task_popup(),
            CloseActiveTask => self.close_active_task(),
            SaveContext => self.dialog = Dialog::SaveContext(String::new()),
            LoadContext => self.open_load_context_popup(),
            ClearContext => {
                self.restore_context(rustpy_settings::tasks::EditorContext::default(), true)
            }
            CreateLauncherScript => self.create_command_line_launcher(),
            CollectDiagnostics => self.collect_diagnostics(),
            CustomizeToolbar => self.configure_chrome(),
            ZoomIdeIn => {
                self.settings.chrome.scale = (self.settings.chrome.effective_scale() + 0.1).min(3.0)
            }
            ZoomIdeOut => {
                self.settings.chrome.scale = (self.settings.chrome.effective_scale() - 0.1).max(0.5)
            }
            ZoomIdeReset => self.settings.chrome.scale = 1.0,
            IdeActivityMonitor | DumpIdeThreads | ProfileIdeCpu | ReportIdeIssue => {
                self.diagnostics_action(action)
            }
            ShowLogInFiles => {
                let dir = rustpy_settings::Settings::log_dir();
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                };
                match std::process::Command::new(opener).arg(&dir).spawn() {
                    Ok(_) => self.set_status(format!("Opened {}", dir.display())),
                    Err(e) => self.set_status(format!("Log folder: {} ({e})", dir.display())),
                }
            }
            AddRemoteInterpreter => {
                self.dialog = Dialog::RemoteInterpreter {
                    kind: "ssh".into(),
                    target: String::new(),
                    python: "python3".into(),
                    remote_root: String::new(),
                    error: None,
                }
            }
            QuickList => {
                let items: Vec<(String, String)> = self
                    .settings
                    .quick_list
                    .iter()
                    .filter_map(|id| {
                        Keymap::action_by_id(id)
                            .map(|a| (a.text().to_string(), self.keymap.shortcut_text(a)))
                    })
                    .collect();
                if items.is_empty() {
                    self.set_status(
                        "Quick List is empty: add action ids in Settings › Keymap › Quick List",
                    );
                } else {
                    self.choice = Some(ChoicePopup::new(
                        ChoiceKind::QuickList,
                        "Quick List".into(),
                        items,
                    ));
                }
            }
            TrustProject => self.trust_current_project(),
            ShowInotifyLimitInstructions => self.show_inotify_instructions(),
            HideInotifyLimitWarning => self.hide_inotify_limit_warning(),
            ToggleSemanticHighlighting => {
                self.settings.semantic_highlighting = !self.settings.semantic_highlighting;
                let _ = self.settings.save();
            }
            OpenInBrowser => self.open_current_web_preview(false),
            OpenWebPreview => self.open_current_web_preview(true),
            ToggleWebPreview => self.toggle_bottom(BottomTool::WebPreview),
            OpenUrl => self.dialog = Dialog::OpenWebUrl,
            BuildSphinxDocs => self.build_sphinx_docs(),
            RunDocutilsTask => self.run_docutils_task(),
            OpenDocumentationPreview => self.open_documentation_preview(),
            Frameworks => self.open_frameworks(),
            RunManageTask => {
                // Ctrl+Alt+R also runs the request at the caret in .http files.
                match self
                    .active
                    .filter(|i| self.tabs[*i].state.language == Language::Http)
                {
                    Some(i) => {
                        let line = self.tabs[i]
                            .state
                            .doc
                            .line_of(self.tabs[i].state.primary().head());
                        self.run_http_at(i, line);
                    }
                    None => self.open_manage_py_console(),
                }
            }
            ShowProjectWindows => self.show_project_windows(),
            NextProjectWindow => self.cycle_project_windows(false),
            PreviousProjectWindow => self.cycle_project_windows(true),
            MergeAllProjectWindows => self.merge_all_project_windows_native(),
            OpenProject => {
                let start = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .or_else(dirs::home_dir)
                    .unwrap_or_else(|| PathBuf::from("/"));
                self.open_path_chooser(start);
            }
            NewProject => {
                let start = self.project_creation_directory();
                self.dialog = Dialog::NewProject(Box::new(NewProjectState::new(start)));
            }
            CloseProject => self.close_project(),
            GetFromVcs => {
                let dir = self.project_creation_directory();
                self.dialog = Dialog::CloneRepo(Box::new(CloneState::new(dir)));
            }
            CodeCleanup => self.request_cleanup(),
            SilentCodeCleanup => self.code_cleanup(),
            InspectCode | RunInspectionByName => self.show_inspection_runner(),
            InjectLanguage => self.inject_language_action(),
            EditInjectedFragment => self.edit_injected_fragment_action(),
            GoToInjectedReference => self.navigate_injected_reference(),
            CheckRegExp => {
                let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
                    return;
                };
                let caret = tab.state.primary().head();
                let text = tab.state.doc.text();
                let parse = tab.state.syntax().clone();
                let inj = rustpy_regex::injections(&text, &parse)
                    .into_iter()
                    .find(|j| j.string_range.contains_inclusive(caret));
                match inj {
                    Some(j) => {
                        self.dialog = Dialog::CheckRegex {
                            pattern: j.pattern.clone(),
                            flags: if j.verbose {
                                "VERBOSE".into()
                            } else {
                                String::new()
                            },
                            sample: String::new(),
                            result: None,
                            error: None,
                            rx: None,
                        }
                    }
                    None => self.open_temporary_regex_checker(),
                }
            }
            FillParagraph => self.fill_paragraph(),
            RefactorThis => self.open_refactor_this(),
            PullMembersUp => self.open_member_refactoring(MemberRefactoring::PullUp),
            PushMembersDown => self.open_member_refactoring(MemberRefactoring::PushDown),
            ExtractSuperclass => {
                if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                    let range = TextRange::empty(tab.state.primary().head());
                    self.open_extract_dialog(extract_ui::ExtractRequest::Superclass, range);
                }
            }
            MakeAbstract => self.make_member_abstract(),
            ImplementAbstractMethods => {
                let Some(i) = self.active else { return };
                self.sync_analysis_now();
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let r =
                    rustpy_refactor::refactor2::implement_abstract_methods(&analysis, &file, caret);
                self.apply_refactoring(r);
            }
            ConvertToDataclass => {
                let Some(i) = self.active else { return };
                self.sync_analysis_now();
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let r = rustpy_refactor::refactor2::convert_to_dataclass(&analysis, &file, caret);
                self.apply_refactoring(r);
            }
            AddTypeHints => {
                let Some(i) = self.active else { return };
                self.sync_analysis_now();
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let r = rustpy_refactor::refactor2::add_type_hints(&analysis, &file, caret);
                self.apply_refactoring(r);
            }
            IntroduceTypeAlias => {
                let Some(i) = self.active else { return };
                let Some((_, file)) = self.analysis_file(i) else {
                    return;
                };
                let k = self.tabs[i].state.primary();
                if !k.has_selection() {
                    self.set_status("Select a type annotation expression to introduce an alias");
                    return;
                }
                let r = rustpy_refactor::refactor2::introduce_type_alias(&file, k.range(), "Alias");
                self.apply_refactoring(r);
            }
            InlineTypeAlias => {
                let Some(i) = self.active else { return };
                self.sync_analysis_now();
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let r = rustpy_refactor::refactor2::inline_type_alias(&analysis, &file, caret);
                self.apply_refactoring(r);
            }
            ConvertToPackage => {
                self.open_conversion(rustpy_refactor::conversions::ConversionKind::Package)
            }
            ConvertToModule => {
                self.open_conversion(rustpy_refactor::conversions::ConversionKind::Module)
            }
            MoveSymbol => self.open_move_dialog(),
            RenameModule => {
                let path = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone());
                match path {
                    Some(p) => {
                        let name = p
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        self.dialog = Dialog::RenameModule { path: p, name };
                    }
                    None => self.set_status("Open the module to rename first"),
                }
            }
            ExtractVariable => self.start_extract(ExtractKind::Variable),
            ExtractConstant => self.start_extract(ExtractKind::Constant),
            ExtractField => self.start_extract(ExtractKind::Field),
            ExtractParameter => self.start_extract(ExtractKind::Parameter),
            ExtractMethod => {
                if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                    let caret = tab.state.primary();
                    if caret.has_selection() {
                        self.open_extract_dialog(
                            extract_ui::ExtractRequest::Callable,
                            caret.range(),
                        );
                    } else {
                        self.set_status("Select the statements to extract");
                    }
                }
            }
            Inline => self.open_inline(),
            ConvertRefactoring => {
                self.open_conversion(rustpy_refactor::conversions::ConversionKind::LocalToField)
            }
            SafeDelete => {
                if crate::project_tree::ProjectTree::has_focus(ctx) {
                    if let Some(path) = self.project.as_ref().and_then(|p| p.tree.selected.clone())
                    {
                        self.open_tree_delete(path);
                    }
                } else {
                    self.safe_delete_at_caret();
                }
            }
            MoveRefactoring => self.move_refactoring(),
            ChangeSignature => self.open_change_signature(),
            RunAllTests => self.run_tests(self.default_test_targets(), ""),
            RunDoctests => {
                let path = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone());
                match path {
                    Some(p) if p.extension().and_then(|e| e.to_str()) == Some("py") => {
                        self.run_config(crate::run_context::doctest_config(&p))
                    }
                    _ => self.set_status("Open a Python file with doctests first"),
                }
            }
            DetachTab => {
                if let Some(i) = self.active {
                    if self.tabs.len() > 1 || self.detached.is_none() {
                        self.detached = Some(i);
                        // Move the active editor to another tab in the main window.
                        if let Some(j) = (0..self.tabs.len()).find(|j| *j != i) {
                            self.active = Some(j);
                        }
                    }
                }
            }
            ReattachTab => {
                self.reattach_editor();
            }
            RunTestsInFile => {
                let path = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone());
                match path {
                    Some(p) if p.extension().and_then(|e| e.to_str()) == Some("py") => {
                        self.run_tests(vec![p.display().to_string()], "")
                    }
                    _ => self.set_status("Open a Python test file first"),
                }
            }
            RerunFailedTests => {
                if let Err(e) = self.test_panel.rerun_failed() {
                    self.set_status(e);
                } else {
                    self.bottom = Some(BottomTool::Tests);
                }
            }
            ToggleTestsView => self.toggle_bottom(BottomTool::Tests),
            TogglePythonPackages => self.toggle_bottom(BottomTool::Packages),
            ManagePythonPackages
            | RunSetupPyTask
            | BuildPythonDistribution
            | ShowPythonDependencies => self.package_workflow_action(action),
            ToggleHierarchyView => self.toggle_bottom(BottomTool::Hierarchy),
            ToggleServicesView => self.toggle_bottom(BottomTool::Services),
            ToggleCoverageView => self.toggle_bottom(BottomTool::Coverage),
            ToggleHttpClientView => self.toggle_bottom(BottomTool::HttpClient),
            ToggleDatabaseView => self.toggle_bottom(BottomTool::Database),
            RunDatabaseSql => self.run_database_sql(),
            DatabaseResolutionScope => self.database_resolution_scope(),
            ToggleDiagramView => {
                self.toggle_bottom(BottomTool::Diagram);
                if self.bottom == Some(BottomTool::Diagram) {
                    self.diagram_panel.request_focus();
                    self.focus_editor = false;
                }
            }
            ToggleEndpointsView => self.toggle_bottom(BottomTool::Endpoints),
            ShowUmlDiagram => self.show_diagram(),
            ShowDjangoDiagram => self.django_model_diagram(),
            ShowErDiagram => self.database_er_diagram(),
            UpdateCopyright => self.update_copyright(),
            CopyFile => {
                if let Some(p) = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone())
                {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let stem = p
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let ext = p
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy()))
                        .unwrap_or_default();
                    let _ = name;
                    self.dialog = Dialog::NewFileKind {
                        dir: p.parent().map(Path::to_path_buf).unwrap_or_default(),
                        kind: "copy",
                        name: format!("{stem}_copy{ext}"),
                    };
                    self.copy_source = Some(p);
                }
            }
            RunHttpRequest => {
                if let Some(i) = self.active {
                    let line = self.tabs[i]
                        .state
                        .doc
                        .line_of(self.tabs[i].state.primary().head());
                    self.run_http_at(i, line);
                }
            }
            RunWithConcurrencyDiagram => {
                self.launch_selected_or_context(false, crate::run_context::RunMode::Concurrency)
            }
            ToggleConcurrencyView => self.toggle_bottom(BottomTool::Concurrency),
            ToggleProfilerView => self.toggle_bottom(BottomTool::Profiler),
            RunWithCoverage | ProfileRun => {
                let mode = if action == RunWithCoverage {
                    crate::run_context::RunMode::Coverage
                } else {
                    crate::run_context::RunMode::Profile
                };
                self.launch_selected_or_context(false, mode);
            }
            TypeHierarchy => self.show_hierarchy(HierarchyKind::Subtypes),
            MethodHierarchy => self.show_hierarchy(HierarchyKind::Method),
            CallHierarchy => self.show_hierarchy(HierarchyKind::Callers),
            GotoImplementation => {
                self.sync_analysis_now();
                let Some(i) = self.active else { return };
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let files = self.project_files();
                let locs = hierarchy::implementations(&analysis, &file, caret, &files);
                self.navigate_locations(locs, "No implementations found");
            }
            GotoSuperMethod => {
                self.sync_analysis_now();
                let Some(i) = self.active else { return };
                let Some((analysis, file)) = self.analysis_file(i) else {
                    return;
                };
                let caret = self.tabs[i].state.primary().head();
                let locs = hierarchy::super_methods(&analysis, &file, caret);
                self.navigate_locations(locs, "No super method found");
            }
            InstallRequirements => {
                let path = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("requirements"))
                    })
                    .or_else(|| {
                        self.settings
                            .testing
                            .requirements_file
                            .as_ref()
                            .map(|path| {
                                if path.is_absolute() {
                                    path.clone()
                                } else {
                                    self.project
                                        .as_ref()
                                        .map(|project| project.root.join(path))
                                        .unwrap_or_else(|| path.clone())
                                }
                            })
                    });
                match path {
                    Some(p) if p.is_file() => {
                        self.packages_panel.interpreter = self
                            .project
                            .as_ref()
                            .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()));
                        if let Some(e) = self.packages_panel.install_requirements(&p) {
                            self.set_status(e);
                        }
                        self.bottom = Some(BottomTool::Packages);
                    }
                    _ => self.set_status(
                        "Open a requirements file or configure it in Python Integrated Tools",
                    ),
                }
            }
            SurroundWith => self.open_surround_with(),
            EmmetPreview => self.open_emmet_preview(false),
            SurroundWithEmmet => self.open_emmet_preview(true),
            InsertLiveTemplate => self.open_insert_live_template(),
            SaveSelectionAsLiveTemplate => self.save_selection_as_live_template(),
            PasteFromHistory => {
                if self.clipboard_history.is_empty() {
                    self.set_status("Clipboard history is empty");
                } else {
                    let items = self
                        .clipboard_history
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (format!("{}", i + 1), preview_line(t)))
                        .collect();
                    self.choice = Some(ChoicePopup::new(
                        ChoiceKind::PasteHistory,
                        "Paste from History".into(),
                        items,
                    ));
                }
            }
            CopyPath => self.open_copy_path_in_context(ctx),
            CopyReference => self.copy_reference(ctx),
            CompleteStatement => self.complete_statement(),
            Unwrap => self.unwrap_statement(),
            ContextInfo => {
                if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                    let crumbs = tab.state.breadcrumbs();
                    let text = if crumbs.is_empty() {
                        "(top level)".to_string()
                    } else {
                        crumbs
                            .iter()
                            .map(|(c, _)| c.as_str())
                            .collect::<Vec<_>>()
                            .join(" › ")
                    };
                    self.context_info = Some((text, ctx.input(|i| i.time)));
                }
            }
            QuickDefinition => self.quick_definition(),
            HippieCompletion => self.hippie_complete(),
            LastEditLocation => self.go_to_last_edit_location(),
            RecentlyChangedFiles => self.open_history_file_list(false),
            RecentlyEditedFiles => self.open_history_file_list(true),
            RecentFiles => self.open_recent_files_popup(),
            RecentLocations => self.open_recent_locations_popup(),
            ToggleBookmark => self.toggle_bookmark(None),
            ToggleBookmarkWithMnemonic => {
                self.dialog = Dialog::BookmarkDescription {
                    index: usize::MAX,
                    text: String::new(),
                }
            }
            ShowBookmarks => self.open_bookmarks_popup(),
            ToggleBookmarksView => self.toggle_left_tool(LeftTool::Bookmarks),
            NextBookmark | PreviousBookmark => self.cycle_bookmark(action == NextBookmark),
            Generate => self.open_generate(),
            AnalyzeStackTrace => self.open_stack_trace_analysis(None, None, false),
            DebugLogSettings => self.open_debug_log_settings(),
            Run => self.run_action(false),
            CoverageContext => self.focused_context_action(
                ctx,
                crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Coverage),
            ),
            ProfileContext => self.focused_context_action(
                ctx,
                crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Profile),
            ),
            ModifyContextConfiguration => {
                self.focused_context_action(ctx, crate::run_context::ContextAction::Modify)
            }
            CreateContextConfiguration => {
                self.focused_context_action(ctx, crate::run_context::ContextAction::Create)
            }
            RunContext => self.focused_context_action(
                ctx,
                crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Run),
            ),
            ExecuteInConsole => self.execute_selection_in_console(),
            RunFileInConsole => self.run_file_in_console(),
            RunCell => self.run_cell_at_caret(false),
            RunCellAndAdvance => self.run_cell_at_caret(true),
            RunCellsAbove => {
                if let Some(index) = self.active {
                    let line = self.tabs[index]
                        .state
                        .doc
                        .line_of(self.tabs[index].state.primary().head());
                    self.scientific_run_above(index, line);
                }
            }
            DebugCell => {
                if let Some(index) = self.active {
                    let line = self.tabs[index]
                        .state
                        .doc
                        .line_of(self.tabs[index].state.primary().head());
                    self.scientific_debug_cell(index, line);
                }
            }
            ToggleSciView => self.toggle_bottom(BottomTool::SciView),
            ToggleScientificMode => self.scientific_toggle(),
            Debug => self.debug_action(false),
            DebugContext => self.focused_context_action(
                ctx,
                crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Debug),
            ),
            Stop => self.stop_or_pick(),
            Rerun => self.rerun_action(),
            RunAnything => self.open_run_anything(),
            ToggleDebugView => self.toggle_bottom(BottomTool::Debug),
            ToggleLineBreakpoint => {
                if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                    if let Some(path) = tab.state.path.clone() {
                        let line = tab.state.doc.line_of(tab.state.primary().head());
                        self.toggle_breakpoint(&path, line);
                    }
                }
            }
            ViewBreakpoints
            | RunToCursor
            | JumpToCursor
            | SmartStepInto
            | ShowExecutionPoint
            | QuickEvaluateExpression
            | ToggleTemporaryLineBreakpoint
            | MuteBreakpoints
            | RestartDebug => self.debugger_action(action),
            StepOver => {
                if let Some(d) = &mut self.debug {
                    d.step_over();
                }
            }
            StepInto => {
                if let Some(d) = &mut self.debug {
                    d.step_into();
                }
            }
            StepOut => {
                if let Some(d) = &mut self.debug {
                    d.step_out();
                }
            }
            Resume => {
                if let Some(d) = &mut self.debug {
                    d.continue_();
                }
            }
            EvaluateExpression => {
                let seed = self.selected_text_seed().unwrap_or_default();
                self.dialog = Dialog::EvaluateExpression(seed);
            }
            EditRunConfigurations => self.dialog = Dialog::RunConfigurations,
            ChooseRunConfiguration => self.open_run_chooser(false),
            ChooseDebugConfiguration => self.open_run_chooser(true),
            ToggleBranchesView => self.toggle_bottom(BottomTool::Branches),
            ToggleGitView => self.toggle_bottom(BottomTool::Git),
            VcsPopup => self.open_git_operations(),
            ToggleOtherVcsView => self.toggle_bottom(BottomTool::OtherVcs),
            // Alt+0 shows or hides the Commit view (layout.rs); Ctrl+K always shows it.
            ToggleCommitView => {
                self.save_for_vcs();
                self.toggle_commit_view();
            }
            Commit => {
                self.save_for_vcs();
                self.bottom = Some(BottomTool::Git);
                self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
                self.git_panel.focus_message = true;
                if let Some(p) = &self.project {
                    if let Some(g) = &p.git {
                        self.git_panel.refresh(g, 0.0);
                    }
                }
            }
            // The Push dialog (Ctrl+Shift+K).
            Push => self.open_push_dialog(),
            GitFetch => self.git_in_background(background::GitOp::Fetch),
            ManageRemotes => self.open_remotes_dialog(),
            UpdateProject => self.update_project(),
            ShowDiff => self.show_diff_with_head(),
            VcsShowNextChangeMarker => self.goto_change(true),
            VcsShowPrevChangeMarker => self.goto_change(false),
            TogglePowerSaveMode => self.set_power_save_mode(!self.settings.power_save_mode),
            ShowBackgroundTasks => self.bg.tasks_popup = !self.bg.tasks_popup,
            ToggleRunView => self.toggle_bottom(BottomTool::Run),
            ToggleStructureView => {
                self.structure_ui.focus_tool = true;
                self.toggle_left_tool(LeftTool::Structure);
            }
            ToggleProjectView => {
                // Alt+1 activates the Project view (it takes the keyboard); in the focused view it hides it.
                if self.left_tool == LeftTool::Project
                    && self.layout.left_visible
                    && ProjectTree::has_focus(ctx)
                {
                    self.layout.left_visible = false;
                    self.focus_editor = true;
                } else {
                    self.left_tool = LeftTool::Project;
                    self.layout.left_visible = true;
                    if let Some(p) = &mut self.project {
                        p.tree.request_focus();
                    }
                }
            }
            JumpToLastToolWindow => self.jump_to_last_tool_window(),
            JumpToNavigationBar => self.focus_navigation_bar(ctx),
            ToggleNotificationsView => self.toggle_bottom(BottomTool::Notifications),
            ToggleTreeDetails => {
                self.settings.appearance.show_tree_details =
                    !self.settings.appearance.show_tree_details
            }
            ToggleZenMode => self.toggle_zen_mode(ctx),
            ExitZenMode => self.exit_zen_mode(ctx),
            ToggleDistractionFree => self.toggle_view_mode(layout::ViewMode::DistractionFree, ctx),
            TogglePresentationMode => self.toggle_view_mode(layout::ViewMode::Presentation, ctx),
            ToggleTerminal => {
                if self.bottom == Some(BottomTool::Terminal) && self.terminal_panel.focused {
                    self.bottom = None;
                    self.focus_editor = true;
                } else {
                    self.bottom = Some(BottomTool::Terminal);
                    // Asked for explicitly: a shell may start even in safe mode.
                    self.terminal_panel.set_auto_shell(true);
                    self.terminal_panel.request_focus();
                }
            }
            OpenEditorTerminal => self.open_editor_terminal(false),
            SendSelectionToTerminal => self.open_editor_terminal(true),
            TogglePythonConsole => {
                self.bottom = Some(BottomTool::PythonConsole);
                self.py_console.request_focus();
            }
            ToggleProblemsView => self.toggle_bottom(BottomTool::Problems),
            ToggleTodoView => self.toggle_bottom(BottomTool::Todo),
            ToggleDocumentationView => {
                self.documentation.docked = true;
                self.quick_doc = None;
                self.toggle_bottom(BottomTool::Documentation);
            }
            HideAllToolWindows => self.hide_all_tool_windows(),
            StoreNewLayout
            | StoreDefaultLayout
            | RestoreDefaultLayout
            | RestoreFactoryDefaultLayout
            | RestoreNamedLayout
            | DeleteNamedLayout => self.tool_window_layout_action(action),
            ZoomIn => self.zoom += 1.0,
            ZoomOut => self.zoom -= 1.0,
            ZoomReset => self.zoom = 0.0,
            ToggleLineNumbers => {
                self.settings.editor.show_line_numbers = !self.settings.editor.show_line_numbers
            }
            ToggleWhitespaces => {
                self.settings.editor.show_whitespaces = !self.settings.editor.show_whitespaces
            }
            ToggleIndentGuides => {
                self.settings.editor.show_indent_guides = !self.settings.editor.show_indent_guides
            }
            ToggleSoftWrap => self.settings.editor.soft_wrap = !self.settings.editor.soft_wrap,
            ToggleInlayParameterHints | ToggleInlayTypeHints => {
                if action == ToggleInlayParameterHints {
                    self.settings.editor.inlay_parameter_hints =
                        !self.settings.editor.inlay_parameter_hints;
                } else {
                    self.settings.editor.inlay_type_hints = !self.settings.editor.inlay_type_hints;
                }
                let _ = self.settings.save();
                for t in &mut self.tabs {
                    t.state.semantic_version = None;
                }
            }
            ToggleFullScreen => {
                if self.layout.zen.is_some() {
                    self.exit_zen_mode(ctx);
                }
                self.layout.zen_native_restore = None;
                let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            QuickSwitchScheme => self.dialog = Dialog::QuickSwitch(Box::default()),
            Escape => {
                if self.find.visible {
                    self.close_find_bar();
                }
            }
            SelectInProjectView => {
                if let Some(path) = self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .and_then(|t| t.state.path.clone())
                {
                    if let Some(p) = &mut self.project {
                        p.tree.reveal(&path);
                        p.tree.request_focus();
                    }
                    self.left_tool = LeftTool::Project;
                    self.layout.left_visible = true;
                }
            }
            other => {
                let text = other.text().trim_end_matches('…').to_string();
                self.set_status(format!("'{text}' is not implemented yet (see roadmap.md)"));
            }
        }
    }

    /// Re-reads settings from disk and re-applies theme, scheme, keymap and templates.
    fn reload_settings(&mut self, ctx: &egui::Context) {
        crate::plugin_host::reload();
        let old_stub_roots = self.settings.python_stub_roots.clone();
        self.settings = Settings::load();
        debug_log_ui::apply_saved(&self.settings);
        if old_stub_roots != self.settings.python_stub_roots {
            self.apply_interpreter_paths();
        }
        self.netsec_settings_reloaded();
        self.theme = UiTheme::for_name(&self.settings.appearance.theme);
        self.scheme = settings_ui::scheme_by_name(&self.settings.editor.color_scheme)
            .unwrap_or_else(ColorScheme::dark);
        self.theme.apply(ctx);
        self.keymap = Keymap::preset(&self.settings.keymap);
        self.shortcuts_applied = false;
        self.templates = load_templates();
    }

    fn toggle_bottom(&mut self, tool: BottomTool) {
        self.cancel_pending_tool_focus();
        self.bottom = if self.bottom == Some(tool) {
            None
        } else {
            Some(tool)
        };
    }

    fn cycle_tab(&mut self, delta: i32) {
        self.emacs_editing.interrupt();
        if self.tabs.is_empty() {
            return;
        }
        let group = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.group)
            .unwrap_or(0);
        let members: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group == group)
            .map(|(i, _)| i)
            .collect();
        if members.is_empty() {
            return;
        }
        let n = members.len() as i32;
        let cur = self
            .active
            .and_then(|a| members.iter().position(|m| *m == a))
            .unwrap_or(0) as i32;
        self.active = Some(members[(((cur + delta) % n + n) % n) as usize]);
        self.focus_editor = true;
    }

    // ----- UI --------------------------------------------------------------

    fn global_shortcuts(&mut self, ui: &mut egui::Ui) {
        if ui.input(|input| input.viewport().focused == Some(false)) {
            return;
        }
        if self.ime_owns_shortcuts(ui) {
            return;
        }
        self.sync_shared_editors();
        if self.notebook_global_shortcuts(ui) {
            return;
        }
        if self.stack_trace_keys(ui.ctx()) {
            return;
        }
        if self.diff_controls_owned() {
            return;
        }
        if self.http_editor_events(ui) {
            return;
        }
        if matches!(self.dialog, Dialog::None)
            && !self.ide_shortcuts_blocked()
            && self
                .debug
                .as_ref()
                .is_some_and(|session| session.is_stopped())
            && ui.input_mut(|input| {
                input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, egui::Key::F9)
            })
        {
            self.debugger_force_run_to_cursor();
            return;
        }
        if self.generate_methods_keys(ui.ctx()) {
            return;
        }
        if self.intentions.is_some() && !self.intentions_accessibility_owner(ui.ctx()) {
            self.intentions = None;
        }
        if self.intentions.is_none() {
            self.intentions_accessibility.area = None;
        }
        if self.editor_accessibility_keyboard(ui) {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        if self.emmet_ui.active() {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        if self.intention_menu.dialog_active() || self.bg.problems.infrastructure.dialog {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        if self.injection_ui.active() {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        if matches!(self.dialog, Dialog::StringPaste(_)) {
            self.run_panel.launcher.reset_ctrl_gesture();
            return;
        }
        if self.error_description_keys(ui.ctx()) {
            return;
        }
        self.emacs_observe_frame(ui);
        // Settings › Keymap recording a shortcut owns every key (it is recorded, not run).
        if self.capture_shortcut_keys(ui.ctx()) {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        if self.navigation_bar_keys(ui.ctx()) {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.search_everywhere_gesture.reset();
            return;
        }
        // Tab / Enter template keys act only in the editor that has the keyboard
        // (not while the Project view or a field has it).
        let editor_focused = self.editor_keyboard()
            && self
                .active
                .and_then(|i| self.tabs.get(i))
                .map(|t| ui.memory(|m| m.has_focus(t.view.id)))
                .unwrap_or(false);
        // Template stops must follow the edits of the last frame before keys use them.
        if self.template_session.is_some() {
            self.sync_template_session();
        }
        if self.live_template_choice_keys(ui) || self.live_template_trigger(ui, editor_focused) {
            return;
        }
        // Double Ctrl → Run Anything; launcher popups own the keyboard while open.
        if self.launcher_shortcuts(ui) {
            self.search_everywhere_gesture.reset();
            return;
        }
        // Double Shift → Search Everywhere.
        if !self.editor_controls_available()
            || self.find_in_files.visible
            || self.coordinated_quit_active()
            || !ui.input(|i| i.focused)
        {
            self.search_everywhere_gesture.reset();
        } else if ui.input(|i| self.search_everywhere_gesture.update(i.time, &i.events)) {
            self.open_everywhere(EverywhereTab::All);
            self.learn_observe(Action::SearchEverywhere);
        }
        if self.everywhere.visible || self.find_in_files.visible {
            // Popups own the keyboard (arrows/enter/escape are handled inside them).
            for key in [
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::Enter,
                egui::Key::Tab,
            ] {
                let _ = key;
            }
        }
        if self.completion.visible
            && editor_focused
            && self.editor_keyboard()
            && self.caret_in_template_variable()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
        {
            self.completion.visible = false;
            self.import_completion = None;
            self.next_template_stop();
        }
        if self.completion.visible {
            self.completion_context_keys(ui);
            let n = self.completion.items.len();
            let consume = |ui: &mut egui::Ui, key: egui::Key| {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
            };
            if consume(ui, egui::Key::ArrowDown) && n > 0 {
                self.completion.selected = if self.completion.selected < n {
                    (self.completion.selected + 1) % n
                } else {
                    0
                };
                self.completion_navigated();
            }
            if consume(ui, egui::Key::ArrowUp) && n > 0 {
                self.completion.selected = if self.completion.selected < n {
                    (self.completion.selected + n - 1) % n
                } else {
                    n - 1
                };
                self.completion_navigated();
            }
            if consume(ui, egui::Key::PageDown) && n > 0 {
                self.completion.selected = self.completion.selected.saturating_add(8).min(n - 1);
                self.completion_navigated();
            }
            if consume(ui, egui::Key::PageUp) {
                self.completion.selected = if self.completion.selected < n {
                    self.completion.selected.saturating_sub(8)
                } else {
                    0
                };
                self.completion_navigated();
            }
            if self.completion.selected < n && consume(ui, egui::Key::Enter) {
                self.accept_completion(false);
            } else if n > 0 && consume(ui, egui::Key::Tab) {
                if self.completion.selected >= n {
                    self.completion.selected = 0;
                }
                // A typed live-template abbreviation (iter, super, main…) expands on
                // Tab even with the lookup open.
                self.completion.visible = false;
                self.import_completion = None;
                if self.live_template_management.expand_key != templates::management::ExpandKey::Tab
                    || !self.try_expand_abbreviation()
                {
                    self.completion.visible = true;
                    self.accept_completion(true);
                }
            }
            if consume(ui, egui::Key::Escape) {
                self.completion.visible = false;
                self.import_completion = None;
            }
        }
        if self.import_completion.is_some()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.import_completion = None;
            self.set_status("Import search cancelled");
        }
        if self.completion_worker.pending() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.completion_worker.cancel();
        }
        if self.quick_doc.is_some()
            && self.documentation_popup_owned(ui)
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.quick_doc = None;
        }
        if self.choice.is_some() {
            self.choice_popup_keys(ui);
        } else if self.inplace_rename_keys(ui) {
            // Enter, Tab or Escape finished the in-place rename.
        } else if !self.completion.visible
            && editor_focused
            && ui.input(|i| i.key_pressed(egui::Key::Tab) && i.modifiers.is_none())
        {
            let handled = if self.template_session.is_some() {
                self.next_template_stop();
                self.template_session.is_some() || true
            } else {
                self.try_expand_emmet()
                    || (self.live_template_management.expand_key
                        == templates::management::ExpandKey::Tab
                        && self.try_expand_abbreviation())
            };
            if handled {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
            }
        } else if !self.completion.visible
            && editor_focused
            && self.caret_in_template_variable()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
        {
            // Enter finishes the variable being edited (like Tab).
            self.next_template_stop();
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_basic_completion();
            self.template_session = None;
            self.clear_sticky_highlights();
        }
        if self.template_session.is_some() {
            self.sync_template_session();
        }
        if self.intentions.is_some() {
            for key in [
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::Enter,
                egui::Key::Escape,
            ] {
                // Peek without consuming: the popup reads key_pressed itself, but keep the editor from acting.
                if ui.input(|i| i.key_pressed(key)) {
                    ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key)));
                    let _ = match key {
                        egui::Key::ArrowUp => self
                            .intentions
                            .as_mut()
                            .map(|(_, s)| *s = s.saturating_sub(1)),
                        egui::Key::ArrowDown => self
                            .intentions
                            .as_mut()
                            .map(|(f, s)| *s = (*s + 1).min(f.len().saturating_sub(1))),
                        egui::Key::Enter => {
                            let s = self.intentions.as_ref().map(|(_, s)| *s).unwrap_or(0);
                            self.apply_intention(s);
                            Some(())
                        }
                        _ => {
                            self.intentions = None;
                            Some(())
                        }
                    };
                }
            }
        }
        if self.parameter_info_keyboard(ui) {
            self.parameter_info_keys(ui);
            if self.param_info.is_some()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.close_parameter_info();
            }
        }
        if self.everywhere.visible || self.find_in_files.visible || self.nav.popup_visible() {
            // Let the popup's text field keep focus; the editor must not see these keys.
            // Escape closes the popup (unless it only closes a menu inside it).
            let pressed_esc = ui.input(|i| i.key_pressed(egui::Key::Escape))
                && !egui::Popup::is_any_open(ui.ctx());
            if pressed_esc {
                self.focus_editor = true;
            }
            return;
        }
        if self.keymap_sequence(ui, editor_focused) {
            return;
        }
        if editor_focused && self.macro_shortcut(ui) {
            return;
        }
        let pressed: Vec<(egui::Key, Option<egui::Key>, egui::Modifiers)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *physical_key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        let terminal_focused = (self.bottom == Some(BottomTool::Terminal)
            && self.terminal_panel.focused)
            || (self.bottom == Some(BottomTool::PythonConsole) && self.py_console.focused);
        let tree_focused = self.project.is_some() && ProjectTree::has_focus(ui.ctx());
        for (key, physical_key, mods) in pressed {
            if editor_focused
                && self
                    .active
                    .and_then(|i| self.tabs.get(i))
                    .is_some_and(|t| t.view.clones_caret(key, mods))
            {
                continue; // Ctrl, Ctrl+Up/Down: the editor clones the caret
            }
            let Some(action) = self.keymap.lookup_key(key, physical_key, mods) else {
                continue;
            };
            if action.bookmark_digit().is_some() && !self.parameter_info_keyboard(ui) {
                continue; // Numbered bookmarks belong to the focused editor.
            }
            if Self::is_emacs_action(action) {
                if editor_focused && self.emacs_keyboard_owned() {
                    if ui.input_mut(|input| input.consume_key(mods, key)) {
                        self.handle_action(action, ui.ctx());
                    }
                } else {
                    self.emacs_editing.interrupt();
                }
                continue;
            }
            if tree_focused && TreeKey::from_key(key, mods, Some(action)).is_some() {
                continue; // the focused Project view handles it (Rename, Delete, arrows, Enter, …)
            }
            if self.line_actions_owner_available(ui.ctx())
                && ((self.bottom == Some(BottomTool::Run)
                    && self.run_panel.claims_context_menu(ui.ctx(), key, mods))
                    || (self.bottom == Some(BottomTool::Services)
                        && self.services_panel.claims_context_menu(ui.ctx(), key, mods))
                    || (self.bottom == Some(BottomTool::Tests)
                        && self.test_panel.claims_tree_menu(ui.ctx(), key, mods))
                    || (self.bottom == Some(BottomTool::PythonConsole)
                        && self.py_console.claims_tree_menu(ui.ctx(), key, mods))
                    || (self.bottom == Some(BottomTool::Terminal)
                        && self.terminal_panel.claims_context_menu(ui.ctx(), key, mods)))
            {
                continue; // The exact focused output opens its own keyboard context menu.
            }
            if self.bottom == Some(BottomTool::Run)
                && self.run_panel.focused
                && matches!(
                    action,
                    Action::Find | Action::FindNext | Action::FindPrevious
                )
            {
                continue; // the Run console has its own find bar
            }
            if self.bottom == Some(BottomTool::Debug)
                && (self.debug_panel.claims(action, key, mods)
                    || self.debug_panel.claims_tree_menu(ui.ctx(), key, mods))
            {
                continue; // The Debug window owns row edits/menus and console completion.
            }
            if self.manage_py_claims(ui.ctx(), action) {
                ui.input_mut(|i| i.consume_key(mods, key));
                continue; // the focused manage.py console completes its own command line
            }
            if terminal_focused
                && self.bottom == Some(BottomTool::Terminal)
                && matches!(action, Action::Escape)
                && matches!(self.dialog, Dialog::None)
                && !self.find.visible
                && !egui::Popup::is_any_open(ui.ctx())
            {
                // Terminal → "Move focus to the editor with Escape"; otherwise
                // the shell gets Escape (vi mode, readline).
                if self.terminal_panel.preferences.escape_focuses_editor
                    && ui.input_mut(|i| i.consume_key(mods, key))
                {
                    self.terminal_panel.release_focus(ui.ctx());
                    self.focus_editor = true;
                }
                continue;
            }
            if terminal_focused
                && self.terminal_panel.override_shortcut(key, mods)
                && !matches!(
                    action,
                    Action::ToggleTerminal
                        | Action::HideActiveToolWindow
                        | Action::ToggleToolWindowMaximized
                        | Action::ToggleToolWindowFloating
                        | Action::WidenToolWindow
                        | Action::StretchToolWindowLeft
                        | Action::StretchToolWindowRight
                        | Action::StretchToolWindowUp
                        | Action::StretchToolWindowDown
                        | Action::MoveElementLeft
                        | Action::MoveElementRight
                        | Action::VcsShowNextChangeMarker
                        | Action::VcsShowPrevChangeMarker
                        | Action::FocusToolWindowControls
                        | Action::HideAllToolWindows
                        | Action::ToggleProjectView
                        | Action::Switcher
                        | Action::SearchEverywhere
                        | Action::FindAction
                        | Action::Exit
                        | Action::Settings
                        | Action::Run
                        | Action::Stop
                        | Action::NextTab
                        | Action::PreviousTab
                )
            {
                continue; // let the shell see the key (Terminal → "Override IDE shortcuts")
            }
            if view::is_editor_action(action) && editor_focused || self.find_bar_owns_key(action) {
                continue; // the editor (or the focused find bar) handles it
            }
            if matches!(action, Action::Cut | Action::Copy | Action::Paste)
                && !editor_focused
                && ui.memory(|m| m.focused().is_some())
            {
                // Shift+Insert & co. in a text field: hand it egui's clipboard event.
                Self::forward_clipboard_chord(ui, action, key, mods);
                continue;
            }
            if matches!(action, Action::Escape)
                && !self.find.visible
                && !matches!(self.dialog, Dialog::None)
                && !egui::Popup::is_any_open(ui.ctx())
            {
                self.dialog = Dialog::None;
                // Like Cancel: the editor gets the keyboard back.
                self.focus_editor = true;
                ui.input_mut(|i| i.consume_key(mods, key));
                continue;
            }
            if view::is_editor_action(action) || self.ide_shortcuts_blocked() {
                continue;
            }
            if ui.input_mut(|i| i.consume_key(mods, key)) {
                let ctx = ui.ctx().clone();
                self.handle_action(action, &ctx);
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut pending: Vec<Action> = Vec::new();
        let km = self.keymap.clone();
        let item = |ui: &mut egui::Ui, pending: &mut Vec<Action>, a: Action| {
            let shortcut = km.shortcut_text(a);
            let btn = egui::Button::new(a.text()).shortcut_text(shortcut);
            if ui.add(btn).clicked() {
                pending.push(a);
                ui.close();
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            if crate::custom_menus::render(ui,"main.root",|ui,a|item(ui,&mut pending,a)){return;}
            // Translates menu_button/checkbox labels: the English literals below are the catalog keys.
            let ui = &mut crate::i18n::LocalizedUi::new(ui);
            ui.menu_button("File", |ui| {
                if crate::custom_menus::render(ui,"main.file",|ui,a|item(ui,&mut pending,a)){return;}
                self.native_ui_file_menu(ui);
                item(ui, &mut pending, Action::NewProject);
                item(ui, &mut pending, Action::NewFile);
                self.plugin_file_templates_menu(ui);
                item(ui, &mut pending, Action::NewScratchFile);
                item(ui, &mut pending, Action::NewScratchBuffer);
                item(ui, &mut pending, Action::OpenProject);
                item(ui, &mut pending, Action::GetFromVcs);
                item(ui, &mut pending, Action::CopyFile);
                item(ui, &mut pending, Action::TrustProject);
                item(ui, &mut pending, Action::CloseProject);
                item(ui, &mut pending, Action::ToggleReadOnly);
                item(ui, &mut pending, Action::MakeWritable);
                item(ui, &mut pending, Action::InvalidateCaches);
                let mut power_save = self.settings.power_save_mode;
                if ui.checkbox(&mut power_save, Action::TogglePowerSaveMode.text()).on_hover_text(crate::i18n::tr_static("No background code analysis, completion popups while typing or semantic highlighting")).clicked() {
                    pending.push(Action::TogglePowerSaveMode);
                    ui.close();
                }
                ui.menu_button("Manage IDE Settings", |ui| {
                    for a in [Action::NewProjectSettings,Action::DialogPreferences,Action::AdvancedRegistry,Action::UiInspector,Action::RepairIde,Action::RestartIde]{item(ui,&mut pending,a);}
                    for a in [Action::SettingsSync, Action::SettingsRepository, Action::ExportSettings, Action::ImportSettings, Action::RestoreDefaultSettings] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Recent Projects", |ui| {
                    let recents = self.settings.recent_projects.clone();
                    if recents.is_empty() {
                        ui.label(crate::i18n::tr_static("(none)"));
                    }
                    for p in recents {
                        if ui.button(p.display().to_string()).clicked() {
                            self.request_open_project(&p);
                            ui.close();
                        }
                    }
                });
                ui.separator();
                item(ui, &mut pending, Action::Save);
                item(ui, &mut pending, Action::SaveAll);
                ui.separator();
                item(ui, &mut pending, Action::CloseEditor);
                item(ui, &mut pending, Action::CloseAllEditors);
                item(ui, &mut pending, Action::CloseOtherEditors);
                item(ui, &mut pending, Action::ReopenClosedTab);
                ui.separator();
                item(ui, &mut pending, Action::Settings);
                ui.separator();
                item(ui, &mut pending, Action::Exit);
            });
            ui.menu_button("Edit", |ui| {
                if crate::custom_menus::render(ui,"main.edit",|ui,a|item(ui,&mut pending,a)){return;}
                ui.menu_button("Emacs", |ui| {
                    for action in [Action::EmacsKillLine, Action::EmacsKillWordForward,
                        Action::EmacsKillWordBackward, Action::EmacsKillRegion,
                        Action::EmacsCopyRegion, Action::EmacsYank, Action::EmacsYankPop,
                        Action::EmacsTranspose] {
                        item(ui, &mut pending, action);
                    }
                });
                for a in [Action::Undo, Action::Redo, Action::UndoHistory] {
                    item(ui, &mut pending, a);
                }
                ui.menu_button("Macros", |ui| {
                    for a in [Action::StartStopMacroRecording, Action::PlaybackLastMacro, Action::EditMacros] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.separator();
                for a in [Action::Cut, Action::Copy, Action::CopyAsRichText, Action::CopyAsPlainText, Action::Paste, Action::PasteFromHistory, Action::PasteSimple, Action::CopyPath, Action::CopyReference, Action::CompareWithClipboard, Action::CompareFiles, Action::LocalHistory, Action::ReloadFromDisk, Action::Synchronize] {
                    item(ui, &mut pending, a);
                }
                ui.separator();
                ui.menu_button("Find", |ui| {
                    for a in [Action::Find, Action::Replace, Action::FindNext, Action::FindPrevious, Action::FindWordAtCaret, Action::FindInFiles, Action::ReplaceInFiles, Action::SearchStructurally, Action::ReplaceStructurally, Action::FindUsages, Action::ShowUsages] {
                        item(ui, &mut pending, a);
                    }
                });
                for a in [Action::SelectAll, Action::ExtendSelection, Action::ShrinkSelection, Action::AddSelectionForNextOccurrence, Action::SelectAllOccurrences, Action::AddCaretAbove, Action::AddCaretBelow, Action::ToggleColumnSelectionMode, Action::ToggleStickySelection] {
                    item(ui, &mut pending, a);
                }
                ui.separator();
                for a in [Action::DuplicateLine, Action::DeleteLine, Action::JoinLines, Action::MoveLineUp, Action::MoveLineDown, Action::ToggleCase, Action::IndentSelection, Action::UnindentSelection] {
                    item(ui, &mut pending, a);
                }
                ui.menu_button("Lines", |ui| {
                    for a in [Action::SortLines, Action::ReverseLines, Action::RemoveDuplicateLines, Action::TrimTrailingWhitespace, Action::ConvertIndentsToSpaces, Action::ConvertIndentsToTabs] {
                        item(ui, &mut pending, a);
                    }
                });
            });
            ui.menu_button("View", |ui| {
                if crate::custom_menus::render(ui,"main.view",|ui,a|item(ui,&mut pending,a)){return;}
                item(ui,&mut pending,Action::ToggleScientificMode);
                if ui.checkbox(&mut self.settings.editor.documentation_on_hover,"Quick Documentation on hover").changed() {
                    let _ = self.settings.save();
                }
                ui.separator();
                ui.menu_button("Inlay Hints", |ui| {
                    for a in [Action::ToggleInlayParameterHints, Action::ToggleInlayTypeHints, Action::ToggleMarkdownPreview, Action::ToggleRstPreview, Action::ToggleSemanticHighlighting] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Tool Windows", |ui| {
                    for a in [Action::ToggleProjectView, Action::ToggleStructureView, Action::ToggleBookmarksView, Action::ToggleCommitView, Action::ToggleTerminal, Action::TogglePythonConsole, Action::ToggleSciView, Action::ToggleDocumentationView, Action::TogglePythonPackages, Action::ToggleRunView, Action::ToggleDebugView, Action::ToggleProblemsView, Action::ToggleTodoView, Action::ToggleGitView, Action::ToggleServicesView, Action::ToggleEndpointsView, Action::ToggleNotificationsView, Action::ToggleAiAssistant] {
                        item(ui, &mut pending, a);
                    }
                    ui.separator();
                    item(ui, &mut pending, Action::JumpToLastToolWindow);
                });
                ui.menu_button("Appearance", |ui| {
                    ui.checkbox(&mut self.settings.appearance.compact_mode, "Compact Mode");
                    ui.checkbox(&mut self.settings.appearance.show_main_toolbar, "Main Toolbar");
                    ui.checkbox(&mut self.settings.chrome.show_main_menu, "Main Menu");
                    ui.checkbox(&mut self.settings.editor.show_sticky_lines, "Sticky Lines");
                    ui.checkbox(&mut self.settings.appearance.show_tool_window_bars, "Tool Window Bars");
                    ui.checkbox(&mut self.settings.appearance.show_status_bar, "Status Bar");
                    ui.checkbox(&mut self.settings.appearance.show_memory_indicator, crate::i18n::tr_static("Memory Indicator"));
                    ui.checkbox(&mut self.settings.appearance.show_navigation_bar, crate::i18n::tr_static("Navigation Bar"));
                    let mut tree_details = self.settings.appearance.show_tree_details;
                    if ui.checkbox(&mut tree_details, Action::ToggleTreeDetails.text()).changed() {
                        pending.push(Action::ToggleTreeDetails);
                    }
                    ui.separator();
                    for a in [Action::ToggleFullScreen, Action::ToggleDistractionFree, Action::TogglePresentationMode, Action::ToggleZenMode, Action::ExitZenMode] {
                        // "Exit … Mode" while that mode is on.
                        if ui.add(egui::Button::new(self.view_mode_menu_text(a)).shortcut_text(km.shortcut_text(a))).clicked() {
                            pending.push(a);
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Active Editor", |ui| {
                    ui.checkbox(&mut self.settings.editor.show_line_numbers, "Show Line Numbers");
                    ui.checkbox(&mut self.settings.editor.show_whitespaces, "Show Whitespaces");
                    ui.checkbox(&mut self.settings.editor.show_indent_guides, "Show Indent Guides");
                    ui.checkbox(&mut self.settings.editor.show_right_margin, "Show Right Margin");
                    ui.checkbox(&mut self.settings.editor.soft_wrap, "Soft-Wrap");
                    ui.checkbox(&mut self.settings.editor.render_doc_comments, "Render Documentation Comments");
                    item(ui, &mut pending, Action::ToggleReaderMode);
                    item(ui, &mut pending, Action::ScrollToCenter);
                });
                for a in [Action::QuickSwitchScheme, Action::RecentFiles, Action::ZoomIn, Action::ZoomOut, Action::ZoomReset, Action::HideAllToolWindows] {
                    item(ui, &mut pending, a);
                }
            });
            ui.menu_button("Navigate", |ui| {
                if crate::custom_menus::render(ui,"main.navigate",|ui,a|item(ui,&mut pending,a)){return;}
                ui.checkbox(&mut self.settings.editor.prefer_source_navigation,"Prefer source declarations over stubs");
                for a in [Action::JumpToNavigationBar, Action::SearchEverywhere, Action::FindAction, Action::GoToClass, Action::GoToFile, Action::GoToSymbol, Action::GoToLine, Action::GoToDeclaration, Action::GoToSourceOrStub, Action::GotoImplementation, Action::GoToTypeDeclaration, Action::GoToTest, Action::GoToRelatedSymbol, Action::FileStructure, Action::RecentFiles, Action::RecentLocations, Action::RecentlyChangedFiles, Action::RecentlyEditedFiles, Action::LastEditLocation, Action::Switcher, Action::NavigateBack, Action::NavigateForward, Action::NextError, Action::PreviousError, Action::ShowErrorDescription, Action::NextTab, Action::PreviousTab, Action::SelectIn, Action::JumpToSource, Action::OpenInRightSplit, Action::GotoSuperMethod, Action::TypeHierarchy, Action::MethodHierarchy, Action::CallHierarchy, Action::ShowUmlDiagram,Action::ShowDjangoDiagram,Action::ShowErDiagram] {
                    item(ui, &mut pending, a);
                }
            });
            ui.menu_button("Code", |ui| {
                if crate::custom_menus::render(ui,"main.code",|ui,a|item(ui,&mut pending,a)){return;}
                for a in [Action::CodeCompletion, Action::SmartTypeCompletion, Action::ParameterInfo, Action::QuickDocumentation, Action::ExternalDocumentation, Action::ShowIntentionActions, Action::Generate, Action::SurroundWith, Action::EmmetPreview, Action::SurroundWithEmmet, Action::InsertLiveTemplate, Action::SaveSelectionAsLiveTemplate, Action::CompleteStatement, Action::CommentLine, Action::CommentBlock, Action::ReformatCode, Action::ReformatFile, Action::RearrangeCode, Action::InspectCode, Action::RunInspectionByName, Action::CodeCleanup, Action::SilentCodeCleanup, Action::FillParagraph, Action::InjectLanguage, Action::EditInjectedFragment, Action::GoToInjectedReference, Action::CheckRegExp, Action::GenerateDocstring, Action::UpdateCopyright, Action::OptimizeImports] {
                    item(ui, &mut pending, a);
                }
                ui.menu_button("AI Actions", |ui| {
                    for a in [Action::ToggleAiAssistant, Action::AiExplainCode, Action::AiSuggestRefactoring, Action::AiWriteTests, Action::AiGenerateDocstring, Action::AiFixError] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Folding", |ui| {
                    for a in [Action::ToggleFold, Action::ExpandRegion, Action::CollapseRegion, Action::ExpandAll, Action::CollapseAll, Action::ToggleRenderedDoc, Action::ToggleRenderedDocAll, Action::NextDocstringSection, Action::PreviousDocstringSection] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Move Statement", |ui| {
                    for a in [Action::MoveStatementUp, Action::MoveStatementDown, Action::MoveLineUp, Action::MoveLineDown, Action::MoveElementLeft, Action::MoveElementRight] {
                        item(ui, &mut pending, a);
                    }
                });
            });
            ui.menu_button("Refactor", |ui| {
                if crate::custom_menus::render(ui,"main.refactor",|ui,a|item(ui,&mut pending,a)){return;}
                for a in [Action::RefactorThis, Action::Rename, Action::ChangeSignature, Action::MoveRefactoring, Action::SafeDelete, Action::Inline, Action::ConvertRefactoring] {
                    item(ui, &mut pending, a);
                }
                ui.menu_button("Class", |ui| {
                    for a in [Action::PullMembersUp, Action::PushMembersDown, Action::ExtractSuperclass, Action::MakeAbstract, Action::ImplementAbstractMethods, Action::ConvertToDataclass] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Module", |ui| {
                    for a in [Action::MoveSymbol, Action::RenameModule, Action::ConvertToPackage, Action::ConvertToModule] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Types", |ui| {
                    for a in [Action::AddTypeHints, Action::IntroduceTypeAlias, Action::InlineTypeAlias] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Extract", |ui| {
                    for a in [Action::ExtractVariable, Action::ExtractConstant, Action::ExtractField, Action::ExtractParameter, Action::ExtractMethod] {
                        item(ui, &mut pending, a);
                    }
                });
            });
            ui.menu_button("Run", |ui| {
                if crate::custom_menus::render(ui,"main.run",|ui,a|item(ui,&mut pending,a)){return;}
                if ui.add_enabled(!self.project_safe_mode(), egui::Button::new(crate::i18n::tr_static("Attach to Local Python Process…"))).clicked(){self.debug_panel.discover_python_processes(self.project.as_ref().and_then(|project|project.interpreter.as_ref()).map(|interpreter|interpreter.path.as_path()));self.bottom=Some(BottomTool::Debug);ui.close();}
                if ui.add_enabled(self.debug.as_ref().is_some_and(|session|session.is_stopped()),egui::Button::new(crate::i18n::tr_static("Force Run to Cursor")).shortcut_text("Ctrl+Alt+F9")).clicked(){self.debugger_force_run_to_cursor();ui.close();}

                for a in [Action::Run, Action::Debug, Action::RunContext, Action::DebugContext, Action::ChooseRunConfiguration, Action::ChooseDebugConfiguration, Action::Rerun, Action::RunAnything, Action::Stop, Action::EditRunConfigurations, Action::ToggleLineBreakpoint, Action::ViewBreakpoints, Action::StepOver, Action::StepInto, Action::StepOut, Action::Resume, Action::EvaluateExpression, Action::RunAllTests, Action::RunTestsInFile, Action::RerunFailedTests, Action::RunDoctests, Action::RunWithCoverage, Action::ProfileRun, Action::RunWithConcurrencyDiagram, Action::RestartDebug, Action::ToggleTemporaryLineBreakpoint, Action::MuteBreakpoints, Action::SmartStepInto, Action::RunToCursor, Action::JumpToCursor, Action::ShowExecutionPoint, Action::QuickEvaluateExpression] {
                    item(ui, &mut pending, a);
                }
                ui.separator();
                for a in [Action::ExecuteInConsole, Action::RunFileInConsole, Action::RunCell, Action::RunCellAndAdvance, Action::RunCellsAbove, Action::DebugCell] {
                    item(ui, &mut pending, a);
                }
            });
            ui.menu_button("Tools", |ui| {
                if crate::custom_menus::render(ui,"main.tools",|ui,a|item(ui,&mut pending,a)){return;}
                item(ui,&mut pending,Action::WebDevelopment);
                item(ui, &mut pending, Action::Collaborate);
                item(ui, &mut pending, Action::AiInlineCompletion);
                item(ui, &mut pending, Action::RemoteDevelopment);
                self.plugin_runtime_menu(ui);
                item(ui,&mut pending,Action::DataFileTools);
                item(ui, &mut pending, Action::Frameworks);
                item(ui, &mut pending, Action::AnalyzeStackTrace);
                ui.menu_button("Python formatters", |ui| {
                    for (label, tool) in [("Black", "black"), ("Ruff", "ruff"), ("isort", "isort"), ("autopep8", "autopep8"), ("yapf", "yapf")] {
                        if ui.button(crate::i18n::tr_static(label)).clicked() { self.start_external_formatter(tool, false); ui.close(); }
                    }
                    ui.separator();
                    if ui.button(crate::i18n::tr_static("Import Ruff diagnostics")).clicked() { self.start_external_formatter("ruff", true); ui.close(); }
                });
                for a in [Action::ToggleTerminal, Action::OpenEditorTerminal, Action::SendSelectionToTerminal, Action::TogglePythonConsole, Action::ManagePythonPackages, Action::RunSetupPyTask, Action::BuildPythonDistribution, Action::ShowPythonDependencies, Action::ToggleHttpClientView, Action::ToggleDatabaseView, Action::RunDatabaseSql, Action::DatabaseResolutionScope, Action::RunHttpRequest] {
                    item(ui, &mut pending, a);
                }
                ui.separator();
                for a in [Action::OpenInBrowser, Action::OpenWebPreview, Action::ToggleWebPreview, Action::OpenUrl, Action::BuildSphinxDocs, Action::RunDocutilsTask, Action::OpenDocumentationPreview, Action::RunManageTask, Action::AddRemoteInterpreter, Action::QuickList, Action::SaveFileAsTemplate] {
                    item(ui, &mut pending, a);
                }
                item(ui, &mut pending, Action::CreateLauncherScript);
                ui.menu_button("Deployment", |ui| {
                    for a in [Action::DeploymentConfiguration, Action::DeploymentUpload, Action::DeploymentDownload, Action::DeploymentSync, Action::DeploymentBrowse] {
                        item(ui, &mut pending, a);
                    }
                });
                ui.menu_button("Tasks & Contexts", |ui| {
                    if ui.button(crate::i18n::tr_static("Show Tasks / Configure Servers…")).clicked(){self.task_workspace_open(false);ui.close();}
                    for a in [Action::OpenTask, Action::SwitchTask, Action::CloseActiveTask] {
                        item(ui, &mut pending, a);
                    }
                    ui.separator();
                    for a in [Action::SaveContext, Action::LoadContext, Action::ClearContext] {
                        item(ui, &mut pending, a);
                    }
                });
                if let Some(t) = crate::plugin_host::external_tools_menu(ui, &self.settings.external_tools) {
                    self.pending_tool = Some(t);
                }
            });
            ui.menu_button("Git", |ui| {
                if crate::custom_menus::render(ui,"main.git",|ui,a|item(ui,&mut pending,a)){return;}
                for a in [Action::Commit, Action::Push, Action::UpdateProject, Action::VcsPopup, Action::GitBranches, Action::GitHubPullRequests, Action::GitHubAccounts, Action::GitHubShareProject, Action::GitHubCreateGist, Action::GitHubCreatePullRequest, Action::GitHubCloneRepo, Action::GitHubOpenOnWeb, Action::GitHubOpenIssue, Action::ShowDiff, Action::VcsShowNextChangeMarker, Action::VcsShowPrevChangeMarker, Action::ShowFileHistory, Action::Annotate, Action::StashChanges, Action::AddToGitignore, Action::ToggleGitView, Action::GitFetch, Action::ManageRemotes] {
                    item(ui, &mut pending, a);
                }
            });
            ui.menu_button("Window", |ui| {
                if crate::custom_menus::render(ui,"main.window",|ui,a|item(ui,&mut pending,a)){return;}
                ui.menu_button("Project Windows", |ui| self.project_windows_menu(ui));
                for action in [Action::NextProjectWindow, Action::PreviousProjectWindow] { item(ui, &mut pending, action); }
                #[cfg(target_os = "macos")]
                ui.add_enabled_ui(self.native_project_window_merge_available(), |ui| {
                    item(ui, &mut pending, Action::MergeAllProjectWindows);
                });
                self.tool_window_layout_menu(ui, &mut pending);
                for a in [Action::NextTab, Action::PreviousTab, Action::PinActiveTab, Action::SortTabsByFilename, Action::CloseAllButPinned, Action::CloseTabsToTheLeft, Action::CloseTabsToTheRight, Action::SplitVertically, Action::SplitHorizontally, Action::SplitAndMoveRight, Action::SplitAndMoveDown, Action::OpenInOppositeGroup, Action::ChangeSplitOrientation, Action::EnlargeFirstSplit, Action::ShrinkFirstSplit, Action::EqualizeSplits, Action::MoveTabToOppositeGroup, Action::NextSplitter, Action::FocusToolWindowControls, Action::IncreaseLeftToolWidth, Action::DecreaseLeftToolWidth, Action::IncreaseBottomToolHeight, Action::DecreaseBottomToolHeight, Action::ToggleToolWindowFloating, Action::ToggleToolWindowMaximized, Action::Unsplit, Action::UnsplitAll, Action::DetachTab, Action::ReattachTab, Action::ShowBackgroundTasks] {
                    item(ui, &mut pending, a);
                }
            });
            ui.menu_button("Help", |ui| {
                if crate::custom_menus::render(ui,"main.help",|ui,a|item(ui,&mut pending,a)){return;}
                ui.menu_button("Diagnostic Tools", |ui| {
                    for a in [Action::CollectDiagnostics, Action::IdeActivityMonitor, Action::DumpIdeThreads, Action::ProfileIdeCpu, Action::AnalyzeStackTrace, Action::DebugLogSettings] { item(ui, &mut pending, a); }
                });
                for a in [Action::ReportIdeIssue, Action::WelcomeGuide, Action::GettingStarted, Action::StartGuidedTour, Action::HelpTopics, Action::OnlineHelp, Action::FindAction, Action::LearnIdeFeatures, Action::KeymapReference, Action::TipOfTheDay, Action::ProductivityGuide, Action::AccessibilityGuide, Action::ContactSupport, Action::SubmitFeedback, Action::ShowLogInFiles, Action::CollectDiagnostics, Action::ShowBackgroundTasks, Action::EditCustomProperties, Action::WhatsNew, Action::CheckForUpdates, Action::About] {
                    item(ui, &mut pending, a);
                }
            });
        });
        let ctx = ui.ctx().clone();
        let opts = self.edit_options();
        for a in pending {
            if !Self::is_emacs_action(a) {
                self.emacs_editing.interrupt();
            }
            if view::is_editor_action(a) {
                if let Some(i) = self.active {
                    self.refresh_file_permissions(i);
                    view::handle_editor_action(a, &mut self.tabs[i].state, &opts);
                    self.focus_editor = true;
                }
                self.learn_observe(a);
                self.macro_record_action(a);
                continue;
            }
            self.handle_action(a, &ctx);
        }
    }

    /// Feeds an executed action to the Features Trainer.
    fn learn_observe(&mut self, a: Action) {
        self.observe_help_action(a);
        if self.learn.active.is_some() && self.learn.observe_action(a) {
            self.lesson_completed();
        }
    }

    fn lesson_completed(&mut self) {
        let mut done: Vec<String> = self.learn.completed.iter().cloned().collect();
        done.sort();
        self.settings.learned_lessons = done;
        let _ = self.settings.save();
        self.set_status("Lesson complete!");
    }

    /// Opens lesson `index` in a fresh practice file.
    fn start_lesson(&mut self, index: usize) {
        let Some((id, code, caret)) = self
            .learn
            .lessons()
            .get(index)
            .map(|l| (l.id, l.code, l.caret))
        else {
            return;
        };
        let dir = Settings::config_dir().join("learn");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.py", id.replace('-', "_")));
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path.as_path()))
        {
            self.close_tab(i);
        }
        if let Err(e) = std::fs::write(&path, code) {
            self.set_status(format!("Cannot create the lesson file: {e}"));
            return;
        }
        self.open_file(&path);
        if let Some(tab) = self
            .opened_editor_index(&path)
            .and_then(|i| self.tabs.get_mut(i))
        {
            tab.view.cancel_session_restore();
            tab.state
                .move_primary_to(rustpy_core::Position::new(caret.0, caret.1));
        }
        self.learn.active = Some(crate::learn_panel::ActiveLesson {
            index,
            step: 0,
            path,
        });
        self.learn_seen_version = None;
        self.focus_editor = true;
    }

    /// Main toolbar: project name, run configuration selector, Run/Debug/Stop.
    fn main_toolbar(&mut self, ui: &mut egui::Ui) {
        self.chrome_toolbar(ui);
    }

    pub(super) fn run_toolbar_widget(&mut self, ui: &mut egui::Ui) {
        use crate::a11y::AccessibleCombo;
        let theme = self.theme.clone();
        ui.horizontal(|ui| {
            let running = self.run_panel.has_running()
                || self.debug.as_ref().is_some_and(|d| d.is_active())
                || self.test_panel.is_running()
                || self.coverage_panel.is_running()
                || self.profiler_panel.is_running()
                || self.concurrency_panel.is_running();
            if ui
                .add_enabled(
                    running,
                    egui::Button::new(egui::RichText::new("■").color(if running {
                        theme.error
                    } else {
                        theme.text_disabled
                    })),
                )
                .icon_hint("Stop (Ctrl+F2)")
                .clicked()
            {
                let ctx2 = ui.ctx().clone();
                self.handle_action(Action::Stop, &ctx2);
            }
            if ui
                .button(egui::RichText::new("🐞").color(theme.success))
                .icon_hint("Debug (Shift+F9)")
                .clicked()
            {
                self.debug_action(false);
            }
            if ui
                .button(egui::RichText::new("▶").color(theme.success))
                .icon_hint("Run (Shift+F10)")
                .clicked()
            {
                self.run_action(false);
            }
            if ui.button("◕").icon_hint("Run with Coverage").clicked() {
                self.launch_selected_or_context(false, crate::run_context::RunMode::Coverage);
            }
            if ui
                .button("◴")
                .icon_hint("Profile selected configuration")
                .clicked()
            {
                self.launch_selected_or_context(false, crate::run_context::RunMode::Profile);
            }
            let selected = self
                .project
                .as_ref()
                .and_then(|p| p.run_configs.selected_config().map(|c| c.name.clone()));
            let mut choices = self
                .project
                .as_ref()
                .map(|p| {
                    p.run_configs
                        .configs
                        .iter()
                        .map(|cfg| {
                            (
                                cfg.name.clone(),
                                cfg.kind_label(),
                                cfg.workflow.folder.clone(),
                                cfg.workflow.pinned,
                                cfg.workflow.last_used,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            choices.sort_by_key(|(_, _, _, pinned, used)| (!*pinned, std::cmp::Reverse(*used)));
            let label = selected
                .clone()
                .unwrap_or_else(|| "Current File".to_string());
            egui::ComboBox::from_id_salt("run_config_combo")
                .selected_text(label)
                .width(200.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(selected.is_none(), "Current File")
                        .on_hover_text(
                            "Run / debug the file in the editor (all tests of a test file)",
                        )
                        .clicked()
                    {
                        if let Some(p) = &mut self.project {
                            p.run_configs.selected = None;
                            let _ = p.run_configs.save(&p.root);
                        }
                    }
                    ui.separator();
                    for (name, kind, folder, pinned, _) in &choices {
                        ui.horizontal(|ui| {
                            let label = format!(
                                "{}{}{} ({kind})",
                                if *pinned { "◆ " } else { "" },
                                if folder.is_empty() {
                                    String::new()
                                } else {
                                    format!("{folder}/")
                                },
                                name
                            );
                            if ui
                                .selectable_label(selected.as_deref() == Some(name.as_str()), label)
                                .clicked()
                            {
                                if let Some(project) = &mut self.project {
                                    project.run_configs.selected = Some(name.clone());
                                    let _ = project.run_configs.save(&project.root);
                                }
                            }
                            if ui
                                .small_button(if *pinned { "Unpin" } else { "Pin" })
                                .clicked()
                            {
                                if let Some(project) = &mut self.project {
                                    if let Some(cfg) = project
                                        .run_configs
                                        .configs
                                        .iter_mut()
                                        .find(|cfg| cfg.name == *name)
                                    {
                                        cfg.workflow.pinned = !*pinned;
                                    }
                                    let _ = project.run_configs.save(&project.root);
                                }
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("Edit Configurations…").clicked() {
                        self.dialog = Dialog::RunConfigurations;
                    }
                })
                .combo_named(crate::i18n::tr_static("Run configuration"));
        });
    }

    /// Active tab of `group`: the remembered one, else its first tab.
    fn group_active_tab(&self, group: u8) -> Option<usize> {
        if let Some(i) = self.active {
            if self.tabs.get(i).is_some_and(|t| t.group == group) {
                return Some(i);
            }
        }
        if let Some(id) = self.group_active[group as usize] {
            if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.group == group && t.view.id == id)
            {
                return Some(i);
            }
        }
        self.tabs.iter().position(|t| t.group == group)
    }

    /// Keeps the split state consistent: remembers active tabs and removes empty groups.
    fn sync_groups(&mut self) {
        self.sync_shared_editors();
        if let Some(i) = self.active {
            if let Some(t) = self.tabs.get(i) {
                self.group_active[t.group as usize] = Some(t.view.id);
            }
        }
        if self.split.is_some() {
            let has0 = self.tabs.iter().any(|t| t.group == 0);
            let has1 = self.tabs.iter().any(|t| t.group == 1);
            if !has0 || !has1 {
                self.split = None;
                for t in &mut self.tabs {
                    t.group = 0;
                }
            }
        } else {
            for t in &mut self.tabs {
                t.group = 0;
            }
        }
    }

    fn editor_area(&mut self, ui: &mut egui::Ui) {
        self.begin_detached_root_targets(ui.ctx());
        self.reconcile_document_markers();
        let theme = self.theme.clone();
        if self.project.is_none() && self.tabs.is_empty() {
            self.welcome_screen(ui);
            return;
        }
        if self.tabs.is_empty() {
            let rect = ui.available_rect_before_wrap();
            ui.painter()
                .rect_filled(rect, 0.0, rgb(self.scheme.editor.background));
            crate::appearance_assets::paint(ui.ctx(), ui.painter(), rect, false);
            let c = rect.center();
            let dim = theme.text_dim;
            let lines = [
                ("Search Everywhere", "Shift Shift"),
                ("Go to File", "Ctrl+Shift+N"),
                ("Recent Files", "Ctrl+E"),
                ("Navigation Bar", "Alt+Home"),
                ("Drop files here to open them", ""),
            ];
            for (i, (a, b)) in lines.iter().enumerate() {
                let y = c.y - 40.0 + i as f32 * 24.0;
                ui.painter().text(
                    Pos2::new(c.x - 10.0, y),
                    Align2::RIGHT_CENTER,
                    a,
                    egui::FontId::proportional(14.0),
                    dim,
                );
                ui.painter().text(
                    Pos2::new(c.x + 10.0, y),
                    Align2::LEFT_CENTER,
                    b,
                    egui::FontId::proportional(14.0),
                    theme.text,
                );
            }
            return;
        }
        self.sync_groups();
        match self.split {
            Some(SplitDir::Horizontal) => {
                let avail = ui.available_width();
                let id = self.editor_split_panel_id(false);
                self.seed_editor_split_panel(ui.ctx(), id);
                let response = egui::Panel::left(id)
                    .resizable(true)
                    .default_size(avail * self.split_fraction)
                    .min_size(120.0)
                    .max_size((avail - 120.0).max(120.0))
                    .show(ui, |ui| self.group_area(ui, 0));
                if avail > 0.0 {
                    self.split_fraction = (response.response.rect.width() / avail).clamp(0.1, 0.9);
                }
                self.group_area(ui, 1);
            }
            Some(SplitDir::Vertical) => {
                let avail = ui.available_height();
                let id = self.editor_split_panel_id(true);
                self.seed_editor_split_panel(ui.ctx(), id);
                let response = egui::Panel::top(id)
                    .resizable(true)
                    .default_size(avail * self.split_fraction)
                    .min_size(80.0)
                    .max_size((avail - 80.0).max(80.0))
                    .show(ui, |ui| self.group_area(ui, 0));
                if avail > 0.0 {
                    self.split_fraction = (response.response.rect.height() / avail).clamp(0.1, 0.9);
                }
                self.group_area(ui, 1);
            }
            None => self.group_area(ui, 0),
        }
    }

    /// One editor group: its tab strip plus the editor of its active tab.
    fn group_area(&mut self, ui: &mut egui::Ui, group: u8) {
        self.tab_policy_group_area(ui, group);
    }

    fn group_editor_area(&mut self, ui: &mut egui::Ui, group: u8) {
        self.reconcile_document_markers();
        let theme = self.theme.clone();
        let Some(i) = self.group_active_tab(group) else {
            let rect = ui.available_rect_before_wrap();
            ui.painter()
                .rect_filled(rect, 0.0, rgb(self.scheme.editor.background));
            crate::appearance_assets::paint(ui.ctx(), ui.painter(), rect, false);
            return;
        };
        if self.diff_editor_tab(ui, i) {
            return;
        }
        if self.focus_editor
            || ui.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Text(_)
                            | egui::Event::Paste(_)
                            | egui::Event::Key { pressed: true, .. }
                            | egui::Event::PointerButton { pressed: true, .. }
                    )
                })
            })
        {
            self.refresh_file_permissions(i);
        }
        if self.detached == Some(i) {
            // A detached tab has one rendering owner. Rendering its EditorView
            // here as well would overwrite viewport state from the OS window.
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label("This editor is open in a separate window.");
                    if ui.button("Reattach").clicked() {
                        self.reattach_editor();
                    }
                });
            });
            return;
        }
        // Non-text viewers are independently presented in both groups. Only the
        // actual clicked group acquires keyboard ownership.
        let auxiliary_view = self.tabs[i].image.is_some()
            || self.tabs[i].notebook.is_some()
            || self.csv_registration(i).is_some()
            || self.tabs[i]
                .state
                .path
                .as_ref()
                .and_then(|p| p.extension())
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("svg"))
            || self.is_large_file_placeholder(i)
            || (matches!(
                self.tabs[i].state.language,
                Language::Markdown | Language::Rst
            ) && self.markdown_mode != MarkdownMode::EditorOnly);
        let rect = ui.available_rect_before_wrap();
        if auxiliary_view
            && self.non_text_controls_available(ui.ctx())
            && ui.rect_contains_pointer(rect)
            && ui.input(|input| input.pointer.primary_pressed())
        {
            if let Some(previous) = self.active.filter(|&previous| previous != i) {
                let view = &mut self.tabs[previous].view;
                ui.memory_mut(|memory| memory.surrender_focus(view.id));
                view.focused = false;
            }
            self.active = Some(i);
            self.group_active[group as usize] = Some(self.tabs[i].view.id);
        }
        if self.tabs[i].image.is_some() || self.tabs[i].notebook.is_some() {
            if self.tabs[i].image.is_some() {
                self.image_viewer(ui, i);
            } else {
                self.notebook_area(ui, i);
            }
            return;
        }
        if self.large_file_banner(ui, i) {
            return;
        }
        let controls_available = self.non_text_controls_available(ui.ctx());
        self.data_files_toolbar(ui, i);
        if ui
            .add_enabled_ui(controls_available, |ui| self.csv_table_area(ui, i))
            .inner
        {
            self.retire_non_text_focus(i, ui.ctx(), controls_available);
            return;
        }
        if ui
            .add_enabled_ui(controls_available, |ui| self.svg_controls(ui, i))
            .inner
        {
            self.retire_non_text_focus(i, ui.ctx(), controls_available);
            self.svg_preview_view(ui, i);
            return;
        }
        self.web_browser_bar(ui, i);
        if matches!(
            self.tabs[i].state.language,
            Language::Markdown | Language::Rst
        ) && self.markdown_preview(ui, i)
        {
            return;
        }
        if self.active != Some(i) {
            // Inactive group: draw its editor without keyboard focus; clicking it activates the group.
            let opts = self.edit_options_for(i);
            let font_size = self.font_size();
            let keymap = self.keymap.clone();
            let tab_path = self.tabs[i].state.path.clone();
            let bp_lines = self.breakpoint_lines(tab_path.as_deref());
            let inline_vals = self.debug_inline_values(i);
            let bm_lines = self.bookmark_lines(tab_path.as_deref());
            let exec_line = self
                .exec_point
                .as_ref()
                .filter(|(p, _)| tab_path.as_deref() == Some(p.as_path()))
                .map(|(_, l)| l.saturating_sub(1));
            let run_lines = self.run_lines(i);
            let line_changes: Vec<rustpy_vcs::LineChange> = self
                .line_changes
                .as_ref()
                .filter(|(p, _, _)| tab_path.as_deref() == Some(p.as_path()))
                .map(|(_, _, c)| {
                    diff_workspace::filter_line_changes(
                        c,
                        &self.tabs[i].state.doc,
                        &self.settings.diff_merge,
                    )
                })
                .unwrap_or_default();
            let search_highlights = self.sticky_highlights_for(i);
            self.bind_context_run_owner(ui.ctx(), i);
            let (gutter_markers, gutter_quick_fixes) = self.gutter_view_inputs(i);
            let focused_now;
            let clipboard_events;
            // A queued activation belongs to the active instance, not retained passive focus.
            let keyboard = self.editor_keyboard_for(i) && !self.focus_editor;
            {
                let scheme = &self.scheme;
                let settings = &self.settings.editor;
                let tab = &mut self.tabs[i];
                tab.view.clipboard_payload = self.clipboard_payload.clone();
                let cfg = ViewConfig {
                    scheme,
                    theme: &theme,
                    settings,
                    opts: &opts,
                    keymap: &keymap,
                    font_size,
                    search_highlights: &search_highlights,
                    request_focus: false,
                    run_lines: &run_lines,
                    line_changes: &line_changes,
                    breakpoints: &bp_lines,
                    exec_line,
                    inline_values: &inline_vals,
                    bookmarks: &bm_lines,
                    gutter_markers: &gutter_markers,
                    gutter_quick_fixes: &gutter_quick_fixes,
                    code_vision: self.code_vision.display_for(&tab.state),
                    annotations: None,
                    coverage: &[],
                    keyboard,
                };
                clipboard_events = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                focused_now = tab.view.focused;
            }
            self.record_auxiliary_editor_events(i, &clipboard_events);
            self.string_paste_events(i, &clipboard_events);
            for event in &clipboard_events {
                self.code_vision_editor_event(i, event);
            }
            self.reconcile_document_markers();
            if focused_now {
                self.active = Some(i);
                self.group_active[group as usize] = Some(self.tabs[i].view.id);
            }
            return;
        }
        if self.find.visible {
            self.find_bar(ui);
        }
        if self.tabs[i].image.is_some() {
            self.image_viewer(ui, i);
            return;
        }
        if self.large_file_banner(ui, i) {
            return;
        }
        if self.tabs[i].notebook.is_some() {
            self.notebook_area(ui, i);
            return;
        }
        self.editor_breadcrumbs(ui, i);
        // An in-place rename in another editor ends (its keys are handled in global_shortcuts).
        self.inplace_rename_check_editor(i);
        let opts = self.edit_options();
        let font_size = self.font_size();
        let keymap = self.keymap.clone();
        // Also with the find bar open: opening a file or jumping to a declaration
        // focuses the editor (the bar's query field cancels this when it takes focus).
        let request_focus = self.active == Some(i)
            && self.editor_keyboard()
            && std::mem::take(&mut self.focus_editor);
        let run_lines = self.run_lines(i);
        let tab_path = self.tabs[i].state.path.clone();
        let bp_lines = self.breakpoint_lines(tab_path.as_deref());
        let inline_vals = self.debug_inline_values(i);
        let bm_lines = self.bookmark_lines(tab_path.as_deref());
        self.update_blame(i);
        let annotations: Option<Vec<(String, u8, String)>> = self
            .blame
            .as_ref()
            .filter(|(p, _, _)| tab_path.as_deref() == Some(p.as_path()))
            .map(|(_, _, l)| l.clone());
        let coverage: Vec<(usize, u8)> = tab_path
            .as_deref()
            .map(|p| self.coverage_panel.lines_for(p))
            .unwrap_or_default()
            .into_iter()
            .map(|(l, s)| {
                (
                    l,
                    match s {
                        rustpy_run::LineCoverage::Covered => 0u8,
                        rustpy_run::LineCoverage::Missed => 1,
                        rustpy_run::LineCoverage::Excluded => 2,
                        rustpy_run::LineCoverage::Partial => 3,
                    },
                )
            })
            .collect();
        let exec_line = self
            .exec_point
            .as_ref()
            .filter(|(p, _)| tab_path.as_deref() == Some(p.as_path()))
            .map(|(_, l)| l.saturating_sub(1));
        self.bind_context_run_owner(ui.ctx(), i);
        let (gutter_markers, gutter_quick_fixes) = self.gutter_view_inputs(i);
        let events;
        let sticky_highlights = self.sticky_highlights_for(i);
        let keyboard = self.editor_keyboard();
        {
            let scheme = &self.scheme;
            let settings = &self.settings.editor;
            let mut search_highlights = if self.find.visible {
                self.find.matches.clone()
            } else {
                Vec::new()
            };
            search_highlights.extend(sticky_highlights);
            let tab = &mut self.tabs[i];
            tab.view.clipboard_payload = self.clipboard_payload.clone();
            let line_changes: Vec<rustpy_vcs::LineChange> = self
                .line_changes
                .as_ref()
                .filter(|(p, _, _)| tab.state.path.as_deref() == Some(p.as_path()))
                .map(|(_, _, c)| {
                    diff_workspace::filter_line_changes(
                        c,
                        &tab.state.doc,
                        &self.settings.diff_merge,
                    )
                })
                .unwrap_or_default();
            let bp_lines = bp_lines.clone();
            let cfg = ViewConfig {
                scheme,
                theme: &theme,
                settings,
                opts: &opts,
                keymap: &keymap,
                font_size,
                search_highlights: &search_highlights,
                request_focus,
                run_lines: &run_lines,
                line_changes: &line_changes,
                breakpoints: &bp_lines,
                exec_line,
                inline_values: &inline_vals,
                bookmarks: &bm_lines,
                gutter_markers: &gutter_markers,
                gutter_quick_fixes: &gutter_quick_fixes,
                code_vision: self.code_vision.display_for(&tab.state),
                annotations: annotations.as_deref(),
                coverage: &coverage,
                keyboard,
            };
            events = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
        }
        self.reconcile_document_markers();
        let ctx = ui.ctx().clone();
        for ev in events {
            match ev {
                ViewEvent::BlameCommit { revision, intent } => {
                    self.open_blame_context(i, revision, intent)
                }
                ViewEvent::ShortcutChoices(actions) => {
                    self.offer_keymap_choices(actions, Some(self.tabs[i].view.id));
                }
                ViewEvent::Action(a) => {
                    // A context menu belongs to this rendered editor, including an inactive split.
                    if a == Action::Paste {
                        self.paste_for_rendered_tab(i);
                    } else if !self.dumb_mode_admits_with(a, |app| app.dumb_editor_target(i, None))
                    {
                        // Refused while indexing, remembered at this editor's carets.
                    } else if !self.copy_action_for_tab(a, i) {
                        self.handle_action(a, &ctx);
                    }
                }
                ViewEvent::CodeVisionFindUsages(target) => {
                    self.code_vision_editor_event(i, &ViewEvent::CodeVisionFindUsages(target));
                }
                ViewEvent::GutterIntent(intent) => self.gutter_actions_intent(i, intent),
                ViewEvent::OpenProblems => {
                    self.code_vision_editor_event(i, &ViewEvent::OpenProblems);
                }
                ViewEvent::Zoom(d) => self.zoom += d,
                ViewEvent::LargePaste(text) => self.dialog = Dialog::ConfirmPaste(text),
                ViewEvent::StringPaste(text) => {
                    self.open_string_paste(i, &text);
                }
                ViewEvent::Performed(a) => {
                    self.emacs_editing.interrupt();
                    self.editor_action_performed(i, a);
                }
                ViewEvent::ClipboardPayload(payload) => self.clipboard_payload = Some(payload),
                ViewEvent::Copied(text) => {
                    self.emacs_editing.interrupt();
                    let rich = self.settings.editor.preferences.rich_text_copy
                        && self.tabs[i].state.copy_text() == text;
                    self.remember_copied(text);
                    if rich {
                        self.publish_editor_rich_copy(i);
                    }
                }
                ViewEvent::GutterClick(line) => {
                    if let Some(path) = self.tabs[i].state.path.clone() {
                        if !path.starts_with("<typeshed>") {
                            self.toggle_breakpoint(&path, line);
                        }
                    }
                }
                ViewEvent::ChangeMarkerClick(line) => {
                    self.open_change_popup(i, line, ctx.input(|inp| inp.pointer.interact_pos()))
                }
                ViewEvent::GutterRun(line) => {
                    self.gutter_run(i, line, crate::run_context::RunMode::Run)
                }
                ViewEvent::ScientificCell { line, action } => match action {
                    Action::RunCell => self.run_cell(i, line, false),
                    Action::RunCellAndAdvance => self.run_cell(i, line, true),
                    Action::RunCellsAbove => self.scientific_run_above(i, line),
                    Action::DebugCell => self.scientific_debug_cell(i, line),
                    _ => {}
                },
                ViewEvent::GutterRunWith(line, mode) => self.gutter_run(i, line, mode),
                ViewEvent::ContextRun(request) => self.editor_context_request(i, *request),
                ViewEvent::GutterContext(line) => {
                    self.open_breakpoint_popup(i, line, ctx.input(|inp| inp.pointer.hover_pos()))
                }
                ViewEvent::OpenUrl(url) => self.open_url(&url),
                ViewEvent::DocumentationLink {
                    target,
                    offset,
                    document,
                    version,
                } => {
                    if self.tabs[i].state.doc.identity() == document
                        && self.tabs[i].state.doc.version() == version
                    {
                        self.follow_inline_documentation_link(i, offset, &target);
                    }
                }
            }
        }
        self.change_popup_ui(ui, i);
        self.inplace_rename_follow(i);
        self.after_editor(ui, i);
    }

    /// Pointer-only lenses/widgets belong to the rendered tab, including a
    /// passive split. Opening Problems is idempotent rather than a toggle.
    fn code_vision_editor_event(&mut self, i: usize, event: &ViewEvent) {
        if self.tabs.get(i).is_none() {
            return;
        }
        match event {
            ViewEvent::GutterIntent(intent) => self.gutter_actions_intent(i, *intent),
            ViewEvent::ContextRun(request) => {
                self.editor_context_request(i, request.as_ref().clone())
            }
            ViewEvent::OpenProblems => {
                self.active = Some(i);
                self.group_active[self.tabs[i].group as usize] = Some(self.tabs[i].view.id);
                self.bottom = Some(BottomTool::Problems);
            }
            ViewEvent::CodeVisionFindUsages(target) => {
                if self.tabs[i].state.doc.identity() != target.document {
                    return;
                }
                // Needs complete indexes; queued as Find Usages at the lens's declaration.
                let at = target.range.start();
                if !self.dumb_mode_admits_with(Action::FindUsages, |app| {
                    app.dumb_editor_target(i, Some(at))
                }) {
                    return;
                }
                let mut runtime = std::mem::take(&mut self.code_vision);
                self.code_vision_find_usages(&mut runtime, target.clone());
                self.code_vision = runtime;
            }
            _ => {}
        }
    }

    /// Whether tab `i`'s editor keeps keyboard focus inside the IDE while the
    /// OS window itself is inactive (egui reports no widget focus then).
    fn editor_focus_without_window(&self, ui: &egui::Ui, i: usize) -> bool {
        !ui.input(|input| input.focused)
            && self.editor_keyboard_for(i)
            && ui.memory(|memory| memory.has_focus(self.tabs[i].view.id))
    }

    /// Semantic interactions after the editor was drawn: Ctrl+click navigation,
    /// completion auto-popup and rendering, hover documentation, quick doc.
    fn after_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        let owns_emacs =
            self.emacs_owner == Some((self.tabs[i].state.doc.identity(), self.tabs[i].view.id));
        if self.emacs_frame_interrupted || (owns_emacs && !self.tabs[i].view.focused) {
            self.emacs_editing.interrupt();
        }
        // Typing into a library or stub file: say why nothing happens (once per refusal).
        if self.tabs[i].state.take_read_only_refusal() {
            self.request_make_writable(i);
        }
        let now = ui.input(|inp| inp.time);
        let (ctrl_click, typed, caret_rect, hovered, focused, version, caret) = {
            let t = &self.tabs[i];
            (
                t.view.ctrl_click,
                t.view.typed.clone(),
                t.view.caret_screen_rect,
                t.view.hovered_offset,
                t.view.focused,
                t.state.doc.version(),
                t.state.primary().head(),
            )
        };
        // The lookup belongs to the focused editor, not to the OS window: it
        // stays open while the whole window is merely inactive (Alt+Tab, a
        // headless display) and closes when focus moves elsewhere in the IDE.
        let lookup_focused = focused || self.editor_focus_without_window(ui, i);
        // Macros record typing in every file type (Enter etc. arrive as performed actions).
        if !typed.is_empty() {
            self.macro_record_text(&typed);
        }
        if let Some(off) = ctrl_click {
            self.go_to_declaration(off);
            return;
        }
        self.draw_source_intentions(ui, i);
        if self.tabs[i].state.language != Language::Python || self.project.is_none() {
            self.document_completion_frame(ui, i, &typed, caret_rect, lookup_focused, now);
            return;
        }
        self.import_workflow_frame(i, &typed, now, focused, ui.ctx());
        // An open lookup refreshes immediately; a new lookup waits for the
        // configured delay and is bound to this exact editor/caret/version.
        if !typed.is_empty() {
            let last = typed.chars().last().unwrap();
            if last == '.' || last == '/' || last == '\\' || rustpy_core::is_ident_continue(last) {
                if self.completion.visible {
                    self.open_completion(false);
                } else if self.completion_auto_popup_allowed(i)
                    || (self.settings.completion.show_as_you_type
                        && self.path_completion_context(i).is_some())
                {
                    let delay =
                        self.settings.completion.auto_popup_delay_ms.min(10000) as f64 / 1000.0;
                    self.completion_ui.due =
                        Some((self.tabs[i].view.id, version, caret, now + delay));
                }
            } else {
                self.completion.visible = false;
                self.import_completion = None;
                self.completion_ui.due = None;
            }
        } else if self.completion.visible && self.completion.doc_version != version {
            if caret >= self.completion.prefix_start {
                let explicit = self.completion.explicit;
                self.open_completion(explicit);
            } else {
                self.completion.visible = false;
                self.import_completion = None;
            }
        }
        if let Some(wait) =
            self.completion_ui
                .popup_wait(self.tabs[i].view.id, version, caret, focused, now)
        {
            if wait.is_zero() {
                self.open_completion(false);
            } else {
                ui.ctx().request_repaint_after(wait);
            }
        }
        // The caret moved without typing (click, End, arrows), or what lies between
        // the prefix start and the caret is no longer one identifier: close, so
        // Enter cannot replace that text.
        if self.completion.visible
            && self.completion.doc_version == version
            && caret != self.completion.caret
        {
            self.completion.visible = false;
            self.import_completion = None;
        }
        // Pressing the mouse on the lookup takes focus from the editor: keep the
        // lookup until the click on an item lands.
        let on_lookup = self
            .completion
            .rect
            .into_iter()
            .chain(self.completion_ui.auxiliary_rect)
            .any(|r| ui.input(|inp| inp.pointer.interact_pos().is_some_and(|p| r.contains(p))));
        if self.completion.visible
            && ((!lookup_focused && !on_lookup)
                || caret < self.completion.prefix_start
                || (!self.completion_ui.paths
                    && !self.tabs[i]
                        .state
                        .doc
                        .slice(TextRange::new(self.completion.prefix_start, caret))
                        .chars()
                        .all(rustpy_core::is_ident_continue)))
        {
            if std::env::var("RUSTPY_DEBUG_COMPLETION").is_ok() {
                log::info!(
                    "debug-completion: lookup closed focused={focused} on_lookup={on_lookup} caret={caret:?} prefix_start={:?}",
                    self.completion.prefix_start
                );
            }
            self.completion.visible = false;
            self.import_completion = None;
        }
        if self.completion.visible {
            if let Some(rect) = caret_rect {
                self.draw_completion(ui, rect);
            }
        }
        if focused && !typed.is_empty() && self.settings.code_style.add_unambiguous_imports_on_fly {
            self.start_automatic_import(i, &typed);
        }
        // Parameter info: auto-show after typing '(' or ','. While it is open,
        // typing or moving the caret follows the current argument and leaving
        // the call's parentheses closes it (Ctrl+P popups included).
        let here = Some((self.tabs[i].view.id, version, caret));
        let active = self.active == Some(i);
        if active {
            self.poll_parameter_info_lookup();
        }
        if active
            && self.settings.completion.show_parameter_info
            && (typed.ends_with('(')
                || typed.ends_with(',')
                || ((self.param_info.is_some() || self.parameter_info_ui.lookup_pending())
                    && self.param_info_at != here))
        {
            self.update_param_info(false);
        }
        if self.quick_doc.is_some()
            && active
            && !self.documentation.pinned
            && self.quick_doc_at != here
        {
            self.quick_doc = None;
        }
        if active && self.param_info.is_some() {
            if let Some(rect) = caret_rect {
                self.draw_parameter_info_with_cycle(ui, rect);
            }
        }
        if let Some((text, since)) = self.context_info.clone() {
            if now - since < 3.0 {
                let theme = self.theme.clone();
                egui::Area::new(ui.id().with("context_info"))
                    .order(egui::Order::Foreground)
                    .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 8.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(theme.panel_bg)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(text).color(theme.text).monospace());
                            });
                    });
            } else {
                self.context_info = None;
            }
        }
        // Hover documentation after the pointer rests on an identifier (not while a menu is open).
        let hovered = if egui::Popup::is_any_open(ui.ctx()) {
            None
        } else {
            hovered
        };
        // While suspended, the debugger's value popup replaces the documentation.
        let hovered = if self.debug_editor_hover(ui, i, hovered, now) {
            None
        } else {
            hovered
        };
        let enabled = active
            && hovered.is_some()
            && self.settings.editor.documentation_on_hover
            && self.documentation_popup_owned(ui);
        self.tick_documentation_hover(ui, i, enabled, self.settings.editor.documentation_delay_ms);
        if active {
            let was_docked = self.documentation.docked;
            self.draw_quick_documentation(
                ui,
                caret_rect
                    .unwrap_or_else(|| Rect::from_min_size(Pos2::new(200.0, 200.0), Vec2::ZERO)),
            );
            if !was_docked && self.documentation.docked {
                self.bottom = Some(BottomTool::Documentation);
                self.quick_doc = None;
            }
        }
    }

    fn draw_intentions(
        &mut self,
        ui: &mut egui::Ui,
        caret: Rect,
        fixes: &[QuickFix],
        selected: usize,
    ) {
        let owner = ui.id().with("intentions");
        self.intentions_accessibility.area = Some(owner);
        if !self.intentions_accessibility_owner(ui.ctx()) {
            self.intentions = None;
            self.intentions_accessibility.area = None;
            return;
        }
        let theme = self.theme.clone();
        let pos = Pos2::new(caret.left(), caret.bottom() + 2.0);
        let mut clicked: Option<usize> = None;
        let mut close = false;
        let popup = egui::Area::new(owner)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_min_width(260.0);
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| {
                                let mut previous_group = None;
                                for (i, f) in fixes.iter().enumerate() {
                                    if let Some(group) = self.intention_menu.group(i) {
                                        if previous_group != Some(group) {
                                            if previous_group.is_some() {
                                                ui.separator();
                                            }
                                            ui.label(
                                                egui::RichText::new(group.label())
                                                    .small()
                                                    .strong()
                                                    .color(theme.text_dim),
                                            );
                                            previous_group = Some(group);
                                        }
                                    }
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width().max(260.0), 22.0),
                                        Sense::click(),
                                    );
                                    intentions_accessibility::row(&resp, &f.title, i == selected);
                                    if i == selected {
                                        ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
                                    } else if resp.hovered() {
                                        ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
                                    }
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 8.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        "💡",
                                        egui::FontId::proportional(12.0),
                                        theme.warning,
                                    );
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 28.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        &f.title,
                                        egui::FontId::proportional(13.0),
                                        if i == selected {
                                            Color32::WHITE
                                        } else {
                                            theme.text
                                        },
                                    );
                                    if i == selected {
                                        resp.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    if resp.clicked() {
                                        clicked = Some(i);
                                    }
                                }
                            });
                        if let Some(fix) = fixes.get(selected) {
                            self.intention_preview(ui, fix);
                        }
                    });
            });
        popup.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Context actions")
        });
        if clicked.is_none() {
            intentions_accessibility::announce(ui.ctx(), popup.response.id, fixes, selected);
        }
        let (up, down, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if let Some(i) = clicked {
            self.apply_intention(i);
            return;
        }
        if let Some((_, sel)) = &mut self.intentions {
            if down && *sel + 1 < fixes.len() {
                *sel += 1;
            }
            if up && *sel > 0 {
                *sel -= 1;
            }
            if enter {
                let s = *sel;
                self.apply_intention(s);
                return;
            }
            if esc {
                close = true;
            }
        }
        if close {
            self.intentions = None;
            self.focus_editor = true;
        }
    }

    fn draw_completion(&mut self, ui: &mut egui::Ui, caret: Rect) {
        crate::performance_trace::completion_drawn(self.completion.items.len());
        let theme = self.theme.clone();
        let items = &self.completion.items;
        let selected = self.completion.selected;
        let row_h = self
            .settings
            .completion
            .lookup_item_height
            .clamp(16.0, 48.0);
        let loading = self.completion_loading();
        let visible_rows = items.len().min(10).max(usize::from(loading));
        let height = visible_rows as f32 * row_h + 8.0;
        let width = if self.settings.completion.show_full_signatures {
            700.0
        } else {
            460.0
        };
        let screen = ui.ctx().content_rect();
        let mut pos = Pos2::new(caret.left() - 24.0, caret.bottom() + 2.0);
        if pos.y + height > screen.bottom() {
            pos.y = (caret.top() - height - 2.0).max(screen.top());
        }
        pos.x = pos.x.min(screen.right() - width - 4.0).max(screen.left());
        let mut clicked: Option<usize> = None;
        let mut hovered_item: Option<usize> = None;
        // The typed prefix is the same for every row: slice the document once.
        let prefix = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|tab| {
                tab.state.doc.slice(TextRange::new(
                    self.completion.prefix_start,
                    self.completion.caret,
                ))
            })
            .unwrap_or_default();
        let area = egui::Area::new(ui.id().with("completion_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        // Always as tall as the rows shown (a remembered smaller size must not clip the list).
                        egui::ScrollArea::vertical()
                            .max_height(visible_rows as f32 * row_h)
                            .min_scrolled_height(visible_rows as f32 * row_h)
                            .auto_shrink([false, true])
                            // Rows abut: the viewport is sized for exactly
                            // `visible_rows * row_h`, so inter-item spacing
                            // would clip the last visible row.
                            .show_rows(
                                ui,
                                row_h,
                                items.len() + usize::from(loading),
                                |ui, range| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    // Only the rows in view are laid out: a lookup
                                    // can hold thousands of items.
                                    // The selection stays in view (as `scroll_to_me` did
                                    // when every row was laid out).
                                    if selected < items.len() && !range.contains(&selected) {
                                        let top = ui.max_rect().top()
                                            + (selected as f32 - range.start as f32) * row_h;
                                        ui.scroll_to_rect(
                                            Rect::from_min_size(
                                                Pos2::new(ui.max_rect().left(), top),
                                                Vec2::new(ui.available_width(), row_h),
                                            ),
                                            None,
                                        );
                                    }
                                    let shown =
                                        range.start.min(items.len())..range.end.min(items.len());
                                    for (idx, item) in items[shown.clone()]
                                        .iter()
                                        .enumerate()
                                        .map(|(k, item)| (shown.start + k, item))
                                    {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(ui.available_width(), row_h),
                                            Sense::click(),
                                        );
                                        resp.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::WidgetType::SelectableLabel,
                                                true,
                                                idx == selected,
                                                format!("{}, {}", item.label, item.detail),
                                            )
                                        });
                                        if idx == selected {
                                            ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
                                            resp.scroll_to_me(None);
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
                                        }
                                        let (badge, color) = completion_badge(item.kind);
                                        let icon = Rect::from_center_size(
                                            Pos2::new(rect.left() + 12.0, rect.center().y),
                                            Vec2::splat(14.0),
                                        );
                                        ui.painter().rect_filled(icon, 7.0, color);
                                        ui.painter().text(
                                            icon.center(),
                                            Align2::CENTER_CENTER,
                                            badge,
                                            egui::FontId::proportional(9.0),
                                            Color32::WHITE,
                                        );
                                        let text_color = if idx == selected {
                                            Color32::WHITE
                                        } else {
                                            theme.text
                                        };
                                        let job = completion_ui::highlight_label(
                                            &item.label,
                                            &prefix,
                                            text_color,
                                            theme.accent,
                                        );
                                        let galley = ui.painter().layout_job(job);
                                        ui.painter().galley(
                                            Pos2::new(
                                                rect.left() + 26.0,
                                                rect.center().y - galley.size().y / 2.0,
                                            ),
                                            galley,
                                            text_color,
                                        );
                                        if !item.detail.is_empty() {
                                            let mut detail = item.detail.clone();
                                            if !self.settings.completion.show_full_signatures
                                                && detail.chars().count() > 48
                                            {
                                                detail = detail.chars().take(45).collect();
                                                detail.push('…');
                                            }
                                            ui.painter().text(
                                                Pos2::new(rect.right() - 6.0, rect.center().y),
                                                Align2::RIGHT_CENTER,
                                                detail,
                                                egui::FontId::proportional(12.0),
                                                if idx == selected {
                                                    Color32::from_gray(220)
                                                } else {
                                                    theme.text_dim
                                                },
                                            );
                                        }
                                        if resp.clicked() {
                                            clicked = Some(idx);
                                        }
                                        if resp.hovered() {
                                            hovered_item = Some(idx);
                                        }
                                    }
                                    // While a request runs past its short wait: a
                                    // loading row, replaced by the results.
                                    if loading && range.contains(&items.len()) {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(ui.available_width(), row_h),
                                            Sense::hover(),
                                        );
                                        resp.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::WidgetType::Label,
                                                true,
                                                "Searching…",
                                            )
                                        });
                                        ui.painter().text(
                                            Pos2::new(rect.left() + 26.0, rect.center().y),
                                            Align2::LEFT_CENTER,
                                            "Searching…",
                                            egui::FontId::proportional(13.5),
                                            theme.text_dim,
                                        );
                                    }
                                },
                            );
                    });
            });
        area.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Code completion")
        });
        if let Some(item) = items.get(selected) {
            crate::a11y::announce(
                ui.ctx(),
                area.response.id,
                &format!(
                    "Completion {} of {}: {}, {}. Up and Down to select; Enter to insert; Escape to close",
                    selected + 1,
                    items.len(),
                    item.label,
                    item.detail
                ),
            );
        }
        self.completion.rect = Some(area.response.rect);
        self.draw_completion_documentation(ui, area.response.rect);
        if let Some(idx) = clicked {
            self.completion.selected = idx;
            self.accept_completion(false);
            self.focus_editor = true;
        }
    }

    /// Problems tool window: diagnostics of the current file.
    fn problems_view(&mut self, ui: &mut egui::Ui) {
        // "Current File" / "Project Errors" tabs; the latter draws its own list.
        if self.problems_scope(ui) {
            return;
        }
        self.problems_filters(ui);
        let theme = self.theme.clone();
        let pinned = self
            .tool_lists
            .problems_pin
            .as_ref()
            .and_then(|(root, id)| {
                (self
                    .project
                    .as_ref()
                    .is_some_and(|project| &project.root == root))
                .then(|| self.tabs.iter().position(|tab| tab.view.id == *id))
                .flatten()
            });
        let Some(i) = pinned.or(self.active) else {
            ui.label(egui::RichText::new("No file open").color(theme.text_dim));
            return;
        };
        let diags = self.tabs[i].state.diagnostics();
        let title = self.tabs[i].state.title();
        let errors = diags
            .iter()
            .filter(|d| d.severity == rustpy_core::Severity::Error)
            .count();
        let warnings = diags.len() - errors;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{title}: {errors} error(s), {warnings} warning(s)"))
                    .color(theme.text_dim),
            );
        });
        ui.separator();
        let mut jump: Option<TextRange> = None;
        let mut fixes = false;
        egui::ScrollArea::vertical()
            .id_salt("problems")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if diags.is_empty() {
                    ui.label(egui::RichText::new("✔ No problems found").color(theme.success));
                }
                for d in &diags {
                    if !self.problem_visible(
                        self.tabs[i].state.path.as_deref().unwrap_or(Path::new("")),
                        d,
                    ) {
                        continue;
                    }
                    let pos = self.tabs[i].state.doc.offset_to_position(d.range.start());
                    let color = match d.severity {
                        rustpy_core::Severity::Error => theme.error,
                        rustpy_core::Severity::Warning => theme.warning,
                        _ => theme.text_dim,
                    };
                    let resp = ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(if d.severity == rustpy_core::Severity::Error {
                                "●"
                            } else {
                                "▲"
                            })
                            .color(color),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(&d.message).color(theme.text))
                                .sense(Sense::click()),
                        )
                        .on_hover_text(d.code.as_str())
                        .union(
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{title}:{}", pos))
                                        .color(theme.text_dim),
                                )
                                .sense(Sense::click()),
                            ),
                        )
                    });
                    resp.inner.context_menu(|ui| {
                        if ui.button("Quick fixes…").clicked() {
                            jump = Some(d.range);
                            fixes = true;
                            ui.close();
                        }
                    });
                    if resp.inner.clicked() {
                        if let Some(path) = self.tabs[i].state.path.clone() {
                            self.tool_lists.problem_selected =
                                Some((path, self.tabs[i].state.doc.line_of(d.range.start())));
                        }
                        if self.tool_lists.problems_follow || resp.inner.double_clicked() {
                            jump = Some(d.range);
                        }
                    }
                    if resp.inner.double_clicked() {
                        jump = Some(d.range);
                    }
                    if resp.inner.hovered() {
                        resp.inner
                            .on_hover_text("Double-click to navigate; right-click for quick fixes");
                    }
                }
            });
        if self.tool_lists.problems_preview {
            if let Some((path, line)) = &self.tool_lists.problem_selected {
                self.tool_source_preview(ui, path, *line);
            }
        }
        if let Some(r) = jump {
            self.active = Some(i);
            self.mark_nav_jump();
            let tab = &mut self.tabs[i];
            tab.state.reveal_offset(r.start());
            tab.state
                .set_carets(vec![crate::editor::Caret::at(r.start())]);
            self.focus_editor = true;
            if fixes {
                self.open_intentions();
            }
        }
    }

    /// TODO tool window: `TODO`/`FIXME` comments across the project.
    fn todo_view(&mut self, ui: &mut egui::Ui) {
        self.todo_workspace(ui);
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        self.tool_window_gear(
            ui,
            layout::ToolRef::Bottom(self.bottom.unwrap_or(BottomTool::Run)),
        );
        let terminal_announcement_owner =
            self.component_menu_owner_available(false) && self.editor_accessibility.owner.is_none();
        self.terminal_panel
            .set_announcement_owner(terminal_announcement_owner);
        self.run_panel
            .set_announcement_owner(terminal_announcement_owner);
        self.py_console
            .set_accessibility_owner(terminal_announcement_owner);
        self.test_panel
            .set_accessibility_owner(terminal_announcement_owner);
        self.hierarchy_panel
            .set_accessibility_owner(terminal_announcement_owner);
        self.services_panel
            .set_accessibility_owner(terminal_announcement_owner);
        self.profiler_panel
            .set_accessibility_owner(terminal_announcement_owner);
        self.endpoints_panel
            .set_accessibility_owner(terminal_announcement_owner);
        self.packages_panel
            .set_accessibility_owner(terminal_announcement_owner);
        let Some(current) = self.bottom else { return };
        self.tool_controls_focus_entry(ui, current.title());
        // Fill the panel: egui shrinks a panel to short content (Git without a
        // repository), and that height would be saved with the project's layout.
        ui.set_min_height(ui.available_height());
        let theme = self.theme.clone();
        ui.horizontal(|ui| {
            for tool in self.legacy_bottom_tool_tabs() {
                if ui.selectable_label(current == tool, tool.title()).clicked() {
                    self.bottom = Some(tool);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("—").icon_hint("Hide (Shift+Escape)").clicked() {
                    self.bottom = None;
                }
                if ui
                    .selectable_label(self.floating_bottom, "⧉")
                    .icon_hint(if self.floating_bottom {
                        "Dock the tool window"
                    } else {
                        "Float the tool window in its own window"
                    })
                    .clicked()
                {
                    self.toggle_tool_window_floating_keyboard(ui.ctx());
                }
                if ui
                    .selectable_label(self.bottom_maximized, "▭")
                    .icon_hint("Maximize / restore the tool window")
                    .clicked()
                {
                    self.toggle_tool_window_maximized_keyboard(ui.ctx());
                }
            });
        });
        ui.separator();
        self.measurement_selector(ui, current);
        match current {
            BottomTool::Run => {
                self.console_stack_trace_link(ui);
                self.run_panel.settings = self.settings.run_console.clone();
                self.run_panel.soft_wrap = self.settings.run_console.soft_wrap;
                self.run_panel.scroll_to_end = self.settings.run_console.scroll_to_end;
                let mono = egui::FontId::new(
                    self.settings.run_console.font_size.clamp(8.0, 40.0),
                    crate::theme::mono_family(false, false),
                );
                let events = self.run_panel.ui(ui, &theme, mono);
                if self.settings.run_console != self.run_panel.settings {
                    self.settings.run_console = self.run_panel.settings.clone();
                    let _ = self.settings.save();
                }
                for event in events {
                    self.handle_run_panel_event(event);
                }
            }
            BottomTool::PullRequests => self.github_panel_ui(ui, &theme),
            BottomTool::Branches => self.branch_workspace_docked_ui(ui, &theme),
            BottomTool::OtherVcs => self.other_vcs_tool_window(ui),
            BottomTool::Git => {
                if !self.builtin_plugin_enabled("git") {
                    ui.label("This bundled capability is disabled in Plugins");
                    return;
                }
                self.vcs_roots_ui(ui);
                self.prepare_branch_workspace();
                self.task_commit_ui(ui);
                let now = ui.input(|i| i.time);
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                let events = {
                    self.configure_git_grammar();
                    let task = self
                        .tasks
                        .as_ref()
                        .and_then(|tasks| tasks.active_task())
                        .map(|task| (task.id.clone(), task.summary.clone()));
                    let workspace_root = self
                        .project
                        .as_ref()
                        .and_then(|project| project.git.as_ref())
                        .map(|repo| repo.root().to_path_buf())
                        .unwrap_or_else(|| root.clone());
                    self.git_panel.workspace_task(&workspace_root, task);
                    self.git_panel.commit_workspace.unsaved = self
                        .tabs
                        .iter()
                        .filter(|tab| tab.has_unsaved_changes())
                        .filter_map(|tab| tab.state.path.clone())
                        .collect();
                    self.configure_operations_context();
                    let menu_owner = self.component_menu_owner_available(false);
                    self.git_panel.set_accessibility_owner(menu_owner);
                    let git = self.project.as_ref().and_then(|p| p.git.as_ref());
                    self.git_panel.ui(ui, &theme, git, &root, now)
                };
                self.prepare_branch_workspace();
                let mut refresh = false;
                for ev in events {
                    match ev {
                        GitEvent::GitHubCommit { root, commit } => {
                            self.github_open_commit(root, commit)
                        }
                        GitEvent::BranchWorkspace(event) => {
                            self.handle_branch_workspace_event(event)
                        }
                        GitEvent::Invoke { root, action } => {
                            self.invoke_operation_action(root, action, ui.ctx())
                        }
                        GitEvent::OpenRepository(root) => self.request_open_project(&root),
                        GitEvent::OperationsSettings(settings) => {
                            self.settings.vcs_operations = settings;
                            self.blame = None;
                            let _ = self.settings.save();
                        }
                        GitEvent::OpenFile(p) => self.open_file(&p),
                        GitEvent::GitignoreTemplates(dir) => self.open_gitignore_templates(dir),
                        GitEvent::ApplyDiff(edit) => {
                            let path = edit.path.clone();
                            let text = edit.replacement.clone();
                            if self.apply_diff_request(edit) {
                                self.git_panel.acknowledge_diff_apply(&path, text);
                            }
                        }
                        GitEvent::CommitWorkspace(event) => self.commit_workspace_event(event),
                        GitEvent::ShowCommit(id) => {
                            self.git_panel.tab = crate::git_panel::GitTab::Log;
                            self.git_panel.selected_commit = Some(id);
                        }
                        GitEvent::ResolveConflict(p) => {
                            if let Some(git) = GitRepo::discover(&p) {
                                match git.conflict_sides(&p) {
                                    Ok((base, ours, theirs)) => {
                                        let diff =
                                            crate::diff_view::SideBySide::new(&ours, &theirs);
                                        self.dialog = Dialog::ResolveConflict {
                                            path: p,
                                            ours,
                                            theirs,
                                            base,
                                            diff: Box::new(diff),
                                        };
                                    }
                                    Err(e) => self.set_status(e.to_string()),
                                }
                            }
                        }
                        GitEvent::Status(m) => {
                            self.report_git_status(m);
                            refresh = true;
                        }
                        GitEvent::FilesChanged => {
                            self.reload_changed_files();
                            self.reload_analysis_from_disk();
                            refresh = true;
                        }
                        // Pushes at once, or opens the Push dialog when push.default or the upstream needs a decision.
                        GitEvent::Push => self.quick_push(),
                        GitEvent::ForcePush => self.force_push_in_background(),
                        GitEvent::Commit { push } => self.commit_from_git_panel(push, now),
                        GitEvent::Update => self.update_project(),
                        GitEvent::Fetch => self.git_in_background(background::GitOp::Fetch),
                        GitEvent::Run(op) => self.start_git_op(op),
                        GitEvent::GenerateCommitMessage => {
                            let (diff, recent) = {
                                let git = self.project.as_ref().and_then(|p| p.git.as_ref());
                                let mut paths: Vec<PathBuf> =
                                    self.git_panel.checked.iter().cloned().collect();
                                paths.sort();
                                let diff: String = git
                                    .map(|g| {
                                        paths
                                            .iter()
                                            .filter_map(|p| {
                                                g.diff_file_to_head(p)
                                                    .ok()
                                                    .map(|d| format!("--- {}\n{d}", p.display()))
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    })
                                    .unwrap_or_default();
                                let recent: Vec<String> = git
                                    .and_then(|g| g.log(10).ok())
                                    .map(|l| l.into_iter().map(|c| c.summary).collect())
                                    .unwrap_or_default();
                                (diff, recent)
                            };
                            if diff.trim().is_empty() {
                                self.set_status("Check the files to commit first");
                            } else {
                                let cfg = self.ai_config();
                                self.ai_panel.redirect =
                                    Some(crate::ai_panel::Redirect::CommitMessage);
                                self.ai_panel.start(
                                    &cfg,
                                    rustpy_ai::commit_message(&diff, &recent),
                                    String::new(),
                                    None,
                                );
                                self.ai_panel.visible = false;
                                self.set_status("Generating a commit message…");
                            }
                        }
                    }
                }
                if refresh {
                    if let Some(p) = &mut self.project {
                        if p.git.is_none() {
                            p.git = GitRepo::discover(&p.root);
                        }
                        p.git_refreshed = -100.0;
                    }
                }
            }
            BottomTool::Debug => {
                if self.project_safe_mode() {
                    ui.label("Trust the project before using debugger commands or evaluations.");
                    return;
                }
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                self.debug_session_tabs(ui);
                let events = self.debug_panel.ui(ui, &theme, self.debug.as_mut(), mono);
                for ev in events {
                    match ev {
                        DebugEvent::Navigate(path, line, column) => {
                            if let Some(encoding) =
                                self.debug.as_ref().map(DebugSession::column_encoding)
                            {
                                self.navigate_debug_frame(&path, line, column, encoding);
                            }
                        }
                        DebugEvent::ForceRunToCursor => self.debugger_force_run_to_cursor(),
                        DebugEvent::InstallDebugger(interpreter) => {
                            if self.project_safe_mode() {
                                self.set_status("Trust the project before installing debugpy");
                            } else {
                                self.debug_panel.setup_task = Some(
                                    rustpy_debug::backend_tasks::BackendTask::install(interpreter),
                                );
                            }
                        }
                        DebugEvent::AttachProcess(pid, interpreter) => {
                            self.attach_run_process(pid, interpreter)
                        }
                        DebugEvent::Status(m) => self.set_status(m),
                        DebugEvent::Action(a) => {
                            let ctx = ui.ctx().clone();
                            self.handle_action(a, &ctx);
                        }
                        DebugEvent::Evaluate(expr) => {
                            if let Some(d) = self
                                .debug
                                .as_mut()
                                .filter(|d| d.is_stopped() && !expr.is_empty())
                            {
                                d.evaluate_dialog(&expr);
                            }
                            self.dialog = Dialog::EvaluateExpression(expr);
                        }
                    }
                }
            }
            BottomTool::HttpClient => {
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                    if tab.state.language == Language::Http {
                        if let Some(p) = tab.state.path.clone() {
                            let text = tab.state.doc.text();
                            if !self.http_panel.source_matches(&p, &text) {
                                self.http_panel.set_file(&p, &text);
                            }
                        }
                    }
                }
                self.http_panel.trusted = !self.project_safe_mode();
                let events = self.http_panel.ui(ui, &theme, mono, &root);
                self.handle_http_events(events, ui.ctx());
            }
            BottomTool::Endpoints => {
                use crate::endpoints_panel::EndpointsEvent;
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                let events = self.endpoints_panel.ui(ui, &theme, &root);
                for ev in events {
                    match ev {
                        EndpointsEvent::Refresh => self.scan_endpoints(),
                        EndpointsEvent::ImportedRequests(text) => {
                            self.create_http_request_buffer(&text)
                        }
                        EndpointsEvent::Navigate(p, line) => self.goto_file_line(&p, line),
                        EndpointsEvent::GenerateRequest(e) => {
                            let req = rustpy_semantic::endpoints::http_request(
                                &e,
                                crate::endpoints_panel::base_url(e.framework),
                            );
                            let path = Settings::config_dir()
                                .join("scratches")
                                .join("generated-requests.http");
                            let _ =
                                std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
                            // Already open: append to the editor buffer, keeping unsaved edits (and undo).
                            if let Some(i) = self
                                .tabs
                                .iter()
                                .position(|t| t.state.path.as_deref() == Some(path.as_path()))
                            {
                                self.open_file(&path);
                                let tab = &mut self.tabs[i];
                                let end = tab.state.doc.len();
                                let doc_text = tab.state.doc.text();
                                let sep = if doc_text.is_empty() || doc_text.ends_with("\n\n") {
                                    ""
                                } else if doc_text.ends_with('\n') {
                                    "\n"
                                } else {
                                    "\n\n"
                                };
                                tab.state.apply_text_edits(
                                    vec![(TextRange::empty(end), format!("{sep}{req}"))],
                                    "Generate HTTP Request",
                                );
                                let last = tab.state.doc.line_count().saturating_sub(1);
                                tab.view.cancel_session_restore();
                                tab.state
                                    .move_primary_to(rustpy_core::Position::new(last as u32, 0));
                                continue;
                            }
                            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
                            if !text.is_empty() && !text.ends_with("\n\n") {
                                text.push('\n');
                            }
                            text.push_str(&req);
                            match std::fs::write(&path, &text) {
                                Ok(()) => {
                                    if let Some(i) = self.tabs.iter().position(|t| {
                                        t.state.path.as_deref() == Some(path.as_path())
                                    }) {
                                        self.close_tab(i);
                                    }
                                    self.open_file(&path);
                                    if let Some(tab) = self
                                        .opened_editor_index(&path)
                                        .and_then(|i| self.tabs.get_mut(i))
                                    {
                                        let last = tab.state.doc.line_count().saturating_sub(1);
                                        tab.view.cancel_session_restore();
                                        tab.state.move_primary_to(rustpy_core::Position::new(
                                            last as u32,
                                            0,
                                        ));
                                    }
                                }
                                Err(e) => {
                                    self.set_status(format!("Cannot write {}: {e}", path.display()))
                                }
                            }
                        }
                        EndpointsEvent::OpenInBrowser(e) => {
                            let url = format!(
                                "{}{}",
                                crate::endpoints_panel::base_url(e.framework),
                                e.path
                            );
                            let opener = if cfg!(target_os = "macos") {
                                "open"
                            } else if cfg!(windows) {
                                "explorer"
                            } else {
                                "xdg-open"
                            };
                            match std::process::Command::new(opener).arg(&url).spawn() {
                                Ok(_) => self.set_status(format!("Opened {url}")),
                                Err(err) => {
                                    self.set_status(format!("Cannot open a browser: {err}"))
                                }
                            }
                        }
                        EndpointsEvent::CopyUrl(e) => {
                            let url = format!(
                                "{}{}",
                                crate::endpoints_panel::base_url(e.framework),
                                e.path
                            );
                            ui.ctx().copy_text(url.clone());
                            self.set_status(format!("Copied {url}"));
                        }
                    }
                }
            }
            BottomTool::Diagram => {
                if let Some(project) = &self.project {
                    self.diagram_panel.project_context(&project.root);
                }
                let events = self.diagram_panel.ui(ui, &theme);
                for ev in events {
                    match ev {
                        DiagramEvent::Navigate(p, r) => self.diagram_navigate(p, r),
                        DiagramEvent::DatabaseObject(source, object) => {
                            match self.db_panel.open_diagram_object(&source, &object) {
                                Ok(()) => self.bottom = Some(BottomTool::Database),
                                Err(error) => self.set_status(error),
                            }
                        }
                    }
                }
                self.consume_diagram_request();
            }
            BottomTool::Database => {
                if !self.builtin_plugin_enabled("database") {
                    ui.label("This bundled capability is disabled in Plugins");
                    return;
                }
                if self.project_safe_mode() {
                    ui.label(
                        "Trust the project before connecting to databases or running queries.",
                    );
                    return;
                }
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                self.db_panel.set_database_execution_allowed(
                    self.builtin_plugin_enabled("database") && !self.project_safe_mode(),
                );
                let events = self.db_panel.ui(ui, &theme, mono, &root);
                for ev in events {
                    match ev {
                        DbEvent::Status(m) => self.set_status(m),
                        DbEvent::Navigate(path, line) => {
                            self.open_file(&path);
                            if let Some(index) = self.active {
                                self.tabs[index].state.go_to_line(line, None);
                            }
                        }
                    }
                }
            }
            BottomTool::WebPreview => self.web_preview_tool(ui),
            BottomTool::Coverage => {
                if self.project_safe_mode() {
                    ui.label("Trust the project before importing coverage data or executing coverage report tools.");
                    return;
                }
                self.configure_coverage();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                self.coverage_panel.python = self
                    .project
                    .as_ref()
                    .and_then(|p| p.interpreter.as_ref())
                    .map(|i| i.path.clone());
                let events = self.coverage_panel.ui(ui, &theme, mono, &root);
                self.settings.coverage.branch = self.coverage_panel.branch;
                self.settings.coverage.hide_complete = self.coverage_panel.settings.hide_complete;
                self.coverage_project_badges();
                for ev in events {
                    match ev {
                        CoverageEvent::OpenFile(p) => self.open_file(&p),
                        CoverageEvent::Status(m) => self.set_status(m),
                    }
                }
            }
            BottomTool::Concurrency => self.concurrency_panel_ui(ui, &theme),
            BottomTool::Profiler => {
                self.profiler_options();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                let events = self.profiler_panel.ui(ui, &theme, mono, &root);
                for ev in events {
                    match ev {
                        ProfilerEvent::Navigate(path, line) => {
                            if path.is_file() {
                                self.mark_nav_jump();
                                self.open_file(&path);
                                if let Some(tab) = self
                                    .opened_editor_index(&path)
                                    .and_then(|i| self.tabs.get_mut(i))
                                {
                                    tab.view.cancel_session_restore();
                                    tab.state.go_to_line(line, None);
                                }
                                self.focus_editor = true;
                            }
                        }
                    }
                }
            }
            BottomTool::Services => self.services_workspace_ui(ui, &theme),
            BottomTool::Hierarchy => {
                if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                    if let Some(path) = tab.state.path.as_ref() {
                        self.hierarchy_panel
                            .follow_editor(path, tab.state.primary().head());
                    }
                }
                let events = self.hierarchy_panel.ui(ui, &theme);
                if let Some((path, range)) = self.hierarchy_panel.preview_location() {
                    let line = self
                        .tabs
                        .iter()
                        .find(|tab| tab.state.path.as_ref() == Some(&path))
                        .map(|tab| tab.state.doc.line_of(range.start()))
                        .or_else(|| {
                            let snapshot = rustpy_core::vfs::Vfs::new()
                                .read(
                                    &path,
                                    rustpy_core::vfs::ReadLimits {
                                        max_file_bytes: 1024 * 1024,
                                        max_cached_bytes: 0,
                                        max_cached_files: 0,
                                    },
                                )
                                .ok()?;
                            let source = std::str::from_utf8(&snapshot.bytes).ok()?;
                            Some(
                                source[..rustpy_core::floor_char_boundary(
                                    source,
                                    usize::from(range.start()),
                                )]
                                    .bytes()
                                    .filter(|byte| *byte == b'\n')
                                    .count(),
                            )
                        })
                        .unwrap_or(0);
                    self.tool_source_preview(ui, &path, line);
                }
                for ev in events {
                    match ev {
                        HierarchyEvent::Navigate(path, range) => self.navigate_to(&path, range),
                        HierarchyEvent::Expand(path, range, kind) => {
                            self.expand_call_node(&path, range, kind)
                        }
                        HierarchyEvent::SwitchKind(kind) => self.refresh_hierarchy(kind),
                        HierarchyEvent::Refresh => {
                            if let Some(kind) = self.hierarchy_panel.kind {
                                self.refresh_hierarchy(kind);
                            }
                        }
                        HierarchyEvent::Rebase(path, range) => {
                            if let Some(kind) = self.hierarchy_panel.kind {
                                self.show_hierarchy_at(kind, path, range);
                            }
                        }
                    }
                }
            }
            BottomTool::Packages => {
                self.configure_package_context();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let events = self.packages_panel.ui(ui, &theme, mono);
                for ev in events {
                    match ev {
                        PackagesEvent::Status(m) => self.set_status(m),
                    }
                }
            }
            BottomTool::Tests => {
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                self.test_panel
                    .set_execution_allowed(!self.project_safe_mode());
                self.test_panel.view = self.settings.testing.view.clone();
                let events = self.test_panel.ui(ui, &theme, mono);
                self.settings.testing.view = self.test_panel.view.clone();
                for ev in events {
                    match ev {
                        TestPanelEvent::Navigate(path, line) => {
                            if path.is_file() {
                                self.mark_nav_jump();
                                self.open_file(&path);
                                if let Some(tab) = self
                                    .opened_editor_index(&path)
                                    .and_then(|i| self.tabs.get_mut(i))
                                {
                                    tab.view.cancel_session_restore();
                                    tab.state.go_to_line(line, None);
                                }
                                self.focus_editor = true;
                            }
                        }
                        TestPanelEvent::Status(m) => self.set_status(m),
                        TestPanelEvent::Debug(id) => self.debug_test_node(id),
                        TestPanelEvent::Measure { profile, target } => {
                            self.measure_test_run(profile, target)
                        }
                        TestPanelEvent::FixtureUsages(path, line) => {
                            self.open_file(&path);
                            if let Some(index) = self.opened_editor_index(&path) {
                                let text = self.tabs[index].state.doc.text();
                                let name_position = text
                                    .lines()
                                    .enumerate()
                                    .skip(line.saturating_sub(1))
                                    .take(32)
                                    .find_map(|(row, text)| {
                                        if text.trim_start().starts_with("def ")
                                            || text.trim_start().starts_with("async def ")
                                        {
                                            text.find("def ").map(|column| (row + 1, column + 5))
                                        } else {
                                            None
                                        }
                                    });
                                let (line, column) = name_position
                                    .map(|(line, column)| (line, Some(column)))
                                    .unwrap_or((line, None));
                                self.tabs[index].state.go_to_line(line, column);
                                self.find_usages();
                            }
                        }
                    }
                }
            }
            BottomTool::Documentation => {
                self.documentation.docked = true;
                self.refresh_documentation_tool();
                self.documentation_tool_ui(ui);
                self.quick_doc = None;
                if !self.documentation.docked {
                    self.bottom = None;
                    self.reopen_documentation_popup();
                }
            }
            BottomTool::Find => self.find_tool_view(ui),
            BottomTool::ManagePy => self.manage_py_console_view(ui),
            BottomTool::Problems => self.problems_view(ui),
            BottomTool::Todo => self.todo_view(ui),
            BottomTool::Notifications => self.notifications_view(ui),
            BottomTool::SciView => self.scientific_ui(ui, &theme),
            BottomTool::PythonConsole => {
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                // The first start waits for interpreter discovery, so the console
                // runs the project's Python rather than the fallback.
                let discovering = self
                    .project
                    .as_ref()
                    .is_some_and(|p| p.interpreter_rx.is_some());
                if !self.project_safe_mode()
                    && !discovering
                    && self.py_console.take_start_request()
                    && self.ensure_py_console()
                {
                    self.py_console.request_focus();
                }
                if self.project_safe_mode() {
                    ui.label(egui::RichText::new("Safe mode: trust the project (File → Trust Project) to start the Python console.").color(theme.warning));
                }
                self.python_console_tabs(ui);
                if let Some(project) = &self.project {
                    let mut remaining = 524_288usize;
                    let open_files = self
                        .tabs
                        .iter()
                        .take(16)
                        .filter_map(|tab| {
                            let path = tab.state.path.clone()?;
                            let size = usize::from(tab.state.doc.len());
                            if size > remaining {
                                return None;
                            }
                            remaining -= size;
                            Some((path, tab.state.doc.text()))
                        })
                        .collect::<Vec<_>>();
                    self.py_console.update_semantic_inspections(
                        &project.analysis,
                        &project.root,
                        &open_files,
                    );
                }
                self.py_console.ui(ui, &theme, mono);
                self.consume_python_console_requests();
            }
            BottomTool::Terminal => {
                let mono = egui::FontId::new(
                    self.settings.run_console.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let cwd = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let env = crate::terminal_panel::terminal_env(
                    self.settings.terminal.activate_virtualenv,
                    self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
                );
                if (self.interpreter_discovery_pending() || self.terminal_on_start.is_some())
                    && self.terminal_panel.tab_count() == 0
                {
                    ui.label("Starting terminal…");
                    ui.ctx().request_repaint();
                } else {
                    self.terminal_tool_window(ui, &theme, mono, &cwd, &env);
                }
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        self.test_main_choice_dialog(ctx);
        self.collect_refactoring_undo_request();
        if self.bg.problems.infrastructure.dialog {
            self.inspection_runner_dialog(ctx);
            return;
        }
        if self.intention_menu.dialog_active() {
            self.intention_run_dialog(ctx);
            return;
        }
        if self.injection_ui.active() {
            self.injection_dialog(ctx);
            return;
        }
        let dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        match dialog {
            Dialog::EncodingReload(request) => self.encoding_reload_dialog(ctx, request),
            Dialog::DataFiles => self.data_files_dialog(ctx),
            Dialog::Frameworks => self.frameworks_dialog(ctx),
            Dialog::WebLanguages => self.web_languages_dialog(ctx),
            Dialog::None => {}
            Dialog::StringPaste(prompt) => self.draw_string_paste(ctx, prompt),
            Dialog::GoToLine(mut text) => {
                let mut open = true;
                let mut go = false;
                let mut cancel = false;
                egui::Window::new(crate::i18n::tr_static("Go to Line:Column"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_static("[Line] [:column]"));
                        let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(200.0));
                        r.request_focus();
                        if (r.lost_focus() || r.has_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            go = true;
                        }
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("OK")).clicked() {
                                go = true;
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                cancel = true;
                            }
                        });
                    });
                if cancel {
                    open = false;
                }
                if go {
                    let current = self.active.map_or(0, |index| {
                        self.tabs[index]
                            .state
                            .doc
                            .line_of(self.tabs[index].state.primary().head())
                    });
                    if let Some((line, column)) = related_navigation::line_target(&text, current) {
                        self.mark_nav_jump();
                        if let Some(tab) = self.active.and_then(|index| self.tabs.get_mut(index)) {
                            tab.view.cancel_session_restore();
                            tab.state.go_to_line(line, column);
                        }
                        self.focus_editor = true;
                    } else {
                        self.set_status("Enter a positive line[:column] or :column");
                        self.dialog = Dialog::GoToLine(text);
                    }
                } else if open {
                    self.dialog = Dialog::GoToLine(text);
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::FileStructure { query, selected } => {
                self.file_structure_popup(ctx, query, selected)
            }
            Dialog::EvaluateExpression(mut expr) => {
                if self.project_safe_mode() {
                    self.set_status("Trust the project before evaluating debugger expressions");
                    return;
                }
                let mut open = true;
                let mut evaluate = false;
                let theme = self.theme.clone();
                egui::Window::new(crate::i18n::tr_static("Evaluate Expression"))
                    .collapsible(false)
                    .resizable(true)
                    .default_width(560.0)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut expr)
                                .desired_width(f32::INFINITY)
                                .hint_text(crate::i18n::tr_static("Expression")),
                        );
                        r.request_focus();
                        if (r.lost_focus() || r.has_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            evaluate = true;
                        }
                        let stopped = self.debug.as_ref().is_some_and(|d| d.is_stopped());
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    stopped,
                                    egui::Button::new(crate::i18n::tr_static("Evaluate")),
                                )
                                .clicked()
                            {
                                evaluate = true;
                            }
                            if !stopped {
                                ui.label(
                                    egui::RichText::new(crate::i18n::tr_static(
                                        "Program must be suspended at a breakpoint",
                                    ))
                                    .color(theme.text_dim),
                                );
                            }
                        });
                        ui.separator();
                        // The result as an expandable tree (children load on demand).
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                if let Some(next) = self.evaluate_dialog_result(ui) {
                                    expr = next;
                                    evaluate = true;
                                }
                            });
                    });
                if evaluate && !expr.trim().is_empty() {
                    if let Some(d) = &mut self.debug {
                        d.evaluate_dialog(expr.trim());
                    }
                }
                if open {
                    self.dialog = Dialog::EvaluateExpression(expr);
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::Breakpoints(state) => self.breakpoints_dialog(ctx, state),
            Dialog::EditBreakpoint(popup) => self.edit_breakpoint_dialog(ctx, *popup),
            Dialog::QuickDefinition {
                title,
                path,
                line,
                text,
            } => {
                let mut open = true;
                let mut go = false;
                let mut esc = false;
                let theme = self.theme.clone();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                egui::Window::new(crate::i18n::tr_fmt(
                    "Quick Definition — {title}",
                    &[("title", &title)],
                ))
                .collapsible(false)
                .resizable(true)
                .default_width(640.0)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    egui::ScrollArea::both().max_height(360.0).show(ui, |ui| {
                        egui::Frame::new()
                            .fill(rgb(self.scheme.editor.background))
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                for (i, l) in text.lines().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{:>4}", line + i))
                                                .font(mono.clone())
                                                .color(theme.text_dim),
                                        );
                                        ui.label(
                                            egui::RichText::new(l)
                                                .font(mono.clone())
                                                .color(rgb(self.scheme.editor.foreground)),
                                        );
                                    });
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .button(crate::i18n::tr_static("Edit Source (F4)"))
                            .clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::F4))
                        {
                            go = true;
                        }
                        ui.label(
                            egui::RichText::new(path.display().to_string())
                                .color(theme.text_dim)
                                .small(),
                        );
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        go = false;
                        esc = true;
                    }
                });
                if esc {
                    open = false;
                }
                if go {
                    self.open_file(&path);
                    if let Some(tab) = self
                        .opened_editor_index(&path)
                        .and_then(|i| self.tabs.get_mut(i))
                    {
                        tab.view.cancel_session_restore();
                        tab.state.go_to_line(line, None);
                    }
                    self.focus_editor = true;
                } else if open {
                    self.dialog = Dialog::QuickDefinition {
                        title,
                        path,
                        line,
                        text,
                    };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::Extract(state) => self.draw_extract_dialog(ctx, state),
            Dialog::ChangeSignature(st) => self.draw_change_signature(ctx, st),
            Dialog::SafeDelete(st) => self.draw_safe_delete(ctx, st),
            Dialog::BookmarkDescription { index, mut text } => {
                let mut open = true;
                let mut done = false;
                let mut esc = false;
                let is_new = index == usize::MAX;
                egui::Window::new(if is_new {
                    crate::i18n::tr_static("Add Mnemonic Bookmark")
                } else {
                    crate::i18n::tr_static("Bookmark Description")
                })
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    if is_new {
                        ui.label(crate::i18n::tr_static(
                            "Type a digit or letter for the mnemonic bookmark:",
                        ));
                    } else {
                        ui.label(crate::i18n::tr_static("Description:"));
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(360.0));
                    r.request_focus();
                    if is_new && !text.is_empty() {
                        done = true;
                    }
                    if (r.lost_focus() || r.has_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        done = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        esc = true;
                    }
                });
                if esc {
                    open = false;
                }
                if done {
                    if is_new {
                        let m = text
                            .chars()
                            .next()
                            .filter(|c| c.is_ascii_alphanumeric())
                            .map(|c| c.to_ascii_uppercase());
                        self.toggle_bookmark(m);
                    } else if let Some(b) = self.bookmarks.get_mut(index) {
                        b.description = text.trim().to_string();
                        self.save_workspace();
                    }
                    self.focus_editor = true;
                } else if open {
                    self.dialog = Dialog::BookmarkDescription { index, text };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::Rename(st) => self.rename_dialog(ctx, st),
            Dialog::Inline(state) => self.inline_dialog(ctx, state),
            Dialog::Conversion(state) => self.conversion_dialog(ctx, state),
            Dialog::RenamePreview(st) => self.rename_preview_dialog(ctx, st),
            Dialog::EditorConfig(state) => self.draw_editorconfig_preview(ctx, state),
            Dialog::FrameworkRename(st) => self.framework_rename_dialog(ctx, st),
            Dialog::RefactoringPreview(st) => self.refactoring_preview_dialog(ctx, st),
            Dialog::UndoWorkspaceRefactoring(index) => {
                self.workspace_refactoring_undo_dialog(ctx, index)
            }
            Dialog::RunConfigurations => {
                // See run_config_dialog.rs; Run / Debug close the dialog.
                if self.run_configurations_dialog(ctx) && matches!(self.dialog, Dialog::None) {
                    self.dialog = Dialog::RunConfigurations;
                }
            }
            Dialog::GitHubModal => self.github_dialog(ctx),
            Dialog::Branches => self.branches_dialog(ctx),
            Dialog::UpdateProject(st) => self.update_project_dialog(ctx, st),
            Dialog::Push(st) => self.push_dialog(ctx, st),
            Dialog::Remotes(st) => self.remotes_dialog(ctx, st),
            Dialog::InterpreterPicker => self.open_interpreter_management(),
            Dialog::MakeWritable {
                document,
                mut request,
            } => {
                let mut answer = None;
                let mut cancel_focused = false;
                let mut open = true;
                egui::Window::new(crate::i18n::tr_static("Make File Writable"))
                    .collapsible(false).resizable(false).open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_static("Change the file's disk attribute and allow editing?"));
                        ui.label(request.path().display().to_string());
                        ui.label(crate::i18n::tr_static("Other permissions are preserved. Library reader protection is released only for this file."));
                        ui.horizontal(|ui| {
                            let make = ui.button(crate::i18n::tr_static("Make Writable"));
                            if request.take_initial_focus() { make.request_focus(); }
                            if make.clicked() { answer = Some(true); }
                            let cancel = ui.button(crate::i18n::tr_static("Cancel"));
                            cancel_focused = cancel.has_focus();
                            if cancel.clicked() { answer = Some(false); }
                        });
                    });
                if !open || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
                    answer = Some(false);
                } else if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                    answer = Some(!cancel_focused);
                }
                if let Some(confirm) = answer {
                    self.answer_make_writable(document, request, confirm);
                } else {
                    self.dialog = Dialog::MakeWritable { document, request };
                }
            }
            Dialog::ConfirmClose { tab, then_exit } => {
                let title = self
                    .tabs
                    .get(tab)
                    .map(|t| t.state.title())
                    .unwrap_or_default();
                let mut decision: Option<layout::CloseAnswer> = None;
                egui::Window::new(crate::i18n::tr_static("Unsaved Changes"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_fmt(
                            "Save changes to {title}?",
                            &[("title", &title)],
                        ));
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Save")).clicked() {
                                decision = Some(layout::CloseAnswer::Save);
                            }
                            if ui.button(crate::i18n::tr_static("Don't Save")).clicked() {
                                decision = Some(layout::CloseAnswer::Discard);
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                decision = Some(layout::CloseAnswer::Cancel);
                            }
                        });
                    });
                match decision {
                    // During an exit: the next modified editor, or exit.
                    Some(answer) => self.answer_confirm_close(tab, then_exit, answer),
                    None => self.dialog = Dialog::ConfirmClose { tab, then_exit },
                }
            }
            Dialog::NewFileKind {
                dir,
                kind,
                mut name,
            } => {
                let mut open = true;
                let mut create = false;
                let mut cancel = false;
                let theme = self.theme.clone();
                let title = match kind {
                    "directory" => crate::i18n::tr_static("New Directory"),
                    "package" => crate::i18n::tr_static("New Python Package"),
                    _ => crate::i18n::tr_static("New File"),
                };
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(crate::i18n::tr_fmt(
                                "in {path}",
                                &[("path", &dir.display())],
                            ))
                            .color(theme.text_dim)
                            .small(),
                        );
                        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
                        if r.gained_focus() {
                            // Typing replaces the suggested name (the extension stays for files).
                            let stem = match kind {
                                "directory" | "package" => name.chars().count(),
                                _ => name
                                    .rfind('.')
                                    .filter(|&i| i > 0)
                                    .map(|i| name[..i].chars().count())
                                    .unwrap_or(name.chars().count()),
                            };
                            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(0),
                                        egui::text::CCursor::new(stem),
                                    )));
                                state.store(ui.ctx(), r.id);
                            }
                        }
                        r.request_focus();
                        if (r.lost_focus() || r.has_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            create = true;
                        }
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("OK")).clicked() {
                                create = true;
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                cancel = true;
                            }
                        });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            cancel = true;
                        }
                    });
                if create && !name.trim().is_empty() {
                    let name = name.trim().to_string();
                    let result = git_workflows::file_change(&dir, "Creating file", || match kind {
                        "copy" => {
                            let target = dir.join(&name);
                            match self.copy_source.clone() {
                                Some(src) if !target.exists() => {
                                    std::fs::copy(&src, &target).map(|_| Some(target))
                                }
                                Some(_) => Ok(Some(target)),
                                None => Ok(None),
                            }
                        }
                        "directory" => std::fs::create_dir_all(dir.join(&name)).map(|_| None),
                        "package" => project_new_ui::create_python_package(&dir, &name).map(Some),
                        _ => {
                            let file_name = crate::plugin_host::file_name_for(&name, kind)
                                .unwrap_or_else(|| ensure_extension(&name, kind));
                            let path = dir.join(&file_name);
                            if path.exists() {
                                Ok(Some(path))
                            } else {
                                let project = self
                                    .project
                                    .as_ref()
                                    .and_then(|p| p.root.file_name())
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let text = crate::plugin_host::render_file_template(
                                    kind, &file_name, &project,
                                )
                                .unwrap_or_else(|| file_template(kind, &file_name));
                                std::fs::write(&path, text).map(|_| Some(path))
                            }
                        }
                    });
                    match result {
                        Ok(open_path) => {
                            if let Some(p) = &mut self.project {
                                p.tree.invalidate_all();
                                p.tree.expanded.insert(dir.clone());
                            }
                            if let Some(p) = open_path {
                                self.open_file(&p);
                                self.vcs_file_created(&p);
                            }
                        }
                        Err(e) => self.set_status(format!("Cannot create: {e}")),
                    }
                    self.focus_editor = true;
                } else if open && !cancel {
                    self.dialog = Dialog::NewFileKind { dir, kind, name };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::RenamePath { path, mut name } => {
                let mut open = true;
                let mut go = false;
                let mut cancel = false;
                egui::Window::new(crate::i18n::tr_static("Rename"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_fmt(
                            "Rename {file}",
                            &[(
                                "file",
                                &path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            )],
                        ));
                        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
                        r.request_focus();
                        if (r.lost_focus() || r.has_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            go = true;
                        }
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Rename")).clicked() {
                                go = true;
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                cancel = true;
                            }
                        });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            cancel = true;
                        }
                    });
                if go && !name.trim().is_empty() {
                    let new_path = path.with_file_name(name.trim());
                    let renamed = rustpy_vcs::RepositoryOperation::enter(&path, "Renaming file")
                        .and_then(|_operation| {
                            std::fs::rename(&path, &new_path)?;

                            Ok(())
                        });
                    match renamed {
                        Ok(()) => {
                            self.vcs_file_moved(&path, &new_path);
                            for t in &mut self.tabs {
                                if let Some(rest) = t
                                    .state
                                    .path
                                    .as_ref()
                                    .and_then(|p| p.strip_prefix(&path).ok())
                                    .map(Path::to_path_buf)
                                {
                                    t.state.path = Some(if rest.as_os_str().is_empty() {
                                        new_path.clone()
                                    } else {
                                        new_path.join(rest)
                                    });
                                    t.state.synced_version = None;
                                    // A new extension (`scratch.py` → `notes.md`) changes the language.
                                    if t.state.path.as_deref() == Some(new_path.as_path())
                                        && path.extension() != new_path.extension()
                                        && t.image.is_none()
                                        && t.notebook.is_none()
                                    {
                                        t.state.set_language(Language::from_path(&new_path));
                                    }
                                }
                            }
                            self.invalidate_analysis_paths(&[path.clone(), new_path.clone()]);
                            self.nav.history.rename_file(&path, Some(&new_path));
                            if let Some(p) = &mut self.project {
                                p.tree.invalidate_all();
                            }
                            self.set_status(format!("Renamed to {}", new_path.display()));
                        }
                        Err(e) => self.set_status(format!("Cannot rename: {e}")),
                    }
                    self.focus_editor = true;
                } else if open && !cancel {
                    self.dialog = Dialog::RenamePath { path, name };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::ExternalChange(path) => {
                let mut open = true;
                let mut reload = false;
                let mut keep = false;
                egui::Window::new(crate::i18n::tr_static("File Changed on Disk"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_fmt(
                            "{path} changed or was removed on disk. Choose which version to keep.",
                            &[("path", &path.display())],
                        ));
                        ui.horizontal(|ui| {
                            if ui
                                .button(crate::i18n::tr_static(
                                    "Reload from disk (lose editor changes)",
                                ))
                                .clicked()
                            {
                                reload = true;
                            }
                            if ui
                                .button(crate::i18n::tr_static("Keep editor version"))
                                .clicked()
                            {
                                keep = true;
                            }
                        });
                    });
                if reload {
                    if let Some(i) = self
                        .tabs
                        .iter()
                        .position(|t| t.state.path.as_deref() == Some(path.as_path()))
                    {
                        self.snapshot(i, "Before reload from disk");
                        self.reload_tab_from_disk(i);
                    }
                } else if keep {
                    self.acknowledge_disk_revision(&path);
                } else if open {
                    self.dialog = Dialog::ExternalChange(path);
                }
                if !matches!(self.dialog, Dialog::ExternalChange(_))
                    && !self.external_changes.is_empty()
                {
                    self.reconcile_external_changes();
                }
            }
            Dialog::Macros { selected, new_name } => {
                self.macro_editor(ctx, selected, new_name);
            }
            Dialog::CheckRegex {
                mut pattern,
                mut flags,
                mut sample,
                mut result,
                mut error,
                mut rx,
            } => {
                let theme = self.theme.clone();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let mut open = true;
                let mut esc = false;
                let mut run = false;
                if let Some(r) = &rx {
                    match r.try_recv() {
                        Ok(Ok(m)) => {
                            error = m.error.clone();
                            result = Some(m);
                            rx = None;
                        }
                        Ok(Err(e)) => {
                            error = Some(e.to_string());
                            rx = None;
                        }
                        Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                    }
                }
                egui::Window::new(crate::i18n::tr_static("Check RegExp"))
                    .collapsible(false)
                    .resizable(true)
                    .default_size([640.0, 420.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_static("Pattern:"));
                        ui.add(
                            egui::TextEdit::singleline(&mut pattern)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                        let errors = rustpy_regex::check(
                            &pattern,
                            flags.contains("VERBOSE") || flags.contains('X'),
                        );
                        if let Some(e) = errors.first() {
                            ui.colored_label(
                                theme.error,
                                crate::i18n::tr_fmt(
                                    "{message} (at {offset})",
                                    &[
                                        ("message", &e.message),
                                        ("offset", &usize::from(e.range.start())),
                                    ],
                                ),
                            );
                        }
                        ui.horizontal(|ui| {
                            ui.label(crate::i18n::tr_static("Flags:"));
                            ui.add(
                                egui::TextEdit::singleline(&mut flags)
                                    .hint_text("IGNORECASE|MULTILINE|VERBOSE")
                                    .desired_width(260.0),
                            );
                            if ui.button(crate::i18n::tr_static("Test")).clicked() {
                                run = true;
                            }
                        });
                        ui.label(crate::i18n::tr_static("Sample text:"));
                        ui.add(
                            egui::TextEdit::multiline(&mut sample)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        );
                        ui.separator();
                        if rx.is_some() {
                            ui.label(
                                egui::RichText::new(crate::i18n::tr_static("Running…"))
                                    .color(theme.text_dim),
                            );
                        }
                        if let Some(e) = &error {
                            ui.colored_label(theme.error, e);
                        }
                        if let Some(m) = &result {
                            if m.matched {
                                ui.label(
                                    egui::RichText::new(crate::i18n::tr_fmt(
                                        "Matches: {count}",
                                        &[("count", &m.spans.len())],
                                    ))
                                    .color(theme.success)
                                    .strong(),
                                );
                                let mut job = egui::text::LayoutJob::default();
                                let mut pos = 0usize;
                                for (s, e) in &m.spans {
                                    let (s, e) = ((*s).min(sample.len()), (*e).min(sample.len()));
                                    if s > pos {
                                        job.append(
                                            &sample[pos..s],
                                            0.0,
                                            egui::text::TextFormat {
                                                font_id: mono.clone(),
                                                color: theme.text,
                                                ..Default::default()
                                            },
                                        );
                                    }
                                    if e > s {
                                        job.append(
                                            &sample[s..e],
                                            0.0,
                                            egui::text::TextFormat {
                                                font_id: mono.clone(),
                                                color: Color32::BLACK,
                                                background: Color32::from_rgb(0xF0, 0xD0, 0x50),
                                                ..Default::default()
                                            },
                                        );
                                    }
                                    pos = e.max(pos);
                                }
                                if pos < sample.len() {
                                    job.append(
                                        &sample[pos..],
                                        0.0,
                                        egui::text::TextFormat {
                                            font_id: mono.clone(),
                                            color: theme.text,
                                            ..Default::default()
                                        },
                                    );
                                }
                                ui.label(job);
                                if !m.groups.is_empty() {
                                    egui::Grid::new("regex_groups")
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for (name, value) in &m.groups {
                                                ui.label(
                                                    egui::RichText::new(name).color(theme.accent),
                                                );
                                                ui.label(
                                                    egui::RichText::new(
                                                        value
                                                            .clone()
                                                            .unwrap_or_else(|| "None".into()),
                                                    )
                                                    .font(mono.clone()),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }
                            } else if error.is_none() {
                                ui.label(
                                    egui::RichText::new(crate::i18n::tr_static("No match"))
                                        .color(theme.warning)
                                        .strong(),
                                );
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            esc = true;
                        }
                    });
                if run {
                    let interp = self
                        .project
                        .as_ref()
                        .and_then(|p| p.interpreter.as_ref())
                        .map(|i| i.path.clone())
                        .unwrap_or_else(|| PathBuf::from("python3"));
                    let (tx, r) = crossbeam_channel::unbounded();
                    let (p, f, s) = (pattern.clone(), flags.clone(), sample.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(rustpy_regex::test_match(&interp, &p, &f, &s));
                    });
                    rx = Some(r);
                    result = None;
                    error = None;
                }
                if open && !esc {
                    self.dialog = Dialog::CheckRegex {
                        pattern,
                        flags,
                        sample,
                        result,
                        error,
                        rx,
                    };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::Members(state) => self.draw_member_refactoring(ctx, state),
            Dialog::Reformat(state) => self.draw_reformat_dialog(ctx, state),
            Dialog::Move(state) => self.draw_move_dialog(ctx, state),
            Dialog::RenameModule { path, mut name } => {
                let mut open = true;
                let mut go = false;
                let mut cancel = false;
                egui::Window::new(crate::i18n::tr_static("Rename Module"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.label(crate::i18n::tr_fmt(
                            "Rename {path} and update imports across the project",
                            &[("path", &path.display())],
                        ));
                        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(300.0));
                        r.request_focus();
                        if (r.lost_focus() || r.has_focus())
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            go = true;
                        }
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Rename")).clicked() {
                                go = true;
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                cancel = true;
                            }
                        });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            cancel = true;
                        }
                    });
                if go && !name.trim().is_empty() {
                    if let Err(e) = self.rename_module_from_dialog(&path, &name) {
                        self.set_status(format!("Cannot rename: {e}"));
                    }
                    self.focus_editor = true;
                } else if open && !cancel {
                    self.dialog = Dialog::RenameModule { path, name };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::OpenTask(st) => self.draw_open_task(ctx, st),
            Dialog::OpenWebUrl => self.web_url_dialog(ctx),
            Dialog::ToolPrompt(st) => self.tool_prompt_dialog(ctx, st),
            Dialog::NewFromTemplate(st) => self.new_from_template_dialog(ctx, st),
            Dialog::ManageSettings {
                mode,
                mut path,
                mut result,
            } => {
                let theme = self.theme.clone();
                let title = [
                    crate::i18n::tr_static("Export Settings"),
                    crate::i18n::tr_static("Import Settings"),
                    crate::i18n::tr_static("Restore Default Settings"),
                ][mode.min(2) as usize];
                let mut open = true;
                let mut close = false;
                egui::Window::new(title).collapsible(false).resizable(false).default_width(560.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
                    let config = Settings::config_dir();
                    if mode == 2 {
                        ui.label(crate::i18n::tr_static("Settings, keymap overrides and user live templates are moved to a backup folder and defaults are restored."));
                    } else {
                        ui.label(if mode == 0 { crate::i18n::tr_static("Export the IDE configuration (settings, color schemes, templates, macros; not plugins) to a file:") } else { crate::i18n::tr_static("Import an exported settings bundle (current files are backed up first; plugins stay as they are):") });
                        ui.add(egui::TextEdit::singleline(&mut path).desired_width(f32::INFINITY));
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr_fmt("Configuration directory: {path}", &[("path", &config.display())])).color(theme.text_dim).small());
                    self.dialog_workspace_progress(ui);
                    match &result {
                        Some(Ok(m)) => {
                            ui.colored_label(theme.success, m);
                        }
                        Some(Err(e)) => {
                            ui.colored_label(theme.error, e);
                        }
                        None => {}
                    }
                    ui.horizontal(|ui| {
                        let label = [crate::i18n::tr_static("Export"), crate::i18n::tr_static("Import"), crate::i18n::tr_static("Restore Defaults")][mode.min(2) as usize];
                        if result.as_ref().is_none_or(|r| r.is_err()) && ui.button(label).clicked() {
                            result = Some(self.start_settings_archive(mode,&path));
                        }
                        if ui.button(crate::i18n::tr_static("Close")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            close = true;
                        }
                    });
                });
                if open && !close {
                    self.dialog = Dialog::ManageSettings { mode, path, result };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::SaveLayout(mut name) => {
                let mut done: Option<bool> = None;
                egui::Window::new(crate::i18n::tr_static("Save Current Layout As"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 120.0))
                    .show_modal(ctx, |ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .hint_text(crate::i18n::tr_static("Layout name"))
                                .desired_width(320.0),
                        );
                        r.request_focus();
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Save")).clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                done = Some(true);
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                done = Some(false);
                            }
                        });
                    });
                match done {
                    Some(true) if !name.trim().is_empty() => {
                        self.save_named_tool_window_layout(&name);
                        self.focus_editor = true;
                    }
                    Some(_) => self.focus_editor = true,
                    None => self.dialog = Dialog::SaveLayout(name),
                }
            }
            Dialog::SaveContext(mut name) => {
                let mut done: Option<bool> = None;
                egui::Window::new(crate::i18n::tr_static("Save Context"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 120.0))
                    .show_modal(ctx, |ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .hint_text(crate::i18n::tr_static("Context name"))
                                .desired_width(320.0),
                        );
                        r.request_focus();
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Save")).clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                done = Some(true);
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                done = Some(false);
                            }
                        });
                    });
                match done {
                    Some(true) if !name.trim().is_empty() => {
                        let c = self.current_context();
                        let n = c.open_files.len();
                        if let Some(s) = self.task_store() {
                            s.save_context(name.trim(), c);
                        }
                        self.save_tasks();
                        self.set_status(format!("Context '{}' saved ({n} file(s))", name.trim()));
                        self.focus_editor = true;
                    }
                    Some(_) => self.focus_editor = true,
                    None => self.dialog = Dialog::SaveContext(name),
                }
            }
            Dialog::ConfirmPaste(text) => {
                let mut choice: Option<bool> = None;
                egui::Window::new(crate::i18n::tr_static("Paste Large Text")).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show_modal(ctx, |ui| {
                    let lines = text.lines().count();
                    ui.label(crate::i18n::tr_fmt("The clipboard holds {megabytes} MB ({lines} lines). Pasting it may make the editor slow.", &[("megabytes", &format!("{:.1}", text.len() as f64 / (1024.0 * 1024.0))), ("lines", &lines)]));
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr_static("Paste")).clicked() {
                            choice = Some(true);
                        }
                        if ui.button(crate::i18n::tr_static("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            choice = Some(false);
                        }
                    });
                });
                match choice {
                    Some(true) => {
                        let opts = self.edit_options();
                        if let Some(i) = self.active {
                            self.tabs[i].state.paste_payload(
                                &text,
                                self.clipboard_payload.as_ref(),
                                &opts,
                            );
                        }
                        self.focus_editor = true;
                    }
                    Some(false) => self.focus_editor = true,
                    None => self.dialog = Dialog::ConfirmPaste(text),
                }
            }
            Dialog::DataView {
                title,
                columns,
                rows,
                total,
                mut offset,
                mut filter,
            } => {
                let theme = self.theme.clone();
                let mono = egui::FontId::new(
                    self.settings.editor.font_size.max(10.0),
                    crate::theme::mono_family(false, false),
                );
                let mut open = true;
                let mut esc = false;
                const PAGE: usize = 200;
                egui::Window::new(crate::i18n::tr_fmt("Data: {title}", &[("title", &title)]))
                    .collapsible(false)
                    .resizable(true)
                    .default_size([900.0, 560.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(crate::i18n::tr_fmt(if rows.len() < total {
                                    crate::i18n::tr_static("{rows} row(s) × {columns} column(s) (first 1000 loaded)")
                                } else {
                                    crate::i18n::tr_static("{rows} row(s) × {columns} column(s)")
                                }, &[("rows", &total), ("columns", &columns.len())]))
                                .color(theme.text_dim),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut filter)
                                    .hint_text(crate::i18n::tr_static("Filter rows"))
                                    .desired_width(160.0),
                            );
                            if ui.add_enabled(offset > 0, egui::Button::new("◀")).clicked() {
                                offset = offset.saturating_sub(PAGE);
                            }
                            if ui
                                .add_enabled(offset + PAGE < rows.len(), egui::Button::new("▶"))
                                .clicked()
                            {
                                offset += PAGE;
                            }
                            if ui.button(crate::i18n::tr_static("Copy as CSV")).clicked() {
                                let mut csv = columns.join(",") + "\n";
                                for r in &rows {
                                    csv.push_str(
                                        &r.iter()
                                            .map(|c| {
                                                if c.contains(',') || c.contains('"') {
                                                    format!("\"{}\"", c.replace('"', "\"\""))
                                                } else {
                                                    c.clone()
                                                }
                                            })
                                            .collect::<Vec<_>>()
                                            .join(","),
                                    );
                                    csv.push('\n');
                                }
                                ui.ctx().copy_text(csv);
                            }
                        });
                        ui.separator();
                        let f = filter.trim().to_lowercase();
                        let visible: Vec<&Vec<String>> = rows
                            .iter()
                            .filter(|r| {
                                f.is_empty() || r.iter().any(|c| c.to_lowercase().contains(&f))
                            })
                            .skip(offset)
                            .take(PAGE)
                            .collect();
                        egui::ScrollArea::both()
                            .id_salt("dataview")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Grid::new("dataview_grid")
                                    .striped(true)
                                    .spacing([14.0, 2.0])
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new("#").strong());
                                        for c in &columns {
                                            ui.label(egui::RichText::new(c).strong());
                                        }
                                        ui.end_row();
                                        for (i, r) in visible.iter().enumerate() {
                                            ui.label(
                                                egui::RichText::new((offset + i).to_string())
                                                    .color(theme.text_dim),
                                            );
                                            for c in r.iter() {
                                                let shown: String = c.chars().take(80).collect();
                                                ui.label(
                                                    egui::RichText::new(shown).font(mono.clone()),
                                                );
                                            }
                                            ui.end_row();
                                        }
                                    });
                            });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            esc = true;
                        }
                    });
                if open && !esc {
                    self.dialog = Dialog::DataView {
                        title,
                        columns,
                        rows,
                        total,
                        offset,
                        filter,
                    };
                }
            }
            Dialog::RemoteInterpreter {
                mut kind,
                mut target,
                mut python,
                mut remote_root,
                error,
            } => {
                let theme = self.theme.clone();
                let mut open = true;
                let mut go = false;
                let mut list_wsl = false;
                let mut cancel = false;
                egui::Window::new(crate::i18n::tr_static("Add Remote Interpreter")).collapsible(false).resizable(false).default_width(520.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (label, k) in [("SSH", "ssh"), ("Docker", "docker"), ("Compose", "compose"), ("WSL", "wsl"), ("Vagrant", "vagrant")] {
                            if ui.selectable_label(kind == k, crate::i18n::tr_static(label)).clicked() {
                                kind = k.into();
                            }
                        }
                    });
                    if kind == "wsl" && cfg!(windows) { list_wsl = ui.button(crate::i18n::tr_static("List registered WSL distributions")).clicked(); }
                    let hint = match kind.as_str() { "ssh" => "user@host", "docker" => crate::i18n::tr_static("image:tag (the project is mounted at /work)"), "compose" => crate::i18n::tr_static("running service name"), "vagrant" => crate::i18n::tr_static("configured machine name"), _ => crate::i18n::tr_static("distribution name (optional)") };
                    ui.horizontal(|ui| {
                        ui.label(match kind.as_str() { "ssh" => crate::i18n::tr_static("Host:"), "docker" => crate::i18n::tr_static("Image:"), "compose" => crate::i18n::tr_static("Service:"), "vagrant" => crate::i18n::tr_static("Machine:"), _ => crate::i18n::tr_static("Distro:") });
                        ui.add(egui::TextEdit::singleline(&mut target).hint_text(hint).desired_width(320.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static("Python:"));
                        ui.add(egui::TextEdit::singleline(&mut python).hint_text("python3").desired_width(320.0));
                    });
                    if kind != "wsl" {
                        ui.horizontal(|ui| {
                            ui.label(crate::i18n::tr_static("Remote project path:"));
                            let hint = match kind.as_str() { "docker" => "/work", "vagrant" => "/vagrant", "compose" => crate::i18n::tr_static("service mount, e.g. /app"), _ => crate::i18n::tr_static("remote checkout (optional)") };
                            ui.add(egui::TextEdit::singleline(&mut remote_root).hint_text(hint).desired_width(320.0));
                        });
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr_static("A wrapper script forwards `python` invocations (run, console, tests) through ssh / docker run / wsl.exe; debugging needs the remote debugpy port and is not forwarded.")).color(theme.text_dim).small());
                    if let Some(e) = &error {
                        let message = if let Some(names) = e.strip_prefix("Registered distributions: ") {
                            crate::i18n::tr_fmt("Registered distributions: {names}", &[("names", &names)])
                        } else {
                            crate::i18n::tr(e).into_owned()
                        };
                        ui.colored_label(if e.starts_with("Registered distributions:") { theme.text_dim } else { theme.error }, message);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr_static("Add")).clicked() {
                            go = true;
                        }
                        if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                            cancel = true;
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                });
                if list_wsl {
                    match rustpy_project::interpreter_remote::WslRegistrationJob::registrations() {
                        Ok(job) => {
                            self.dialog = Dialog::WslDistributions {
                                root: self
                                    .project
                                    .as_ref()
                                    .map(|p| p.root.clone())
                                    .unwrap_or_default(),
                                python,
                                job,
                            }
                        }
                        Err(error) => {
                            self.dialog = Dialog::RemoteInterpreter {
                                kind,
                                target,
                                python,
                                remote_root,
                                error: Some(error),
                            }
                        }
                    }
                } else if go {
                    use rustpy_project::interpreter_remote::{
                        DiscoveryJob, RemoteCandidate, RemoteKind,
                    };
                    let root = self
                        .project
                        .as_ref()
                        .map(|p| p.root.clone())
                        .unwrap_or_default();
                    let candidate = RemoteCandidate {
                        kind: match kind.as_str() {
                            "ssh" => RemoteKind::Ssh,
                            "docker" => RemoteKind::Docker,
                            "compose" => RemoteKind::Compose,
                            "vagrant" => RemoteKind::Vagrant,
                            _ => RemoteKind::Wsl,
                        },
                        target: target.trim().into(),
                        python: if python.trim().is_empty() {
                            "python3".into()
                        } else {
                            python.trim().into()
                        },
                        project_root: root.clone(),
                        docker: None,
                        remote_root: Some(remote_root.trim().to_string())
                            .filter(|r| !r.is_empty() && kind != "wsl"),
                    };
                    match DiscoveryJob::remote(
                        candidate,
                        Settings::config_dir().join("interpreters"),
                        !self.project_safe_mode(),
                    ) {
                        Ok(job) => self.dialog = Dialog::InterpreterProbe { root, job },
                        Err(error) => {
                            self.dialog = Dialog::RemoteInterpreter {
                                kind,
                                target,
                                python,
                                remote_root,
                                error: Some(error),
                            }
                        }
                    }
                } else if open && !cancel {
                    self.dialog = Dialog::RemoteInterpreter {
                        kind,
                        target,
                        python,
                        remote_root,
                        error,
                    };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::WslDistributions { root, python, job } => {
                if self.project.as_ref().is_some_and(|p| p.root == root) {
                    match job.receiver.try_recv() {
                        Ok(Ok(names)) => {
                            self.dialog = Dialog::RemoteInterpreter {
                                kind: "wsl".into(),
                                target: names.first().cloned().unwrap_or_default(),
                                python,
                                remote_root: String::new(),
                                error: Some(if names.is_empty() {
                                    crate::i18n::tr_static("No registered WSL distributions").into()
                                } else {
                                    format!("Registered distributions: {}", names.join(", "))
                                }),
                            }
                        }
                        Ok(Err(error)) => {
                            self.dialog = Dialog::RemoteInterpreter {
                                kind: "wsl".into(),
                                target: String::new(),
                                python,
                                remote_root: String::new(),
                                error: Some(error),
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            let mut cancel = false;
                            egui::Window::new(crate::i18n::tr_static("WSL distributions"))
                                .collapsible(false)
                                .show_modal(ctx, |ui| {
                                    crate::a11y::spinner(ui);
                                    ui.label(crate::i18n::tr_static(
                                        "Listing registered distributions…",
                                    ));
                                    cancel = ui.button(crate::i18n::tr_static("Cancel")).clicked();
                                });
                            if !cancel {
                                self.dialog = Dialog::WslDistributions { root, python, job };
                            }
                            ctx.request_repaint_after(std::time::Duration::from_millis(50));
                        }
                        Err(_) => self.set_status("WSL registration discovery cancelled"),
                    }
                }
            }
            Dialog::InterpreterProbe { root, job } => {
                if self.project_safe_mode() || self.project.as_ref().is_none_or(|p| p.root != root)
                {
                    self.set_status(
                        "Interpreter discovery cancelled because the project or trust changed",
                    );
                } else {
                    match job.receiver.try_recv() {
                        Ok(Ok(metadata)) => {
                            let interpreter = metadata.interpreter;
                            if let Some(project) = &mut self.project {
                                project.interpreters.retain(|i| i.path != interpreter.path);
                                project.interpreters.insert(0, interpreter.clone());
                            }
                            self.select_interpreter(interpreter);
                            self.set_status(format!(
                                "Interpreter added ({} / {} bit)",
                                metadata.architecture, metadata.bits
                            ));
                        }
                        Ok(Err(error)) => self.set_status(format!("Invalid interpreter: {error}")),
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            self.set_status("Interpreter probe cancelled")
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            let mut cancel = false;
                            egui::Window::new(crate::i18n::tr_static("Discovering interpreter")).collapsible(false).show_modal(ctx, |ui| {
                                crate::a11y::spinner(ui); ui.label(crate::i18n::tr_static("Checking interpreter version, architecture and library paths…"));
                                cancel = ui.button(crate::i18n::tr_static("Cancel")).clicked();
                            });
                            if !cancel {
                                self.dialog = Dialog::InterpreterProbe { root, job };
                            }
                            ctx.request_repaint_after(std::time::Duration::from_millis(50));
                        }
                    }
                }
            }
            Dialog::TrustProject(root) => {
                let mut open = true;
                let mut decided = false;
                let mut trust = false;
                let theme = self.theme.clone();
                egui::Window::new(crate::i18n::tr_static("Trust and Open Project?")).collapsible(false).resizable(false).default_width(520.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
                    ui.label(format!("{}", root.display()));
                    ui.label(egui::RichText::new(crate::i18n::tr_static("Safe mode runs nothing from the project: no run configurations, tests, console, startup tasks, file watchers or external tools, and not the Python of its own virtual environment. Trust the project if you know its source.")).color(theme.text_dim));
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr_static("Trust Project")).clicked() {
                            trust = true;
                            decided = true;
                        }
                        if ui.button(crate::i18n::tr_static("Preview in Safe Mode")).clicked() {
                            self.safe_mode = true;
                            decided = true;
                        }
                    });
                });
                if trust {
                    self.trust_project_at(&root);
                    self.focus_editor = true;
                } else if decided || !open {
                    self.save_workspace();
                    self.focus_editor = true;
                } else {
                    self.dialog = Dialog::TrustProject(root);
                }
            }
            Dialog::KeymapReference => {
                let mut open = true;
                let theme = self.theme.clone();
                egui::Window::new(crate::i18n::tr_static("Keyboard Shortcuts"))
                    .collapsible(false)
                    .resizable(true)
                    .default_size([720.0, 560.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        self.keymap_reference_export(ui);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            egui::Grid::new("keymap_ref")
                                .num_columns(2)
                                .striped(true)
                                .spacing([24.0, 4.0])
                                .show(ui, |ui| {
                                    for &a in Action::ALL {
                                        let sc = self.keymap.shortcut_text(a);
                                        if sc.is_empty() {
                                            continue;
                                        }
                                        ui.label(
                                            egui::RichText::new(sc).monospace().color(theme.accent),
                                        );
                                        ui.label(a.text());
                                        ui.end_row();
                                    }
                                });
                        });
                    });
                if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.dialog = Dialog::KeymapReference;
                }
            }
            Dialog::HelpTopics => {
                let mut open = true;
                let theme = self.theme.clone();
                egui::Window::new(crate::i18n::tr_static("Help"))
                    .collapsible(false)
                    .resizable(true)
                    .default_size([760.0, 560.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            crate::markdown_view::render_markdown(ui, &theme, HELP_TEXT, 14.0);
                        });
                        if ui
                            .button(crate::i18n::tr_static(
                                "Welcome / Accessibility guides and bundled HTML…",
                            ))
                            .clicked()
                        {
                            self.help_action(Action::WelcomeGuide, ctx);
                        }
                    });
                if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.dialog = Dialog::HelpTopics;
                }
            }
            Dialog::TipOfTheDay(mut idx) => {
                let mut open = true;
                let theme = self.theme.clone();
                let tips = TIPS;
                idx %= tips.len();
                egui::Window::new(crate::i18n::tr_static("Tip of the Day"))
                    .collapsible(false)
                    .resizable(false)
                    .default_width(520.0)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        crate::markdown_view::render_markdown(
                            ui,
                            &theme,
                            &crate::i18n::tr(tips[idx]),
                            14.0,
                        );
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut self.settings.show_tips_on_startup,
                                crate::i18n::tr_static("Show tips on startup"),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(crate::i18n::tr_static("Next Tip")).clicked() {
                                        idx = (idx + 1) % tips.len();
                                    }
                                    if ui.button(crate::i18n::tr_static("Previous Tip")).clicked() {
                                        idx = (idx + tips.len() - 1) % tips.len();
                                    }
                                },
                            );
                        });
                    });
                if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.dialog = Dialog::TipOfTheDay(idx);
                } else {
                    let _ = self.settings.save();
                }
            }
            Dialog::About => {
                let mut open = true;
                egui::Window::new(crate::i18n::tr_static("About RustPy"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.heading("RustPy");
                        let info = help_workspace::about_info();
                        ui.monospace(&info);
                        if ui.button(crate::i18n::tr_static("Copy Info")).clicked() {
                            ctx.copy_text(info);
                        }
                        ui.label(crate::i18n::tr_static(
                            "Built with egui. Fonts: Hack (MIT, Bitstream Vera).",
                        ));
                    });
                if open {
                    self.dialog = Dialog::About;
                }
            }
            Dialog::Settings => {
                self.settings_window(ctx);
            }
            Dialog::OpenPath(b) => self.open_path_dialog(ctx, b),
            Dialog::NewScratch(query, selected) => self.new_scratch_dialog(ctx, query, selected),
            Dialog::GitignoreTemplates(st) => self.gitignore_templates_dialog(ctx, st),
            Dialog::LocalHistory(st) => self.local_history_dialog(ctx, st),
            Dialog::UndoHistory(st) => self.undo_history_dialog(ctx, st),
            Dialog::ConfirmRefactoringUndo(request) => self.refactoring_undo_dialog(ctx, request),
            Dialog::FolderHistory(st) => self.folder_history_dialog(ctx, st),
            Dialog::Structural(st) => self.structural_dialog(ctx, st),
            Dialog::ResolveConflict {
                path,
                ours,
                theirs,
                base,
                diff,
            } => {
                let mut open = true;
                let mut resolve: Option<String> = None;
                let mut esc = false;
                let title = crate::i18n::tr_fmt(
                    "Resolve Conflict — {file}",
                    &[(
                        "file",
                        &path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    )],
                );
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([1000.0, 620.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        resolve = self.visual_merge_workspace(ui, &path, &base, &ours, &theirs);
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            esc = true;
                        }
                    });
                if let Some(content) = resolve {
                    if let Some(git) = GitRepo::discover(&path) {
                        let root = git.root();
                        crate::history_store::snapshot_paths(
                            &crate::history_store::base_dir(),
                            Some(root),
                            std::slice::from_ref(&path),
                            "Before resolve",
                            false,
                        );
                        match git.resolve_conflict(&path, &content) {
                            Ok(()) => self.set_status(format!("Resolved {}", path.display())),
                            Err(e) => self.set_status(e.to_string()),
                        }
                    }
                    self.reload_changed_files();
                    if let Some(p) = &mut self.project {
                        p.git_refreshed = -100.0;
                    }
                    self.git_panel.last_refresh = -100.0;
                    self.focus_editor = true;
                } else if open && !esc {
                    self.dialog = Dialog::ResolveConflict {
                        path,
                        ours,
                        theirs,
                        base,
                        diff,
                    };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::CompareBranch {
                branch,
                diffs,
                mut selected,
            } => {
                let theme = self.theme.clone();
                let mut open = true;
                let mut esc = false;
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                egui::Window::new(crate::i18n::tr_fmt(
                    "Compare with Branch '{branch}'",
                    &[("branch", &branch)],
                ))
                .collapsible(false)
                .resizable(true)
                .default_size([1000.0, 600.0])
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    egui::Panel::left("cmp_branch_files")
                        .resizable(true)
                        .default_size(320.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("cmp_branch_list")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if diffs.is_empty() {
                                        ui.label(
                                            egui::RichText::new(crate::i18n::tr_static(
                                                "No differences",
                                            ))
                                            .color(theme.text_dim),
                                        );
                                    }
                                    for (i, d) in diffs.iter().enumerate() {
                                        let rel = d
                                            .path
                                            .strip_prefix(&root)
                                            .unwrap_or(&d.path)
                                            .display()
                                            .to_string();
                                        let color =
                                            crate::git_panel::status_color(d.status, &theme)
                                                .unwrap_or(theme.text);
                                        if ui
                                            .selectable_label(
                                                i == selected,
                                                egui::RichText::new(format!(
                                                    "{rel}  +{} −{}",
                                                    d.additions, d.deletions
                                                ))
                                                .color(color),
                                            )
                                            .clicked()
                                        {
                                            selected = i;
                                        }
                                    }
                                });
                        });
                    egui::ScrollArea::both()
                        .id_salt("cmp_branch_patch")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(d) = diffs.get(selected) {
                                crate::git_panel::diff_text_ui(ui, &theme, &d.patch);
                            }
                        });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        esc = true;
                    }
                });
                if open && !esc {
                    self.dialog = Dialog::CompareBranch {
                        branch,
                        diffs,
                        selected,
                    };
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::Diff(st) => self.rich_diff_dialog(ctx, st),
            Dialog::DirectoryComparison => self.draw_directory_comparison(ctx),
            Dialog::DirectoryComparePick(state) => {
                self.draw_directory_comparison_picker(ctx, state)
            }
            Dialog::CompareFilePick(mut state) => {
                let (b, _) = &mut *state;
                let theme = self.theme.clone();
                let mut open = true;
                let mut chosen: Option<PathBuf> = None;
                let mut chosen_tab = None;
                let mut cancel = false;
                egui::Window::new(crate::i18n::tr_static("Compare File with…"))
                    .collapsible(false)
                    .resizable(true)
                    .default_size([640.0, 460.0])
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .open(&mut open)
                    .show_modal(ctx, |ui| {
                        ui.menu_button(crate::i18n::tr_static("Compare with open editor"), |ui| {
                            for tab in &self.tabs {
                                if ui.button(tab.state.title()).clicked() {
                                    chosen_tab = Some(file_comparisons::ComparisonSource {
                                        title: tab.state.title(),
                                        text: tab.state.doc.text(),
                                        path: tab.state.path.clone(),
                                        asset: tab.image.is_some() || tab.notebook.is_some(),
                                    });
                                    ui.close();
                                }
                            }
                        });
                        if let Some(p) = b.ui(ui, &theme, true, 360.0) {
                            chosen = Some(p);
                        }
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr_static("Compare")).clicked() {
                                chosen = b.selected.clone().filter(|p| p.is_file());
                            }
                            if ui.button(crate::i18n::tr_static("Cancel")).clicked() {
                                cancel = true;
                            }
                        });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            cancel = true;
                        }
                    });
                if let Some(other) = chosen_tab {
                    if let Err(error) = self.compare_captured_editor(&state.1, other) {
                        self.set_status(error);
                        self.dialog = Dialog::CompareFilePick(state);
                    }
                } else if let Some(p) = chosen {
                    self.confirm_comparison_picker(state, &p);
                } else if open && !cancel {
                    self.dialog = Dialog::CompareFilePick(state);
                } else {
                    self.focus_editor = true;
                }
            }
            Dialog::NewProject(st) => self.new_project_dialog(ctx, st),
            Dialog::CloneRepo(st) => self.clone_dialog(ctx, st),
            Dialog::OpenProject(st) => self.open_project_dialog(ctx, st),
            Dialog::WorkspacePersistence(transition) => {
                self.workspace_persistence_dialog(ctx, transition)
            }
            Dialog::Notice { title, text, then } => self.notice_dialog(ctx, title, text, then),
            Dialog::QuickSwitch(st) => self.quick_switch_dialog(ctx, st),
        }
    }
}

impl eframe::App for IdeApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Smoke tests: replay scripted input only after startup settled.
        if self.frame_count < 10 {
            self.clipboard_shortcuts.normalize(raw_input);
            self.save_synchronization.note_input(raw_input);
            if self.input_player.is_some() {
                ctx.request_repaint();
            }
            return;
        }
        if let Some(p) = &mut self.input_player {
            // A finished player stays installed: it keeps the headless window
            // focused (see `InputPlayer::feed`) and marks the smoke session.
            let was_running = !p.is_finished();
            if p.feed(raw_input) {
                ctx.request_repaint();
            } else if was_running {
                log::info!("smoke: input script finished");
                ctx.request_repaint();
            }
        }
        self.clipboard_shortcuts.normalize(raw_input);
        if !self.coordinated_quit_active() && !self.coordinated_quit_committed {
            self.plugin_modal_input(ctx, raw_input);
        }
        self.save_synchronization.note_input(raw_input);
        if self.editor_keyboard() {
            if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                if ctx.memory(|memory| memory.has_focus(tab.view.id)) {
                    crate::performance_trace::input(
                        raw_input,
                        tab.state.doc.line_count(),
                        tab.state.is_large_file(),
                    );
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.coordinated_quit_active() && !self.coordinated_quit_committed {
            self.refresh_project_trust();
        }
        self.sync_native_tool_modality(ui.ctx());
        self.web_preview_begin_frame(ui.ctx());
        self.poll_vcs_mappings(ui.ctx());
        self.poll_other_vcs(ui.ctx());
        self.theme.bind_editor_scheme(&self.scheme);
        crate::diff_view::configure(&self.settings.diff_merge);
        self.retire_diff_workspace();
        self.poll_diff_workspace_jobs(ui.ctx());
        self.poll_http_workspace(ui.ctx());
        self.poll_svg_previews(ui.ctx());
        self.poll_directory_comparison(ui.ctx());
        self.prune_file_permissions();
        let focused = ui.input(|input| input.viewport().focused.unwrap_or(false));
        if focused && !self.permissions_window_focused {
            if let Some(index) = self.active {
                self.refresh_file_permissions(index);
            }
        }
        self.permissions_window_focused = focused;
        self.detect_clipboard_stack_trace(ui.ctx());
        self.poll_commit_preparation();
        if self.git_panel.commit_workspace.preparation.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self.coordinated_quit_committed || self.coordinated_quit_active() {
            self.run_panel.launcher.reset_ctrl_gesture();
            self.emacs_editing.interrupt();
            // Committed draws only Close; uncommitted rejects native close and
            // renders the dedicated guard without ordinary publication callbacks.
            self.coordinated_quit_guard_ui(ui.ctx());
            return;
        }
        self.capture_quit_backdrop(ui.ctx());
        self.reconcile_document_markers();
        crate::i18n::sync_language(ui.ctx(), &self.settings.appearance.language);
        rustpy_i18n::regional::configure(
            &self.settings.appearance.date_locale,
            self.settings.appearance.date_iso,
            self.settings.appearance.clock_12_hour,
        );
        // Freeze detector: this frame ends when the guard drops (end of `ui`).
        let mut frame_guard = self.bg.freeze.frame();
        if ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Text(_) | egui::Event::Key { pressed: true, .. }
                )
            })
        }) {
            frame_guard.set_had_input(true);
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))) {
                self.bg.freeze.note_action("typing");
            }
        }
        self.chrome_frame_start(ui.ctx());
        self.refresh_editor_window_title(ui.ctx());
        if !self.shortcuts_applied {
            self.shortcuts_applied = true;
            self.apply_custom_shortcuts();
            self.apply_ui_font_size(ui.ctx());
            self.terminal_panel.shell =
                Some(self.settings.terminal.shell.clone()).filter(|s| !s.trim().is_empty());
            self.startup_notices();
        }
        let ctx = ui.ctx().clone();
        // Settings closed some other way (Escape, a dialog taking its place): that is Cancel.
        if self.settings_ui.snapshot.is_some() && !matches!(self.dialog, Dialog::Settings) {
            self.restore_settings_snapshot(&ctx);
        }
        if ctx.input(|i| i.viewport().close_requested())
            && (self.directory_comparisons_pending()
                || self.native_skeletons_pending()
                || self.run_workflows_pending())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.intercept_close(&ctx) {
            if let Some(i) = self.tabs.iter().position(|t| t.has_unsaved_changes()) {
                if !self.exit_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.dialog = Dialog::ConfirmClose {
                        tab: i,
                        then_exit: true,
                    };
                }
            }
        }
        // Apply the theme once per frame start (cheap) so toggles take effect.
        static THEME_APPLIED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !THEME_APPLIED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            self.theme.apply(&ctx);
        }
        self.menus_frame(&ctx);
        self.accessibility_frame(&ctx);
        crate::fonts::update(
            &ctx,
            &crate::fonts::FontChoice::from_settings(&self.settings),
        );
        // Dropped files open as tabs.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            if p.is_dir() {
                self.request_open_project(&p);
            } else {
                self.open_file(&p);
            }
        }
        // Files macOS asked to open (Finder's Open With, the Dock icon); empty elsewhere.
        for p in self.take_macos_window_paths(&ctx) {
            if p.is_dir() {
                self.request_open_project(&p);
            } else {
                self.open_file(&p);
            }
        }
        if let Some((_, t)) = &mut self.status {
            *t += ctx.input(|i| i.stable_dt) as f64;
            if *t > 8.0 {
                self.status = None;
            }
        }

        // Before shortcuts: modal certificate / master-password prompts own the keyboard.
        self.netsec_tick(&ctx);
        self.layout_frame_start(&ctx);
        self.global_shortcuts(ui);
        let now = ctx.input(|i| i.time);
        self.poll_interpreters();
        self.poll_background(&ctx);
        self.poll_data_files(&ctx);
        self.db_panel.set_database_execution_allowed(
            self.builtin_plugin_enabled("database") && !self.project_safe_mode(),
        );
        self.db_panel.poll_database();
        if self.diagram_panel.poll_capture() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        if self.db_panel.database_has_owners() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.poll_web_languages(&ctx);
        self.poll_diagnostic_export();
        self.poll_ide_capture(&ctx, false);
        self.poll_debug_sessions(&ctx);
        self.poll_debug(&ctx);
        if self
            .debug
            .as_mut()
            .is_some_and(|session| std::mem::take(&mut session.backend.scientific_requested))
        {
            self.bottom = Some(BottomTool::SciView);
        }
        self.configure_package_context();
        if self.packages_panel.poll() {
            ctx.request_repaint();
        }
        if self.packages_panel.take_packages_changed() {
            self.packages_changed();
        }
        self.poll_failure_notifications(&ctx);
        self.poll_project_index(&ctx);
        if self.py_console.poll() {
            ctx.request_repaint();
        }
        if self.py_console.take_plot_request() {
            self.bottom = Some(BottomTool::SciView);
        }
        self.poll_scientific(&ctx);
        self.scientific_floating(&ctx);
        self.configure_coverage();
        let coverage_changed = self.coverage_panel.poll();
        let profiler_changed = self.profiler_panel.poll();
        let concurrency_changed = self.concurrency_panel.poll();
        if coverage_changed
            || profiler_changed
            || concurrency_changed
            || self.concurrency_panel.is_running()
            || self.coverage_panel.is_running()
            || self.profiler_panel.is_running()
        {
            self.coverage_project_badges();
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        self.poll_retained_measurements(&ctx);
        self.poll_testing_ui(&ctx);
        if self.test_panel.poll() || self.test_panel.wants_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        for (host, port) in std::mem::take(&mut self.test_panel.debugger_endpoints) {
            self.attach_failed_test_debugger(host, port);
        }
        self.flush_pending_copy(&ctx);
        if let Some(a) = self.pending_action.take() {
            self.handle_action(a, &ctx);
        }
        self.poll_markdown_jobs();
        self.external_tools_tick(&ctx);
        self.poll_pending_venv(&ctx);
        self.poll_plugin_jobs(&ctx);
        if self.exit_requested {
            self.services_panel.docker.cancel();
            self.poll_docker_services_retirement(&ctx);
        } else {
            self.poll_docker_services(&ctx);
        }
        self.poll_git_console_jobs(&ctx);
        self.tasks_tick(&ctx);
        self.sync_textmate_bundles(&ctx);
        for tab in &mut self.tabs {
            tab.state.set_todo_settings(&self.settings.todo);
        }
        self.index_tick(&ctx);
        self.updates_tick(&ctx);
        self.settings_sync_tick(&ctx, true);
        if !self.exit_requested {
            self.project_windows_tick(&ctx);
            self.project_windows_ui(&ctx);
            self.native_ui_tick(&ctx);
            self.native_ui_windows(&ctx);
        }
        if !self.exit_requested {
            self.plugin_runtime_tick(&ctx);
            self.plugin_runtime_windows(&ctx);
        }
        {
            let theme = self.theme.clone();
            let root = self.project.as_ref().map(|p| p.root.clone());
            let deployment_allowed = !self.project_safe_mode();
            if !deployment_allowed {
                self.deploy.cancel_all();
            }
            for ev in if deployment_allowed {
                self.deploy.ui(&ctx, &theme, root.as_deref())
            } else {
                Vec::new()
            } {
                match ev {
                    crate::deploy_ui::DeployEvent::Status(m) => self.set_status(m),
                    crate::deploy_ui::DeployEvent::DockerDebug(
                        command,
                        host,
                        port,
                        remote,
                        root,
                    ) => self.attach_deployment_docker(command, host, port, remote, root),
                    crate::deploy_ui::DeployEvent::FilesChanged => self.reload_changed_files(),
                    crate::deploy_ui::DeployEvent::OpenFile(path) => self.open_file(&path),
                    crate::deploy_ui::DeployEvent::Compare(local, remote) => {
                        if let Err(error) = self.open_asset_diff(local, remote) {
                            self.set_status(error);
                        }
                    }
                    crate::deploy_ui::DeployEvent::Terminal(title, program, args, cwd) => {
                        match self
                            .terminal_panel
                            .open_command(&title, &program, &args, &cwd, &[])
                        {
                            Ok(()) => self.bottom = Some(BottomTool::Terminal),
                            Err(error) => self.set_status(error.to_string()),
                        }
                    }
                    crate::deploy_ui::DeployEvent::Interpreter(server, root) => {
                        if !self.project_safe_mode() {
                            self.deploy.prepare_interpreter(&root, *server);
                        }
                    }
                    crate::deploy_ui::DeployEvent::InterpreterReady(interpreter, root) => {
                        if self
                            .project
                            .as_ref()
                            .is_some_and(|project| project.root == root)
                        {
                            self.select_interpreter(interpreter);
                        }
                    }
                }
            }
        }
        let mut nb_busy = false;
        let mut notebook_debuggers = Vec::new();
        for t in &mut self.tabs {
            if let Some(nb) = t.notebook.as_mut() {
                nb.poll();
                if let Some(endpoint) = nb.take_debugger_endpoint() {
                    notebook_debuggers.push(endpoint);
                }
                nb_busy |= nb.is_busy();
                if nb.owns_quit_process() {
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            }
        }
        for endpoint in notebook_debuggers {
            match endpoint {
                Ok((host, port)) => self.attach_notebook_debugger(host, port),
                Err(error) => self.set_status(error),
            }
        }
        if nb_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if !self.background_status.is_empty() {
            let mut done = Vec::new();
            self.background_status.retain(|rx| match rx.try_recv() {
                Ok(m) => {
                    done.push(m);
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(_) => false,
            });
            if !done.is_empty() {
                self.services_panel.docker.force_refresh = true;
                if let Some(p) = &mut self.project {
                    p.git_refreshed = -100.0;
                }
            }
            for m in done {
                self.report_background_result(m);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        self.poll_github(&ctx);
        self.poll_branch_workspace_jobs(&ctx);
        self.poll_git_jobs(&ctx);
        self.sync_inspection_highlighting();
        // Startup tasks wait for interpreter discovery, so they run with the project's Python.
        if !self.startup_tasks_done
            && !self.project_safe_mode()
            && self
                .project
                .as_ref()
                .is_some_and(|p| p.interpreter_rx.is_none())
            && self.frame_count > 5
        {
            self.startup_tasks_done = true;
            let tasks = self.effective_workspace().startup_tasks;
            let cfgs: Vec<RunConfig> = self
                .project
                .as_ref()
                .map(|p| {
                    p.run_configs
                        .configs
                        .iter()
                        .filter(|c| tasks.contains(&c.name))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for cfg in cfgs {
                self.run_config(cfg);
            }
        }
        let now_t = ctx.input(|i| i.time);
        let previous_fs_dirty = self.fs_dirty_since;
        self.poll_fs_events(now_t);
        self.refresh_project_new_if_changed();
        if self.fs_dirty_since != previous_fs_dirty {
            if let Some(index) = self.active {
                self.refresh_file_permissions(index);
            }
        }
        if self.fs_dirty_since.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(target) = self.pending_select_in.take() {
            self.select_in(target, &ctx);
        }
        self.observe_navigation();
        self.observe_local_history(&ctx);
        self.project_view_dialog(&ctx);
        self.autoscroll_from_source();
        self.refresh_git(now);
        self.run_panel.settings = self.settings.run_console.clone();
        if self.run_panel.poll() || self.run_panel.has_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(id) = self
            .run_panel
            .attention
            .take()
            .filter(|id| self.run_panel.sessions.iter().any(|s| s.id == *id))
        {
            self.run_panel.active = Some(id);
            self.bottom = Some(BottomTool::Run);
        }
        if !rustpy_debug::dap::poll_retiring_transports().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        self.poll_terminal_workspace(&ctx);
        if !rustpy_terminal::poll_retiring_terminals().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        if self.terminal_panel.poll() {
            ctx.request_repaint();
        } else if matches!(
            self.bottom,
            Some(BottomTool::Terminal | BottomTool::PythonConsole)
        ) {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        self.sync_runtime_types(now);
        self.poll_runtime_docs();
        self.py_console.configure_output(&self.settings.run_console);
        self.sync_editor_preferences();
        self.sync_analysis(now);
        // Existing annotation lifecycle owns parsing; lenses only capture a
        // matching cached FileInfo and wait while that lifecycle catches up.
        let mut code_vision = std::mem::take(&mut self.code_vision);
        self.code_vision_tick(&mut code_vision, &ctx);
        self.code_vision = code_vision;
        self.gutter_actions_tick(&ctx);
        self.collaboration_tick(&ctx);
        self.collaboration_ui(&ctx);
        self.ai_inline_ui(&ctx);

        self.zen_mode_frame(&ctx);
        self.custom_window_header(ui);
        if self.shows_menu_bar()
            && (self.settings.chrome.native_header || self.settings.chrome.separate_main_menu)
        {
            egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        }
        if self.settings.appearance.show_main_toolbar
            && self.project.is_some()
            && self.shows_chrome()
        {
            egui::Panel::top("main_toolbar").show(ui, |ui| self.main_toolbar(ui));
        }
        if self.settings.appearance.show_status_bar && self.shows_chrome() {
            egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        }
        self.navigation_bar(ui);
        // Reserve persistent edge bars before any dock consumes editor space.
        self.left_tool_stripe(ui);
        self.top_tool_stripe(ui);
        self.right_tool_stripe(ui);
        self.bottom_tool_stripe(ui);
        if let Some((crate::ai_panel::Redirect::CommitMessage, text)) = self.ai_panel.poll(ui.ctx())
        {
            self.git_panel.message = text;
            self.set_status("Commit message generated");
        }
        self.sync_tool_window_maximized();
        self.tool_windows_docks(ui);
        // A maximized docked tool window takes the editor's place.
        let editor_hidden = self.editor_behind_tool_window();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(rgb(self.scheme.editor.background)))
            .show(ui, |ui| {
                if !editor_hidden {
                    self.editor_area_for_view_mode(ui);
                }
            });
        self.detached_window(&ctx);
        self.detached_terminal_window(&ctx);
        self.editor_accessibility_ui(&ctx);
        self.emmet_preview_ui(&ctx);
        self.error_description_ui(&ctx);
        self.draw_choice_popup(&ctx);
        self.live_template_choices_ui(&ctx);
        self.generate_methods_popup(&ctx);
        self.stack_trace_analysis_ui(&ctx);
        self.diff_asset_windows(&ctx);
        self.poll_directory_comparison(&ctx);
        self.poll_native_skeletons(&ctx);
        self.dialogs(&ctx);
        self.interpreter_management_frame(&ctx);
        self.poll_run_workflows(&ctx);
        self.run_console_dialog(&ctx);
        self.history_auxiliary_dialogs(&ctx);
        self.poll_directory_comparison(&ctx);
        self.draw_exit_prompt(&ctx);
        self.sync_native_tool_modality(&ctx);
        self.background_ui(&ctx);
        self.save_synchronization_tick(&ctx);
        {
            let theme = self.theme.clone();
            let keymap = self.keymap.clone();
            let root = self
                .project
                .as_ref()
                .map(|p| p.root.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            self.find_in_files.open_files = self
                .tabs
                .iter()
                .filter_map(|t| t.state.path.clone())
                .collect();
            self.find_in_files.current_dir = self
                .active
                .and_then(|i| self.tabs.get(i))
                .and_then(|t| t.state.path.as_ref())
                .and_then(|p| p.parent().map(Path::to_path_buf));
            self.find_in_files.excluded = self
                .project
                .as_ref()
                .map(|p| p.tree.excluded.iter().cloned().collect())
                .unwrap_or_default();
            let mut changed: Vec<PathBuf> = self
                .project
                .as_ref()
                .map(|project| {
                    project
                        .tree
                        .vcs_status
                        .iter()
                        .filter(|(_, status)| {
                            !matches!(
                                status,
                                rustpy_vcs::FileStatus::Unmodified
                                    | rustpy_vcs::FileStatus::Deleted
                                    | rustpy_vcs::FileStatus::Ignored
                            )
                        })
                        .map(|(path, _)| path.clone())
                        .collect()
                })
                .unwrap_or_default();
            changed.extend(
                self.tabs
                    .iter()
                    .filter(|tab| tab.state.doc.is_modified())
                    .filter_map(|tab| tab.state.path.clone()),
            );
            changed.sort();
            changed.dedup();
            let libraries = self
                .project
                .as_ref()
                .map(|project| {
                    project
                        .analysis
                        .resolver
                        .search_paths
                        .iter()
                        .chain(&project.analysis.resolver.stub_roots)
                        .filter(|path| !path.starts_with(&project.root))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            self.find_in_files.set_extra_sources(changed, libraries);
            self.refresh_find_scope_filters();
            let tabs = &self.tabs;
            let buffer_text = |p: &Path| Self::buffer_text(tabs, p);
            let evs: Vec<NavEvent> = self
                .everywhere
                .ui(&ctx, &theme, &root, &keymap)
                .into_iter()
                .chain(self.find_in_files.ui(&ctx, &theme, &root, &buffer_text))
                .collect();
            for ev in evs {
                self.handle_nav_event(ev, &ctx);
            }
        }
        self.nav_popups(&ctx);
        self.launcher_ui(&ctx);
        self.draw_notifications(&ctx);
        self.layout_frame_end(&ctx);
        self.help_workspace_tick(&ctx);
        self.platform_notices_tick(&ctx);
        self.remote_development_tick(&ctx);
        if !self.exit_requested {
            self.dialog_workspace_tick(&ctx);
        }
        self.diagnostics_workspace_ui(&ctx);
        self.debug_log_settings_ui(&ctx);
        self.frame_count += 1;
        if self.run_on_start && self.frame_count == 3 {
            self.run_on_start = false;
            self.run_action(true);
        }
        if let Some((line, col)) = self.goto_on_start.take() {
            if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                tab.view.cancel_session_restore();
                tab.state.go_to_line(line, Some(col));
            }
        }
        let elapsed = {
            let start = *self.start_time.get_or_insert(now);
            now - start
        };
        let debugger_busy = self
            .debug
            .as_ref()
            .is_some_and(|d| d.is_active() && !d.is_stopped());
        if !self.actions_on_start.is_empty()
            && elapsed > 1.2
            && now - self.last_smoke_action > 0.25
            && !debugger_busy
        {
            self.last_smoke_action = now;
            let id = self.actions_on_start.remove(0);
            if let Some(a) = Action::ALL.iter().find(|a| a.id() == id).copied() {
                if view::is_editor_action(a) {
                    let opts = self.edit_options();
                    if let Some(i) = self.active {
                        self.refresh_file_permissions(i);
                        view::handle_editor_action(a, &mut self.tabs[i].state, &opts);
                    }
                } else {
                    self.handle_action(a, &ctx);
                }
            } else {
                log::error!("smoke: unknown action id {id}");
            }
        }
        if let Some(tool) = self.tool_on_start.take() {
            if let Some(page) = tool.strip_prefix("settings:") {
                self.settings_ui.page = match page {
                    "keymap" => settings_ui::SettingsPage::Keymap,
                    "font" => settings_ui::SettingsPage::EditorFont,
                    "scheme" => settings_ui::SettingsPage::ColorScheme,
                    "templates" => settings_ui::SettingsPage::LiveTemplates,
                    "filetemplates" => settings_ui::SettingsPage::FileTemplates,
                    "interpreter" => settings_ui::SettingsPage::Interpreter,
                    "inspections" => settings_ui::SettingsPage::Inspections,
                    "shared-indexes" => settings_ui::SettingsPage::SharedIndexes,
                    "plugins" => settings_ui::SettingsPage::Plugins,
                    "external-tools" => settings_ui::SettingsPage::ExternalTools,
                    "proxy" => settings_ui::SettingsPage::HttpProxy,
                    "passwords" => settings_ui::SettingsPage::Passwords,
                    "certificates" => settings_ui::SettingsPage::ServerCertificates,
                    "reader" => settings_ui::SettingsPage::ReaderMode,
                    "updates" => settings_ui::SettingsPage::Updates,
                    "debugger" => settings_ui::SettingsPage::Debugger,
                    "concurrency" => settings_ui::SettingsPage::Concurrency,
                    "project-structure" => settings_ui::SettingsPage::ProjectStructure,
                    "notifications" => settings_ui::SettingsPage::Notifications,
                    _ => settings_ui::SettingsPage::Appearance,
                };
                self.dialog = Dialog::Settings;
            }
            if let Some(pat) = tool.strip_prefix("ssr:") {
                let mut st = structural_ui::StructuralState::default();
                if !pat.is_empty() {
                    st.pattern = pat.to_string();
                }
                self.structural_search_now(&mut st);
                self.dialog = Dialog::Structural(Box::new(st));
            }
            if let Some(code) = tool.strip_prefix("pyconsole:") {
                self.bottom = Some(BottomTool::PythonConsole);
                let cwd = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let python = self
                    .project
                    .as_ref()
                    .and_then(|p| p.interpreter.as_ref())
                    .map(|i| i.path.clone())
                    .unwrap_or_else(|| PathBuf::from("python3"));
                self.py_console.start(&python, &cwd, &[], &cwd);
                for stmt in code.split(";;") {
                    if let Some(expr) = stmt.trim().strip_prefix("table:") {
                        self.py_console.request_table(expr);
                    } else {
                        self.py_console.execute(stmt.trim());
                    }
                }
            }
            if let Some(abbr) = tool.strip_prefix("expand:") {
                if let Some(i) = self.active {
                    let caret = self.tabs[i].state.primary().head();
                    let abbr = abbr.to_string();
                    self.tabs[i].state.apply(
                        vec![rustpy_text::Edit::insert(caret, abbr.clone())],
                        vec![crate::editor::Caret::at(
                            caret + TextSize::new(abbr.len() as u32),
                        )],
                        rustpy_text::EditGroup::Separate,
                        Some("type"),
                    );
                    let ok = self.try_expand_abbreviation();
                    log::info!("smoke: expand {abbr}: {ok}");
                }
            }
            if let Some(spec) = tool.strip_prefix("breakpoint:") {
                if let Some((f, l)) = spec.rsplit_once(':') {
                    if let Ok(line) = l.parse::<usize>() {
                        self.breakpoints.set(Path::new(f), line, Default::default());
                    }
                }
            }
            if let Some(q) = tool.strip_prefix("find:") {
                self.find_in_files.open(false, Some(q.to_string()));
            } else if let Some(q) = tool.strip_prefix("everywhere:") {
                self.open_everywhere(EverywhereTab::All);
                self.everywhere.query = q.to_string();
            }
            self.bottom = match tool.as_str() {
                "git" => Some(BottomTool::Git),
                "branches" => Some(BottomTool::Branches),
                "other-vcs" | "svn" | "mercurial" | "perforce" => Some(BottomTool::OtherVcs),
                "pull-requests" => Some(BottomTool::PullRequests),
                "run" => Some(BottomTool::Run),
                "terminal" => Some(BottomTool::Terminal),
                "problems" => Some(BottomTool::Problems),
                "debug" => Some(BottomTool::Debug),
                "tests" => Some(BottomTool::Tests),
                "packages" => Some(BottomTool::Packages),
                "hierarchy" => Some(BottomTool::Hierarchy),
                "services" => Some(BottomTool::Services),
                "coverage" => Some(BottomTool::Coverage),
                "http" => Some(BottomTool::HttpClient),
                "database" => Some(BottomTool::Database),
                "diagram" => Some(BottomTool::Diagram),
                "endpoints" => Some(BottomTool::Endpoints),
                "manage.py" => Some(BottomTool::ManagePy),
                "packages-vuln" => {
                    self.packages_panel.vuln_check_pending = true;
                    Some(BottomTool::Packages)
                }
                "profiler" => Some(BottomTool::Profiler),
                "concurrency" => Some(BottomTool::Concurrency),
                "gitlog" => {
                    self.git_panel.tab = crate::git_panel::GitTab::Log;
                    Some(BottomTool::Git)
                }
                t if t.starts_with("pyconsole:") => Some(BottomTool::PythonConsole),
                "documentation" => Some(BottomTool::Documentation),
                "todo" => Some(BottomTool::Todo),
                "console" => Some(BottomTool::PythonConsole),
                "notifications" => Some(BottomTool::Notifications),
                _ => None,
            };
        }
        if self.terminal_on_start.is_some() {
            ctx.request_repaint();
            if let Some(StartupTerminalDispatch { cwd, env, command }) =
                self.take_startup_terminal(elapsed)
            {
                self.bottom = Some(BottomTool::Terminal);
                match self.terminal_panel.open(&cwd, &env) {
                    Ok(()) => {
                        if let Some(tab) =
                            self.terminal_panel.tabs.get_mut(self.terminal_panel.active)
                        {
                            tab.terminal.write(format!("{command}\n").as_bytes());
                        }
                    }
                    Err(error) => self.set_status(format!("Could not start terminal: {error}")),
                }
            }
        }
        if let Some(path) = self.screenshot_path.clone() {
            ctx.request_repaint();
            // Capture once the UI has settled (semantic analysis is debounced,
            // and it and Find in Files run in the background).
            // Keep normal smoke runs' 20s deadline, while allowing explicitly
            // delayed captures enough time to finish under smoke.sh's 120s cap.
            let screenshot_delay = std::env::var("RUSTPY_SMOKE_DELAY")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|delay| delay.is_finite() && (0.0..=90.0).contains(delay))
                .unwrap_or(2.5);
            // Performance runs opt in to an adaptive capture: wait for the
            // whole input script (a slow UI takes longer to replay it) up to
            // an explicit deadline under the wrapper's own timeout.
            let await_script = std::env::var_os("RUSTPY_SMOKE_AWAIT_SCRIPT").is_some();
            let screenshot_deadline = std::env::var("RUSTPY_SMOKE_DEADLINE")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|deadline| deadline.is_finite() && *deadline > 0.0)
                .unwrap_or(0.0)
                .max(20.0_f64.max(screenshot_delay + 15.0));
            if elapsed > screenshot_delay
                && (!await_script || self.input_player.as_ref().is_none_or(|p| p.is_finished()))
                && self.actions_on_start.is_empty()
                && self.frame_count.is_multiple_of(7)
                && !self.screenshot_requested
                && !self.bg.analysis.has_job()
                && !self.find_in_files.is_searching()
            {
                self.screenshot_requested = true;
                if self.find_in_files.visible {
                    log::info!(
                        "smoke: find query = {:?} ({} matches)",
                        self.find_in_files.query,
                        self.find_in_files.results.matches.len()
                    );
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot {
                        image, user_data, ..
                    } if !window_lifecycle::is_quit_backdrop(user_data) => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let [w, h] = img.size;
                let buf: Vec<u8> = img
                    .pixels
                    .iter()
                    .flat_map(|c| [c.r(), c.g(), c.b(), 255u8])
                    .collect();
                match image::RgbaImage::from_raw(w as u32, h as u32, buf) {
                    Some(rgba) => {
                        if let Err(e) = rgba.save(&path) {
                            log::error!("cannot save screenshot: {e}");
                        } else {
                            log::info!("screenshot saved to {}", path.display());
                        }
                    }
                    None => log::error!("screenshot buffer size mismatch"),
                }
                self.exit_requested = true;
            }
            if elapsed > screenshot_deadline {
                log::error!("screenshot timed out");
                self.exit_requested = true;
            }
        }
        if self.exit_requested {
            let terminals_pending = self.terminal_retirement_pending(&ctx);
            self.cancel_window_operations();
            self.services_panel.docker.cancel();
            self.poll_docker_services_retirement(&ctx);
            self.poll_dialog_workspace_retirement(&ctx);
            let native_ui_pending = self.retire_native_ui(&ctx);
            let project_windows_pending = self.retire_project_windows(&ctx);
            let appearance_pending = self.retire_appearance_asset(&ctx);
            self.stop_all_settings_sync();
            self.poll_settings_sync_retirement(&ctx);
            let vault_retirement = self.network_security_retire(&ctx).and_then(|pending| {
                if !pending {
                    self.finish_network_security_persistence()?;
                }
                Ok(pending)
            });
            match vault_retirement {
                Ok(true) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.exit_requested = false;
                    self.cancel_pending_update_restart();
                    self.cancel_dialog_restart();
                    self.set_status(format!("Quit cancelled: password storage failed: {error}"));
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    return;
                }
            }
            self.diagram_panel.cancel_capture();
            let diagram_pending = self.diagram_panel.poll_capture();
            self.stop_ai_workers();
            let plugins_pending = self.retire_plugin_runtime(&ctx);
            self.stop_all_remote_development();
            self.poll_remote_development_retirement(&ctx);
            self.stop_web_languages();
            self.poll_web_languages(&ctx);
            self.stop_collaboration();
            let diagnostic_capture_pending = self.poll_ide_capture(&ctx, true);
            let diagnostic_export_pending = self.diagnostic_export.retire();
            self.stop_update_transfer();
            self.poll_update_retirement(&ctx);
            self.deploy.cancel_all();
            self.deploy.poll_retirement(&ctx);
            self.data_files.cancel();
            self.db_panel.stop_all_database();
            self.db_panel.poll_database();
            let notebooks_pending = self.poll_notebook_retirement(&ctx);
            self.cancel_web_preview_project();
            self.stop_svg_previews();
            self.cancel_directory_comparisons();
            self.cancel_native_skeletons();
            self.cancel_interpreter_management();
            self.cancel_run_workflows();
            self.poll_run_workflows(&ctx);
            self.poll_directory_comparison(&ctx);
            self.poll_native_skeletons(&ctx);
            if terminals_pending
                || self.svg_jobs.is_active()
                || diagnostic_capture_pending
                || diagram_pending
                || diagnostic_export_pending
                || self.web_languages.is_running()
                || plugins_pending
                || native_ui_pending
                || project_windows_pending
                || appearance_pending
                || self.docker_services_pending().is_some()
                || self.dialog_workspace_pending().is_some()
                || self.poll_debug_transport_retirement(&ctx)
                || !project_dialogs::abandoned_clone_cleanup_pending().is_empty()
                || !self.remote_development_pending().is_empty()
                || self.collaboration_running()
                || !self.ai_worker_ids().is_empty()
                || self.settings_sync_busy()
                || self.network_security_pending()
                || self.update_transfer_state().is_some()
                || self.deploy.is_busy()
                || self.data_files.busy()
                || self.db_panel.database_has_owners()
                || notebooks_pending
                || self.web_preview_pending()
                || (self.directory_comparisons_pending()
                    || self.native_skeletons_pending()
                    || self.run_workflows_pending())
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            self.save_workspace();
            let _ = self.settings_to_persist().save();
            if self.native_project_close(&ctx) {
                return;
            }
            crate::netsec::shutdown_prompts();
            self.finish_requested_restart();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.web_preview_frame(_frame, ui.ctx());
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if self.coordinated_quit_active() || self.coordinated_quit_committed {
            return;
        }
        self.save_workspace();
        let _ = self.settings_to_persist().save();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.coordinated_quit_active() || self.coordinated_quit_committed {
            return;
        }
        self.persist_terminal_layout();
        self.checkpoint_task_context();
        self.save_workspace();
        let _ = self.settings_to_persist().save();
    }
}

fn default_name_for(kind: &str) -> &'static str {
    match kind {
        "python" => "script.py",
        "unittest" => "test_something.py",
        "pytest" => "test_something.py",
        "stub" => "module.pyi",
        "markdown" => "README.md",
        "json" => "data.json",
        "requirements" => "requirements.txt",
        _ => "file.txt",
    }
}

fn ensure_extension(name: &str, kind: &str) -> String {
    let ext = match kind {
        "python" | "unittest" | "pytest" => ".py",
        "stub" => ".pyi",
        "markdown" => ".md",
        "json" => ".json",
        _ => "",
    };
    if ext.is_empty() || name.contains('.') {
        name.to_string()
    } else {
        format!("{name}{ext}")
    }
}

/// File and Code Templates (Settings › Editor › File and Code Templates equivalent).
fn file_template(kind: &str, file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let class_name: String = stem
        .split(['_', '-'])
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    match kind {
        "python" => format!(
            "\"\"\"{stem}.\n\nCreated on {date}.\n\"\"\"\n\n\ndef main() -> None:\n    pass\n\n\nif __name__ == \"__main__\":\n    main()\n"
        ),
        "unittest" => format!(
            "import unittest\n\n\nclass {class_name}(unittest.TestCase):\n    def test_something(self) -> None:\n        self.assertEqual(True, True)  # add assertion here\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
        ),
        "pytest" => {
            "import pytest\n\n\ndef test_something() -> None:\n    assert True\n".to_string()
        }
        "stub" => "from typing import Any\n\ndef function(arg: Any) -> Any: ...\n".to_string(),
        "markdown" => format!("# {stem}\n\n"),
        "json" => "{\n}\n".to_string(),
        "requirements" => "# Project dependencies (pip install -r requirements.txt)\n".to_string(),
        _ => String::new(),
    }
}

const HELP_TEXT: &str = r#"# RustPy Help

RustPy is a Python IDE written in Rust. The workspace is organized as follows:

- **Project tool window** (left): files, Structure, Bookmarks. Right-click a file or folder for New / Run / Rename / Delete.
- **Editor** (center): syntax highlighting, completion (`Ctrl+Space`), parameter info (`Ctrl+P`), quick documentation (`Ctrl+Q`), intentions and quick fixes (`Alt+Enter`), refactorings (`Ctrl+Alt+Shift+T`), live templates (`Tab`), inlay hints, sticky lines, soft wrap, splits.
- **Tool windows** (bottom): Terminal, Python Console, Run, Debug, Tests, Python Packages, Services, Git, Find, Problems, TODO, Hierarchy.

## Running and debugging

- `Shift+F10` runs the selected configuration, `Shift+F9` debugs it; `Ctrl+Shift+F10` runs the current file.
- Click the gutter to toggle a breakpoint (`Ctrl+F8`). Step with `F7`/`F8`/`Shift+F8`, resume with `F9`, evaluate with `Alt+F8`.
- Tests: Run → Run All Tests, or the gutter ▶ next to a test.

## Navigation

- `Shift Shift` Search Everywhere, `Ctrl+Shift+N` Go to File, `Ctrl+N` Go to Class, `Ctrl+Alt+Shift+N` Go to Symbol.
- `Ctrl+B` Go to Declaration (on a declaration it shows the usages, like `Ctrl+Alt+F7` Show Usages), `Ctrl+Shift+B` Type Declaration, `Ctrl+Shift+T` Go to Test, `Ctrl+Alt+B` Implementations, `Ctrl+U` Super Method, `Ctrl+H` Type Hierarchy, `Ctrl+Alt+H` Call Hierarchy.
- `Ctrl+E` Recent Files, `Ctrl+Tab` Switcher, `Ctrl+Shift+E` Recent Locations, `Ctrl+Alt+←/→` Back/Forward, `F11` Bookmarks.

## Settings

`Ctrl+Alt+S` opens Settings: Appearance, Keymap, Editor (General, Font, Color Scheme, Code Style, Inlay Hints, Inspections, Live Templates), Python Interpreter, Terminal, Git, Plugins.

See `roadmap.md` in the repository for the complete feature list and status.
"#;

const TIPS: &[&str] = &[
    "**Search Everywhere**: press `Shift` twice to find files, classes, symbols, actions and settings from one popup.",
    "**Alt+Enter** shows intentions and quick fixes for the problem at the caret — including *Create function*, *Add import* and *Convert to f-string*.",
    "**Live templates**: type `main` or `iter` and press `Tab`. Surround a selection with `Ctrl+Alt+T`.",
    "**Postfix completion**: type `expr.if`, `expr.return`, `expr.not` or `expr.for` and press `Tab` or `Enter`.",
    "**Extract Variable** (`Ctrl+Alt+V`) replaces an expression with a variable and lets you rename it in place.",
    "**Annotate with Git Blame** from the Git menu shows who changed each line, with color bands per commit.",
    "**Local History** keeps snapshots of every save, reformat and refactoring — Edit → Show Local History to revert.",
    "**Split the editor** from the Window menu, then move tabs between groups and navigate with *Goto Next Splitter*.",
    "**Run with Coverage / Profile**: use the Run menu to see line coverage in the gutter or a cProfile table.",
    "**Inlay hints** show parameter names and inferred variable types; toggle them in View → Inlay Hints.",
];

fn preview_line(t: &str) -> String {
    let first = t
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    let n = t.lines().count();
    let mut p: String = first.chars().take(80).collect();
    if n > 1 {
        p.push_str(&format!("  … ({n} lines)"));
    }
    p
}

/// Exact prefix matches first, then names accepted more often (stable otherwise).
fn rank_by_usage(
    items: Vec<CompletionItem>,
    prefix: &str,
    stats: &std::collections::HashMap<String, u32>,
) -> Vec<CompletionItem> {
    if stats.is_empty() || items.len() < 2 {
        return items;
    }
    let mut keyed: Vec<(bool, u32, usize, CompletionItem)> = items
        .into_iter()
        .enumerate()
        .map(|(k, it)| {
            let exact = !prefix.is_empty() && it.label.as_str() == prefix;
            let uses = if it.label.starts_with(prefix) {
                stats.get(it.label.as_str()).copied().unwrap_or(0)
            } else {
                0
            };
            (!exact, u32::MAX - uses, k, it)
        })
        .collect();
    keyed.sort_by_key(|(e, u, k, _)| (*e, *u, *k));
    keyed.into_iter().map(|(_, _, _, it)| it).collect()
}

fn completion_stats_path() -> PathBuf {
    Settings::config_dir().join("completion_stats.json")
}

fn load_completion_stats() -> std::collections::HashMap<String, u32> {
    std::fs::read_to_string(completion_stats_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_completion_stats(stats: &std::collections::HashMap<String, u32>) {
    // Keep the file small: the 2000 most used names.
    let mut v: Vec<(&String, &u32)> = stats.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    v.truncate(2000);
    let map: std::collections::HashMap<&String, &u32> = v.into_iter().collect();
    if let Ok(t) = serde_json::to_string(&map) {
        let _ = std::fs::write(completion_stats_path(), t);
    }
}

/// The spelling dictionary: bundled words plus `<config>/dictionary.txt` (loaded once).
fn spell_dictionary() -> std::sync::Arc<rustpy_spell::Dictionary> {
    static DICT: std::sync::OnceLock<
        std::sync::RwLock<Option<std::sync::Arc<rustpy_spell::Dictionary>>>,
    > = std::sync::OnceLock::new();
    let cell = DICT.get_or_init(Default::default);
    if !reload_marker().load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(d) = cell.read().ok().and_then(|g| g.clone()) {
            return d;
        }
    }
    let words = rustpy_spell::load_user_words(user_dictionary_path()).unwrap_or_default();
    let d = std::sync::Arc::new(rustpy_spell::Dictionary::bundled().with_user_words(&words));
    if let Ok(mut g) = cell.write() {
        *g = Some(d.clone());
    }
    reload_marker().store(false, std::sync::atomic::Ordering::Relaxed);
    d
}

/// Project words extend the IDE dictionary without changing another project's
/// spelling policy. Stable identities keep analysis cache keys reusable.
fn spell_dictionary_for(root: Option<&Path>) -> Arc<rustpy_spell::Dictionary> {
    let base = spell_dictionary();
    let Some(root) = root else { return base };
    type Entry = (
        Arc<rustpy_spell::Dictionary>,
        Vec<(PathBuf, Vec<u8>)>,
        Arc<rustpy_spell::Dictionary>,
    );
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Entry>>> =
        std::sync::OnceLock::new();
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join(".rustpy/dictionaries")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("txt") {
                if let Ok(bytes) = std::fs::read(&path) {
                    files.push((path, bytes));
                }
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some((old_base, old_files, dictionary)) = cache.get(root) {
        if Arc::ptr_eq(old_base, &base) && old_files == &files {
            return dictionary.clone();
        }
    }
    let mut dictionary = (*base).clone();
    for (_, bytes) in &files {
        for word in String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .filter(|word| !word.is_empty() && !word.starts_with('#'))
        {
            dictionary.add_word(word);
        }
    }
    let dictionary = Arc::new(dictionary);
    cache.insert(root.to_path_buf(), (base, files, dictionary.clone()));
    dictionary
}

/// Drops the cached dictionary so the next check reloads the user words.
fn static_reset_dictionary() {
    reload_marker().store(true, std::sync::atomic::Ordering::Relaxed);
}

fn reload_marker() -> &'static std::sync::atomic::AtomicBool {
    static M: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &M
}

fn user_dictionary_path() -> PathBuf {
    Settings::config_dir().join("dictionary.txt")
}

/// Typos as diagnostics with "Change to …" and "Save '…' to dictionary" fixes.
fn spelling_diagnostics(
    typos: &[rustpy_spell::Typo],
) -> (Vec<rustpy_core::Diagnostic>, QuickFixes) {
    use rustpy_analysis::file_analysis::{spelling_problems, DictionaryActionPolicy};
    flatten_prose_problems(spelling_problems(
        typos,
        DictionaryActionPolicy::IncludeSaveWord,
    ))
}

/// Grammar issues as typo-style diagnostics plus "Replace with" quick fixes.
type QuickFixes = Vec<(TextRange, Vec<rustpy_analysis::inspections::QuickFix>)>;

fn grammar_diagnostics(
    issues: &[rustpy_grammar::Issue],
) -> (Vec<rustpy_core::Diagnostic>, QuickFixes) {
    flatten_prose_problems(rustpy_analysis::file_analysis::grammar_problems(issues))
}

fn flatten_prose_problems(
    problems: Vec<inspections::Problem>,
) -> (Vec<rustpy_core::Diagnostic>, QuickFixes) {
    let fixes = problems
        .iter()
        .filter(|p| !p.fixes.is_empty())
        .map(|p| (p.diagnostic.range, p.fixes.clone()))
        .collect();
    let diagnostics = problems.into_iter().map(|p| p.diagnostic).collect();
    (diagnostics, fixes)
}

/// Builtin live templates, then those of enabled plugins, then the user's own
/// (a later one replaces an earlier one with the same abbreviation).
fn load_templates() -> Vec<LiveTemplate> {
    let user = templates::load_user_templates(&Settings::config_dir().join("live_templates.json"));
    crate::plugin_host::merge_live_templates(
        templates::builtin_templates(),
        &crate::plugin_host::contributions(),
        user,
    )
}

/// Shifts `range` through `edits` (applied together, sorted by position). `active` extends on insertion at its end.
fn remap_range(range: TextRange, edits: &[rustpy_text::Edit], active: bool) -> Option<TextRange> {
    let mut start = usize::from(range.start()) as i64;
    let mut end = usize::from(range.end()) as i64;
    let mut sorted: Vec<&rustpy_text::Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.range.start());
    let mut acc = 0i64;
    for e in sorted {
        let es = usize::from(e.range.start()) as i64 + acc;
        let ee = usize::from(e.range.end()) as i64 + acc;
        let delta = e.text.len() as i64 - (ee - es);
        if ee <= start && !(es == start && ee == start && es == end) {
            if ee == start && es == ee && active {
                // Insertion right at the start of an empty/active stop: it becomes part of the stop.
                end += delta;
            } else {
                start += delta;
                end += delta;
            }
        } else if es >= end {
            if es == end && ee == es && active {
                end += delta;
            }
        } else if es >= start && ee <= end {
            end += delta;
        } else {
            return None;
        }
        acc += delta;
    }
    if start < 0 || end < start {
        return None;
    }
    Some(TextRange::new(
        TextSize::new(start as u32),
        TextSize::new(end as u32),
    ))
}

fn completion_badge(kind: CompletionKind) -> (&'static str, Color32) {
    match kind {
        CompletionKind::Template => ("T", Color32::from_rgb(0xC8, 0x8F, 0xD6)),
        CompletionKind::Postfix => ("⇢", Color32::from_rgb(0x9E, 0x9E, 0x9E)),
        CompletionKind::Keyword => ("k", Color32::from_rgb(0xCC, 0x78, 0x32)),
        CompletionKind::Variable => ("v", Color32::from_rgb(0x6E, 0x9E, 0xD8)),
        CompletionKind::Parameter => ("p", Color32::from_rgb(0x8A, 0x6E, 0xC4)),
        CompletionKind::Function => ("f", Color32::from_rgb(0x59, 0xA8, 0x69)),
        CompletionKind::Method => ("m", Color32::from_rgb(0x59, 0xA8, 0x69)),
        CompletionKind::Class => ("C", Color32::from_rgb(0x4A, 0x88, 0xC7)),
        CompletionKind::Module => ("M", Color32::from_rgb(0x9E, 0x88, 0x0D)),
        CompletionKind::Attribute => ("a", Color32::from_rgb(0x6E, 0x9E, 0xD8)),
        CompletionKind::Property => ("P", Color32::from_rgb(0xB0, 0x8A, 0x2E)),
        CompletionKind::Constant => ("c", Color32::from_rgb(0xB0, 0x8A, 0x2E)),
    }
}

fn structure_badge(kind: crate::editor::syntax_support::StructureKind) -> (&'static str, Color32) {
    use crate::editor::syntax_support::StructureKind as K;
    match kind {
        K::Class => ("C", Color32::from_rgb(0x4A, 0x88, 0xC7)),
        K::Function => ("f", Color32::from_rgb(0x59, 0xA8, 0x69)),
        K::Method => ("m", Color32::from_rgb(0x59, 0xA8, 0x69)),
        K::Variable => ("v", Color32::from_rgb(0xB0, 0x8A, 0x2E)),
        K::Import => ("i", Color32::from_rgb(0x80, 0x80, 0x80)),
    }
}

#[cfg(test)]
mod usage_rank_tests {
    use super::*;

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            kind: CompletionKind::Variable,
            detail: String::new(),
            insert: label.into(),
            caret_offset: None,
            priority: 100,
        }
    }

    #[test]
    fn exact_first_then_most_used_prefix_matches() {
        let items = vec![item("pardir"), item("path"), item("pathsep"), item("pa")];
        let mut stats = std::collections::HashMap::new();
        stats.insert("path".to_string(), 5);
        stats.insert("pathsep".to_string(), 1);
        let ranked: Vec<String> = rank_by_usage(items, "pa", &stats)
            .into_iter()
            .map(|i| i.label.to_string())
            .collect();
        assert_eq!(ranked, vec!["pa", "path", "pathsep", "pardir"]);
    }
}

#[cfg(test)]
mod code_vision_event_bridge_tests {
    use super::*;

    #[test]
    fn problems_click_is_idempotent_and_belongs_to_rendered_split() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("first = 1\n", Language::Python));
        app.push_tab(EditorState::from_text("second = 2\n", Language::Python));
        app.tabs[0].group = 0;
        app.tabs[1].group = 1;
        app.split = Some(SplitDir::Horizontal);
        app.active = Some(1);
        app.bottom = Some(BottomTool::Find);
        app.code_vision_editor_event(0, &ViewEvent::OpenProblems);
        assert_eq!(app.active, Some(0));
        assert_eq!(app.group_active[0], Some(app.tabs[0].view.id));
        assert!(matches!(app.bottom, Some(BottomTool::Problems)));
        app.code_vision_editor_event(0, &ViewEvent::OpenProblems);
        assert!(matches!(app.bottom, Some(BottomTool::Problems)));
        assert_eq!(app.active, Some(0));
    }
}

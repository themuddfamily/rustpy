//! Settings dialog (Ctrl+Alt+S): page tree on the left,
//! page content on the right. Pages: Appearance, HTTP Proxy, Passwords, Keymap,
//! Editor (General, Font, Color Scheme, Code Style, Inlay Hints, Inspections,
//! Live Templates), Project (Python Interpreter), Tools (Terminal, Server
//! Certificates, …), Version Control, Plugins. The network and password pages
//! are in `netsec_ui`.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::i18n::tr_static;
use egui::FontId;
use rustpy_highlight::{highlight_python, HighlightTag, TextStyle};

#[cfg(test)]
#[path = "settings_accessibility_tests.rs"]
mod accessibility_tests;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum SettingsPage {
    Frameworks,
    WebDevelopment,
    #[default]
    Appearance,
    Notifications,
    Accessibility,
    MenusToolbars,
    Scopes,
    Keymap,
    EditorGeneral,
    EditorCodeEditing,
    EditorFolding,
    SchemeGeneral,
    SchemeDefaults,
    SchemeConsole,
    SchemeDebugger,
    SchemeDiff,
    SchemeFileTypes,
    SchemeVcs,
    EditorAppearance,
    AutoImport,
    Breadcrumbs,
    SmartKeys,
    EditorConsole,
    EditorTabs,
    GutterIcons,
    PostfixCompletion,
    FileEncodings,
    FileTypes,
    Copyright,
    Duplicates,
    Images,
    LanguageInjections,
    CodeVision,
    StickyLines,
    Emmet,

    EditorFont,
    ColorScheme,
    CodeStyle,
    InlayHints,
    ReaderMode,
    Inspections,
    NaturalLanguages,
    CodeCompletion,
    PythonTools,
    Intentions,
    Refactoring,
    ExternalTools,
    FileWatchers,
    Markdown,
    StartupTasks,
    WebBrowsers,
    ExternalDocumentation,
    AiAssistant,
    LiveTemplates,
    FileTemplates,
    TextMateBundles,
    Todo,
    Interpreter,
    PythonStubs,
    ProjectStructure,
    Terminal,
    VersionControl,
    DiffMerge,
    GitHub,
    TrustedLocations,
    SharedIndexes,
    Debugger,
    Concurrency,
    PythonConsole,
    Scientific,
    Jupyter,
    DataFiles,
    Coverage,
    Profiler,
    Plugins,
    HttpProxy,
    Passwords,
    ServerCertificates,
    Updates,
    DiagnosticTools,
    WindowChrome,
    DataSharing,
    PathVariables,
}

impl SettingsPage {
    /// The page header (`Group › Page`) in the UI language.
    pub(super) fn title(self) -> &'static str {
        tr_static(match self {
            SettingsPage::Scopes => "Appearance & Behavior › Scopes",
            SettingsPage::Appearance => "Appearance & Behavior › Appearance",
            SettingsPage::Notifications => "Appearance & Behavior › Notifications",
            SettingsPage::Accessibility => "Appearance & Behavior › Accessibility",
            SettingsPage::MenusToolbars => "Appearance & Behavior › Menus and Toolbars",
            SettingsPage::Keymap => "Keymap",
            SettingsPage::EditorGeneral => "Editor › General",
            SettingsPage::EditorCodeEditing => "Editor › Code Editing",
            SettingsPage::EditorFolding => "Editor › General › Code Folding",
            SettingsPage::SchemeGeneral => "Editor › Color Scheme › General",
            SettingsPage::SchemeDefaults => "Editor › Color Scheme › Language Defaults",
            SettingsPage::SchemeConsole => "Editor › Color Scheme › Console Font & Colors",
            SettingsPage::SchemeDebugger => "Editor › Color Scheme › Debugger",
            SettingsPage::SchemeDiff => "Editor › Color Scheme › Diff & Merge",
            SettingsPage::SchemeFileTypes => "Editor › Color Scheme › User-Defined File Types",
            SettingsPage::SchemeVcs => "Editor › Color Scheme › Version Control",

            SettingsPage::EditorAppearance => "Editor › General › Appearance",
            SettingsPage::AutoImport => "Editor › General › Auto Import",
            SettingsPage::Breadcrumbs => "Editor › General › Breadcrumbs",
            SettingsPage::SmartKeys => "Editor › General › Smart Keys",
            SettingsPage::EditorConsole => "Editor › General › Console",
            SettingsPage::EditorTabs => "Editor › General › Editor Tabs",
            SettingsPage::GutterIcons => "Editor › General › Gutter Icons",
            SettingsPage::PostfixCompletion => "Editor › General › Postfix Completion",
            SettingsPage::FileEncodings => "Editor › File Encodings",
            SettingsPage::FileTypes => "Editor › File Types",
            SettingsPage::Copyright => "Editor › Copyright",
            SettingsPage::Duplicates => "Editor › Duplicates",
            SettingsPage::Images => "Editor › Images",
            SettingsPage::LanguageInjections => "Editor › Language Injections",
            SettingsPage::CodeVision => "Editor › Inlay Hints › Code Vision",
            SettingsPage::StickyLines => "Editor › General › Sticky Lines",
            SettingsPage::Emmet => "Editor › Emmet",

            SettingsPage::EditorFont => "Editor › Font",
            SettingsPage::ColorScheme => "Editor › Color Scheme › Python",
            SettingsPage::CodeStyle => "Editor › Code Style",
            SettingsPage::InlayHints => "Editor › Inlay Hints",
            SettingsPage::ReaderMode => "Editor › Reader Mode",
            SettingsPage::CodeCompletion => "Editor › General › Code Completion",
            SettingsPage::Intentions => "Editor › Intentions",
            SettingsPage::Refactoring => "Editor › Refactoring",
            SettingsPage::ExternalTools => "Tools › External Tools",
            SettingsPage::FileWatchers => "Tools › File Watchers",
            SettingsPage::Markdown => "Editor › Markdown",
            SettingsPage::StartupTasks => "Tools › Startup Tasks",
            SettingsPage::WebDevelopment => "Languages & Frameworks › Web Development",
            SettingsPage::WebBrowsers => "Tools › Web Browsers and Preview",
            SettingsPage::ExternalDocumentation => "Tools › Python External Documentation",
            SettingsPage::PythonTools => "Tools › Python Integrated Tools",
            SettingsPage::AiAssistant => "Tools › AI Assistant",
            SettingsPage::Inspections => "Editor › Inspections",
            SettingsPage::NaturalLanguages => "Editor › Natural Languages",
            SettingsPage::LiveTemplates => "Editor › Live Templates",
            SettingsPage::FileTemplates => "Editor › File and Code Templates",
            SettingsPage::TextMateBundles => "Editor › TextMate Bundles",
            SettingsPage::Todo => "Editor › TODO",
            SettingsPage::Interpreter => "Project › Python Interpreter",
            SettingsPage::PythonStubs => "Project › Python Stubs",
            SettingsPage::ProjectStructure => "Project › Project Structure",
            SettingsPage::Terminal => "Tools › Terminal",
            SettingsPage::Frameworks => "Languages & Frameworks › Python Frameworks",
            SettingsPage::GitHub => "Version Control › GitHub",
            SettingsPage::VersionControl => "Version Control › Git",
            SettingsPage::DiffMerge => "Version Control › Diff & Merge",
            SettingsPage::TrustedLocations => "Build, Execution, Deployment › Trusted Locations",
            SettingsPage::SharedIndexes => "Tools › Shared Indexes",
            SettingsPage::DataFiles => "Languages & Frameworks › Data File Editors",
            SettingsPage::PythonConsole => "Build, Execution, Deployment › Python Console",
            SettingsPage::Scientific => "Tools › Python Scientific",
            SettingsPage::Jupyter => "Tools › Jupyter",
            SettingsPage::Coverage => "Build, Execution, Deployment › Python Coverage",
            SettingsPage::Profiler => "Build, Execution, Deployment › Python Profiler",
            SettingsPage::Concurrency => {
                "Build, Execution, Deployment › Thread Concurrency Visualization"
            }
            SettingsPage::Debugger => "Build, Execution, Deployment › Python Debugger",
            SettingsPage::Plugins => "Plugins",
            SettingsPage::HttpProxy => "Appearance & Behavior › System Settings › HTTP Proxy",
            SettingsPage::Passwords => "Appearance & Behavior › System Settings › Passwords",
            SettingsPage::ServerCertificates => "Tools › Server Certificates",
            SettingsPage::Updates => "Appearance & Behavior › System Settings › Updates",
            SettingsPage::DiagnosticTools => "Tools › Diagnostic Tools",
            SettingsPage::WindowChrome => "Appearance & Behavior › Menus, Toolbars and Window",
            SettingsPage::DataSharing => "Appearance & Behavior › System Settings › Data Sharing",
            SettingsPage::PathVariables => "Appearance & Behavior › Path Variables",
        })
    }
}

const TREE: &[(&str, &[(&str, SettingsPage)])] = &[
    // "Parent/Page" nests a page under a non-selectable "Parent" node.
    (
        "Appearance & Behavior",
        &[
            ("Appearance", SettingsPage::Appearance),
            ("Menus, Toolbars and Window", SettingsPage::WindowChrome),
            ("Scopes and File Colors", SettingsPage::Scopes),
            ("System Settings/HTTP Proxy", SettingsPage::HttpProxy),
            ("System Settings/Passwords", SettingsPage::Passwords),
            ("System Settings/Updates", SettingsPage::Updates),
            ("System Settings/Data Sharing", SettingsPage::DataSharing),
            ("Notifications", SettingsPage::Notifications),
            ("Accessibility", SettingsPage::Accessibility),
            ("Menus and Toolbars", SettingsPage::MenusToolbars),
            ("Path Variables", SettingsPage::PathVariables),
        ],
    ),
    ("Keymap", &[("Keymap", SettingsPage::Keymap)]),
    (
        "Editor",
        &[
            ("General", SettingsPage::EditorGeneral),
            ("Code Editing", SettingsPage::EditorCodeEditing),
            ("General/Code Folding", SettingsPage::EditorFolding),
            ("Color Scheme/General", SettingsPage::SchemeGeneral),
            (
                "Color Scheme/Language Defaults",
                SettingsPage::SchemeDefaults,
            ),
            (
                "Color Scheme/Console Font & Colors",
                SettingsPage::SchemeConsole,
            ),
            ("Color Scheme/Debugger", SettingsPage::SchemeDebugger),
            ("Color Scheme/Diff & Merge", SettingsPage::SchemeDiff),
            (
                "Color Scheme/User-Defined File Types",
                SettingsPage::SchemeFileTypes,
            ),
            ("Color Scheme/Version Control", SettingsPage::SchemeVcs),
            ("General/Appearance", SettingsPage::EditorAppearance),
            ("General/Auto Import", SettingsPage::AutoImport),
            ("General/Breadcrumbs", SettingsPage::Breadcrumbs),
            ("General/Smart Keys", SettingsPage::SmartKeys),
            ("General/Console", SettingsPage::EditorConsole),
            ("General/Editor Tabs", SettingsPage::EditorTabs),
            ("General/Gutter Icons", SettingsPage::GutterIcons),
            (
                "General/Postfix Completion",
                SettingsPage::PostfixCompletion,
            ),
            ("File Encodings", SettingsPage::FileEncodings),
            ("File Types", SettingsPage::FileTypes),
            ("Copyright", SettingsPage::Copyright),
            ("Duplicates", SettingsPage::Duplicates),
            ("Images", SettingsPage::Images),
            ("Language Injections", SettingsPage::LanguageInjections),
            ("Inlay Hints/Code Vision", SettingsPage::CodeVision),
            ("General/Sticky Lines", SettingsPage::StickyLines),
            ("Emmet", SettingsPage::Emmet),
            ("Font", SettingsPage::EditorFont),
            ("Color Scheme", SettingsPage::ColorScheme),
            ("Code Style", SettingsPage::CodeStyle),
            ("Inlay Hints", SettingsPage::InlayHints),
            ("Code Completion", SettingsPage::CodeCompletion),
            ("Intentions", SettingsPage::Intentions),
            ("Refactoring", SettingsPage::Refactoring),
            ("Inspections", SettingsPage::Inspections),
            ("Natural Languages", SettingsPage::NaturalLanguages),
            ("Live Templates", SettingsPage::LiveTemplates),
            ("TextMate Bundles", SettingsPage::TextMateBundles),
            ("TODO", SettingsPage::Todo),
            ("Reader Mode", SettingsPage::ReaderMode),
            ("File and Code Templates", SettingsPage::FileTemplates),
        ],
    ),
    (
        "Project",
        &[
            ("Python Interpreter", SettingsPage::Interpreter),
            ("Python Stubs", SettingsPage::PythonStubs),
            ("Project Structure", SettingsPage::ProjectStructure),
        ],
    ),
    (
        "Version Control",
        &[
            ("Git", SettingsPage::VersionControl),
            ("Diff & Merge", SettingsPage::DiffMerge),
            ("GitHub", SettingsPage::GitHub),
        ],
    ),
    (
        "Build, Execution, Deployment",
        &[
            ("Python Debugger", SettingsPage::Debugger),
            (
                "Thread Concurrency Visualization",
                SettingsPage::Concurrency,
            ),
            ("Python Console", SettingsPage::PythonConsole),
            ("Python Coverage", SettingsPage::Coverage),
            ("Python Profiler", SettingsPage::Profiler),
            ("Trusted Locations", SettingsPage::TrustedLocations),
        ],
    ),
    (
        "Tools",
        &[
            ("Frameworks", SettingsPage::Frameworks),
            ("Terminal", SettingsPage::Terminal),
            ("Python Scientific", SettingsPage::Scientific),
            ("Data File Editors", SettingsPage::DataFiles),
            ("Python Integrated Tools", SettingsPage::PythonTools),
            ("Jupyter", SettingsPage::Jupyter),
            ("External Tools", SettingsPage::ExternalTools),
            ("File Watchers", SettingsPage::FileWatchers),
            ("Markdown", SettingsPage::Markdown),
            ("Startup Tasks", SettingsPage::StartupTasks),
            ("Web Development", SettingsPage::WebDevelopment),
            ("Web Browsers", SettingsPage::WebBrowsers),
            ("AI Assistant", SettingsPage::AiAssistant),
            ("Shared Indexes", SettingsPage::SharedIndexes),
            ("Server Certificates", SettingsPage::ServerCertificates),
            ("Diagnostic Tools", SettingsPage::DiagnosticTools),
            (
                "Python External Documentation",
                SettingsPage::ExternalDocumentation,
            ),
        ],
    ),
    ("Plugins", &[("Plugins", SettingsPage::Plugins)]),
];

impl SettingsUi {
    /// Recording a shortcut (Change…) or looking one up (Find by shortcut).
    pub(super) fn capturing_keys(&self) -> bool {
        self.recording.is_some() || self.find_by_shortcut
    }

    /// One key press for the armed recorder: the recorded action gets the
    /// chord (or a prompt when another action has it), or the chord becomes
    /// the Find by shortcut filter. True once done (a lone modifier waits).
    pub(super) fn record_key(
        &mut self,
        keymap: &mut Keymap,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        let pressed = recorder_key(key, mods);
        if pressed == RecorderKey::Wait {
            return false;
        }
        if let Some(action) = self.recording.take() {
            match pressed {
                RecorderKey::Remove => keymap.clear_action(action),
                RecorderKey::Chord(chord) => match keymap.lookup(chord) {
                    Some(owner) if owner != action => {
                        self.keymap_conflict = Some((action, chord, owner))
                    }
                    _ => {
                        keymap.clear_action(action);
                        keymap.bind(chord, action);
                    }
                },
                RecorderKey::Cancel | RecorderKey::Wait => {}
            }
        } else {
            // Find by shortcut: the chord becomes the filter; Esc gives up.
            match pressed {
                RecorderKey::Chord(chord) => self.keymap_filter = chord.to_string(),
                RecorderKey::Remove => self.keymap_filter = "Backspace".into(),
                RecorderKey::Cancel | RecorderKey::Wait => {}
            }
            self.find_by_shortcut = false;
        }
        true
    }
}

#[derive(Default)]
pub(super) struct SettingsUi {
    pub page: SettingsPage,
    pub inspection_focus: Option<String>,
    pub inspection_request_focus: bool,
    pub search: String,
    pub keymap_filter: String,
    /// Action whose shortcut is being recorded.
    pub recording: Option<Action>,
    pub scheme_tag: Option<HighlightTag>,
    pub scheme_save_name: String,
    pub code_style: super::code_style_ui::State,
    pub python_style_page: String,
    pub scheme_advanced: super::scheme_editor::State,
    pub ui_theme_file: String,
    pub ui_theme_entries: Option<Vec<(String, Result<rustpy_settings::ui_themes::Theme, String>)>>,
    pub interpreter_input: String,
    pub stub_root_input: String,
    pub venv_status: Option<String>,
    /// Project-owned environment creation: target, progression and active command.
    pub venv_rx: Option<(
        PathBuf,
        rustpy_project::setup::Setup,
        rustpy_project::setup::CommandTask,
    )>,
    pub message: Option<String>,
    /// Grammars of the TextMate bundles (Editor › TextMate Bundles).
    pub textmate: super::textmate_bundles::TextMateBundles,
    /// A recorded shortcut already used by another action: (action, chord, current owner).
    pub keymap_conflict: Option<(Action, KeyChord, Action)>,
    /// "Find by shortcut": the next pressed chord becomes the filter.
    pub find_by_shortcut: bool,
    /// The shortcut "Find by shortcut" looks for (only actions bound to it are listed).
    pub find_chord: Option<KeyChord>,
    /// Keymap › Export / Import file.
    pub keymap_file: String,
    /// Editor › File and Code Templates.
    pub file_templates: super::file_templates_ui::FileTemplatesPage,
    /// State when the dialog opened (or was last applied): Cancel restores it.
    pub snapshot: Option<Box<SettingsSnapshot>>,
    pub trusted_location_input: String,
    pub plugins: super::plugins_ui::PluginsPage,
    /// HTTP Proxy, Server Certificates and Passwords pages, certificate and
    /// master-password prompts, password-safe worker.
    pub netsec: super::netsec_ui::NetsecUi,
    /// Appearance & Behavior › Path Variables.
    pub path_variables: super::path_variables_ui::PathVariablesPage,
}

/// What the Settings dialog can change in place: restored on Cancel.
pub(super) struct SettingsSnapshot {
    pub tool_bar_options: (bool, bool),
    pub settings: Settings,
    pub project_watchers: Vec<rustpy_settings::FileWatcher>,
    pub watcher_root: Option<PathBuf>,
    pub theme: UiTheme,
    pub scheme: ColorScheme,
    pub keymap: Keymap,
    /// Last frame the dialog was drawn (a dialog closed by something else counts as cancelled).
    pub frame: u64,
}

/// Allocate translated actions first so a long diagnostic cannot hide them.
fn settings_footer(
    ui: &mut egui::Ui,
    message: Option<&str>,
    modified: bool,
    color: egui::Color32,
) -> [egui::Response; 3] {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let ok = ui.button(tr_static("OK"));
        let apply = ui.add_enabled(modified, egui::Button::new(tr_static("Apply")));
        let cancel = ui.button(tr_static("Cancel"));
        if let Some(message) = message {
            ui.add(
                egui::Label::new(egui::RichText::new(message).color(color))
                    .truncate()
                    .show_tooltip_when_elided(false),
            )
            .on_hover_text(message);
        }
        [ok, apply, cancel]
    })
    .inner
}

fn unavailable_custom_theme_message(selected: &str, active: &str, error: &str) -> String {
    let recovery = if selected == active {
        "keeping last valid custom colors".to_owned()
    } else {
        format!("using {active} fallback")
    };
    format!("Selected UI theme unavailable; {recovery}: {error}")
}

/// egui's default text styles scaled for a UI font size (13 = 100 %). Always
/// computed from the defaults, so applying it again never compounds.
pub(super) fn scaled_text_styles(
    ui_font_size: f32,
) -> std::collections::BTreeMap<egui::TextStyle, FontId> {
    let scale = (ui_font_size / 13.0).clamp(0.6, 2.0);
    egui::Style::default()
        .text_styles
        .into_iter()
        .map(|(k, f)| (k, FontId::new(f.size * scale, f.family)))
        .collect()
}

/// Shortcut overrides of `keymap` relative to its preset, as stored in settings.
fn custom_shortcuts_of(keymap: &Keymap) -> Vec<(String, Vec<String>)> {
    Action::ALL
        .iter()
        .copied()
        .filter(|a| !keymap.is_default(*a))
        .map(|a| (a.id().to_string(), keymap.shortcut_definitions(a)))
        .collect()
}

/// What the Keymap page's shortcut recorder makes of a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecorderKey {
    /// A modifier on its own (Ctrl, Shift, …): the chord is not complete yet.
    Wait,
    /// Esc: stop recording.
    Cancel,
    /// Backspace: the action loses its shortcuts.
    Remove,
    Chord(KeyChord),
}

pub(super) fn recorder_key(key: egui::Key, mods: egui::Modifiers) -> RecorderKey {
    use egui::Key::*;
    match key {
        ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft
        | SuperRight => RecorderKey::Wait,
        Escape if mods.is_none() => RecorderKey::Cancel,
        Backspace if mods.is_none() => RecorderKey::Remove,
        _ => RecorderKey::Chord(KeyChord::new(key, mods)),
    }
}

/// Takes this frame's keyboard input out of `input` for the shortcut
/// recorder: key presses (winit reports Ctrl+C / Ctrl+X / Ctrl+V as
/// clipboard events), with typed text and releases dropped.
fn take_recorder_presses(input: &mut egui::InputState) -> Vec<(egui::Key, egui::Modifiers)> {
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..egui::Modifiers::NONE
    };
    let mut out = Vec::new();
    input.events.retain(|e| match e {
        egui::Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } => {
            if *pressed {
                out.push((*key, *modifiers));
            }
            false
        }
        egui::Event::Copy => {
            out.push((egui::Key::C, ctrl));
            false
        }
        egui::Event::Cut => {
            out.push((egui::Key::X, ctrl));
            false
        }
        egui::Event::Paste(_) => {
            out.push((egui::Key::V, ctrl));
            false
        }
        egui::Event::Text(_) => false,
        _ => true,
    });
    out
}

/// Whether `page` shows a value that differs between `old` and `new`.
pub(super) fn page_modified(page: SettingsPage, old: &Settings, new: &Settings) -> bool {
    let (a, b) = (&old.editor, &new.editor);
    match page {
        SettingsPage::Scopes => old.scopes != new.scopes,
        SettingsPage::Todo => old.todo != new.todo,
        SettingsPage::Appearance => {
            old.appearance != new.appearance
                || old.menus.appearance != new.menus.appearance
                || old.native_ui != new.native_ui
                || old.default_project_directory != new.default_project_directory
                || old.reopen_last_project != new.reopen_last_project
                || old.confirm_exit != new.confirm_exit
                || old.save_synchronization != new.save_synchronization
                || old.show_tips_on_startup != new.show_tips_on_startup
                || old.open_project_in() != new.open_project_in()
        }
        SettingsPage::Keymap => {
            old.keymap != new.keymap
                || old.custom_shortcuts != new.custom_shortcuts
                || old.keymap_overrides != new.keymap_overrides
                || old.custom_keymaps != new.custom_keymaps
                || old.keymap_quick_lists != new.keymap_quick_lists
                || old.macro_shortcuts != new.macro_shortcuts
                || old.quick_list != new.quick_list
                || old.external_tools != new.external_tools
        }
        SettingsPage::EditorAppearance
        | SettingsPage::AutoImport
        | SettingsPage::Breadcrumbs
        | SettingsPage::SmartKeys
        | SettingsPage::EditorConsole
        | SettingsPage::EditorTabs
        | SettingsPage::GutterIcons
        | SettingsPage::PostfixCompletion
        | SettingsPage::FileEncodings
        | SettingsPage::FileTypes
        | SettingsPage::Copyright
        | SettingsPage::Duplicates
        | SettingsPage::Images
        | SettingsPage::LanguageInjections
        | SettingsPage::CodeVision
        | SettingsPage::StickyLines
        | SettingsPage::Emmet => {
            old.editor.preferences != new.editor.preferences
                || old.editor != new.editor
                || old.code_style != new.code_style
                || old.run_console != new.run_console
                || old.python_console != new.python_console
                || old.web_development != new.web_development
                || old.copyright_profiles != new.copyright_profiles
        }
        SettingsPage::EditorCodeEditing
        | SettingsPage::EditorFolding
        | SettingsPage::SchemeGeneral
        | SettingsPage::SchemeDefaults
        | SettingsPage::SchemeConsole
        | SettingsPage::SchemeDebugger
        | SettingsPage::SchemeDiff
        | SettingsPage::SchemeFileTypes
        | SettingsPage::SchemeVcs => old.editor != new.editor || old.run_console != new.run_console,
        SettingsPage::EditorGeneral => {
            let strip = |e: &rustpy_settings::EditorSettings| {
                let mut e = e.clone();
                e.font_family.clear();
                e.font_size = 0.0;
                e.line_spacing = 0.0;
                e.enable_ligatures = false;
                e.preferences.font_fallbacks.clear();
                e.preferences.scheme_fonts.clear();
                e.color_scheme.clear();
                (
                    e.tab_size,
                    e.indent_size,
                    e.use_tab_character,
                    e.right_margin,
                    e.detect_indents,
                    e.editorconfig_enabled,
                ) = (0, 0, false, 0, false, false);
                e.visual_guides.clear();
                e.wrap_on_typing = false;
                (
                    e.inlay_parameter_hints,
                    e.inlay_type_hints,
                    e.inlay_return_hints,
                    e.code_vision_usages,
                    e.code_vision_author,
                    e.rainbow_brackets,
                ) = (false, false, false, false, false, false);
                e.reader_mode = Default::default();
                e
            };
            strip(a) != strip(b) || old.navigation_history != new.navigation_history
        }
        SettingsPage::EditorFont => {
            (
                &a.font_family,
                &a.preferences.font_fallbacks,
                a.font_size,
                a.line_spacing,
                a.enable_ligatures,
            ) != (
                &b.font_family,
                &b.preferences.font_fallbacks,
                b.font_size,
                b.line_spacing,
                b.enable_ligatures,
            )
        }
        SettingsPage::ColorScheme => {
            a.color_scheme != b.color_scheme
                || a.preferences.scheme_fonts != b.preferences.scheme_fonts
                || a.rainbow_brackets != b.rainbow_brackets
                || old.semantic_highlighting != new.semantic_highlighting
        }
        SettingsPage::CodeStyle => {
            old.save_actions != new.save_actions
                || old.code_style != new.code_style
                || old.code_style_schemes != new.code_style_schemes
                || old.copyright != new.copyright
                || old.copyright_profiles != new.copyright_profiles
                || (
                    a.tab_size,
                    a.indent_size,
                    a.use_tab_character,
                    a.right_margin,
                    a.wrap_on_typing,
                    &a.visual_guides,
                    a.detect_indents,
                    a.editorconfig_enabled,
                ) != (
                    b.tab_size,
                    b.indent_size,
                    b.use_tab_character,
                    b.right_margin,
                    b.wrap_on_typing,
                    &b.visual_guides,
                    b.detect_indents,
                    b.editorconfig_enabled,
                )
        }
        SettingsPage::InlayHints => {
            if a.preferences.inlay_parameter_blacklist != b.preferences.inlay_parameter_blacklist
                || a.preferences.inlay_disabled_languages != b.preferences.inlay_disabled_languages
                || a.preferences.code_vision_inheritors != b.preferences.code_vision_inheritors
                || a.preferences.code_vision_position != b.preferences.code_vision_position
            {
                return true;
            }

            (
                a.inlay_parameter_hints,
                a.inlay_type_hints,
                a.inlay_return_hints,
                a.code_vision_usages,
                a.code_vision_author,
            ) != (
                b.inlay_parameter_hints,
                b.inlay_type_hints,
                b.inlay_return_hints,
                b.code_vision_usages,
                b.code_vision_author,
            )
        }
        SettingsPage::ReaderMode => a.reader_mode != b.reader_mode,
        SettingsPage::MenusToolbars => {
            old.menus != new.menus || old.chrome.toolbar != new.chrome.toolbar
        }
        SettingsPage::Accessibility => {
            old.accessibility != new.accessibility
                || old.chrome.scale != new.chrome.scale
                || old.appearance.theme != new.appearance.theme
        }
        SettingsPage::Inspections => old.inspections != new.inspections,
        SettingsPage::NaturalLanguages => {
            old.inspections != new.inspections
                || a.unicode_bidi != b.unicode_bidi
                || a.unicode_base_direction != b.unicode_base_direction
        }
        SettingsPage::CodeCompletion => old.completion != new.completion,
        SettingsPage::PythonStubs => old.python_stub_roots != new.python_stub_roots,
        SettingsPage::PythonTools => {
            old.collect_runtime_types != new.collect_runtime_types
                || old.docstring_format != new.docstring_format
                || old.documentation != new.documentation
        }
        SettingsPage::Intentions => {
            old.disabled_intentions != new.disabled_intentions
                || a.show_intention_bulb != b.show_intention_bulb
                || a.show_intention_preview != b.show_intention_preview
        }
        SettingsPage::Refactoring => old.refactoring != new.refactoring,
        SettingsPage::ExternalTools => old.external_tools != new.external_tools,
        SettingsPage::FileWatchers => old.file_watchers != new.file_watchers,
        SettingsPage::Markdown => old.markdown != new.markdown,
        SettingsPage::WebDevelopment => old.web_development != new.web_development,
        SettingsPage::WebBrowsers => {
            old.browser != new.browser || old.web_browsers != new.web_browsers
        }
        SettingsPage::ExternalDocumentation => old.external_doc_urls != new.external_doc_urls,
        SettingsPage::AiAssistant => old.ai != new.ai,
        SettingsPage::Terminal => old.terminal != new.terminal,
        SettingsPage::VersionControl => {
            old.vcs != new.vcs || old.local_history != new.local_history
        }
        SettingsPage::DiffMerge => old.diff_merge != new.diff_merge,
        SettingsPage::TrustedLocations => {
            old.trusted_locations != new.trusted_locations
                || old.trusted_projects != new.trusted_projects
        }
        SettingsPage::PythonConsole => old.python_console != new.python_console,
        SettingsPage::Scientific => old.scientific != new.scientific,
        SettingsPage::SharedIndexes => old.shared_indexes != new.shared_indexes,
        SettingsPage::Jupyter => old.jupyter != new.jupyter,
        SettingsPage::DataFiles => old.data_files != new.data_files,
        SettingsPage::Coverage => old.coverage != new.coverage,
        SettingsPage::Profiler => old.profiler != new.profiler,
        SettingsPage::Concurrency => old.concurrency != new.concurrency,
        SettingsPage::Frameworks => false,
        SettingsPage::GitHub => old.github != new.github,
        SettingsPage::Debugger => old.debugger != new.debugger,
        SettingsPage::TextMateBundles => old.textmate_bundles != new.textmate_bundles,
        SettingsPage::Plugins => old.plugin_catalog_url != new.plugin_catalog_url,
        SettingsPage::HttpProxy => old.http_proxy != new.http_proxy,
        SettingsPage::Passwords => old.passwords != new.passwords,
        SettingsPage::ServerCertificates => old.certificates != new.certificates,
        SettingsPage::Updates => super::updates_ui::page_modified(old, new),
        SettingsPage::DiagnosticTools => {
            old.diagnostics.profiler_program != new.diagnostics.profiler_program
                || old.diagnostics.debugger_program != new.diagnostics.debugger_program
                || old.diagnostics.profile_seconds != new.diagnostics.profile_seconds
        }
        SettingsPage::DataSharing => {
            old.diagnostics.collect_usage_statistics != new.diagnostics.collect_usage_statistics
        }
        SettingsPage::PathVariables => old.path_variables != new.path_variables,
        SettingsPage::WindowChrome => {
            old.chrome != new.chrome
                || old.native_ui != new.native_ui
                || old.appearance.show_navigation_bar != new.appearance.show_navigation_bar
                || old.appearance.show_memory_indicator != new.appearance.show_memory_indicator
        }
        SettingsPage::Notifications => old.notifications != new.notifications,
        SettingsPage::StartupTasks
        | SettingsPage::LiveTemplates
        | SettingsPage::Interpreter
        | SettingsPage::ProjectStructure
        | SettingsPage::FileTemplates => false,
    }
}

/// Quick Switch Scheme (Ctrl+`) popup.
#[derive(Default)]
pub(super) struct QuickSwitchState {
    pub filter: String,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum QuickItem {
    /// A UI theme by name ("Dark", "Light", "High Contrast").
    Theme(&'static str),
    Scheme(String),
}

/// UI themes: `(appearance.theme, label)`.
const THEMES: &[(&str, &str)] = &[
    ("Dark", "Dark"),
    ("Light", "Light"),
    ("High Contrast", "High Contrast"),
];

/// The editor color scheme to use after switching to a dark (`dark`) or light theme:
/// the current one when it already fits, else the one last used with that theme,
/// else the builtin default.
pub(super) fn scheme_for_theme(
    dark: bool,
    current: &str,
    current_is_dark: bool,
    remembered: &str,
    is_dark: impl Fn(&str) -> Option<bool>,
) -> String {
    if current_is_dark == dark {
        return current.to_string();
    }
    if !remembered.is_empty() && is_dark(remembered) == Some(dark) {
        return remembered.to_string();
    }
    if dark { "Dark" } else { "Light" }.to_string()
}

/// Builtin scheme, a user scheme saved under `<config>/colors/<name>.json`,
/// or a scheme contributed by an enabled plugin.
pub(super) fn scheme_by_name(name: &str) -> Option<ColorScheme> {
    if let Some(s) = ColorScheme::by_name(name) {
        return Some(s);
    }
    let path = Settings::config_dir()
        .join("colors")
        .join(format!("{name}.json"));
    let user = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    user.or_else(|| crate::plugin_host::color_scheme(name))
}

/// Scheme names after the builtin ones: user schemes, then plugin schemes.
fn custom_scheme_names() -> Vec<String> {
    crate::plugin_host::merge_scheme_names(
        user_scheme_names(),
        &crate::plugin_host::contributions(),
    )
}

fn user_scheme_names() -> Vec<String> {
    let dir = Settings::config_dir().join("colors");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn tag_label(tag: HighlightTag) -> String {
    let s = format!("{tag:?}");
    // CamelCase → words
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

const KNOWN_INTENTIONS: &[&str] = &[
    "Insert documentation string stub",
    "Insert type placeholders in the documentation comment stub",
    "Optimize imports",
    "Invert 'if' condition",
    "Split 'if'",
    "Merge nested 'if's",
    "Flip '=='",
    "Negate comparison",
    "Remove redundant parentheses",
    "Convert to f-string",
    "Convert to single quotes",
    "Convert to double quotes",
    "Convert to triple-quoted string",
    "Convert dict() call to dict literal",
    "Convert to list comprehension",
    "Convert comprehension to for loop",
    "Convert lambda to function",
    "Convert method to property",
    "Make method static",
    "Add 'self' parameter",
    "Convert to 'return expr'",
    "Convert to conditional expression",
    "Convert to 'import'",
    "Convert to 'from import'",
    "Move import to top",
];

const PREVIEW: &str = r#"import os
from dataclasses import dataclass


@dataclass
class Point:
    """A point."""
    x: float = 0.0

    def norm(self) -> float:
        return abs(self.x)  # comment


def main(name: str = "world") -> None:
    total = sum(range(10)) + 0x1F
    print(f"Hello, {name}! {total}")
"#;

impl IdeApp {
    /// Settings is modal: IDE shortcuts do not run under it.
    pub(super) fn ide_shortcuts_blocked(&self) -> bool {
        if self.bg.problems.infrastructure.dialog {
            return true;
        }
        matches!(self.dialog, Dialog::Settings)
    }

    /// While Settings › Keymap records a shortcut (Change…) or looks one up
    /// (Find by shortcut), every key press belongs to it, before global
    /// shortcuts or any field see it: a chord bound to an IDE action is
    /// recorded instead of run, and Esc ends only the recording. Returns
    /// whether the recorder was armed.
    pub(super) fn capture_shortcut_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.capture_extended_shortcut(ctx) {
            return true;
        }
        if !matches!(self.dialog, Dialog::Settings) || !self.settings_ui.capturing_keys() {
            return false;
        }
        let recording = self.settings_ui.recording.is_some();
        for (key, mods) in ctx.input_mut(take_recorder_presses) {
            if self.settings_ui.record_key(&mut self.keymap, key, mods) {
                if recording {
                    self.store_custom_shortcuts();
                }
                break;
            }
        }
        true
    }

    /// Recording, looking up and conflict prompts end with the dialog.
    fn end_shortcut_capture(&mut self) {
        self.settings_ui.recording = None;
        self.settings_ui.find_by_shortcut = false;
        self.settings_ui.keymap_conflict = None;
    }

    /// Rebuilds the keymap: the configured keymap (`settings.keymap`, a
    /// bundled preset or a plugin's keymap) with `settings.custom_shortcuts`
    /// on top. Runs on the first frame and whenever the keymap or the plugins change.
    pub(super) fn apply_custom_shortcuts(&mut self) {
        self.reset_keymap_input();
        self.keymap = crate::plugin_host::keymap(self.keymap_parent());
        self.keymap.name = self.settings.keymap_name().to_owned();
        self.keymap.apply_overrides(&self.settings.custom_shortcuts);
    }

    /// Mirrors the keymap's overrides into the settings (saved by Apply / OK).
    fn store_custom_shortcuts(&mut self) {
        self.settings.custom_shortcuts = custom_shortcuts_of(&self.keymap);
    }

    pub(super) fn apply_ui_font_size(&mut self, ctx: &egui::Context) {
        self.theme.apply(ctx);
        let styles = scaled_text_styles(self.settings.appearance.ui_font_size);
        ctx.all_styles_mut(|s| s.text_styles = styles.clone());
    }

    /// Settings to write to disk: while the Settings dialog has edits that were not
    /// applied, the state from before them.
    pub(super) fn settings_to_persist(&self) -> &Settings {
        self.settings_ui
            .snapshot
            .as_ref()
            .map(|s| &s.settings)
            .unwrap_or(&self.settings)
    }

    fn take_settings_snapshot(&mut self) {
        self.load_file_watchers();
        self.settings_ui.snapshot = Some(Box::new(SettingsSnapshot {
            tool_bar_options: (
                self.layout.tools.saved.side_by_side,
                self.layout.tools.saved.group_tabs,
            ),
            settings: self.settings.clone(),
            project_watchers: self.watcher_runtime.project.clone(),
            watcher_root: self.project.as_ref().map(|p| p.root.clone()),
            theme: self.theme.clone(),
            scheme: self.scheme.clone(),
            keymap: self.keymap.clone(),
            frame: self.frame_count,
        }));
    }

    /// Cancel: puts back everything the dialog changed in place.
    pub(super) fn restore_settings_snapshot(&mut self, ctx: &egui::Context) {
        self.end_shortcut_capture();
        let Some(snap) = self.settings_ui.snapshot.take() else {
            return;
        };
        if self.project.as_ref().map(|p| p.root.clone()) == snap.watcher_root {
            self.watcher_runtime.project = snap.project_watchers;
            self.layout.tools.saved.side_by_side = snap.tool_bar_options.0;
            self.layout.tools.saved.group_tabs = snap.tool_bar_options.1;
        }
        self.settings = snap.settings;
        self.scheme = snap.scheme;
        self.keymap = snap.keymap;
        self.theme = snap.theme;
        self.apply_ui_font_size(ctx);
        self.terminal_panel.shell =
            Some(self.settings.terminal.shell.clone()).filter(|s| !s.trim().is_empty());
        for t in &mut self.tabs {
            // Inlay hint and inspection switches may have changed back.
            t.state.semantic_version = None;
        }
    }

    /// Switches the UI theme. The editor scheme follows only when it does not fit
    /// the new theme: then the scheme last used with that theme comes back (a
    /// custom one too), else the builtin Dark / Light. Does not save.
    pub(super) fn switch_theme(&mut self, dark: bool, ctx: &egui::Context) {
        let current = self.scheme.name.clone();
        let leaving_high_contrast = self.theme.high_contrast;
        if self.scheme.dark == self.theme.dark && !leaving_high_contrast {
            if self.theme.dark {
                self.settings.appearance.dark_scheme = current.clone();
            } else {
                self.settings.appearance.light_scheme = current.clone();
            }
        }
        self.theme = if dark {
            UiTheme::dark()
        } else {
            UiTheme::light()
        };
        self.settings.appearance.theme = if dark { "Dark" } else { "Light" }.into();
        let remembered = if dark {
            self.settings.appearance.dark_scheme.clone()
        } else {
            self.settings.appearance.light_scheme.clone()
        };
        // The High Contrast scheme leaves with the High Contrast theme.
        let current_fits = if leaving_high_contrast && current == "High Contrast" {
            !dark
        } else {
            self.scheme.dark
        };
        let name = scheme_for_theme(dark, &current, current_fits, &remembered, |n| {
            scheme_by_name(n).map(|s| s.dark)
        });
        if name != current {
            if let Some(s) = scheme_by_name(&name) {
                self.scheme = s;
            }
        }
        self.settings.editor.color_scheme = self.scheme.name.clone();
        self.apply_ui_font_size(ctx);
    }

    /// Selects editor scheme `name`, switching the UI theme too when the scheme is
    /// for the other one (a light scheme in the dark theme). Does not save.
    fn set_editor_scheme(&mut self, name: &str, ctx: &egui::Context) {
        let Some(s) = scheme_by_name(name) else {
            return;
        };
        if s.dark != self.theme.dark
            && !self
                .settings
                .appearance
                .theme
                .starts_with(rustpy_settings::ui_themes::PREFIX)
        {
            self.switch_theme(s.dark, ctx);
        }
        if s.dark {
            self.settings.appearance.dark_scheme = name.to_string();
        } else {
            self.settings.appearance.light_scheme = name.to_string();
        }
        self.scheme = s;
        self.settings.editor.color_scheme = name.to_string();
    }

    /// Quick Switch Scheme: theme and editor color scheme in one popup (filter,
    /// arrows, Enter; Esc closes). The choice applies and is saved at once.
    pub(super) fn quick_switch_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<QuickSwitchState>,
    ) {
        let current_theme = self.theme.name();
        let mut items: Vec<(&str, String, QuickItem, bool)> = THEMES
            .iter()
            .map(|(name, label)| {
                (
                    "Theme",
                    label.to_string(),
                    QuickItem::Theme(name),
                    current_theme == *name,
                )
            })
            .collect();
        let mut names: Vec<String> = ColorScheme::builtin_names()
            .iter()
            .map(|s| s.to_string())
            .collect();
        names.extend(custom_scheme_names());
        for n in names {
            let current = n == self.scheme.name;
            items.push((
                "Editor Color Scheme",
                n.clone(),
                QuickItem::Scheme(n),
                current,
            ));
        }
        let q = st.filter.trim().to_lowercase();
        items.retain(|(section, label, _, _)| {
            q.is_empty() || label.to_lowercase().contains(&q) || section.to_lowercase().contains(&q)
        });
        st.selected = st.selected.min(items.len().saturating_sub(1));
        let theme = self.theme.clone();
        let mut open = true;
        let mut chosen: Option<usize> = None;
        egui::Window::new("Quick Switch Scheme")
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 120.0))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut st.filter)
                        .hint_text("Type to filter")
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.changed() {
                    st.selected = 0;
                }
                let (up, down, enter, esc) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::ArrowUp),
                        i.key_pressed(egui::Key::ArrowDown),
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                if down && st.selected + 1 < items.len() {
                    st.selected += 1;
                }
                if up {
                    st.selected = st.selected.saturating_sub(1);
                }
                if enter && !items.is_empty() {
                    chosen = Some(st.selected);
                }
                if esc {
                    chosen = Some(usize::MAX);
                }
                let mut section = "";
                for (i, (sec, label, _, current)) in items.iter().enumerate() {
                    if *sec != section {
                        section = sec;
                        ui.label(egui::RichText::new(*sec).color(theme.text_dim).small());
                    }
                    let text = if *current {
                        format!("• {label}")
                    } else {
                        format!("   {label}")
                    };
                    if ui.selectable_label(i == st.selected, text).clicked() {
                        chosen = Some(i);
                    }
                }
            });
        match chosen {
            Some(i) if i < items.len() => {
                match items[i].2.clone() {
                    QuickItem::Theme(name) => self.set_ui_theme(name, ctx),
                    QuickItem::Scheme(name) => self.set_editor_scheme(&name, ctx),
                }
                let _ = self.settings.save();
                self.focus_editor = true;
            }
            Some(_) => self.focus_editor = true,
            None if open => self.dialog = Dialog::QuickSwitch(st),
            None => self.focus_editor = true,
        }
    }

    /// Reports a `settings.json` that could not be loaded (it was backed up), once,
    /// as an error notification that stays until it is closed.
    pub(super) fn startup_notices(&mut self) {
        self.sync_notification_groups();
        if let Some(err) = Settings::take_load_error() {
            let actions = err
                .backup
                .iter()
                .map(|b| {
                    (
                        "Open the Saved Copy".to_string(),
                        crate::notifications::NotificationAction::OpenFile(b.clone(), None),
                    )
                })
                .collect();
            self.notify(
                crate::notifications::NotificationKind::Error,
                "Settings",
                "Settings not loaded",
                err.to_string(),
                actions,
            );
            if let Some(id) = self.notifications.items().last().map(|n| n.id) {
                self.notifications.set_sticky(id);
            }
        }
    }

    pub(super) fn notice_dialog(
        &mut self,
        ctx: &egui::Context,
        title: String,
        text: String,
        then: Option<Box<Dialog>>,
    ) {
        let mut open = true;
        let mut ok = false;
        let size = self.dialog_workspace.size("notice", [520., 280.]);
        let response = egui::Window::new(title.as_str())
            .collapsible(false)
            .resizable(true)
            .default_size(size)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(&text);
                ui.add_space(6.0);
                if ui.button("OK").clicked()
                    || ui.input(|i| {
                        i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)
                    })
                {
                    ok = true;
                }
            });
        if ok || !open {
            if let Some(response) = response {
                self.dialog_workspace
                    .remember_size("notice", response.response.rect.size().into());
            }
            self.dialog = then.map(|d| *d).unwrap_or(Dialog::None);
            self.focus_editor = true;
        } else {
            self.dialog = Dialog::Notice { title, text, then };
        }
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        // A dialog that was closed some other way (another dialog replaced it) counts as cancelled.
        if self
            .settings_ui
            .snapshot
            .as_ref()
            .is_some_and(|s| s.frame + 1 < self.frame_count)
        {
            self.restore_settings_snapshot(ctx);
        }
        // Opening: the search field takes the keyboard, nothing is being recorded.
        let opened = self.settings_ui.snapshot.is_none();
        if opened {
            self.settings_ui.ui_theme_entries = None;
            self.take_settings_snapshot();
            self.settings_ui.message = None;
            self.end_shortcut_capture();
        }
        let frame = self.frame_count;
        let modified: Vec<SettingsPage> = match &self.settings_ui.snapshot {
            Some(s) => TREE
                .iter()
                .flat_map(|(_, pages)| pages.iter().map(|(_, p)| *p))
                .filter(|p| {
                    (*p == SettingsPage::FileWatchers
                        && s.project_watchers != self.watcher_runtime.project)
                        || (*p == SettingsPage::Appearance
                            && (s.tool_bar_options
                                != (
                                    self.layout.tools.saved.side_by_side,
                                    self.layout.tools.saved.group_tabs,
                                )
                                || s.settings.tool_window_layouts
                                    != self.settings.tool_window_layouts))
                        || page_modified(*p, &s.settings, &self.settings)
                        || (*p == SettingsPage::ColorScheme && s.scheme != self.scheme)
                })
                .collect(),
            None => Vec::new(),
        };
        if let Some(s) = &mut self.settings_ui.snapshot {
            s.frame = frame;
        }
        let mut open = true;
        let theme = self.theme.clone();
        let (mut ok, mut apply, mut cancel) = (false, false, false);
        let size = self.dialog_workspace.size("settings", [900., 600.]);
        let response=egui::Window::new(tr_static("Settings")).id(egui::Id::new("Settings")).collapsible(false).resizable(true).default_size(size).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.horizontal(|ui| {
                let search = ui.add(egui::TextEdit::singleline(&mut self.settings_ui.search).hint_text(format!("🔍 {}", tr_static("Search settings"))).desired_width(220.0));
                if opened {
                    search.request_focus();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(self.settings_ui.page.title()).strong());
                });
            });
            ui.separator();
            egui::Panel::left("settings_tree").resizable(true).default_size(230.0).min_size(160.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("settings_tree_scroll").show(ui, |ui| {
                    let filter = self.settings_ui.search.trim().to_lowercase();
                    for (group, pages) in TREE {
                        if !filter.is_empty()&&self.dialog_workspace.prefs.search_sections.contains(*group){continue;}
                        let visible: Vec<&(&str, SettingsPage)> = pages.iter().filter(|(name, page)| crate::i18n::matches_filter(name, &filter) || crate::i18n::matches_filter(group, &filter)||self.settings_option_matches(*page,&filter)).collect();
                        if visible.is_empty() {
                            continue;
                        }
                        // Pages with changes that are not applied yet are marked (blue, with a dot).
                        let current = self.settings_ui.page;
                        let label = |name: &'static str, page: SettingsPage, strong: bool| {
                            let name = tr_static(name);
                            let mut t = match (modified.contains(&page), page == current) {
                                (true, false) => egui::RichText::new(format!("{name} •")).color(theme.accent),
                                (true, true) => egui::RichText::new(format!("{name} •")),
                                _ => egui::RichText::new(name),
                            };
                            if strong {
                                t = t.strong();
                            }
                            if !filter.is_empty(){t=t.background_color(theme.accent.gamma_multiply(0.15));}
                            t
                        };
                        if pages.len() == 1 && pages[0].0 == *group {
                            if ui.selectable_label(self.settings_ui.page == pages[0].1, label(group, pages[0].1, true)).clicked() {
                                self.settings_ui.page = pages[0].1;
                            }
                            continue;
                        }
                        ui.label(egui::RichText::new(tr_static(group)).strong().color(theme.text));
                        let mut parent: Option<&str> = None;
                        for (name, page) in visible {
                            let (node, leaf) = match name.split_once('/') {
                                Some((n, l)) => (Some(n), l),
                                None => (None, *name),
                            };
                            if let Some(n) = node.filter(|n| Some(*n) != parent) {
                                ui.horizontal(|ui| {
                                    // Lines up with the selectable page labels.
                                    ui.add_space(14.0 + ui.spacing().button_padding.x);
                                    ui.label(egui::RichText::new(n).color(theme.text));
                                });
                            }
                            parent = node;
                            ui.horizontal(|ui| {
                                ui.add_space(if node.is_some() { 28.0 } else { 14.0 });
                                if ui.selectable_label(self.settings_ui.page == *page, label(leaf, *page, false)).clicked() {
                                    self.settings_ui.page = *page;
                                }
                            });
                        }
                    }
                });
            });
            egui::Panel::bottom("settings_buttons").show_separator_line(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let buttons = settings_footer(ui, self.settings_ui.message.as_deref(), !modified.is_empty(), theme.text_dim);
                    ok |= buttons[0].clicked();
                    apply |= buttons[1].clicked();
                    cancel |= buttons[2].clicked();
                });
            });
            egui::ScrollArea::vertical().id_salt("settings_page_scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                self.settings_page_shell(ui,ctx);
                match self.settings_ui.page {
                    SettingsPage::Appearance => self.page_appearance(ui, ctx),
                    SettingsPage::Keymap => self.page_keymap(ui),
                    SettingsPage::Scopes => self.scope_settings_page(ui),
                    SettingsPage::EditorAppearance | SettingsPage::AutoImport | SettingsPage::Breadcrumbs | SettingsPage::SmartKeys | SettingsPage::EditorConsole | SettingsPage::EditorTabs | SettingsPage::GutterIcons | SettingsPage::PostfixCompletion | SettingsPage::FileEncodings | SettingsPage::FileTypes | SettingsPage::Copyright | SettingsPage::Duplicates | SettingsPage::Images | SettingsPage::LanguageInjections | SettingsPage::CodeVision | SettingsPage::StickyLines | SettingsPage::Emmet => self.editor_preferences_page(ui, self.settings_ui.page),
                    SettingsPage::EditorCodeEditing => self.editor_preferences_page(ui,SettingsPage::SmartKeys),
                    SettingsPage::EditorFolding => self.editor_folding_preferences(ui),
                    SettingsPage::SchemeGeneral => self.editor_color_section(ui,"General"),
                    SettingsPage::SchemeDefaults => self.editor_color_section(ui,"Language Defaults"),
                    SettingsPage::SchemeConsole => self.editor_color_section(ui,"Console Font & Colors"),
                    SettingsPage::SchemeDebugger => self.editor_color_section(ui,"Debugger"),
                    SettingsPage::SchemeDiff => self.editor_color_section(ui,"Diff & Merge"),
                    SettingsPage::SchemeFileTypes => self.editor_color_section(ui,"User-Defined File Types"),
                    SettingsPage::SchemeVcs => self.editor_color_section(ui,"Version Control"),
                    SettingsPage::EditorGeneral => self.page_editor_general(ui),
                    SettingsPage::EditorFont => self.page_editor_font(ui, ctx),
                    SettingsPage::ColorScheme => self.page_color_scheme(ui),
                    SettingsPage::CodeStyle => self.page_code_style(ui),
                    SettingsPage::InlayHints => self.page_inlay_hints(ui),
                    SettingsPage::ReaderMode => self.page_reader_mode(ui),
                    SettingsPage::CodeCompletion => {
                        let c = &mut self.settings.completion;
                        ui.heading("Code Completion");
                        if c.case_sensitive { c.match_case = rustpy_settings::CompletionCaseMatch::All; c.case_sensitive = false; }
                        ui.checkbox(&mut c.show_as_you_type, "Show suggestions as you type");
                        egui::ComboBox::from_label("Match case").selected_text(format!("{:?}", c.match_case)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut c.match_case, rustpy_settings::CompletionCaseMatch::None, "None");
                            ui.selectable_value(&mut c.match_case, rustpy_settings::CompletionCaseMatch::FirstLetter, "First letter");
                            ui.selectable_value(&mut c.match_case, rustpy_settings::CompletionCaseMatch::All, "All letters");
                        });
                        ui.checkbox(&mut c.sort_lexicographically, "Sort lexicographically instead of relevance");
                        ui.checkbox(&mut c.show_documentation, "Show documentation beside completion");
                        ui.checkbox(&mut c.show_full_signatures, "Show full signatures");
                        ui.checkbox(&mut c.preselect_first, "Preselect the first suggestion");
                        ui.checkbox(&mut c.show_parameter_info, "Show the parameter info popup");
                        ui.checkbox(&mut c.insert_by_context_characters, "Insert selected suggestion by pressing space, dot or other context-dependent keys");
                        ui.horizontal(|ui| { ui.label("Autopopup code completion in (ms)"); ui.add(egui::DragValue::new(&mut c.auto_popup_delay_ms).range(0..=10000)); });
                        ui.horizontal(|ui| { ui.label("Lookup item height"); ui.add(egui::DragValue::new(&mut c.lookup_item_height).range(16.0..=48.0)); });
                        ui.checkbox(&mut c.match_middle, "Match middle of words and CamelHumps (aT → assertTrue)");
                        ui.checkbox(&mut c.hide_used, "Hide suggestions already typed");
                        ui.checkbox(&mut c.insert_parentheses, "Insert parentheses when completing callables");
                        ui.label(egui::RichText::new("Ctrl+Space: basic completion (press twice for names from other modules with auto-import). Ctrl+Shift+Space: smart type completion.").color(self.theme.text_dim).small());
                    }
                    SettingsPage::Intentions => self.intention_preferences_page(ui, KNOWN_INTENTIONS),
                    SettingsPage::Refactoring => {
                        let r = &mut self.settings.refactoring;
                        ui.heading("Refactorings");
                        ui.checkbox(&mut r.rename_in_place, "Enable in-place mode for Rename (multi-caret editing when all usages are in the current file)");
                        ui.checkbox(&mut r.rename_variables_in_place, "Rename variables in place");
                        ui.checkbox(&mut r.preview_refactoring, "Preview refactoring before applying changes");
                        ui.checkbox(&mut r.search_in_comments, "Search in comments and strings when renaming");
                        ui.checkbox(&mut r.prefer_from_imports, "Prefer `from module import name` when moving symbols");
                        ui.checkbox(&mut r.confirm_undo, "Confirm undo of refactorings");
                    }
                    SettingsPage::ExternalTools => {
                        ui.heading("External Tools");
                        ui.label(egui::RichText::new("Arguments are split at spaces (\"…\" or '…' keep spaces) before macros expand, so paths with spaces need no quotes. The program runs directly, not through a shell. Tools appear in Tools › External Tools, in the editor and Project view context menus (by group) and in Find Action; a shortcut runs a tool from anywhere.").color(self.theme.text_dim).small());
                        ui.collapsing("Macros", |ui| {
                            egui::Grid::new("ext_tool_macros").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                                for (name, what) in crate::external_tools::MACROS {
                                    ui.label(egui::RichText::new(format!("${name}$")).monospace());
                                    ui.label(egui::RichText::new(*what).color(self.theme.text_dim));
                                    ui.end_row();
                                }
                            });
                        });
                        if ui.button("+ Add tool").clicked() {
                            self.settings.external_tools.push(rustpy_settings::ExternalTool { name: format!("Tool {}", self.settings.external_tools.len() + 1), group: "External Tools".into(), program: "echo".into(), arguments: "$FilePath$".into(), working_dir: "$ProjectFileDir$".into(), show_console: true, shortcut: String::new(), ..Default::default() });
                        }
                        let mut remove: Option<usize> = None;
                        for (i, t) in self.settings.external_tools.iter_mut().enumerate() {
                            egui::Frame::new().fill(self.theme.hover_bg).inner_margin(egui::Margin::same(6)).corner_radius(4).show(ui, |ui| {
                                egui::Grid::new(("ext_tool", i)).num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                                    ui.label("Name");
                                    ui.text_edit_singleline(&mut t.name);
                                    ui.end_row();
                                    ui.label("Group");
                                    ui.add(egui::TextEdit::singleline(&mut t.group).hint_text("none: listed directly"));
                                    ui.end_row();
                                    ui.label("Program");
                                    ui.text_edit_singleline(&mut t.program);
                                    ui.end_row();
                                    ui.label("Arguments");
                                    ui.text_edit_singleline(&mut t.arguments);
                                    ui.end_row();
                                    ui.label("Working directory");
                                    ui.text_edit_singleline(&mut t.working_dir);
                                    ui.end_row();
                                    ui.label("Shortcut");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut t.shortcut).desired_width(140.0).hint_text("e.g. Ctrl+Alt+Shift+B"));
                                        if let Some(problem) = crate::external_tools::shortcut_problem(&t.shortcut, &self.keymap) {
                                            ui.label(egui::RichText::new(problem).color(self.theme.warning).small());
                                        }
                                    });
                                    ui.end_row();
                                });
                                ui.push_id(("external-tool-options",i),|ui|crate::external_tool_settings::options(ui,t));
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut t.show_console, "Open console");
                                    if ui.button("Remove").clicked() {
                                        remove = Some(i);
                                    }
                                });
                            });
                        }
                        if let Some(i) = remove {
                            self.settings.external_tools.remove(i);
                        }
                        self.plugin_external_tools_list(ui);
                    }
                    SettingsPage::Markdown => self.markdown_settings_page(ui),
                    SettingsPage::FileWatchers => {
                        self.file_watchers_settings(ui);
                    }
                    SettingsPage::StartupTasks => {
                        ui.heading("Startup Tasks");
                        ui.label(egui::RichText::new("Run configurations started when this project opens.").color(self.theme.text_dim).small());
                        if self.project.is_some() {
                            let ws = self.effective_workspace();
                            let names: Vec<String> = self.project.as_ref().map(|p| p.run_configs.configs.iter().map(|c| c.name.clone()).collect()).unwrap_or_default();
                            let mut changes = Vec::new();
                            for n in names {
                                let mut on = ws.startup_tasks.contains(&n);
                                if ui.checkbox(&mut on, &n).changed() {
                                    changes.push((n, on));
                                }
                            }
                            for (name, selected) in changes { self.set_startup_task(&name, selected); }
                        } else {
                            ui.label("Open a project first.");
                        }
                    }
                    SettingsPage::AiAssistant => {
                        let theme = self.theme.clone();
                        let ai = &mut self.settings.ai;
                        ui.horizontal(|ui| {
                            ui.label("Provider:");
                            ui.selectable_value(&mut ai.provider,"anthropic".into(),"Anthropic");
                            ui.selectable_value(&mut ai.provider,"openai-compatible".into(),"OpenAI-compatible Chat Completions");
                        });
                        if ai.provider == "openai-compatible" {
                            ui.label("Configure a model supported by your provider. Source context is sent only when you invoke an AI action. Keys stay in the password safe; OPENAI_API_KEY is the fallback.");
                            ui.horizontal(|ui| { ui.label("API endpoint base:"); ui.text_edit_singleline(&mut ai.openai_base_url); });
                            ui.horizontal(|ui| { ui.label("Model ID:"); ui.text_edit_singleline(&mut ai.openai_model); });
                            ui.horizontal(|ui| { ui.label("API key:"); ui.add(egui::TextEdit::singleline(&mut ai.openai_api_key).password(true)); ui.checkbox(&mut ai.remember_openai_api_key,"Remember"); });
                            ui.horizontal(|ui| {ui.label("Max completion tokens (0 = provider default):");ui.add(egui::DragValue::new(&mut ai.max_tokens).range(0..=128_000));});
                        } else {
                        ui.label(egui::RichText::new("Anthropic Messages API. A key entered here is kept in the password safe (Appearance & Behavior › System Settings › Passwords), never in settings.json; without one, the ANTHROPIC_API_KEY environment variable is used.").color(theme.text_dim));
                        ui.horizontal(|ui| {
                            ui.label("API key:");
                            ui.add(egui::TextEdit::singleline(&mut ai.api_key).password(true).hint_text(if std::env::var("ANTHROPIC_API_KEY").is_ok() { "using ANTHROPIC_API_KEY" } else { "sk-ant-…" }).desired_width(320.0));
                            ui.checkbox(&mut ai.remember_api_key, "Remember").on_hover_text("Keep the key in the password safe; otherwise it is forgotten when RustPy exits");
                        });
                        ui.horizontal(|ui| {
                            let model_label = ui.label("Model:");
                            let current = if ai.model.is_empty() { rustpy_ai::DEFAULT_MODEL.to_string() } else { ai.model.clone() };
                            egui::ComboBox::from_id_salt("ai_model").selected_text(rustpy_ai::model_label(&current).to_string()).show_ui(ui, |ui| {
                                for m in rustpy_ai::KNOWN_MODELS {
                                    if ui.selectable_label(current == m.id, format!("{} ({})", m.label, m.id)).clicked() {
                                        ai.model = m.id.to_string();
                                    }
                                }
                            }).combo_labelled(&model_label);
                            ui.add(egui::TextEdit::singleline(&mut ai.model).hint_text("or a custom model id").desired_width(200.0));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Max output tokens (0 = default):");
                            ui.add(egui::DragValue::new(&mut ai.max_tokens).range(0..=128_000).speed(256));
                        });
                        ui.horizontal(|ui| {
                            ui.label("API base URL:");
                            ui.add(egui::TextEdit::singleline(&mut ai.base_url).hint_text(rustpy_ai::DEFAULT_BASE_URL).desired_width(320.0));
                        });
                        ui.checkbox(&mut ai.refusal_fallback, "Server-side refusal fallback (beta header; retries a declined request on a fallback model)");
                        }
                    }
                    SettingsPage::WebDevelopment => self.web_development_settings(ui),
                    SettingsPage::WebBrowsers => self.web_browsers_settings(ui),
                    SettingsPage::ExternalDocumentation => self.page_external_documentation(ui),
                    SettingsPage::PythonTools => {
                        ui.heading("Debugger and code insight");
                        ui.checkbox(&mut self.settings.collect_runtime_types, "Collect run-time types information for code insight").on_hover_text("Runs Python scripts, modules and test configurations under a profiler hook that records argument and return types into .rustpy/runtime_types.json; unannotated code then gets those types in completion, hints and inspections. Slows the program down while enabled.");
                        ui.add_space(8.0);
                        self.documentation_settings_page(ui);
                        ui.add_space(8.0);
                        self.sphinx_settings_page(ui);
                        ui.add_space(8.0);
                        self.testing_settings_page(ui);
                        self.test_generation_settings(ui);
                    }
                    SettingsPage::Accessibility => self.page_accessibility(ui),
                    SettingsPage::MenusToolbars => self.page_menus_toolbars(ui),
                    SettingsPage::Inspections => self.page_inspections(ui),
                    SettingsPage::NaturalLanguages => self.page_natural_languages(ui),
                    SettingsPage::LiveTemplates => self.page_live_templates(ui),
                    SettingsPage::FileTemplates => self.page_file_templates(ui),
                    SettingsPage::TextMateBundles => self.page_textmate_bundles(ui),
                    SettingsPage::Todo => self.todo_settings(ui),
                    SettingsPage::Interpreter => self.page_interpreter(ui),
                    SettingsPage::PythonStubs => self.page_python_stubs(ui),
                    SettingsPage::ProjectStructure => self.page_project_structure(ui),
                    SettingsPage::Notifications => self.page_notifications(ui),
                    SettingsPage::Terminal => self.page_terminal(ui),
                    SettingsPage::VersionControl => {self.page_vcs(ui);self.local_history_settings(ui);},
                    SettingsPage::DiffMerge => self.diff_merge_settings_page(ui),
                    SettingsPage::TrustedLocations => self.page_trusted_locations(ui),
                    SettingsPage::SharedIndexes => self.page_shared_indexes(ui),
                    SettingsPage::Coverage => self.coverage_settings_page(ui),
                    SettingsPage::Profiler => self.profiler_settings_page(ui),
                    SettingsPage::Scientific => self.scientific_settings(ui),
                    SettingsPage::Jupyter=>self.jupyter_settings_page(ui),
                    SettingsPage::DataFiles => self.data_file_settings(ui),
                    SettingsPage::PythonConsole => {
                        let s = &mut self.settings.python_console;
                        ui.checkbox(&mut s.use_ipython, "Use IPython if available");
                        ui.checkbox(&mut s.interactive, "Interactive mode");
                        ui.checkbox(&mut s.add_content_roots, "Add content roots to PYTHONPATH");
                        ui.checkbox(&mut s.add_source_roots, "Add source roots to PYTHONPATH");
                        ui.checkbox(&mut s.use_existing_console, "Use existing console");
                        ui.checkbox(&mut s.show_variables, "Show console variables by default");
                        ui.checkbox(&mut s.command_queue, "Command queue mode");
                        ui.checkbox(&mut s.execute_by_lines, "Execute selection by lines");
                        ui.checkbox(&mut s.soft_wrap, "Soft-wrap output");
                        ui.label("Startup script (applies to new consoles)");
                        ui.add(egui::TextEdit::multiline(&mut s.startup_script).code_editor().desired_rows(8));
                        let mut cwd = s.working_directory.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                        ui.label("Working directory (blank uses project root)");
                        if ui.text_edit_singleline(&mut cwd).changed() { s.working_directory = (!cwd.is_empty()).then(|| PathBuf::from(cwd)); }
                        ui.label("Environment variables (including PATH)");
                        let mut remove = None;
                        for (key, value) in &mut s.environment { ui.horizontal(|ui| { ui.label(key); ui.text_edit_singleline(value); if ui.button("Remove").clicked() { remove = Some(key.clone()); } }); }
                        if let Some(key) = remove { s.environment.remove(&key); }
                        if ui.button("Add environment variable").clicked() { s.environment.entry("NEW_VARIABLE".into()).or_default(); }
                    }
                    SettingsPage::Concurrency => self.page_concurrency(ui),
                    SettingsPage::Frameworks => self.framework_settings_page(ui),
                    SettingsPage::GitHub => self.github_accounts_page(ui),
                    SettingsPage::Debugger => self.page_debugger(ui),
                    SettingsPage::Plugins => self.page_plugins(ui),
                    SettingsPage::HttpProxy => self.page_http_proxy(ui),
                    SettingsPage::Passwords => self.page_passwords(ui),
                    SettingsPage::ServerCertificates => self.page_server_certificates(ui),
                    SettingsPage::Updates => self.page_updates(ui),
                    SettingsPage::DiagnosticTools => self.diagnostics_settings_page(ui),
                    SettingsPage::WindowChrome => self.window_chrome_settings(ui),
                    SettingsPage::DataSharing => self.data_sharing_settings_page(ui),
                    SettingsPage::PathVariables => self.page_path_variables(ui),
                }
            });
        });
        if ok || apply || cancel || !open {
            if let Some(response) = response {
                self.dialog_workspace
                    .remember_size("settings", response.response.rect.size().into());
            }
        }
        if ok || apply {
            if let Err(error) = self.validate_dialog_settings() {
                self.settings_ui.message = Some(error);
                self.dialog = Dialog::Settings;
                return;
            }
            if let Err(error) = self.sync_dialog_project_preferences() {
                self.settings_ui.message = Some(error);
                self.dialog = Dialog::Settings;
                return;
            }
            if let Err(error) = self.save_project_scopes() {
                self.settings_ui.message = Some(error);
                self.dialog = Dialog::Settings;
                return;
            }
            if self
                .settings_ui
                .snapshot
                .as_ref()
                .is_some_and(|s| s.project_watchers != self.watcher_runtime.project)
            {
                if let Err(error) = self.save_project_watchers() {
                    self.settings_ui.message = Some(error.to_string());
                    return;
                }
            }
            if let Some(project) = &self.project {
                if self.settings.code_style_schemes.selected == "Project" {
                    if let Err(error) = rustpy_settings::code_style_schemes::save_project(
                        &project.root,
                        &rustpy_settings::code_style_schemes::Scheme::capture(&self.settings),
                    ) {
                        self.settings_ui.message = Some(error);
                        return;
                    }
                }
            }
            if let Err(error) = self.settings.save() {
                self.settings_ui.message = Some(format!("Cannot save settings: {error}"));
                self.dialog = Dialog::Settings;
                return;
            }
            self.settings_ui.message = Some("Settings saved".into());
            // Proxy, certificates and secrets take effect (secrets go to the password safe).
            if let Some(old) = self
                .settings_ui
                .snapshot
                .as_ref()
                .map(|s| s.settings.clone())
            {
                self.apply_netsec_settings(&old);
                if old.python_stub_roots != self.settings.python_stub_roots {
                    self.apply_interpreter_paths();
                }
                if old.path_variables != self.settings.path_variables {
                    // Paths written with a newly defined variable resolve now.
                    self.reload_project_structure();
                }
            }
            self.settings_ui.ui_theme_entries = None;
            self.take_settings_snapshot();
        }
        if let Some(d) = self.pending_dialog.take() {
            // Another dialog takes over (Add Remote Interpreter…): keep the edits.
            if self
                .settings_ui
                .snapshot
                .as_ref()
                .is_some_and(|s| s.project_watchers != self.watcher_runtime.project)
            {
                if let Err(error) = self.save_project_watchers() {
                    self.settings_ui.message = Some(error.to_string());
                    self.pending_dialog = Some(d);
                    return;
                }
            }
            let _ = self.settings.save();
            if let Some(old) = self
                .settings_ui
                .snapshot
                .as_ref()
                .map(|s| s.settings.clone())
            {
                self.apply_netsec_settings(&old);
                if old.python_stub_roots != self.settings.python_stub_roots {
                    self.apply_interpreter_paths();
                }
            }
            self.settings_ui.snapshot = None;
            self.end_shortcut_capture();
            self.dialog = d;
        } else if ok || cancel || !open {
            if !ok {
                self.restore_settings_snapshot(ctx);
            }
            self.settings_ui.snapshot = None;
            self.end_shortcut_capture();
            self.focus_editor = true;
        } else {
            self.dialog = Dialog::Settings;
        }
    }

    fn page_trusted_locations(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.heading("Trusted Locations");
        ui.label(egui::RichText::new("Projects inside these directories open in normal mode: their run configurations, startup tasks, tools and virtual environments can run. Other projects open in safe mode until you trust them. A project cannot trust itself: trust is stored only in the IDE settings.").color(theme.text_dim));
        ui.add_space(4.0);
        let mut remove: Option<usize> = None;
        for (i, l) in self.settings.trusted_locations.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(l.display().to_string());
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.settings.trusted_locations.remove(i);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.trusted_location_input)
                    .hint_text("/path/to/directory (~ allowed)")
                    .desired_width(360.0),
            );
            if ui.button("Add").clicked() {
                let raw = self.settings_ui.trusted_location_input.trim().to_string();
                let path = match raw
                    .strip_prefix("~/")
                    .or(if raw == "~" { Some("") } else { None })
                {
                    Some(rest) => dirs::home_dir()
                        .map(|h| h.join(rest))
                        .unwrap_or_else(|| PathBuf::from(&raw)),
                    None => PathBuf::from(&raw),
                };
                if !path.is_absolute() {
                    self.settings_ui.message = Some("Enter an absolute directory path".into());
                } else {
                    match rustpy_settings::trust::canonical_directory(&path) {
                        Ok(path) if !self.settings.trusted_locations.contains(&path) => {
                            self.settings.trusted_locations.push(path);
                            self.settings_ui.trusted_location_input.clear();
                        }
                        Ok(_) => {}
                        Err(error) => self.settings_ui.message = Some(error.to_string()),
                    }
                }
            }
        });
        ui.add_space(8.0);
        ui.heading("Trusted Projects");
        if self.settings.trusted_projects.is_empty() {
            ui.label(
                egui::RichText::new(
                    "None yet (File › Trust Project, or Trust Project when a project opens).",
                )
                .color(theme.text_dim),
            );
        }
        let mut forget: Option<usize> = None;
        for (i, p) in self.settings.trusted_projects.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(p.display().to_string());
                if ui
                    .small_button("Remove")
                    .on_hover_text("Open it in safe mode again next time")
                    .clicked()
                {
                    forget = Some(i);
                }
            });
        }
        if let Some(i) = forget {
            self.settings.trusted_projects.remove(i);
        }
    }

    fn custom_ui_themes(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use rustpy_settings::ui_themes;
        let config = Settings::config_dir();
        if self.settings_ui.ui_theme_entries.is_none() {
            self.settings_ui.ui_theme_entries = Some(ui_themes::list(&config));
            if self
                .settings
                .appearance
                .theme
                .starts_with(ui_themes::PREFIX)
            {
                if self.settings_ui.ui_theme_file.is_empty() {
                    if let Ok(path) = ui_themes::path(&config, &self.settings.appearance.theme) {
                        self.settings_ui.ui_theme_file = path.display().to_string();
                    }
                }
                if let Err(e) = ui_themes::load(&config, &self.settings.appearance.theme) {
                    self.settings_ui.message = Some(unavailable_custom_theme_message(
                        &self.settings.appearance.theme,
                        self.theme.name(),
                        &e,
                    ));
                }
            }
        }
        ui.add_space(8.0);
        ui.label(
            "Custom JSON UI themes (menus, tabs and tool windows; editor schemes are separate)",
        );
        let mut select = None;
        let mut refresh = false;
        let mut reload_selected = false;
        for (id, entry) in self.settings_ui.ui_theme_entries.as_ref().unwrap() {
            match entry {
                Ok(theme) => {
                    if ui
                        .selectable_label(
                            self.settings.appearance.theme == *id,
                            format!(
                                "{} ({})",
                                theme.name,
                                id.trim_start_matches(ui_themes::PREFIX)
                            ),
                        )
                        .clicked()
                    {
                        select = Some(id.clone());
                    }
                }
                Err(e) => {
                    ui.colored_label(self.theme.error, e);
                }
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Reload themes").clicked() { refresh = true; reload_selected = true; }
            if ui.button("Create example JSON").clicked() {
                let base = if self.theme.high_contrast { ui_themes::Base::HighContrast } else if self.theme.dark { ui_themes::Base::Dark } else { ui_themes::Base::Light };
                let color = self.theme.accent;
                let sample = ui_themes::Theme { name: "My UI theme".into(), base, colors: [("accent".into(), format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b()))].into() };
                match ui_themes::install(&config, &sample) {
                    Ok(id) => {
                        self.settings_ui.ui_theme_file = ui_themes::path(&config, &id).unwrap().display().to_string();
                        self.settings_ui.message = Some("Example created. Edit the JSON file, then Reload themes and select it.".into());
                        refresh = true;
                    }
                    Err(e) => self.settings_ui.message = Some(e),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("JSON file");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.ui_theme_file)
                    .desired_width(280.0)
                    .hint_text("Path to a UI theme JSON file"),
            );
            if ui.button("Import UI theme").clicked() {
                match ui_themes::import(&config, Path::new(self.settings_ui.ui_theme_file.trim())) {
                    Ok(id) => {
                        select = Some(id);
                        refresh = true;
                    }
                    Err(e) => {
                        self.settings_ui.message = Some(format!("Cannot import UI theme: {e}"))
                    }
                }
            }
        });
        if ui.button("Open JSON in editor").on_hover_text("Open an editor tab, close Settings to edit and save, then return here and Reload themes").clicked() {
            let path = PathBuf::from(self.settings_ui.ui_theme_file.trim());
            if path.is_file() {
                self.open_file(&path);
                self.settings_ui.message = Some("JSON opened in an editor tab. Close Settings to edit and save; then return and Reload themes.".into());
            } else {
                self.settings_ui.message = Some("Enter an existing JSON file path, or Create example JSON first".into());
            }
        }
        if let Ok(path) = ui_themes::path(&config, &self.settings.appearance.theme) {
            ui.label(format!("Selected theme file: {}", path.display()));
        }
        ui.collapsing("JSON format and color keys", |ui| {
            ui.monospace(r##"{"name":"My theme","base":"Dark","colors":{"accent":"#8ab4f8"}}"##);
            ui.label("Base: Dark, Light or High Contrast. Colors inherit the base; use #RRGGBB.");
            ui.label(ui_themes::COLOR_KEYS.join(", "));
        });
        if refresh {
            self.settings_ui.ui_theme_entries = Some(ui_themes::list(&config));
            if reload_selected
                && select.is_none()
                && self
                    .settings
                    .appearance
                    .theme
                    .starts_with(ui_themes::PREFIX)
            {
                // Reload keeps the last valid colors if an edited file is broken.
                select = Some(self.settings.appearance.theme.clone());
            }
        }
        if let Some(id) = select {
            if let Ok(path) = ui_themes::path(&config, &id) {
                self.settings_ui.ui_theme_file = path.display().to_string();
            }
            self.set_ui_theme(&id, ctx);
        }
    }

    fn page_appearance(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.tool_window_bars_settings(ui);
        ui.heading(tr_static("Theme"));
        ui.horizontal(|ui| {
            let current = self.settings.appearance.theme.clone();
            for (name, label) in THEMES {
                let hint = if *name == "High Contrast" { "White on black with strong borders and focus rings; uses the High Contrast editor scheme" } else { label };
                if ui.selectable_label(current == *name, *label).on_hover_text(hint).clicked() && current != *name {
                    self.settings.menus.appearance.sync_with_os=false;
                    self.set_ui_theme(name, ctx);
                }
            }
        });
        self.custom_ui_themes(ui, ctx);
        ui.add_space(8.0);
        crate::i18n::language_section(ui, &mut self.settings.appearance.language);
        self.regional_settings(ui);
        self.appearance_extras_page(ui);
        ui.add_space(8.0);
        ui.heading(tr_static("UI Options"));
        ui.checkbox(&mut self.settings.menus.appearance.smooth_scrolling, "Smooth scrolling in IDE panels")
            .on_hover_text("Animate ordinary IDE scroll areas. Reduced motion takes precedence. Editor scrolling has its own setting.");
        let before = self.settings.appearance.ui_font_size;
        ui.horizontal(|ui| {
            ui.label("UI font size");
            ui.add(egui::Slider::new(
                &mut self.settings.appearance.ui_font_size,
                10.0..=20.0,
            ));
        });
        if (before - self.settings.appearance.ui_font_size).abs() > 0.01 {
            self.apply_ui_font_size(ctx);
        }
        self.ui_font_row(ui);
        let a = &mut self.settings.appearance;
        ui.checkbox(&mut a.show_tool_window_bars, "Show tool window bars");
        ui.checkbox(&mut a.show_status_bar, "Show status bar");
        ui.checkbox(&mut a.show_main_toolbar, "Show main toolbar");
        ui.checkbox(&mut a.show_memory_indicator, "Show memory indicator")
            .on_hover_text("Memory used by RustPy, in the status bar");
        ui.checkbox(&mut a.show_navigation_bar, "Show navigation bar");
        ui.checkbox(&mut a.compact_mode, "Compact mode");
        ui.add_space(8.0);
        ui.heading(tr_static("System Settings"));
        ui.horizontal(|ui| {
            ui.label("Default project directory:");
            let mut directory = self
                .settings
                .default_project_directory
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut directory)
                        .hint_text("Unset: active project parent or home"),
                )
                .changed()
            {
                self.settings.default_project_directory = if directory.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(directory))
                };
            }
            if ui.small_button("Reset").clicked() {
                self.settings.default_project_directory = None;
            }
        });
        ui.small("Machine-local. Missing directories are created only by Create or Clone.");
        if let Some(path) = &self.settings.default_project_directory {
            if let Err(error) = rustpy_settings::project_directory::validate(path) {
                ui.colored_label(egui::Color32::RED, error);
            }
        }

        ui.checkbox(
            &mut self.settings.reopen_last_project,
            "Reopen projects on startup",
        );
        ui.checkbox(
            &mut self.settings.confirm_exit,
            "Confirm before exiting the IDE",
        );
        ui.checkbox(
            &mut self.settings.show_tips_on_startup,
            "Show tips on startup",
        );
        ui.horizontal(|ui| {
            ui.label("Open project in:");
            let cur = self.settings.open_project_in();
            for (label, v) in [
                ("Ask", rustpy_settings::OpenProjectIn::Ask),
                ("This window", rustpy_settings::OpenProjectIn::CurrentWindow),
                ("New window", rustpy_settings::OpenProjectIn::NewWindow),
            ] {
                if ui
                    .selectable_label(cur == v, crate::i18n::tr_static(label))
                    .clicked()
                {
                    self.settings.set_open_project_in(v);
                }
            }
        });
        self.save_synchronization_settings_ui(ui);
    }

    fn page_keymap(&mut self, ui: &mut egui::Ui) {
        self.keymap_workspace_ui(ui);
        let theme = self.theme.clone();
        crate::external_tool_settings::shortcuts(
            ui,
            &mut self.settings.external_tools,
            &self.keymap,
        );
        ui.horizontal(|ui| {
            let keymap_label = ui.label("Keymap:");
            let current = if self.settings.keymap.is_empty() {
                "Default".to_string()
            } else {
                self.settings.keymap.clone()
            };
            let mut pick: Option<String> = None;
            egui::ComboBox::from_id_salt("keymap_preset")
                .selected_text(&current)
                .show_ui(ui, |ui| {
                    for n in crate::plugin_host::keymap_names().into_iter().chain(
                        self.settings
                            .custom_keymaps
                            .iter()
                            .map(|(name, _)| name.clone()),
                    ) {
                        if ui.selectable_label(current == n, &n).clicked() {
                            pick = Some(n);
                        }
                    }
                })
                .combo_labelled(&keymap_label);
            if let Some(n) = pick {
                // The user's own changes carry over to the new preset (saved by Apply / OK).
                self.settings.switch_keymap(&n);
                self.apply_custom_shortcuts();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.keymap_filter)
                    .hint_text("Filter actions")
                    .desired_width(200.0),
            );
            let find = ui
                .selectable_label(self.settings_ui.find_by_shortcut, "⌨ Find by shortcut")
                .on_hover_text("Press a shortcut to list the action bound to it");
            if find.clicked() {
                self.settings_ui.find_by_shortcut = !self.settings_ui.find_by_shortcut;
                self.settings_ui.recording = None;
                // A focused button would also take Space / Enter as a click.
                find.surrender_focus();
            }
            if ui
                .button("Reset to preset")
                .on_hover_text("Drop your own shortcut changes")
                .clicked()
            {
                self.keymap = crate::plugin_host::keymap(self.keymap_parent());
                self.keymap.name = self.settings.keymap_name().to_owned();
                self.settings.custom_shortcuts.clear();
                self.settings_ui.keymap_conflict = None;
            }
        });
        // The keys themselves are taken by `capture_shortcut_keys` at the start of the frame.
        if self.settings_ui.find_by_shortcut {
            ui.colored_label(theme.accent, "Press the shortcut to look up (Esc cancels)…");
        }
        if let Some(chord) = self.settings_ui.find_chord {
            ui.horizontal(|ui| {
                ui.label("Actions with the shortcut");
                ui.label(egui::RichText::new(chord.to_string()).strong());
                if ui.small_button("Show all").clicked() {
                    self.settings_ui.find_chord = None;
                }
            });
        }
        if let Some((action, chord, owner)) = self.settings_ui.keymap_conflict {
            ui.horizontal(|ui| {
                ui.colored_label(
                    theme.warning,
                    format!("{chord} is already assigned to \"{}\".", owner.text()),
                );
                if ui
                    .button(format!("Reassign to \"{}\"", action.text()))
                    .on_hover_text(format!("\"{}\" loses the shortcut", owner.text()))
                    .clicked()
                {
                    self.keymap.set_shortcut(action, chord);
                    self.settings_ui.keymap_conflict = None;
                    self.store_custom_shortcuts();
                }
                if ui
                    .button("Cancel")
                    .on_hover_text("Leave the shortcut with the other action")
                    .clicked()
                {
                    self.settings_ui.keymap_conflict = None;
                }
            });
        }
        if let Some(rec) = self.settings_ui.recording {
            ui.colored_label(
                theme.warning,
                format!(
                    "Press the new shortcut for \"{}\" (Esc cancels, Backspace removes)…",
                    rec.text()
                ),
            );
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Quick List (Ctrl+Alt+Shift+Q):").strong());
            // Keeps a typed ',' (parsed_field.rs): rebuilding the text from the list dropped it.
            let field = crate::parsed_field::singleline(
                ui,
                egui::Id::new("settings_quick_list"),
                &self.settings.quick_list.join(", "),
                |t| {
                    t.hint_text("action ids, comma separated, e.g. ReformatCode, OptimizeImports")
                        .desired_width(420.0)
                },
            );
            if field.changed {
                self.settings.quick_list = field
                    .text
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        });
        ui.horizontal(|ui| {
            if self.settings_ui.keymap_file.is_empty() {
                self.settings_ui.keymap_file = dirs::home_dir()
                    .unwrap_or_default()
                    .join("rustpy-keymap.json")
                    .display()
                    .to_string();
            }
            ui.label("File:");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.keymap_file).desired_width(320.0),
            );
            if ui
                .button("Export")
                .on_hover_text("Write the keymap's name and your changes to the file")
                .clicked()
            {
                let json = if self.settings_ui.keymap_file.ends_with(".xml") {
                    Keymap::export_xml(self.keymap_parent(), &self.settings.custom_shortcuts)
                } else {
                    Keymap::export_json(self.keymap_parent(), &self.settings.custom_shortcuts)
                };
                let path = PathBuf::from(self.settings_ui.keymap_file.trim());
                self.settings_ui.message = Some(match std::fs::write(&path, json) {
                    Ok(()) => format!("Keymap exported to {}", path.display()),
                    Err(e) => format!("Cannot write {}: {e}", path.display()),
                });
            }
            if ui
                .button("Import")
                .on_hover_text(
                    "Select the file's keymap and apply its changes (OK or Apply keeps them)",
                )
                .clicked()
            {
                let path = PathBuf::from(self.settings_ui.keymap_file.trim());
                let result = Keymap::import_file(&path);
                self.settings_ui.message = Some(match result {
                    Ok((preset, overrides, skipped)) => {
                        self.settings.switch_keymap(&preset);
                        self.settings.custom_shortcuts = overrides;
                        self.keymap = Keymap::preset(&preset);
                        self.apply_custom_shortcuts();
                        self.store_custom_shortcuts();
                        let note = match skipped.len() {
                            0 => String::new(),
                            1 => format!(" (skipped: {})", skipped[0]),
                            n => format!(" ({n} entries skipped: {})", skipped.join("; ")),
                        };
                        format!("Imported {} over {preset}{note}", path.display())
                    }
                    Err(e) => format!("Cannot import {}: {e}", path.display()),
                });
            }
        });
        ui.separator();
        let filter = self.settings_ui.keymap_filter.trim().to_lowercase();
        let find_chord = self.settings_ui.find_chord;
        let mut change: Option<Action> = None;
        let mut reset: Option<Action> = None;
        egui::Grid::new("keymap_grid")
            .num_columns(4)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Action").strong());
                ui.label(egui::RichText::new("Shortcut").strong());
                ui.label(egui::RichText::new("Id").strong());
                ui.label("");
                ui.end_row();
                for &a in Action::ALL {
                    let text = a.text();
                    if !filter.is_empty()
                        && !text.to_lowercase().contains(&filter)
                        && !a.english_text().to_lowercase().contains(&filter)
                        && !a.id().to_lowercase().contains(&filter)
                        && !self
                            .keymap
                            .shortcut_definitions(a)
                            .join(" ")
                            .to_lowercase()
                            .contains(&filter)
                    {
                        continue;
                    }
                    ui.label(text);
                    // Changed relative to the selected preset.
                    let custom = !self.keymap.is_default(a);
                    ui.label(
                        egui::RichText::new(self.keymap.shortcut_definitions(a).join("; "))
                            .color(if custom { theme.accent } else { theme.text }),
                    );
                    ui.label(egui::RichText::new(a.id()).color(theme.text_dim).small());
                    ui.horizontal(|ui| {
                        let recording = self.settings_ui.recording == Some(a);
                        let b = ui.selectable_label(recording, "Change…");
                        if b.clicked() {
                            change = Some(a);
                            b.surrender_focus();
                        }
                        if custom
                            && ui
                                .small_button("Reset")
                                .on_hover_text("Back to the preset's shortcuts")
                                .clicked()
                        {
                            reset = Some(a);
                        }
                    });
                    ui.end_row();
                }
            });
        if find_chord.is_some_and(|c| self.keymap.lookup(c).is_none()) {
            ui.label(egui::RichText::new("No action has this shortcut.").color(theme.text_dim));
        }
        if let Some(a) = change {
            self.settings_ui.recording = Some(a);
            self.settings_ui.find_by_shortcut = false;
            self.settings_ui.keymap_conflict = None;
        }
        if let Some(a) = reset {
            // Back to the preset's shortcut (VS Code's, not the default keymap's, under VS Code).
            self.keymap.reset_action(a);
            self.store_custom_shortcuts();
        }
    }

    fn page_editor_general(&mut self, ui: &mut egui::Ui) {
        self.editor_general_additional_options(ui);
        self.navigation_history_options(ui);
        let e = &mut self.settings.editor;
        ui.heading("Error Navigation");
        ui.checkbox(
            &mut e.error_navigation_errors_only,
            "Navigate to errors only",
        );
        ui.heading("Mouse Control");
        ui.checkbox(
            &mut e.mouse_wheel_zoom,
            "Change font size with Ctrl+Mouse Wheel",
        );
        ui.heading("Soft Wraps");
        ui.checkbox(&mut e.soft_wrap, "Soft-wrap these files");
        ui.heading("Caret");
        ui.checkbox(&mut e.caret_blinking, "Caret blinking");
        ui.horizontal(|ui| {
            ui.label("Blink period (ms)");
            ui.add(egui::Slider::new(&mut e.caret_blink_period_ms, 100..=2000));
        });
        let old_shape = if e.block_caret {
            rustpy_settings::CaretShape::Block
        } else {
            e.caret_shape
        };
        let mut shape = old_shape;
        ui.horizontal(|ui| {
            let caret_label = ui.label("Caret shape");
            egui::ComboBox::from_id_salt("caret_shape")
                .selected_text(format!("{shape:?}"))
                .show_ui(ui, |ui| {
                    for choice in [
                        rustpy_settings::CaretShape::Thin,
                        rustpy_settings::CaretShape::Block,
                        rustpy_settings::CaretShape::Underscore,
                    ] {
                        if ui
                            .selectable_value(&mut shape, choice, format!("{choice:?}"))
                            .clicked()
                        {
                            e.caret_shape = choice;
                            e.block_caret = false;
                        }
                    }
                })
                .combo_labelled(&caret_label);
        });
        ui.horizontal(|ui| {
            ui.label("Caret width / underscore thickness (px)");
            ui.add(egui::Slider::new(&mut e.caret_width, 1..=10));
        });
        ui.heading("Scrolling");
        ui.checkbox(&mut e.smooth_scrolling, "Smooth scrolling");
        ui.checkbox(
            &mut e.scroll_beyond_end,
            "Scroll beyond the end of the file",
        );
        ui.heading("Appearance");
        ui.checkbox(&mut e.show_line_numbers, "Show line numbers");
        ui.checkbox(&mut e.relative_line_numbers, "Relative line numbers");
        ui.checkbox(&mut e.show_whitespaces, "Show whitespaces");
        ui.add_enabled_ui(e.show_whitespaces, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut e.show_leading_whitespaces, "Leading");
                ui.checkbox(&mut e.show_inner_whitespaces, "Inner");
                ui.checkbox(&mut e.show_trailing_whitespaces, "Trailing");
            });
        });
        ui.checkbox(&mut e.show_line_separators, "Show line separators");
        ui.checkbox(&mut e.show_method_separators, "Show method separators");
        ui.checkbox(&mut e.show_css_color_preview, "Show CSS color previews");
        ui.checkbox(&mut e.show_indent_guides, "Show indent guides");
        ui.checkbox(&mut e.highlight_current_line, "Highlight current line");
        ui.checkbox(&mut e.show_right_margin, "Show right margin");
        ui.checkbox(&mut e.show_breadcrumbs, "Show breadcrumbs");
        ui.add_enabled_ui(e.show_breadcrumbs, |ui| {
            ui.checkbox(
                &mut e.breadcrumbs_at_top,
                "Show breadcrumbs above the editor",
            );
        });
        ui.checkbox(
            &mut e.allow_caret_after_end_of_line,
            "Allow caret after end of line",
        );
        ui.checkbox(
            &mut e.allow_caret_after_end_of_line_in_block_selection,
            "Allow caret after end of line in column selection",
        );
        ui.checkbox(&mut e.show_sticky_lines, "Show sticky lines");
        ui.checkbox(&mut e.render_doc_comments, "Render documentation comments").on_hover_text("Show docstrings as formatted text. Toggle one with the gutter icon or Toggle Rendered View (Ctrl+Alt+Q); a caret in a docstring shows its source.");
        ui.heading("Documentation");
        quick_documentation_ui::documentation_options(
            ui,
            &mut e.documentation_on_hover,
            &mut e.documentation_delay_ms,
        );
        ui.separator();
        ui.heading("Smart Keys");
        ui.checkbox(&mut e.auto_indent, "Auto-indent on Enter");
        ui.checkbox(&mut e.auto_close_brackets, "Insert pair bracket");
        ui.checkbox(&mut e.auto_close_quotes, "Insert pair quote");
        ui.checkbox(
            &mut e.surround_selection_on_typing,
            "Surround selection on typing quote or brace",
        );
        ui.checkbox(&mut e.smart_home_end, "Smart Home/End");
        ui.checkbox(&mut e.smart_backspace, "Smart Backspace (unindent)");
        ui.checkbox(&mut e.camel_humps_words, "Use CamelHumps words");
        ui.checkbox(
            &mut e.jump_outside_brackets_with_tab,
            "Jump outside closing bracket/quote with Tab",
        );
        ui.horizontal(|ui| {
            ui.label("Reformat on paste");
            for (label, v) in [
                ("Do not reformat", rustpy_settings::ReformatOnPaste::None),
                (
                    "Indent block",
                    rustpy_settings::ReformatOnPaste::IndentBlock,
                ),
                ("Reformat", rustpy_settings::ReformatOnPaste::Reformat),
            ] {
                if ui
                    .selectable_label(e.reformat_on_paste == v, crate::i18n::tr_static(label))
                    .clicked()
                {
                    e.reformat_on_paste = v;
                }
            }
        });
        ui.label(egui::RichText::new("Python").strong());
        ui.checkbox(
            &mut e.insert_type_placeholders_in_docstring_stub,
            "Insert type placeholders in the documentation comment stub",
        );
        ui.checkbox(
            &mut e.insert_rtype_in_docstring_stub,
            "Insert return type (:rtype:) in the documentation comment stub",
        );
        ui.checkbox(
            &mut e.auto_close_triple_quotes,
            "Insert closing triple quotes",
        );
        ui.checkbox(
            &mut e.insert_docstring_stub,
            "Insert documentation comment stub on Enter after \"\"\"",
        );
        ui.checkbox(
            &mut e.insert_self_in_methods,
            "Insert 'self' when defining a method",
        );
        ui.heading("Code Folding");
        ui.checkbox(&mut e.code_folding, "Enable code folding");
        ui.checkbox(
            &mut e.fold_imports_by_default,
            "Collapse imports by default",
        );
        ui.checkbox(
            &mut e.fold_docstrings_by_default,
            "Collapse docstrings by default",
        );
        ui.heading("Highlighting");
        ui.checkbox(&mut e.highlight_matching_brace, "Highlight matching brace");
        ui.checkbox(
            &mut e.highlight_usages_on_caret_move,
            "Highlight usages of element at caret",
        );
        ui.heading("Editor Tabs");
        super::tab_policy_settings::show(ui, &mut e.tabs);
        ui.horizontal(|ui| {
            ui.label("Tab limit");
            ui.add(egui::Slider::new(&mut e.tab_limit, 1..=50));
        });
        ui.checkbox(
            &mut e.mark_modified_tabs_with_asterisk,
            "Mark modified tabs with asterisk",
        );
        ui.checkbox(
            &mut e.open_in_preview_tab,
            "Open declaration source in preview tab",
        );
        ui.heading("On Save");
        ui.horizontal(|ui| {
            ui.label("Strip trailing spaces");
            for (label, v) in [
                ("None", rustpy_settings::StripTrailing::None),
                (
                    "Modified lines",
                    rustpy_settings::StripTrailing::ModifiedLines,
                ),
                ("All", rustpy_settings::StripTrailing::All),
            ] {
                if ui
                    .selectable_label(
                        e.strip_trailing_spaces_on_save == v,
                        crate::i18n::tr_static(label),
                    )
                    .clicked()
                {
                    e.strip_trailing_spaces_on_save = v;
                }
            }
        });
        ui.checkbox(&mut e.keep_trailing_spaces_on_caret_line, "Always keep trailing spaces on caret lines")
            .on_hover_text("Protect the line at every caret head, including multiple carets. Selection anchor lines are not protected unless they also contain a caret head. Applies when trailing spaces are stripped, including when EditorConfig enables stripping.");
        if e.strip_trailing_spaces_on_save == rustpy_settings::StripTrailing::None {
            ui.label("Caret-line protection applies only when stripping is enabled; EditorConfig can enable it for individual files.");
        }
        ui.checkbox(
            &mut e.ensure_newline_at_eof,
            "Ensure every saved file ends with a line break",
        );
    }

    fn page_reader_mode(&mut self, ui: &mut egui::Ui) {
        let r = &mut self.settings.editor.reader_mode;
        ui.heading("Reader Mode");
        ui.checkbox(
            &mut r.protect_library_sources,
            "Protect library source files from editing",
        );
        ui.label(egui::RichText::new("Turn this off to edit standard-library and installed-package sources across projects. Bundled stubs remain read-only. Reader mode controls their presentation separately.").color(self.theme.text_dim).small());
        ui.add_space(6.0);
        ui.checkbox(
            &mut r.enabled,
            "Use reader mode for library and read-only files",
        );
        ui.label(egui::RichText::new("Files in the interpreter's site-packages and standard library, bundled stubs and read-only files open in reader mode. The Reader Mode switch in the editor's top-right corner turns it off or on for one file.").color(self.theme.text_dim).small());
        ui.add_space(6.0);
        ui.add_enabled_ui(r.enabled, |ui| {
            ui.label(egui::RichText::new("Show in reader mode").strong());
            ui.checkbox(&mut r.render_docs, "Rendered documentation comments");
            ui.checkbox(&mut r.show_warnings, "Warnings (errors are always shown)");
            ui.checkbox(&mut r.increase_line_height, "Increased line height");
            ui.checkbox(&mut r.show_line_numbers, "Line numbers");
            ui.checkbox(&mut r.show_inlay_hints, "Inlay hints");
            ui.checkbox(&mut r.ligatures, "Font ligatures");
        });
    }

    fn page_editor_font(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        self.editor_font_page(ui);
        self.scheme_preview(ui);
    }

    fn scheme_preview(&self, ui: &mut egui::Ui) {
        let scheme = &self.scheme;
        let font_size = self.settings.editor.font_size.max(8.0);
        let spans = highlight_python(PREVIEW);
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let fg = rgb(scheme.editor.foreground);
        let mut pos = 0usize;
        for s in spans {
            let (a, b) = (usize::from(s.range.start()), usize::from(s.range.end()));
            if a > pos {
                job.append(
                    &PREVIEW[pos..a],
                    0.0,
                    egui::TextFormat {
                        font_id: FontId::new(font_size, crate::theme::mono_family(false, false)),
                        color: fg,
                        ..Default::default()
                    },
                );
            }
            let st = scheme.style(s.tag);
            let color = st.fg.map(rgb).unwrap_or(fg);
            job.append(
                &PREVIEW[a..b],
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(font_size, crate::theme::mono_family(st.bold, false)),
                    color,
                    italics: st.italic,
                    underline: if st.underline {
                        Stroke::new(1.0, color)
                    } else {
                        Stroke::NONE
                    },
                    background: st.bg.map(rgb).unwrap_or(Color32::TRANSPARENT),
                    ..Default::default()
                },
            );
            pos = b;
        }
        if pos < PREVIEW.len() {
            job.append(
                &PREVIEW[pos..],
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(font_size, crate::theme::mono_family(false, false)),
                    color: fg,
                    ..Default::default()
                },
            );
        }
        egui::Frame::new()
            .fill(rgb(scheme.editor.background))
            .inner_margin(egui::Margin::same(8))
            .corner_radius(4)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(job);
            });
    }

    pub(super) fn page_color_scheme(&mut self, ui: &mut egui::Ui) {
        self.color_scheme_font_options(ui);
        self.scheme_profile_actions(ui);
        let theme = self.theme.clone();
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut self.settings.semantic_highlighting,
                "Semantic highlighting (rainbow locals)",
            );
            ui.checkbox(
                &mut self.settings.editor.rainbow_brackets,
                "Rainbow brackets",
            );
        });
        ui.horizontal(|ui| {
            let scheme_label = ui.label("Scheme:");
            let mut names: Vec<String> = ColorScheme::builtin_names()
                .iter()
                .map(|s| s.to_string())
                .collect();
            names.extend(custom_scheme_names());
            let current = self.scheme.name.clone();
            egui::ComboBox::from_id_salt("scheme_combo")
                .selected_text(&current)
                .show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(*n == current, n).clicked() {
                            if let Some(s) = scheme_by_name(n) {
                                self.scheme = s;
                                self.settings.editor.color_scheme = n.clone();
                            }
                        }
                    }
                })
                .combo_labelled(&scheme_label);
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.scheme_save_name)
                    .hint_text("New scheme name")
                    .desired_width(160.0),
            );
            if ui.button("Save As…").clicked()
                && !self.settings_ui.scheme_save_name.trim().is_empty()
            {
                let name = self.settings_ui.scheme_save_name.trim().to_string();
                let mut s = self.scheme.clone();
                s.name = name.clone();
                let dir = Settings::config_dir().join("colors");
                let _ = std::fs::create_dir_all(&dir);
                match serde_json::to_string_pretty(&s)
                    .map_err(|e| e.to_string())
                    .and_then(|t| {
                        std::fs::write(dir.join(format!("{name}.json")), t)
                            .map_err(|e| e.to_string())
                    }) {
                    Ok(()) => {
                        // The scheme file is written now; selecting it is applied with Apply / OK.
                        self.scheme = s;
                        self.settings.editor.color_scheme = name;
                        self.settings_ui.message = Some("Scheme saved".into());
                    }
                    Err(e) => self.settings_ui.message = Some(format!("Cannot save scheme: {e}")),
                }
            }
            if ui.button("Reset").clicked() {
                if let Some(s) = ColorScheme::by_name(&self.scheme.name)
                    .or_else(|| scheme_by_name(&self.scheme.name))
                {
                    self.scheme = s;
                }
            }
        });
        ui.separator();
        self.scheme_preview(ui);
        let previous_scheme = self.scheme.name.clone();
        super::scheme_editor::show(
            ui,
            &mut self.scheme,
            self.settings_ui.scheme_tag,
            &mut self.settings_ui.scheme_advanced,
        );
        if self.scheme.name != previous_scheme {
            self.settings.editor.color_scheme = self.scheme.name.clone();
        }
        ui.separator();
        ui.columns(2, |cols| {
            let mut tags: Vec<HighlightTag> = self.scheme.styles.keys().copied().collect();
            tags.extend(
                rustpy_highlight::PYTHON_ATTRIBUTE_KEYS
                    .iter()
                    .map(|(_, tag)| *tag),
            );
            tags.sort();
            tags.dedup();
            egui::ScrollArea::vertical()
                .id_salt("scheme_tags")
                .max_height(260.0)
                .show(&mut cols[0], |ui| {
                    for t in &tags {
                        let st = self.scheme.style(*t);
                        let color = st.fg.map(rgb).unwrap_or(theme.text);
                        if ui
                            .selectable_label(
                                self.settings_ui.scheme_tag == Some(*t),
                                egui::RichText::new(tag_label(*t)).color(color),
                            )
                            .clicked()
                        {
                            self.settings_ui.scheme_tag = Some(*t);
                        }
                    }
                });
            let ui = &mut cols[1];
            if let Some(tag) = self.settings_ui.scheme_tag {
                let mut st: TextStyle = self.scheme.style(tag);
                ui.label(egui::RichText::new(tag_label(tag)).strong());
                let mut changed = false;
                ui.horizontal(|ui| {
                    let mut has_fg = st.fg.is_some();
                    if ui.checkbox(&mut has_fg, "Foreground").changed() {
                        st.fg = if has_fg {
                            Some(self.scheme.editor.foreground)
                        } else {
                            None
                        };
                        changed = true;
                    }
                    if let Some(c) = st.fg {
                        let mut rgbv = [c.0, c.1, c.2];
                        if ui.color_edit_button_srgb(&mut rgbv).changed() {
                            st.fg = Some(rustpy_highlight::Rgb(rgbv[0], rgbv[1], rgbv[2]));
                            changed = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let mut has_bg = st.bg.is_some();
                    if ui.checkbox(&mut has_bg, "Background").changed() {
                        st.bg = if has_bg {
                            Some(self.scheme.editor.background)
                        } else {
                            None
                        };
                        changed = true;
                    }
                    if let Some(c) = st.bg {
                        let mut rgbv = [c.0, c.1, c.2];
                        if ui.color_edit_button_srgb(&mut rgbv).changed() {
                            st.bg = Some(rustpy_highlight::Rgb(rgbv[0], rgbv[1], rgbv[2]));
                            changed = true;
                        }
                    }
                });
                changed |= ui.checkbox(&mut st.bold, "Bold").changed();
                changed |= ui.checkbox(&mut st.italic, "Italic").changed();
                changed |= ui.checkbox(&mut st.underline, "Underline").changed();
                if changed {
                    self.scheme.inherits.remove(&tag);
                    self.scheme.styles.insert(tag, st);
                }
                ui.label(
                    egui::RichText::new(
                        "Changes apply immediately; use Save As… to keep them as a custom scheme.",
                    )
                    .color(theme.text_dim)
                    .small(),
                );
            } else {
                ui.label(
                    egui::RichText::new("Select an attribute to edit its colors")
                        .color(theme.text_dim),
                );
            }
            ui.separator();
            ui.label(egui::RichText::new("Editor colors").strong());
            let ed = &mut self.scheme.editor;
            for (label, c) in [
                ("Background", &mut ed.background),
                ("Foreground", &mut ed.foreground),
                ("Caret row", &mut ed.caret_row),
                ("Selection", &mut ed.selection),
                ("Line numbers", &mut ed.line_number),
                ("Gutter", &mut ed.gutter_background),
            ] {
                ui.horizontal(|ui| {
                    let mut v = [c.0, c.1, c.2];
                    if ui.color_edit_button_srgb(&mut v).changed() {
                        *c = rustpy_highlight::Rgb(v[0], v[1], v[2]);
                    }
                    ui.label(crate::i18n::tr_static(label));
                });
            }
        });
    }

    fn page_code_style(&mut self, ui: &mut egui::Ui) {
        if !self.code_style_header(ui) {
            return;
        }
        self.python_style_page(ui);
        ui.horizontal(|ui| {
            let separator_label = ui.label("Line separator for new files");
            egui::ComboBox::from_id_salt("new-file-line-separator-style")
                .selected_text(&self.settings.editor.preferences.new_line_separator)
                .show_ui(ui, |ui| {
                    for value in ["preserve", "LF", "CRLF", "CR"] {
                        ui.selectable_value(
                            &mut self.settings.editor.preferences.new_line_separator,
                            value.into(),
                            crate::i18n::tr_static(value),
                        );
                    }
                })
                .combo_labelled(&separator_label);
        });
        let e = &mut self.settings.editor;
        ui.heading("Tabs and Indents");
        ui.checkbox(&mut e.use_tab_character, "Use tab character");
        ui.horizontal(|ui| {
            ui.label("Tab size");
            ui.add(egui::Slider::new(&mut e.tab_size, 1..=8));
        });
        ui.horizontal(|ui| {
            ui.label("Indent");
            ui.add(egui::Slider::new(&mut e.indent_size, 1..=8));
        });
        ui.checkbox(&mut e.detect_indents, "Detect and use existing file indents for editing").on_hover_text("A file that is consistently indented with tabs or another number of spaces keeps its indentation (an .editorconfig or the status-bar Indent widget still wins).");
        ui.checkbox(&mut e.editorconfig_enabled, "Enable EditorConfig");
        ui.heading("Wrapping and Braces");
        ui.checkbox(&mut e.wrap_on_typing, "Wrap text at right margin while typing")
            .on_hover_text("Wraps plain text, Python comments and bracket continuations. Preserves long words, string literals and statements that require explicit continuation.");
        ui.horizontal(|ui| {
            ui.label("Hard wrap at (columns)");
            ui.add(egui::Slider::new(&mut e.right_margin, 40..=200));
        });
        ui.horizontal(|ui| {
            ui.label("Visual guides (comma separated)");
            let mut text = e
                .visual_guides
                .iter()
                .map(|g| g.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if ui.text_edit_singleline(&mut text).changed() {
                e.visual_guides = text
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
            }
        });
        ui.heading("Formatter");
        let cs = &mut self.settings.code_style;
        ui.horizontal(|ui| {
            ui.label("Quote style");
            for (label, v) in [
                ("Preserve", "preserve"),
                ("Double", "double"),
                ("Single", "single"),
            ] {
                if ui
                    .selectable_label(cs.quote_style == v, crate::i18n::tr_static(label))
                    .clicked()
                {
                    cs.quote_style = v.into();
                }
            }
        });
        ui.checkbox(
            &mut cs.magic_trailing_comma,
            "Magic trailing comma (one element per line when a trailing comma is present)",
        );
        ui.horizontal(|ui| {
            ui.label("Blank lines around top-level definitions");
            ui.add(egui::Slider::new(&mut cs.blank_lines_top_level, 0..=3));
        });
        ui.horizontal(|ui| {
            ui.label("Blank lines around methods");
            ui.add(egui::Slider::new(&mut cs.blank_lines_in_class, 0..=2));
        });
        ui.horizontal(|ui| {
            ui.label("Engine");
            for (label, v) in [
                ("Built-in", "builtin"),
                ("Black", "black"),
                ("Ruff", "ruff"),
                ("isort", "isort"),
                ("autopep8", "autopep8"),
                ("yapf", "yapf"),
            ] {
                if ui
                    .selectable_label(cs.formatter == v, crate::i18n::tr_static(label))
                    .clicked()
                {
                    cs.formatter = v.into();
                }
            }
        });
        ui.checkbox(&mut cs.use_black_default, "Use Black as default formatter");
        ui.horizontal(|ui| {
            ui.label("Default extra arguments");
            ui.text_edit_singleline(&mut cs.formatter_args);
        });
        ui.label("External tools receive the current buffer on stdin. Empty executable runs the selected project interpreter with python -m.");
        for tool in ["black", "ruff", "isort", "autopep8", "yapf"] {
            if !cs
                .external_formatters
                .iter()
                .any(|config| config.tool == tool)
            {
                cs.external_formatters
                    .push(rustpy_settings::ExternalFormatterSettings::for_tool(tool));
            }
        }
        for config in &mut cs.external_formatters {
            ui.collapsing(format!("{} integration", config.tool), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Executable (empty: python -m)");
                    ui.text_edit_singleline(&mut config.executable);
                });
                ui.horizontal(|ui| {
                    ui.label("Arguments (quoted argv)");
                    ui.text_edit_singleline(&mut config.arguments);
                });
                ui.horizontal(|ui| {
                    ui.label("File masks (; separated)");
                    ui.text_edit_singleline(&mut config.file_masks);
                });
                ui.checkbox(&mut config.on_reformat, "On code reformat (when selected)");
                ui.checkbox(
                    &mut config.on_save,
                    "On save (when selected and reformat on save enabled)",
                );
            });
        }
        ui.checkbox(&mut cs.format_on_save, "Reformat code on save");
        ui.checkbox(
            &mut cs.format_whole_file_on_save,
            "Whole-file save formatting (including inactive editors)",
        );
        ui.checkbox(&mut cs.optimize_imports_on_save, "Optimize imports on save");
        ui.checkbox(
            &mut cs.optimize_imports_on_commit,
            "Optimize imports before commit (checked files only)",
        );
        ui.checkbox(
            &mut cs.optimize_imports_on_fly,
            "Optimize imports on the fly (after editing pauses)",
        );
        ui.checkbox(
            &mut cs.preview_optimize_imports,
            "Preview explicit Optimize Imports",
        );
        ui.checkbox(
            &mut cs.add_unambiguous_imports_on_fly,
            "Add unambiguous imports on the fly",
        );
        ui.checkbox(
            &mut cs.show_import_popup,
            "Show import popup for classes and static methods",
        );
        ui.checkbox(&mut cs.rearrange_on_save, "Rearrange code on save");
        ui.checkbox(&mut cs.cleanup_on_save, "Code cleanup on save");
        ui.checkbox(
            &mut cs.show_reformat_dialog,
            "Show Reformat File dialog (Ctrl+Alt+L)",
        );
        ui.add_space(8.0);
        self.save_action_settings(ui);
        self.copyright_profiles_panel(ui);
    }

    fn page_inlay_hints(&mut self, ui: &mut egui::Ui) {
        let before = (
            self.settings.editor.inlay_parameter_hints,
            self.settings.editor.inlay_type_hints,
        );
        self.additional_inlay_preferences(ui);
        ui.heading("Python");
        ui.checkbox(
            &mut self.settings.editor.inlay_parameter_hints,
            "Parameter names at call sites",
        );
        ui.checkbox(
            &mut self.settings.editor.inlay_type_hints,
            "Inferred types of local variables",
        );
        ui.checkbox(
            &mut self.settings.editor.inlay_return_hints,
            "Inferred function return types",
        );
        ui.separator();
        ui.heading("Code Vision");
        ui.checkbox(
            &mut self.settings.editor.code_vision_usages,
            "Resolved usages above declarations",
        );
        ui.checkbox(
            &mut self.settings.editor.code_vision_author,
            "Git authors above declarations",
        );
        if before
            != (
                self.settings.editor.inlay_parameter_hints,
                self.settings.editor.inlay_type_hints,
            )
        {
            for t in &mut self.tabs {
                t.state.semantic_version = None;
            }
        }
    }

    fn page_inspections(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.inspections.clone();
        self.inspection_profiles_page(ui);
        let selected = self.settings_ui.inspection_focus.clone();
        let focus = std::mem::take(&mut self.settings_ui.inspection_request_focus);
        let i = &mut self.settings.inspections;
        let codes: std::collections::BTreeSet<_> = i
            .disabled_inspections
            .iter()
            .cloned()
            .chain(selected.iter().cloned())
            .collect();
        if !codes.is_empty() {
            ui.heading("Individual inspections");
            for code in codes {
                let mut enabled = !i.disabled_inspections.contains(&code);
                let response = ui.checkbox(&mut enabled, &code);
                if focus && selected.as_ref() == Some(&code) {
                    response.request_focus();
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                if response.changed() {
                    i.disabled_inspections.retain(|id| id != &code);
                    if !enabled {
                        i.disabled_inspections.push(code);
                    }
                }
            }
        }
        ui.collapsing("Style, naming, documentation and metrics",|ui|{
            let style=&mut i.style;
            egui::ComboBox::from_id_salt("style_backend").selected_text(&style.pep8_backend).show_ui(ui,|ui|{for value in ["auto","builtin","external"]{ui.selectable_value(&mut style.pep8_backend,value.into(),crate::i18n::tr_static(value));}}).combo_named(tr_static("PEP 8 backend"));
            ui.label("Auto: complete bundled pycodestyle through the trusted project interpreter; Rust checks when no interpreter is available.");
            let mut ignore=style.pep8_ignore.join(",");ui.horizontal(|ui|{ui.label("Ignored E/W codes or prefixes");if ui.text_edit_singleline(&mut ignore).changed(){style.pep8_ignore=ignore.split(',').map(str::trim).filter(|s|!s.is_empty()).map(String::from).collect();}});
            let mut ignore=style.naming_ignore.join(",");ui.horizontal(|ui|{ui.label("Ignored naming conventions: names");if ui.text_edit_singleline(&mut ignore).changed(){style.naming_ignore=ignore.split(',').map(str::trim).filter(|s|!s.is_empty()).map(String::from).collect();}});
            let mut patterns=style.naming_ignore_patterns.join(",");ui.horizontal(|ui|{ui.label("Ignored name patterns (* and ?)");if ui.text_edit_singleline(&mut patterns).changed(){style.naming_ignore_patterns=patterns.split(',').map(str::trim).filter(|s|!s.is_empty()).map(String::from).collect();}});
            for(value,label)in[(&mut style.ignore_overrides,"Exempt actual inherited overrides"),(&mut style.check_constants,"Module constants must use UPPER_CASE"),(&mut style.doc_modules,"Module docstrings"),(&mut style.doc_classes,"Class docstrings"),(&mut style.doc_functions,"Function docstrings"),(&mut style.doc_methods,"Method docstrings"),(&mut style.doc_private,"Include private elements"),(&mut style.single_quoted_docstrings,"Triple-double-quoted docstrings"),(&mut style.mandatory_encoding,"Mandatory encoding declaration"),(&mut style.missing_shebang,"Require Python shebang"),(&mut style.non_ascii_identifiers,"Report non-ASCII identifiers"),(&mut style.non_ascii_comments,"Report non-ASCII comments"),(&mut style.trailing_comma,"Require multiline trailing commas"),(&mut style.duplicate_code,"Project-wide duplicated fragments")]{ui.checkbox(value,crate::i18n::tr_static(label));}
            ui.label("Function metric limits (0 disables); duplicate minima remain positive");
            for(value,label)in[(&mut style.max_arguments,"Arguments"),(&mut style.max_branches,"Branches"),(&mut style.max_returns,"Returns"),(&mut style.max_statements,"Statements"),(&mut style.max_locals,"Locals"),(&mut style.duplicate_min_lines,"Duplicate minimum lines"),(&mut style.duplicate_min_tokens,"Duplicate minimum tokens")]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.add(egui::DragValue::new(value).range(0..=10000));});}
            egui::ComboBox::from_id_salt("inspection_quote_style").selected_text(&style.quote_style).show_ui(ui,|ui|{for value in ["code_style","preserve","single","double"]{ui.selectable_value(&mut style.quote_style,value.into(),crate::i18n::tr_static(value));}}).combo_named(tr_static("Quote style"));
        });
        ui.heading("Python");
        ui.checkbox(
            &mut i.unresolved_references,
            "Unresolved references (always on)",
        );
        ui.checkbox(
            &mut i.type_checker,
            "Type checker: incompatible types, wrong argument counts (always on)",
        );
        ui.checkbox(&mut i.unused_imports, "Unused import statement");
        ui.checkbox(&mut i.unused_variables, "Unused local variable");
        ui.checkbox(
            &mut i.pep8,
            "PEP 8 coding style violation (line length, whitespace)",
        );
        ui.checkbox(&mut i.naming, "PEP 8 naming convention violation");
        ui.checkbox(&mut i.code_smells, "Code smells: unreachable code, redeclarations, mutable defaults, shadowed builtins, statement has no effect, …");
        ui.checkbox(&mut i.probable_bugs, "Probable bugs: unbound local variables, names shadowing outer scopes, protected member access, attributes defined outside __init__, abstract classes, coroutines not awaited");
        ui.checkbox(&mut i.security, "Security: eval/exec, shell=True, unsafe deserialization, hardcoded secrets, os.system, mktemp");
        ui.checkbox(
            &mut i.compatibility,
            "Python version compatibility (uses the project interpreter version)",
        );
        ui.checkbox(
            &mut i.missing_docstrings,
            "Missing or empty docstring (off by default)",
        );
        ui.checkbox(
            &mut i.grammar,
            "Grammar (comments, docstrings, Markdown, commit messages)",
        );
        ui.add_enabled_ui(i.grammar, |ui| {
            use rustpy_grammar::policy::{Category, Language};
            let grammar_label = ui.label("Grammar language");
            egui::ComboBox::from_id_salt("grammar-language")
                .selected_text(Language::from_key(&i.grammar_language).map_or_else(
                    |_| i.grammar_language.clone(),
                    |language| language.label().into(),
                ))
                .show_ui(ui, |ui| {
                    for language in Language::ALL {
                        ui.selectable_value(
                            &mut i.grammar_language,
                            language.key().into(),
                            language.label(),
                        );
                    }
                })
                .combo_labelled(&grammar_label);
            ui.small("Built-in English rules are shared by both locales.");
            for category in Category::ALL {
                let mut enabled = !i
                    .grammar_disabled_categories
                    .iter()
                    .any(|key| key == category.key());
                if ui.checkbox(&mut enabled, category.label()).changed() {
                    i.grammar_disabled_categories
                        .retain(|key| key != category.key());
                    if !enabled {
                        i.grammar_disabled_categories.push(category.key().into());
                    }
                }
            }
            if let Err(error) = super::grammar_policy::configured(i) {
                ui.colored_label(egui::Color32::RED, error.to_string());
            }
        });
        ui.checkbox(
            &mut i.spelling,
            "Spelling (typos in identifiers, comments, strings and text files)",
        );
        if before != self.settings.inspections {
            self.invalidate_inspection_profile();
        }
    }

    fn page_live_templates(&mut self, ui: &mut egui::Ui) {
        self.live_template_management_page(ui);
    }

    fn page_interpreter(&mut self, ui: &mut egui::Ui) {
        let project_safe_mode = self.project_safe_mode();
        let theme = self.theme.clone();
        let Some(p) = &self.project else {
            ui.label("Open a project to configure its interpreter.");
            return;
        };
        let root = p.root.clone();
        let discovering = p.interpreter_rx.is_some();
        let interpreters = p.interpreters.clone();
        let current = p.interpreter.as_ref().map(|i| i.path.clone());
        ui.heading("Python Interpreter");
        if ui.button("Add New Interpreter / Show All…").clicked() {
            self.open_interpreter_management();
        }
        let mut select: Option<Interpreter> = None;
        egui::Grid::new("interp_grid")
            .num_columns(3)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for i in &interpreters {
                    let is_cur = current.as_deref() == Some(i.path.as_path());
                    if ui.radio(is_cur, format!("Python {}", i.version)).clicked() && !is_cur {
                        select = Some(i.clone());
                    }
                    ui.label(egui::RichText::new(&i.kind).color(theme.text_dim))
                        .on_hover_ui(|ui| {
                            if let Some(Ok(metadata)) =
                                rustpy_project::interpreter::cached_probe(&i.path)
                            {
                                ui.label(format!(
                                    "{} / {} bit",
                                    metadata.architecture, metadata.bits
                                ));
                                ui.label(metadata.full_version);
                                for (name, path) in metadata.sysconfig {
                                    ui.label(format!("{name}: {}", path.display()));
                                }
                            }
                        });
                    ui.label(
                        egui::RichText::new(i.path.display().to_string())
                            .color(theme.text_dim)
                            .small(),
                    );
                    ui.end_row();
                }
            });
        if interpreters.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No interpreters found yet (discovery runs in the background).",
                )
                .color(theme.text_dim),
            );
        }
        if let Some(i) = select {
            self.select_interpreter(i);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Add interpreter:");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.interpreter_input)
                    .hint_text("/path/to/python")
                    .desired_width(320.0),
            );
            if ui.button("Add").clicked() {
                let path = PathBuf::from(self.settings_ui.interpreter_input.trim());
                match rustpy_project::interpreter_remote::DiscoveryJob::local(
                    path,
                    !project_safe_mode,
                ) {
                    Ok(job) => {
                        self.pending_dialog = Some(Dialog::InterpreterProbe {
                            root: root.clone(),
                            job,
                        })
                    }
                    Err(error) => self.settings_ui.message = Some(error),
                }
            }
        });
        if ui
            .button("Add remote interpreter (SSH / Docker / Compose / WSL / Vagrant)…")
            .clicked()
        {
            self.pending_dialog = Some(Dialog::RemoteInterpreter {
                kind: "ssh".into(),
                target: String::new(),
                python: "python3".into(),
                remote_root: String::new(),
                error: None,
            });
        }
        for candidate in rustpy_project::interpreter_remote::configured_candidates(
            &Settings::config_dir().join("interpreters"),
            &root,
        ) {
            ui.horizontal(|ui| {
                ui.label(candidate.label());
                if ui
                    .add_enabled(
                        !project_safe_mode,
                        egui::Button::new("Discover configured interpreter"),
                    )
                    .clicked()
                {
                    match rustpy_project::interpreter_remote::DiscoveryJob::remote(
                        candidate.clone(),
                        Settings::config_dir().join("interpreters"),
                        !project_safe_mode,
                    ) {
                        Ok(job) => {
                            self.pending_dialog = Some(Dialog::InterpreterProbe {
                                root: root.clone(),
                                job,
                            })
                        }
                        Err(error) => self.settings_ui.message = Some(error),
                    }
                }
            });
        }
        if let Some(path) = self
            .project
            .as_ref()
            .and_then(|p| p.tree.model().sdk)
            .or(self.effective_workspace().interpreter)
        {
            if !interpreters.iter().any(|i| i.path == path) && !discovering {
                let label = rustpy_project::interpreter::cached_probe(&path).and_then(Result::err)
                    .map(|invalid| invalid.label()).unwrap_or_else(|| format!("Invalid interpreter: {} (saved SDK is unavailable or could not be probed)", path.display()));
                ui.colored_label(theme.error, label);
            }
        }
        ui.horizontal(|ui| {
            let venv = root.join(".venv");
            let running = self.settings_ui.venv_rx.is_some();
            if ui
                .add_enabled(
                    !project_safe_mode && !running && !venv.exists(),
                    egui::Button::new("Create virtual environment (.venv)"),
                )
                .clicked()
            {
                let base = current.clone().unwrap_or_else(|| PathBuf::from("python3"));
                use rustpy_project::setup::{
                    self, CommandTask, EnvTool, ProjectKind, Setup, SetupAction,
                };
                let mut setup = Setup::new(
                    root.clone(),
                    base,
                    setup::plan(ProjectKind::Pure, true, EnvTool::Venv, false, false, ""),
                );
                if let Some(SetupAction::Command { command, .. }) = setup.next_action(false, None) {
                    let task = CommandTask::spawn(command, setup.command_token());
                    self.settings_ui.venv_rx = Some((venv.clone(), setup, task));
                }
                self.settings_ui.venv_status = Some("Creating .venv…".into());
            }
            if venv.exists() {
                ui.label(
                    egui::RichText::new(format!("{} exists", venv.display())).color(theme.text_dim),
                );
            }
            if let Some(st) = &self.settings_ui.venv_status {
                ui.label(st);
            }
        });
        self.poll_interpreter_venv(ui.ctx());
        ui.separator();
        self.package_settings(ui);
    }

    /// Create virtual environment (.venv): takes the result every frame, also
    /// after the Settings dialog was closed. A failure becomes an error
    /// notification; a new environment becomes the interpreter of the project
    /// it was created in.
    pub(super) fn poll_interpreter_venv(&mut self, ctx: &egui::Context) {
        if self.project_safe_mode() {
            if let Some((_, setup, task)) = self.settings_ui.venv_rx.as_mut() {
                setup.cancel();
                task.cancel();
            }
            self.settings_ui.venv_rx = None;
            return;
        }
        let Some((venv, setup, task)) = &mut self.settings_ui.venv_rx else {
            return;
        };
        let Some(result) = task.poll() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        };
        setup.complete(result.clone());
        let interpreter = match setup.next_action(false, None) {
            Some(rustpy_project::setup::SetupAction::SelectEnvironment { interpreter }) => {
                setup.complete(Ok(()));
                Some(interpreter)
            }
            _ => None,
        };
        let venv = venv.clone();
        let res = result.map_err(|error| error.to_string());
        self.settings_ui.venv_rx = None;
        match res {
            Ok(()) => {
                self.settings_ui.venv_status = Some("Virtual environment created".into());
                if !self
                    .project
                    .as_ref()
                    .is_some_and(|p| venv.parent() == Some(p.root.as_path()))
                {
                    self.set_status(format!("Virtual environment created in {}", venv.display()));
                    return;
                }
                let Some(py) = interpreter else { return };
                if let Some(i) = rustpy_project::interpreter::probe(&py, "venv") {
                    if let Some(p) = &mut self.project {
                        p.interpreters.insert(0, i.clone());
                    }
                    self.select_interpreter(i);
                }
            }
            Err(e) => {
                self.settings_ui.venv_status = Some(format!("venv failed: {e}"));
                self.notify(
                    crate::notifications::NotificationKind::Error,
                    "Python Interpreter",
                    "Creating the virtual environment failed",
                    e,
                    vec![(
                        "Configure Python Interpreter…".into(),
                        crate::notifications::NotificationAction::OpenSettings("interpreter"),
                    )],
                );
            }
        }
    }

    fn page_terminal(&mut self, ui: &mut egui::Ui) {
        self.terminal_settings(ui);
        ui.heading("Application Settings");
        ui.horizontal(|ui| {
            ui.label("Shell path");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.settings.terminal.shell)
                        .hint_text("default: $SHELL")
                        .desired_width(320.0),
                )
                .changed()
            {
                let sh =
                    Some(self.settings.terminal.shell.clone()).filter(|s| !s.trim().is_empty());
                self.terminal_panel.shell = sh.clone();
            }
        });
        ui.checkbox(
            &mut self.settings.terminal.activate_virtualenv,
            "Activate virtualenv in new terminals",
        );
        ui.label(
            egui::RichText::new(
                "New terminal tabs use the configured shell; existing tabs keep running.",
            )
            .color(self.theme.text_dim)
            .small(),
        );
    }

    fn page_vcs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Git");
        ui.horizontal(|ui| {
            ui.label("Path to Git executable");
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.vcs.git_executable)
                    .hint_text("git")
                    .desired_width(320.0),
            );
            if ui.button("Test").clicked() {
                self.test_git_executable();
            }
        });
        ui.checkbox(&mut self.settings.vcs.confirm_before_push, "Confirm before push").on_hover_text("Push in the Git tool window and Commit and Push open the Push dialog instead of pushing at once (Git › Push… always shows it)");
        ui.horizontal(|ui| {
            ui.label("Update method");
            let vcs = &mut self.settings.vcs;
            let rebase = vcs.update_method == "rebase";
            if ui.radio(!rebase, "Merge").clicked() {
                vcs.update_method = "merge".into();
            }
            if ui.radio(rebase, "Rebase").clicked() {
                vcs.update_method = "rebase".into();
            }
        });
        ui.checkbox(
            &mut self.settings.vcs.update_autostash,
            "Stash local changes before Update Project and restore them afterwards",
        );
        let mut ask = !self.settings.vcs.update_dont_ask;
        if ui
            .checkbox(&mut ask, "Show the Update Project dialog")
            .changed()
        {
            self.settings.vcs.update_dont_ask = !ask;
        }
        if let Some(p) = &self.project {
            ui.label(egui::RichText::new(match &p.git { Some(_) => format!("Repository: {}", p.root.display()), None => "No Git repository in this project (Git › Enable Version Control Integration)".into() }).color(self.theme.text_dim).small());
        }
        self.vcs_mapping_settings(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: egui::Modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        alt: false,
        shift: false,
        mac_cmd: false,
    };

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn library_protection_change_marks_reader_mode_settings_dirty() {
        let old = Settings::default();
        let mut new = old.clone();
        new.editor.reader_mode.protect_library_sources = false;
        assert!(page_modified(SettingsPage::ReaderMode, &old, &new));
        assert!(!page_modified(SettingsPage::EditorGeneral, &old, &new));
    }

    #[test]
    fn the_recorder_waits_for_a_whole_chord() {
        assert_eq!(
            recorder_key(egui::Key::ControlLeft, CTRL),
            RecorderKey::Wait,
            "a real keyboard sends Ctrl itself first"
        );
        assert_eq!(
            recorder_key(egui::Key::ShiftRight, egui::Modifiers::SHIFT),
            RecorderKey::Wait
        );
        assert_eq!(
            recorder_key(egui::Key::AltLeft, egui::Modifiers::ALT),
            RecorderKey::Wait
        );
        assert_eq!(
            recorder_key(egui::Key::E, CTRL),
            RecorderKey::Chord(KeyChord::parse("Ctrl+E").unwrap())
        );
        assert_eq!(
            recorder_key(egui::Key::K, egui::Modifiers::NONE),
            RecorderKey::Chord(KeyChord::parse("K").unwrap())
        );
        assert_eq!(
            recorder_key(egui::Key::Escape, egui::Modifiers::NONE),
            RecorderKey::Cancel
        );
        assert_eq!(
            recorder_key(egui::Key::Backspace, egui::Modifiers::NONE),
            RecorderKey::Remove
        );
        assert_eq!(
            recorder_key(egui::Key::Escape, egui::Modifiers::SHIFT),
            RecorderKey::Chord(KeyChord::parse("Shift+Escape").unwrap())
        );
    }

    #[test]
    fn the_recorder_takes_every_key_press_and_the_typed_text() {
        let mut input = egui::InputState::default();
        let moved = egui::Event::PointerMoved(egui::Pos2::new(3.0, 4.0));
        let released = egui::Event::Key {
            key: egui::Key::E,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: CTRL,
        };
        input.events = vec![
            press(egui::Key::ControlLeft, CTRL),
            press(egui::Key::E, CTRL),
            egui::Event::Text("e".into()),
            released,
            egui::Event::Copy,
            moved.clone(),
        ];
        let presses = take_recorder_presses(&mut input);
        assert_eq!(
            presses,
            [
                (egui::Key::ControlLeft, CTRL),
                (egui::Key::E, CTRL),
                (egui::Key::C, CTRL)
            ],
            "winit reports Ctrl+C as a Copy event"
        );
        assert_eq!(
            input.events,
            [moved],
            "nothing else (fields, global shortcuts) sees the keys"
        );
    }

    #[test]
    fn recording_a_chord_an_ide_action_uses_asks_before_moving_it() {
        let mut keymap = Keymap::default_keymap();
        let mut ui = SettingsUi {
            recording: Some(Action::CloseProject),
            ..Default::default()
        };
        assert!(ui.capturing_keys());
        assert!(
            !ui.record_key(&mut keymap, egui::Key::ControlLeft, CTRL),
            "Ctrl alone waits for the rest of the chord"
        );
        assert_eq!(ui.recording, Some(Action::CloseProject));
        assert!(ui.record_key(&mut keymap, egui::Key::E, CTRL));
        assert_eq!(ui.recording, None);
        assert_eq!(
            ui.keymap_conflict,
            Some((
                Action::CloseProject,
                KeyChord::parse("Ctrl+E").unwrap(),
                Action::RecentFiles
            ))
        );
        assert_eq!(
            keymap.lookup(KeyChord::parse("Ctrl+E").unwrap()),
            Some(Action::RecentFiles),
            "nothing moves before the prompt is answered"
        );
        // A free chord is recorded at once.
        ui.recording = Some(Action::CloseProject);
        assert!(ui.record_key(
            &mut keymap,
            egui::Key::K,
            egui::Modifiers {
                ctrl: true,
                command: true,
                alt: true,
                shift: true,
                mac_cmd: false
            }
        ));
        assert_eq!(
            keymap.shortcut_text(Action::CloseProject),
            "Ctrl+Alt+Shift+K"
        );
        assert!(!ui.capturing_keys());
        // Esc ends the recording and changes nothing; Backspace removes the shortcut.
        ui.recording = Some(Action::CloseProject);
        assert!(ui.record_key(&mut keymap, egui::Key::Escape, egui::Modifiers::NONE));
        assert_eq!(
            (
                ui.recording,
                keymap.shortcut_text(Action::CloseProject).as_str()
            ),
            (None, "Ctrl+Alt+Shift+K")
        );
        ui.recording = Some(Action::CloseProject);
        assert!(ui.record_key(&mut keymap, egui::Key::Backspace, egui::Modifiers::NONE));
        assert!(keymap.shortcuts(Action::CloseProject).is_empty());
    }

    #[test]
    fn find_by_shortcut_looks_up_any_chord_and_esc_gives_up() {
        let mut keymap = Keymap::default_keymap();
        let mut ui = SettingsUi {
            find_by_shortcut: true,
            keymap_filter: "old".into(),
            ..Default::default()
        };
        assert!(ui.record_key(&mut keymap, egui::Key::E, CTRL));
        assert_eq!(
            (ui.keymap_filter.as_str(), ui.find_by_shortcut),
            ("Ctrl+E", false)
        );
        ui.find_by_shortcut = true;
        assert!(ui.record_key(&mut keymap, egui::Key::Escape, egui::Modifiers::NONE));
        assert_eq!(
            (ui.keymap_filter.as_str(), ui.find_by_shortcut),
            ("Ctrl+E", false)
        );
        assert_eq!(
            keymap.lookup(KeyChord::parse("Ctrl+E").unwrap()),
            Some(Action::RecentFiles)
        );
    }

    #[test]
    fn custom_shortcuts_are_the_changes_to_the_active_preset() {
        let mut keymap = Keymap::preset("VS Code");
        assert!(
            custom_shortcuts_of(&keymap).is_empty(),
            "picking a preset is not a change"
        );
        keymap.clear_action(Action::CloseProject);
        keymap.bind(
            KeyChord::parse("Ctrl+Alt+Shift+K").unwrap(),
            Action::CloseProject,
        );
        let custom = custom_shortcuts_of(&keymap);
        assert_eq!(
            custom,
            [(
                "CloseProject".to_string(),
                vec!["Ctrl+Alt+Shift+K".to_string()]
            )]
        );
        // Back on Default with the same overrides: only that change, Ctrl+P is Parameter Info again.
        let mut default = Keymap::preset("Default");
        for (id, chords) in &custom {
            let action = Keymap::action_by_id(id).unwrap();
            default.clear_action(action);
            for c in chords {
                default.bind(KeyChord::parse(c).unwrap(), action);
            }
        }
        assert_eq!(custom_shortcuts_of(&default), custom);
        assert_eq!(
            default.lookup(KeyChord::parse("Ctrl+P").unwrap()),
            Some(Action::ParameterInfo)
        );
        assert_eq!(
            default.lookup(KeyChord::parse("Ctrl+W").unwrap()),
            Some(Action::ExtendSelection)
        );
    }

    #[test]
    fn ui_font_size_does_not_compound() {
        let defaults = egui::Style::default().text_styles;
        assert_eq!(scaled_text_styles(13.0), defaults, "13 is 100 %");
        let big = scaled_text_styles(16.0);
        let body = &big[&egui::TextStyle::Body];
        assert!((body.size - defaults[&egui::TextStyle::Body].size * 16.0 / 13.0).abs() < 1e-4);
        // Dragging the slider applies the size many times: the result only depends on the value.
        let mut styles = defaults.clone();
        for size in [14.0, 15.0, 16.0, 17.0, 13.0] {
            styles = scaled_text_styles(size);
        }
        assert_eq!(styles, defaults);
    }

    #[test]
    fn code_vision_switches_belong_to_inlay_settings_page() {
        let old = Settings::default();
        for option in 0..3 {
            let mut new = old.clone();
            match option {
                0 => new.editor.inlay_return_hints = true,
                1 => new.editor.code_vision_usages = true,
                _ => new.editor.code_vision_author = true,
            }
            assert!(page_modified(SettingsPage::InlayHints, &old, &new));
            assert!(!page_modified(SettingsPage::EditorGeneral, &old, &new));
            assert!(!page_modified(SettingsPage::EditorFont, &old, &new));
        }
    }

    #[test]
    fn actual_general_documentation_checkbox_changes_only_general_owned_setting() {
        let mut app = IdeApp::new_welcome(Settings::default());
        let old = app.settings.clone();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut frame = |events| {
            ctx.run_ui(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::Vec2::new(1000.0, 4000.0),
                    )),
                    ..Default::default()
                },
                |ui| app.page_editor_general(ui),
            )
        };
        let painted = frame(vec![]);
        let point = painted
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text == "Show documentation on mouse move" =>
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .expect("General must mount the actual Documentation checkbox");
        painted.drop_without_applying_deltas();
        frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ])
        .drop_without_applying_deltas();
        frame(vec![egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }])
        .drop_without_applying_deltas();
        assert!(!app.settings.editor.documentation_on_hover);
        assert_eq!(app.settings.editor.documentation_delay_ms, 600);
        assert!(page_modified(
            SettingsPage::EditorGeneral,
            &old,
            &app.settings
        ));
        assert!(!page_modified(SettingsPage::Todo, &old, &app.settings));
    }

    #[test]
    fn modified_pages_follow_their_settings() {
        let old = Settings::default();
        let mut new = old.clone();
        assert!(!page_modified(SettingsPage::Appearance, &old, &new));
        new.editor.font_size = 15.0;
        assert!(page_modified(SettingsPage::EditorFont, &old, &new));
        assert!(
            !page_modified(SettingsPage::EditorGeneral, &old, &new),
            "font fields belong to the Font page"
        );
        let mut wrapping = old.clone();
        wrapping.editor.wrap_on_typing = true;
        assert!(page_modified(SettingsPage::CodeStyle, &old, &wrapping));
        assert!(!page_modified(SettingsPage::EditorGeneral, &old, &wrapping));
        new.editor.tab_size = 2;
        assert!(page_modified(SettingsPage::CodeStyle, &old, &new));
        new.editor.show_line_numbers = !new.editor.show_line_numbers;
        assert!(page_modified(SettingsPage::EditorGeneral, &old, &new));
        let mut language = old.clone();
        language.appearance.language = "ja".into();
        assert!(
            page_modified(SettingsPage::Appearance, &old, &language),
            "the UI language is on the Appearance page"
        );
        new.set_open_project_in(rustpy_settings::OpenProjectIn::NewWindow);
        assert!(page_modified(SettingsPage::Appearance, &old, &new));
        new.trusted_locations.push(PathBuf::from("/work"));
        assert!(page_modified(SettingsPage::TrustedLocations, &old, &new));
        assert!(!page_modified(SettingsPage::Inspections, &old, &new));
        // Network and password pages; a typed secret counts as a change too.
        assert!(!page_modified(SettingsPage::HttpProxy, &old, &new));
        new.http_proxy.password = "pw".into();
        assert!(page_modified(SettingsPage::HttpProxy, &old, &new));
        new.certificates.accept_automatically = true;
        assert!(page_modified(SettingsPage::ServerCertificates, &old, &new));
        assert!(!page_modified(SettingsPage::Passwords, &old, &new));
        new.passwords.storage = rustpy_settings::PasswordStorage::Memory;
        assert!(page_modified(SettingsPage::Passwords, &old, &new));
        new.ai.api_key = "sk-ant".into();
        assert!(page_modified(SettingsPage::AiAssistant, &old, &new));
        // Every page is in the tree.
        for page in [
            SettingsPage::HttpProxy,
            SettingsPage::Passwords,
            SettingsPage::ServerCertificates,
        ] {
            assert!(
                TREE.iter()
                    .any(|(_, pages)| pages.iter().any(|(_, p)| *p == page)),
                "{page:?}"
            );
        }
    }

    #[test]
    fn long_unicode_settings_error_keeps_action_buttons_inside_one_footer_row() {
        for width in [280.0, 650.0] {
            let ctx = egui::Context::default();
            let message = format!(
                "Cannot import UI theme /夜/{}: Unknown UI color acccent",
                "long path ".repeat(400)
            );
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 80.0),
                )),
                ..Default::default()
            };
            let mut geometry = None;
            ctx.run_ui(input, |ui| {
                ui.horizontal(|ui| {
                    let bounds = ui.available_rect_before_wrap();
                    let buttons = settings_footer(ui, Some(&message), true, egui::Color32::GRAY);
                    geometry = Some((bounds, buttons.map(|r| r.rect)));
                });
            })
            .drop_without_applying_deltas();
            let (bounds, buttons) = geometry.unwrap();
            for button in buttons {
                assert!(
                    button.min.x >= bounds.min.x && button.max.x <= bounds.max.x,
                    "{button:?} outside {bounds:?}"
                );
                assert!(button.width() > 20.0 && button.height() > 10.0);
                assert_eq!(
                    button.center().y,
                    buttons[0].center().y,
                    "actions stay in one row"
                );
            }
            assert!(buttons[2].max.x < buttons[1].min.x && buttons[1].max.x < buttons[0].min.x);
        }
    }

    #[test]
    fn cancel_keeps_snapshotted_custom_colors_when_json_becomes_invalid() {
        let config = tempfile::tempdir().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = format!("custom:cancel-{nonce}.json");
        std::fs::create_dir(rustpy_settings::ui_themes::directory(config.path())).unwrap();
        let path = rustpy_settings::ui_themes::path(config.path(), &id).unwrap();
        std::fs::write(
            &path,
            br##"{"name":"Valid before Cancel","base":"Light","colors":{"accent":"#123456"}}"##,
        )
        .unwrap();
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Settings::default()
            },
            None,
        );
        let ctx = egui::Context::default();
        app.apply_custom_ui_theme(
            &id,
            crate::theme::custom::load(config.path(), &id).unwrap(),
            &ctx,
        );
        let editor_before = app.scheme.name.clone();
        app.take_settings_snapshot();
        std::fs::write(&path, "invalid JSON").unwrap();
        assert!(crate::theme::custom::load(config.path(), &id).is_err());
        app.restore_settings_snapshot(&ctx);
        assert_eq!(app.settings.appearance.theme, id);
        assert_eq!(app.theme.name(), id);
        assert_eq!(app.theme.accent, egui::Color32::from_rgb(18, 52, 86));
        assert!(!app.theme.dark);
        assert_eq!(app.scheme.name, editor_before);
    }

    #[test]
    fn missing_custom_theme_warning_distinguishes_startup_fallback_and_retained_colors() {
        let id = "custom:night.json";
        assert!(unavailable_custom_theme_message(id, "Dark", "bad JSON")
            .contains("using Dark fallback"));
        let retained = unavailable_custom_theme_message(id, id, "bad JSON");
        assert!(retained.contains("keeping last valid custom colors"));
        assert!(!retained.contains("fallback"));
    }

    #[test]
    fn custom_chrome_and_opposite_editor_scheme_remain_independent_after_persistence() {
        use rustpy_settings::ui_themes::{self, Base, Theme};
        for base in [Base::Light, Base::Dark] {
            let config = tempfile::tempdir().unwrap();
            let definition = Theme {
                name: "Custom chrome".into(),
                base,
                colors: [("accent".into(), "#123456".into())].into(),
            };
            let id = ui_themes::install(config.path(), &definition).unwrap();
            let mut app = IdeApp::new(
                Settings {
                    reopen_last_project: false,
                    ..Settings::default()
                },
                None,
            );
            let ctx = egui::Context::default();
            let editor_before = app.scheme.name.clone();
            app.apply_custom_ui_theme(
                &id,
                crate::theme::custom::load(config.path(), &id).unwrap(),
                &ctx,
            );
            assert_eq!(
                app.scheme.name, editor_before,
                "custom selection never changes editor scheme"
            );
            let matching = if base == Base::Light { "Light" } else { "Dark" };
            let opposite = if base == Base::Light { "Dark" } else { "Light" };
            app.set_editor_scheme(matching, &ctx);
            app.set_editor_scheme(opposite, &ctx);
            assert_eq!(app.settings.appearance.theme, id);
            assert_eq!(app.theme.name(), id);
            assert_eq!(app.theme.accent, egui::Color32::from_rgb(18, 52, 86));
            assert_ne!(app.scheme.dark, app.theme.dark);
            let file = config.path().join("settings.json");
            std::fs::write(&file, serde_json::to_vec(&app.settings).unwrap()).unwrap();
            let restored: Settings = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
            let chrome =
                crate::theme::custom::load(config.path(), &restored.appearance.theme).unwrap();
            let editor = scheme_by_name(&restored.editor.color_scheme).unwrap();
            assert_eq!(chrome.name(), id);
            assert_eq!(chrome.accent, app.theme.accent);
            assert_eq!(editor.name, opposite);
            assert_ne!(chrome.dark, editor.dark);
        }
    }

    #[test]
    fn theme_switch_keeps_fitting_and_remembered_schemes() {
        let is_dark = |n: &str| match n {
            "Dark" | "My Dark" => Some(true),
            "Light" | "My Light" => Some(false),
            _ => None,
        };
        // A custom dark scheme is replaced by the builtin light one…
        assert_eq!(
            scheme_for_theme(false, "My Dark", true, "", is_dark),
            "Light"
        );
        // …and comes back when switching to dark again.
        assert_eq!(
            scheme_for_theme(true, "Light", false, "My Dark", is_dark),
            "My Dark"
        );
        // A remembered custom light scheme is used for the light theme.
        assert_eq!(
            scheme_for_theme(false, "My Dark", true, "My Light", is_dark),
            "My Light"
        );
        // A scheme that already fits the new theme is kept.
        assert_eq!(
            scheme_for_theme(false, "My Light", false, "", is_dark),
            "My Light"
        );
        // A remembered scheme that no longer exists falls back to the builtin.
        assert_eq!(
            scheme_for_theme(true, "Light", false, "Deleted", is_dark),
            "Dark"
        );
    }
}

#[cfg(test)]
#[path = "todo_settings_connected_tests.rs"]
mod todo_settings_connected_tests;

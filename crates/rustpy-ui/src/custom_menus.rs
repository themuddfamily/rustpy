//! Menu providers retain their own captured action dispatch; this layer only orders controls.
use crate::actions::{Action, KeyChord, Keymap};
use rustpy_settings::menus::{Entry, Settings};
#[derive(Clone)]
struct Snapshot {
    settings: Settings,
    keymap: Keymap,
    owner_available: bool,
}
fn key(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("custom-menu-settings", ctx.viewport_id()))
}
/// Resolve only the current viewport's actual ancestry. A snapshot marks an
/// actual project owner; inherited preferences are never republished as owners.
pub(crate) fn appearance_owner(ctx: &egui::Context) -> Option<egui::ViewportId> {
    let current = ctx.viewport_id();
    let parents = ctx.input(|input| {
        input
            .raw
            .viewports
            .iter()
            .map(|(id, info)| (*id, info.parent))
            .collect::<std::collections::HashMap<_, _>>()
    });
    let mut candidate = current;
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if !visited.insert(candidate) {
            return None;
        }
        let key = egui::Id::new(("custom-menu-settings", candidate));
        if ctx.data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(key).is_some()) {
            return Some(candidate);
        }
        candidate = parents.get(&candidate).copied().flatten()?;
    }
    None
}
/// Shared, so the menus drawn each frame do not copy the settings and keymap.
fn snapshot(ctx: &egui::Context) -> Option<std::sync::Arc<Snapshot>> {
    let owner = appearance_owner(ctx)?;
    let key = egui::Id::new(("custom-menu-settings", owner));
    ctx.data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(key))
}
pub fn configure(ctx: &egui::Context, settings: &Settings, keymap: &Keymap, owner_available: bool) {
    let key = key(ctx);
    ctx.data_mut(|data| {
        data.insert_temp(
            key,
            std::sync::Arc::new(Snapshot {
                settings: settings.clone(),
                keymap: keymap.clone(),
                owner_available,
            }),
        )
    });
    ctx.global_style_mut(|style| {
        style.interaction.tooltip_delay = if settings.appearance.tooltip_delay.is_finite() {
            settings.appearance.tooltip_delay.clamp(0.0, 5.0)
        } else {
            0.5
        }
    });
    ctx.tessellation_options_mut(|options| {
        options.feathering = settings.appearance.smooth_shapes;
        options.round_text_to_pixels = settings.appearance.pixel_aligned_text;
    });
}
pub fn render(
    ui: &mut egui::Ui,
    location: &str,
    mut action: impl FnMut(&mut egui::Ui, Action),
) -> bool {
    let snapshot = snapshot(ui.ctx());
    let Some(snapshot) = snapshot else {
        return false;
    };
    let Some(entries) = snapshot.settings.overrides.get(location) else {
        return false;
    };
    if rustpy_settings::menus::validate(entries).is_err() {
        return false;
    }
    fn rows(
        ui: &mut egui::Ui,
        entries: &[Entry],
        icons: bool,
        action: &mut impl FnMut(&mut egui::Ui, Action),
    ) {
        for entry in entries {
            match entry {
                Entry::Separator => {
                    ui.separator();
                }
                Entry::Group { label, children } => {
                    let top_level = !egui::menu::is_in_menu(ui);
                    let response = ui.menu_button(label, |ui| rows(ui, children, icons, action));
                    if top_level {
                        mnemonic(&response.response, label);
                    }
                }
                Entry::Action { id, icon } => {
                    if let Some(value) = Keymap::action_by_id(id) {
                        let merge_key = egui::Id::new((
                            "native-project-window-merge",
                            appearance_owner(ui.ctx()).unwrap_or_else(|| ui.ctx().viewport_id()),
                        ));
                        if id == "MergeAllProjectWindows"
                            && (!cfg!(target_os = "macos")
                                || !ui
                                    .ctx()
                                    .data(|data| data.get_temp::<bool>(merge_key).unwrap_or(false)))
                        {
                            ui.add_enabled(false, egui::Button::new(value.text()));
                            continue;
                        }
                        if icons && !icon.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(icon);
                                action(ui, value);
                            });
                        } else {
                            action(ui, value);
                        }
                    } else {
                        ui.add_enabled(
                            false,
                            egui::Button::new(format!("Unavailable action: {id}")),
                        );
                    }
                }
            }
        }
    }
    if entries.is_empty() {
        ui.label(crate::i18n::tr_static("No actions in this custom menu"));
    }
    let mut dispatch = |ui: &mut egui::Ui, value: Action| {
        if location == "run.popup" && !run_action_allowed(value) {
            ui.add_enabled(false, egui::Button::new(value.text()))
                .on_hover_text("This action requires a different context");
        } else {
            action(ui, value);
        }
    };
    rows(
        ui,
        entries,
        snapshot.settings.appearance.menu_icons,
        &mut dispatch,
    );
    true
}
pub fn action_button(ui: &mut egui::Ui, action: Action) -> egui::Response {
    let snapshot = snapshot(ui.ctx());
    let mut button = egui::Button::new(action.text());
    if let Some(snapshot) = snapshot {
        button = button.shortcut_text(snapshot.keymap.shortcut_text(action));
    }
    ui.add(button)
}
pub fn mnemonic(response: &egui::Response, source: &str) {
    let Some(snapshot) = snapshot(&response.ctx) else {
        return;
    };
    if !snapshot.settings.appearance.menu_mnemonics
        || !snapshot.owner_available
        || !response.enabled()
        || response
            .ctx
            .input(|input| input.viewport().focused == Some(false))
    {
        return;
    }
    let (letter, key) = match source {
        "File" => ("F", egui::Key::F),
        "Edit" => ("E", egui::Key::E),
        "View" => ("V", egui::Key::V),
        "Navigate" => ("N", egui::Key::N),
        "Code" => ("C", egui::Key::C),
        "Refactor" => ("O", egui::Key::O),
        "Run" => ("R", egui::Key::R),
        "Tools" => ("T", egui::Key::T),
        "Git" => ("G", egui::Key::G),
        "Window" => ("W", egui::Key::W),
        "Help" => ("H", egui::Key::H),
        _ => return,
    };
    let chord = KeyChord::new(key, egui::Modifiers::ALT);
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_keyboard_shortcut(format!("Alt+{letter}"))
    });
    if snapshot.keymap.lookup(chord).is_some() {
        return;
    }
    if response
        .ctx
        .input_mut(|input| input.consume_key(egui::Modifiers::ALT, key))
    {
        response.request_focus();
        egui::Popup::open_id(&response.ctx, egui::Popup::default_response_id(response));
        response.ctx.request_repaint();
    }
}
#[derive(Default)]
pub struct AppearancePlugin;
impl egui::plugin::Plugin for AppearancePlugin {
    fn debug_name(&self) -> &'static str {
        "Menu appearance"
    }
    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        let Some(snapshot) = snapshot(ui.ctx()) else {
            return;
        };
        let settings = &snapshot.settings;
        let aa = &settings.appearance;
        ui.ctx().tessellation_options_mut(|options| {
            options.feathering = aa.smooth_shapes;
            options.round_text_to_pixels = aa.pixel_aligned_text;
        });
        eframe::text_rendering::begin_frame(
            ui.ctx(),
            aa.ide_text_antialiasing.coverage(aa.subpixel_bgr),
            aa.editor_text_antialiasing.coverage(aa.subpixel_bgr),
        );
        ui.ctx().global_style_mut(|style| {
            style.scroll_animation = if settings
                .appearance
                .animate_scrolling(crate::a11y::reduced_motion())
            {
                egui::style::ScrollAnimation::default()
            } else {
                egui::style::ScrollAnimation::none()
            };
            style.interaction.tooltip_delay = if settings.appearance.tooltip_delay.is_finite() {
                settings.appearance.tooltip_delay.clamp(0.0, 5.0)
            } else {
                0.5
            }
        });
    }
}

pub fn defaults(location: &str) -> Vec<Entry> {
    let actions = match location {
        "main.file" => vec![
            Action::NewProject,
            Action::NewFile,
            Action::NewScratchFile,
            Action::NewScratchBuffer,
            Action::OpenProject,
            Action::GetFromVcs,
            Action::CopyFile,
            Action::TrustProject,
            Action::CloseProject,
            Action::ToggleReadOnly,
            Action::MakeWritable,
            Action::InvalidateCaches,
            Action::TogglePowerSaveMode,
            Action::SettingsSync,
            Action::SettingsRepository,
            Action::ExportSettings,
            Action::ImportSettings,
            Action::RestoreDefaultSettings,
            Action::Save,
            Action::SaveAll,
            Action::CloseEditor,
            Action::CloseAllEditors,
            Action::CloseOtherEditors,
            Action::ReopenClosedTab,
            Action::Settings,
            Action::Exit,
        ],
        "main.edit" => vec![
            Action::EmacsKillLine,
            Action::EmacsKillWordForward,
            Action::EmacsKillWordBackward,
            Action::EmacsKillRegion,
            Action::EmacsCopyRegion,
            Action::EmacsYank,
            Action::EmacsYankPop,
            Action::EmacsTranspose,
            Action::Undo,
            Action::Redo,
            Action::UndoHistory,
            Action::StartStopMacroRecording,
            Action::PlaybackLastMacro,
            Action::EditMacros,
            Action::Cut,
            Action::Copy,
            Action::CopyAsRichText,
            Action::CopyAsPlainText,
            Action::Paste,
            Action::PasteFromHistory,
            Action::PasteSimple,
            Action::CopyPath,
            Action::CopyReference,
            Action::CompareWithClipboard,
            Action::CompareFiles,
            Action::LocalHistory,
            Action::ReloadFromDisk,
            Action::Synchronize,
            Action::Find,
            Action::Replace,
            Action::FindNext,
            Action::FindPrevious,
            Action::FindWordAtCaret,
            Action::FindInFiles,
            Action::ReplaceInFiles,
            Action::SearchStructurally,
            Action::ReplaceStructurally,
            Action::FindUsages,
            Action::ShowUsages,
            Action::SelectAll,
            Action::ExtendSelection,
            Action::ShrinkSelection,
            Action::AddSelectionForNextOccurrence,
            Action::SelectAllOccurrences,
            Action::AddCaretAbove,
            Action::AddCaretBelow,
            Action::ToggleColumnSelectionMode,
            Action::ToggleStickySelection,
            Action::DuplicateLine,
            Action::DeleteLine,
            Action::JoinLines,
            Action::MoveLineUp,
            Action::MoveLineDown,
            Action::ToggleCase,
            Action::IndentSelection,
            Action::UnindentSelection,
            Action::SortLines,
            Action::ReverseLines,
            Action::RemoveDuplicateLines,
            Action::TrimTrailingWhitespace,
            Action::ConvertIndentsToSpaces,
            Action::ConvertIndentsToTabs,
        ],
        "main.view" => vec![
            Action::ToggleScientificMode,
            Action::ToggleInlayParameterHints,
            Action::ToggleInlayTypeHints,
            Action::ToggleMarkdownPreview,
            Action::ToggleRstPreview,
            Action::ToggleSemanticHighlighting,
            Action::ToggleProjectView,
            Action::ToggleStructureView,
            Action::ToggleBookmarksView,
            Action::ToggleCommitView,
            Action::ToggleTerminal,
            Action::TogglePythonConsole,
            Action::ToggleSciView,
            Action::ToggleDocumentationView,
            Action::TogglePythonPackages,
            Action::ToggleRunView,
            Action::ToggleDebugView,
            Action::ToggleProblemsView,
            Action::ToggleTodoView,
            Action::ToggleGitView,
            Action::ToggleServicesView,
            Action::ToggleEndpointsView,
            Action::ToggleNotificationsView,
            Action::ToggleAiAssistant,
            Action::JumpToLastToolWindow,
            Action::ToggleFullScreen,
            Action::ToggleDistractionFree,
            Action::TogglePresentationMode,
            Action::ToggleZenMode,
            Action::ExitZenMode,
            Action::ToggleTreeDetails,
            Action::ToggleReaderMode,
            Action::ScrollToCenter,
            Action::QuickSwitchScheme,
            Action::RecentFiles,
            Action::ZoomIn,
            Action::ZoomOut,
            Action::ZoomReset,
            Action::HideAllToolWindows,
        ],
        "main.navigate" => vec![
            Action::JumpToNavigationBar,
            Action::SearchEverywhere,
            Action::FindAction,
            Action::GoToClass,
            Action::GoToFile,
            Action::GoToSymbol,
            Action::GoToLine,
            Action::GoToDeclaration,
            Action::GoToSourceOrStub,
            Action::GotoImplementation,
            Action::GoToTypeDeclaration,
            Action::GoToTest,
            Action::GoToRelatedSymbol,
            Action::FileStructure,
            Action::RecentFiles,
            Action::RecentLocations,
            Action::RecentlyChangedFiles,
            Action::RecentlyEditedFiles,
            Action::LastEditLocation,
            Action::Switcher,
            Action::NavigateBack,
            Action::NavigateForward,
            Action::NextError,
            Action::PreviousError,
            Action::ShowErrorDescription,
            Action::NextTab,
            Action::PreviousTab,
            Action::SelectIn,
            Action::JumpToSource,
            Action::OpenInRightSplit,
            Action::GotoSuperMethod,
            Action::TypeHierarchy,
            Action::MethodHierarchy,
            Action::CallHierarchy,
            Action::ShowUmlDiagram,
            Action::ShowDjangoDiagram,
            Action::ShowErDiagram,
        ],
        "main.code" => vec![
            Action::CodeCompletion,
            Action::SmartTypeCompletion,
            Action::ParameterInfo,
            Action::QuickDocumentation,
            Action::ExternalDocumentation,
            Action::ShowIntentionActions,
            Action::Generate,
            Action::SurroundWith,
            Action::EmmetPreview,
            Action::SurroundWithEmmet,
            Action::InsertLiveTemplate,
            Action::SaveSelectionAsLiveTemplate,
            Action::CompleteStatement,
            Action::CommentLine,
            Action::CommentBlock,
            Action::ReformatCode,
            Action::ReformatFile,
            Action::RearrangeCode,
            Action::InspectCode,
            Action::RunInspectionByName,
            Action::CodeCleanup,
            Action::SilentCodeCleanup,
            Action::FillParagraph,
            Action::InjectLanguage,
            Action::EditInjectedFragment,
            Action::GoToInjectedReference,
            Action::CheckRegExp,
            Action::GenerateDocstring,
            Action::UpdateCopyright,
            Action::OptimizeImports,
            Action::ToggleAiAssistant,
            Action::AiExplainCode,
            Action::AiSuggestRefactoring,
            Action::AiWriteTests,
            Action::AiGenerateDocstring,
            Action::AiFixError,
            Action::ToggleFold,
            Action::ExpandRegion,
            Action::CollapseRegion,
            Action::ExpandAll,
            Action::CollapseAll,
            Action::ToggleRenderedDoc,
            Action::ToggleRenderedDocAll,
            Action::NextDocstringSection,
            Action::PreviousDocstringSection,
            Action::MoveStatementUp,
            Action::MoveStatementDown,
            Action::MoveLineUp,
            Action::MoveLineDown,
            Action::MoveElementLeft,
            Action::MoveElementRight,
        ],
        "main.refactor" => vec![
            Action::RefactorThis,
            Action::Rename,
            Action::ChangeSignature,
            Action::MoveRefactoring,
            Action::SafeDelete,
            Action::Inline,
            Action::ConvertRefactoring,
            Action::PullMembersUp,
            Action::PushMembersDown,
            Action::ExtractSuperclass,
            Action::MakeAbstract,
            Action::ImplementAbstractMethods,
            Action::ConvertToDataclass,
            Action::MoveSymbol,
            Action::RenameModule,
            Action::ConvertToPackage,
            Action::ConvertToModule,
            Action::AddTypeHints,
            Action::IntroduceTypeAlias,
            Action::InlineTypeAlias,
            Action::ExtractVariable,
            Action::ExtractConstant,
            Action::ExtractField,
            Action::ExtractParameter,
            Action::ExtractMethod,
        ],
        "main.run" => vec![
            Action::Run,
            Action::Debug,
            Action::RunContext,
            Action::DebugContext,
            Action::ChooseRunConfiguration,
            Action::ChooseDebugConfiguration,
            Action::Rerun,
            Action::RunAnything,
            Action::Stop,
            Action::EditRunConfigurations,
            Action::ToggleLineBreakpoint,
            Action::ViewBreakpoints,
            Action::StepOver,
            Action::StepInto,
            Action::StepOut,
            Action::Resume,
            Action::EvaluateExpression,
            Action::RunAllTests,
            Action::RunTestsInFile,
            Action::RerunFailedTests,
            Action::RunDoctests,
            Action::RunWithCoverage,
            Action::ProfileRun,
            Action::RunWithConcurrencyDiagram,
            Action::RestartDebug,
            Action::ToggleTemporaryLineBreakpoint,
            Action::MuteBreakpoints,
            Action::SmartStepInto,
            Action::RunToCursor,
            Action::JumpToCursor,
            Action::ShowExecutionPoint,
            Action::QuickEvaluateExpression,
            Action::ExecuteInConsole,
            Action::RunFileInConsole,
            Action::RunCell,
            Action::RunCellAndAdvance,
            Action::RunCellsAbove,
            Action::DebugCell,
        ],
        "main.tools" => vec![
            Action::WebDevelopment,
            Action::Collaborate,
            Action::AiInlineCompletion,
            Action::DataFileTools,
            Action::Frameworks,
            Action::AnalyzeStackTrace,
            Action::ToggleTerminal,
            Action::OpenEditorTerminal,
            Action::SendSelectionToTerminal,
            Action::TogglePythonConsole,
            Action::ManagePythonPackages,
            Action::RunSetupPyTask,
            Action::BuildPythonDistribution,
            Action::ShowPythonDependencies,
            Action::ToggleHttpClientView,
            Action::ToggleDatabaseView,
            Action::RunDatabaseSql,
            Action::DatabaseResolutionScope,
            Action::RunHttpRequest,
            Action::OpenInBrowser,
            Action::OpenWebPreview,
            Action::ToggleWebPreview,
            Action::OpenUrl,
            Action::BuildSphinxDocs,
            Action::RunDocutilsTask,
            Action::OpenDocumentationPreview,
            Action::RunManageTask,
            Action::AddRemoteInterpreter,
            Action::QuickList,
            Action::SaveFileAsTemplate,
            Action::CreateLauncherScript,
            Action::DeploymentConfiguration,
            Action::DeploymentUpload,
            Action::DeploymentDownload,
            Action::DeploymentSync,
            Action::DeploymentBrowse,
            Action::OpenTask,
            Action::SwitchTask,
            Action::CloseActiveTask,
            Action::SaveContext,
            Action::LoadContext,
            Action::ClearContext,
        ],
        "main.git" => vec![
            Action::Commit,
            Action::Push,
            Action::UpdateProject,
            Action::VcsPopup,
            Action::GitBranches,
            Action::GitHubPullRequests,
            Action::GitHubAccounts,
            Action::GitHubShareProject,
            Action::GitHubCreateGist,
            Action::GitHubCreatePullRequest,
            Action::GitHubCloneRepo,
            Action::GitHubOpenOnWeb,
            Action::GitHubOpenIssue,
            Action::ShowDiff,
            Action::VcsShowNextChangeMarker,
            Action::VcsShowPrevChangeMarker,
            Action::ShowFileHistory,
            Action::Annotate,
            Action::StashChanges,
            Action::AddToGitignore,
            Action::ToggleGitView,
            Action::GitFetch,
            Action::ManageRemotes,
        ],
        "main.window" => vec![
            Action::NextTab,
            Action::PreviousTab,
            Action::PinActiveTab,
            Action::SortTabsByFilename,
            Action::CloseAllButPinned,
            Action::CloseTabsToTheLeft,
            Action::CloseTabsToTheRight,
            Action::SplitVertically,
            Action::SplitHorizontally,
            Action::SplitAndMoveRight,
            Action::SplitAndMoveDown,
            Action::OpenInOppositeGroup,
            Action::ChangeSplitOrientation,
            Action::EnlargeFirstSplit,
            Action::ShrinkFirstSplit,
            Action::EqualizeSplits,
            Action::MoveTabToOppositeGroup,
            Action::NextSplitter,
            Action::StoreNewLayout,
            Action::StoreDefaultLayout,
            Action::RestoreDefaultLayout,
            Action::RestoreFactoryDefaultLayout,
            Action::RestoreNamedLayout,
            Action::DeleteNamedLayout,
            Action::FocusToolWindowControls,
            Action::IncreaseLeftToolWidth,
            Action::DecreaseLeftToolWidth,
            Action::IncreaseBottomToolHeight,
            Action::DecreaseBottomToolHeight,
            Action::ToggleToolWindowFloating,
            Action::ToggleToolWindowMaximized,
            Action::Unsplit,
            Action::UnsplitAll,
            Action::DetachTab,
            Action::ReattachTab,
            Action::ShowBackgroundTasks,
        ],
        "main.help" => vec![
            Action::CollectDiagnostics,
            Action::IdeActivityMonitor,
            Action::DumpIdeThreads,
            Action::ProfileIdeCpu,
            Action::AnalyzeStackTrace,
            Action::DebugLogSettings,
            Action::ReportIdeIssue,
            Action::WelcomeGuide,
            Action::GettingStarted,
            Action::StartGuidedTour,
            Action::HelpTopics,
            Action::OnlineHelp,
            Action::FindAction,
            Action::LearnIdeFeatures,
            Action::KeymapReference,
            Action::TipOfTheDay,
            Action::ProductivityGuide,
            Action::AccessibilityGuide,
            Action::ContactSupport,
            Action::SubmitFeedback,
            Action::ShowLogInFiles,
            Action::ShowBackgroundTasks,
            Action::EditCustomProperties,
            Action::WhatsNew,
            Action::CheckForUpdates,
            Action::About,
        ],
        "editor.popup" => vec![
            Action::ShowIntentionActions,
            Action::Cut,
            Action::Copy,
            Action::Paste,
            Action::GoToDeclaration,
            Action::FindUsages,
            Action::Rename,
            Action::RunContext,
            Action::DebugContext,
        ],
        "project.popup" | "navigation.popup" => vec![
            Action::CopyPath,
            Action::FindUsages,
            Action::Rename,
            Action::ReformatCode,
            Action::RunContext,
            Action::DebugContext,
            Action::LocalHistory,
        ],
        "run.popup" => vec![
            Action::Rerun,
            Action::Stop,
            Action::CloseEditor,
            Action::ToggleServicesView,
            Action::HideActiveToolWindow,
        ],
        _ => Vec::new(),
    };
    #[cfg(target_os = "macos")]
    if location == "main.window" {
        actions.insert(0, Action::MergeAllProjectWindows);
    }
    if location == "main.root" {
        return rustpy_settings::menus::LOCATIONS
            .iter()
            .filter(|(key, _)| key.starts_with("main.") && *key != "main.root")
            .map(|(key, label)| Entry::Group {
                label: label.trim_end_matches(" menu").into(),
                children: defaults(key),
            })
            .collect();
    }
    actions
        .into_iter()
        .map(|a| Entry::Action {
            id: a.id().into(),
            icon: String::new(),
        })
        .collect()
}

pub fn path_action(
    owner: &crate::run_context::ProjectOwner,
    path: &std::path::Path,
    action: Action,
) -> Option<crate::project_tree::TreeAction> {
    use crate::run_context::{ContextAction, RunMode};
    let context = match action {
        Action::RunContext => Some(ContextAction::Launch(RunMode::Run)),
        Action::DebugContext => Some(ContextAction::Launch(RunMode::Debug)),
        Action::CoverageContext => Some(ContextAction::Launch(RunMode::Coverage)),
        Action::ProfileContext => Some(ContextAction::Launch(RunMode::Profile)),
        Action::ModifyContextConfiguration => Some(ContextAction::Modify),
        Action::CreateContextConfiguration => Some(ContextAction::Create),
        _ => None,
    };
    if let Some(action) = context {
        let stamp = crate::run_context::PathStamp::read(path)?;
        let configuration = crate::run_context::config_for_path(&owner.root, path);
        Some(crate::project_tree::TreeAction::ContextRun(Box::new(
            crate::run_context::PathRequest {
                owner: owner.clone(),
                path: path.into(),
                stamp,
                configuration,
                action,
            },
        )))
    } else {
        Some(crate::project_tree::TreeAction::Invoke(path.into(), action))
    }
}
pub fn run_action_allowed(action: Action) -> bool {
    matches!(
        action,
        Action::Run
            | Action::Rerun
            | Action::Stop
            | Action::CloseEditor
            | Action::ToggleServicesView
            | Action::HideActiveToolWindow
            | Action::Settings
    )
}
pub fn tooltip_shortcuts(ctx: &egui::Context) -> bool {
    snapshot(ctx).is_none_or(|snapshot| snapshot.settings.appearance.tooltip_shortcuts)
}

pub fn tree_indent(ctx: &egui::Context) -> (f32, bool) {
    snapshot(ctx).map_or((14.0, false), |s| {
        (
            if s.settings.appearance.compact_tree_indents {
                10.0
            } else {
                14.0
            },
            s.settings.appearance.tree_indent_guides,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn floating_children_inherit_nearest_project_without_sibling_leakage() {
        let ctx = egui::Context::default();
        let keymap = Keymap::default_keymap();
        let first = egui::ViewportId::from_hash_of("owner-first");
        let second = egui::ViewportId::from_hash_of("owner-second");
        let floating = egui::ViewportId::from_hash_of("floating-first");
        let nested = egui::ViewportId::from_hash_of("nested-first");
        for (owner, enabled) in [(first, true), (second, false)] {
            let mut input = egui::RawInput {
                viewport_id: owner,
                ..Default::default()
            };
            input.viewports.insert(
                owner,
                egui::ViewportInfo {
                    parent: Some(egui::ViewportId::ROOT),
                    ..Default::default()
                },
            );
            let output = ctx.run_ui(input, |ui| {
                let mut settings = Settings::default();
                settings.appearance.tooltip_shortcuts = enabled;
                settings.appearance.pixel_aligned_text = enabled;
                settings.appearance.ide_text_antialiasing = if enabled {
                    rustpy_settings::menus::TextAntialiasing::None
                } else {
                    rustpy_settings::menus::TextAntialiasing::Grayscale
                };
                configure(ui.ctx(), &settings, &keymap, true);
            });
            output.drop_without_applying_deltas();
        }
        ctx.add_plugin(AppearancePlugin);
        let mut input = egui::RawInput {
            viewport_id: nested,
            ..Default::default()
        };
        for (id, parent) in [
            (first, egui::ViewportId::ROOT),
            (second, egui::ViewportId::ROOT),
            (floating, first),
            (nested, floating),
        ] {
            input.viewports.insert(
                id,
                egui::ViewportInfo {
                    parent: Some(parent),
                    ..Default::default()
                },
            );
        }
        let mut output = ctx.run_ui(input, |ui| {
            assert_eq!(appearance_owner(ui.ctx()), Some(first));
            assert!(tooltip_shortcuts(ui.ctx()));
            assert!(ui
                .ctx()
                .tessellation_options(|options| options.round_text_to_pixels));
            ui.label("Floating e\u{301}日本語");
        });
        // This headless fixture consumes shapes without a renderer uploading
        // the generated font atlas. Retire its texture payload before assertions.
        output.textures_delta.clear();
        let shapes =
            eframe::text_rendering::prepare_shapes_for_viewport(&ctx, nested, output.shapes, true);
        assert!(shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.rows.iter().any(|row| row.visuals.mesh.vertices.iter().any(|v| v.uv.x < 0.0)))));
        // The child did not become an owner by inheriting settings.
        let nested_key = egui::Id::new(("custom-menu-settings", nested));
        assert!(ctx
            .data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(nested_key))
            .is_none());
        let orphan = egui::ViewportId::from_hash_of("cyclic-orphan");
        let mut input = egui::RawInput {
            viewport_id: orphan,
            ..Default::default()
        };
        input.viewports.insert(
            orphan,
            egui::ViewportInfo {
                parent: Some(orphan),
                ..Default::default()
            },
        );
        input.viewports.insert(second, Default::default());
        let output = ctx.run_ui(input, |ui| assert_eq!(appearance_owner(ui.ctx()), None));
        output.drop_without_applying_deltas();
    }
    #[test]
    fn separate_project_viewports_retain_their_own_menu_snapshots() {
        let ctx = egui::Context::default();
        let keymap = Keymap::default_keymap();
        for (name, enabled) in [("first-project", true), ("second-project", false)] {
            let viewport = egui::ViewportId::from_hash_of(name);
            let mut input = egui::RawInput {
                viewport_id: viewport,
                ..Default::default()
            };
            input.viewports.insert(viewport, Default::default());
            let output = ctx.run_ui(input, |ui| {
                let mut settings = Settings::default();
                settings.appearance.tooltip_shortcuts = enabled;
                configure(ui.ctx(), &settings, &keymap, true);
                assert_eq!(tooltip_shortcuts(ui.ctx()), enabled);
            });
            output.drop_without_applying_deltas();
        }
        for (name, enabled) in [("first-project", true), ("second-project", false)] {
            let key = egui::Id::new(("custom-menu-settings", egui::ViewportId::from_hash_of(name)));
            assert_eq!(
                ctx.data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(key))
                    .unwrap()
                    .settings
                    .appearance
                    .tooltip_shortcuts,
                enabled
            );
        }
    }
    #[test]
    fn missing_override_uses_native_provider_and_explicit_empty_override_replaces_it() {
        let ctx = egui::Context::default();
        let keymap = Keymap::default_keymap();
        let mut settings = Settings::default();
        configure(&ctx, &settings, &keymap, true);
        let mut native = false;
        let output = ctx.run_ui(Default::default(), |ui| {
            native = !render(ui, "editor.popup", |_, _| panic!("no custom actions"));
        });
        output.drop_without_applying_deltas();
        assert!(native);
        settings.overrides.insert("editor.popup".into(), Vec::new());
        configure(&ctx, &settings, &keymap, true);
        let output = ctx.run_ui(Default::default(), |ui| {
            assert!(render(ui, "editor.popup", |_, _| panic!("empty override")));
        });
        output.drop_without_applying_deltas();
        settings.overrides.remove("editor.popup");
        configure(&ctx, &settings, &keymap, true);
        let output = ctx.run_ui(Default::default(), |ui| {
            assert!(!render(ui, "editor.popup", |_, _| {}));
        });
        output.drop_without_applying_deltas();
    }
    #[test]
    fn path_menu_request_preserves_captured_project_session_and_source_stamp() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.py");
        std::fs::write(&path, "print('captured')\n").unwrap();
        let owner = crate::run_context::ProjectOwner {
            root: directory.path().into(),
            session: 73,
        };
        let request = path_action(&owner, &path, Action::RunContext).unwrap();
        let crate::project_tree::TreeAction::ContextRun(request) = request else {
            panic!("context request")
        };
        assert_eq!(request.owner, owner);
        assert_eq!(request.path, path);
        std::fs::write(&path, "print('changed source after opening menu')\n").unwrap();
        assert_ne!(
            crate::run_context::PathStamp::read(&path),
            Some(request.stamp)
        );
    }
    #[test]
    fn modal_or_foreign_keyboard_owner_blocks_main_menu_mnemonics() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        settings.appearance.menu_mnemonics = true;
        configure(&ctx, &settings, &Keymap::default_keymap(), false);
        let output = ctx.run_ui(
            egui::RawInput {
                focused: true,
                events: vec![egui::Event::Key {
                    key: egui::Key::F,
                    physical_key: Some(egui::Key::F),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::ALT,
                }],
                ..Default::default()
            },
            |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    crate::i18n::LocalizedUi::new(ui)
                        .menu_button("File", |_| panic!("foreign owner opened menu"));
                });
            },
        );
        assert!(!egui::Popup::is_any_open(&ctx));
        output.drop_without_applying_deltas();
    }
}

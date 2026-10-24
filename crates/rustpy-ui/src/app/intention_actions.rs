//! Typed Alt+Enter inspection management. Existing quick-fix dispatch stays intact.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_text::DocumentId;
use std::collections::{HashMap, HashSet};
mod definition_requests;
mod editor_requests;
mod import_dialog;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Group {
    QuickFixes,
    Intentions,
    InspectionSettings,
    Suppressions,
}
impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Self::QuickFixes => "Quick fixes",
            Self::Intentions => "Intentions",
            Self::InspectionSettings => "Inspection settings",
            Self::Suppressions => "Suppressions",
        }
    }
}
#[derive(Clone, Debug)]
enum Action {
    DisableInspection(String),
    EnableInspection(String),
    EditProfile(String),
    RunInspection(String),
    DisableIntention(String),
    Import(rustpy_analysis::intentions::import_actions::ImportRequest),
    Definition(rustpy_analysis::intentions::definition_actions::RequestAction),
    Editor(editor_requests::Request),
}
#[derive(Clone)]
struct Owner {
    view: egui::Id,
    document: DocumentId,
    version: u64,
    path: Option<PathBuf>,
    caret: TextSize,
}
#[derive(Default)]
pub(super) struct IntentionMenu {
    owner: Option<Owner>,
    groups: Vec<Group>,
    actions: HashMap<usize, Action>,
    run: Option<(Owner, String)>,
    import: Option<import_dialog::ImportDialog>,
    editor: Option<editor_requests::EditorState>,
}
impl IntentionMenu {
    pub fn group(&self, index: usize) -> Option<Group> {
        self.groups.get(index).copied()
    }
    pub fn dialog_active(&self) -> bool {
        self.run.is_some() || self.import.is_some() || self.editor.is_some()
    }
}
impl IdeApp {
    fn intention_owner(&self) -> Option<Owner> {
        let tab = self.tabs.get(self.active?)?;
        Some(Owner {
            view: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            caret: tab.state.primary().head(),
        })
    }
    fn intention_owner_current(&self, owner: &Owner) -> bool {
        self.intention_owner().is_some_and(|now| {
            now.view == owner.view
                && now.document == owner.document
                && now.version == owner.version
                && now.path == owner.path
                && now.caret == owner.caret
        })
    }
    pub(super) fn intention_context_current(&self) -> bool {
        self.intention_menu
            .owner
            .as_ref()
            .is_some_and(|owner| self.intention_owner_current(owner))
    }
    pub(super) fn prepare_intention_management(
        &mut self,
        fixes: &mut Vec<QuickFix>,
        quick_titles: &HashSet<String>,
    ) {
        let Some(owner) = self.intention_owner() else {
            return;
        };
        let i = self.active.unwrap();
        let caret = owner.caret;
        let line = self.tabs[i].state.doc.line_of(caret);
        let diagnostics = self.tabs[i].state.diagnostics();
        let at_caret = diagnostics
            .iter()
            .any(|d| d.range.contains_inclusive(caret));
        let mut codes: Vec<String> = diagnostics
            .iter()
            .filter(|d| {
                d.code != "syntax"
                    && (d.range.contains_inclusive(caret)
                        || (!at_caret && self.tabs[i].state.doc.line_of(d.range.start()) == line))
            })
            .map(|d| d.code.to_string())
            .collect();
        codes.sort();
        codes.dedup();
        let mut rows: Vec<_> = std::mem::take(fixes)
            .into_iter()
            .map(|fix| {
                let group = if fix.is_suppression() {
                    Group::Suppressions
                } else if quick_titles.contains(&fix.title) {
                    Group::QuickFixes
                } else {
                    Group::Intentions
                };
                (fix, group, None)
            })
            .collect();
        if let Some((db, file)) = self.analysis_file(i) {
            for (title, request) in
                rustpy_analysis::intentions::definition_actions::ui_requests(&db, &file, caret)
            {
                if !self.settings.disabled_intentions.contains(&title) {
                    rows.push((
                        QuickFix {
                            title,
                            edits: Vec::new(),
                        },
                        Group::Intentions,
                        Some(Action::Definition(request)),
                    ));
                }
            }
            for (title, request) in
                rustpy_analysis::intentions::import_actions::requests(&db, &file, caret)
            {
                if !self.settings.disabled_intentions.contains(&title) {
                    rows.push((
                        QuickFix {
                            title,
                            edits: Vec::new(),
                        },
                        Group::Intentions,
                        Some(Action::Import(request)),
                    ));
                }
            }
        }
        for (title, request) in self.editor_context_requests() {
            if !self.settings.disabled_intentions.contains(&title)
                && !rows.iter().any(|(fix, _, _)| fix.title == title)
            {
                rows.push((
                    QuickFix {
                        title,
                        edits: Vec::new(),
                    },
                    Group::Intentions,
                    Some(Action::Editor(request)),
                ));
            }
        }
        // The existing suppression producer supplies the exact inspection ID and
        // scope. Expose its statement directive under the editor-level wording.
        if !self
            .settings
            .disabled_intentions
            .iter()
            .any(|title| title == "Add # noinspection")
        {
            if let Some((fix, _, _)) = rows.iter().find(|(fix, _, _)| {
                fix.title.starts_with("Suppress ") && fix.title.ends_with("for statement")
            }) {
                rows.push((
                    QuickFix {
                        title: "Add # noinspection".into(),
                        edits: fix.edits.clone(),
                    },
                    Group::Suppressions,
                    None,
                ));
            }
        }
        for title in quick_titles {
            if let Some(distribution) = inspections::install_package_target(title) {
                let title = format!("Install package '{distribution}' (interpreter/version…)");
                if !rows.iter().any(|(fix, _, _)| fix.title == title) {
                    rows.push((
                        QuickFix {
                            title,
                            edits: Vec::new(),
                        },
                        Group::QuickFixes,
                        Some(Action::Import(
                            rustpy_analysis::intentions::import_actions::ImportRequest::Install {
                                distribution: distribution.into(),
                            },
                        )),
                    ));
                }
            }
        }
        let intentions: Vec<String> = rows
            .iter()
            .filter(|(_, group, _)| *group == Group::Intentions)
            .map(|(fix, _, _)| fix.title.clone())
            .collect();
        let mut push = |title: String, action: Action| {
            rows.push((
                QuickFix {
                    title,
                    edits: Vec::new(),
                },
                Group::InspectionSettings,
                Some(action),
            ))
        };
        for code in codes {
            push(
                format!("Edit inspection profile setting: {code}"),
                Action::EditProfile(code.clone()),
            );
            push(
                format!("Disable inspection: {code}"),
                Action::DisableInspection(code.clone()),
            );
            push(
                format!("Run inspection on…: {code}"),
                Action::RunInspection(code),
            );
        }
        for code in &self.settings.inspections.disabled_inspections {
            push(
                format!("Enable inspection: {code}"),
                Action::EnableInspection(code.clone()),
            );
            push(
                format!("Edit inspection profile setting: {code}"),
                Action::EditProfile(code.clone()),
            );
            push(
                format!("Run inspection on…: {code}"),
                Action::RunInspection(code.clone()),
            );
        }
        for title in intentions {
            push(
                format!("Disable intention: {title}"),
                Action::DisableIntention(title),
            );
        }
        rows.sort_by_key(|(_, group, _)| *group);
        self.intention_menu = IntentionMenu {
            owner: Some(owner),
            ..Default::default()
        };
        for (index, (fix, group, action)) in rows.into_iter().enumerate() {
            fixes.push(fix);
            self.intention_menu.groups.push(group);
            if let Some(action) = action {
                self.intention_menu.actions.insert(index, action);
            }
        }
    }
    pub(super) fn apply_intention_management(&mut self, index: usize) -> bool {
        let Some(action) = self.intention_menu.actions.get(&index).cloned() else {
            return false;
        };
        if !self.intention_context_current() {
            self.set_status("Inspection target changed; reopen context actions");
            return true;
        }
        match action {
            Action::Import(request) => self.begin_import_request(request),
            Action::Definition(request) => self.apply_definition_request(request),
            Action::Editor(request) => self.begin_editor_request(request),
            Action::DisableInspection(code) => {
                if !self
                    .settings
                    .inspections
                    .disabled_inspections
                    .contains(&code)
                {
                    self.settings
                        .inspections
                        .disabled_inspections
                        .push(code.clone());
                }
                self.invalidate_inspection_profile();
                self.persist_intention_setting(format!("Inspection {code} disabled"));
            }
            Action::EnableInspection(code) => {
                self.settings
                    .inspections
                    .disabled_inspections
                    .retain(|id| id != &code);
                self.invalidate_inspection_profile();
                self.persist_intention_setting(format!("Inspection {code} enabled"));
            }
            Action::EditProfile(code) => {
                self.settings_ui.inspection_focus = Some(code);
                self.settings_ui.inspection_request_focus = true;
                self.settings_ui.page = settings_ui::SettingsPage::Inspections;
                self.dialog = Dialog::Settings;
            }
            Action::RunInspection(code) => {
                self.intention_menu.run = self.intention_owner().map(|owner| (owner, code));
            }
            Action::DisableIntention(title) => {
                if !self.settings.disabled_intentions.contains(&title) {
                    self.settings.disabled_intentions.push(title.clone());
                }
                self.persist_intention_setting(format!("Intention disabled: {title}"));
            }
        }
        self.focus_editor = true;
        true
    }
    fn persist_intention_setting(&mut self, message: String) {
        match self.settings.save() {
            Ok(()) => self.set_status(message),
            Err(error) => self.set_status(format!(
                "Preference changed for this session; cannot save settings: {error}"
            )),
        }
    }
    pub(super) fn invalidate_inspection_profile(&mut self) {
        self.bg.analysis.cancel();
        self.bg.problems.cancel();
        for tab in &mut self.tabs {
            tab.state.semantic_version = None;
        }
    }
    pub(super) fn intention_run_dialog(&mut self, ctx: &egui::Context) {
        self.import_request_dialog(ctx);
        self.editor_request_dialog(ctx);
        let Some((owner, code)) = self.intention_menu.run.take() else {
            return;
        };
        if !self.intention_owner_current(&owner) {
            self.set_status("Inspection target changed; reopen context actions");
            return;
        }
        let mut scope = None;
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Window::new("Run inspection on…")
            .collapsible(false)
            .resizable(false)
            .show_modal(ctx, |ui| {
                ui.label(&code);
                if ui.button("Current file").clicked() {
                    scope = Some(false);
                }
                if ui
                    .add_enabled(self.project.is_some(), egui::Button::new("Project"))
                    .clicked()
                {
                    scope = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        if cancel {
            self.focus_editor = true;
        } else if let Some(project) = scope {
            self.run_selected_inspection(code, project);
            self.focus_editor = true;
        } else {
            self.intention_menu.run = Some((owner, code));
        }
    }
}

#[cfg(test)]
#[path = "intention_actions_tests.rs"]
mod tests;

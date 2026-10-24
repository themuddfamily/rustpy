//! Definition intentions use the existing project-aware dialogs, never a
//! file-only imitation of rename, safe delete or change signature.
use super::*;
use rustpy_analysis::intentions::definition_actions::{self, ParameterChange, RequestAction};
use rustpy_refactor::change_signature::{ParamKind, SigParam};
/// The refactoring action a definition intention opens, when it needs complete indexes.
fn definition_request_action(request: &RequestAction) -> Option<crate::actions::Action> {
    match request {
        RequestAction::ChangeSignature => Some(crate::actions::Action::ChangeSignature),
        RequestAction::Rename(_) => Some(crate::actions::Action::Rename),
        RequestAction::SafeDelete => Some(crate::actions::Action::SafeDelete),
        RequestAction::Parameter { change, .. } => {
            Some(if matches!(change, ParameterChange::Rename) {
                crate::actions::Action::Rename
            } else {
                crate::actions::Action::ChangeSignature
            })
        }
        _ => None,
    }
}

impl IdeApp {
    pub(super) fn apply_definition_request(&mut self, request: RequestAction) {
        // Project-wide refactorings wait for indexing (queued at the caret as
        // the plain refactoring action; a rename seed or parameter edit is not kept).
        if let Some(action) = definition_request_action(&request) {
            if !self.dumb_mode_admits(action) {
                return;
            }
        }
        match request {
            RequestAction::ChangeSignature => self.open_change_signature(),
            RequestAction::Rename(seed) => self.open_rename(true, seed),
            RequestAction::SafeDelete => self.safe_delete_at_caret(),
            RequestAction::GenerateDocstring => self.generate_docstring(),
            RequestAction::UpdateDocstring => {
                let Some(index) = self.active else { return };
                let Some((db, file)) = self.analysis_file(index) else {
                    return;
                };
                let at = self.tabs[index].state.primary().head();
                if let Some(intention) =
                    definition_actions::update_docstring(&db, &file, at, self.docstring_format())
                {
                    self.tabs[index]
                        .state
                        .apply_text_edits(intention.edits, &intention.title);
                }
            }
            RequestAction::Parameter { name, change } => {
                if matches!(change, ParameterChange::Rename) {
                    self.open_rename(true, None);
                    return;
                }
                self.open_change_signature();
                if let Dialog::ChangeSignature(state) = &mut self.dialog {
                    if let Some(parameter) = state
                        .model
                        .params
                        .iter_mut()
                        .find(|parameter| parameter.name == name)
                    {
                        match change {
                            ParameterChange::KeywordOnly => parameter.kind = ParamKind::KeywordOnly,
                            ParameterChange::PositionalOnly => {
                                parameter.kind = ParamKind::PositionalOnly
                            }
                            ParameterChange::AddDefault => parameter.default = Some("None".into()),
                            ParameterChange::RemoveDefault => parameter.default = None,
                            ParameterChange::Rename => {}
                        }
                    }
                }
            }
            RequestAction::CallParameter {
                path,
                offset,
                name,
                keyword_only,
            } => {
                self.open_file(&path);
                let Some(index) = self
                    .active
                    .filter(|index| self.tabs[*index].state.path.as_ref() == Some(&path))
                else {
                    return;
                };
                self.tabs[index]
                    .state
                    .set_carets(vec![crate::editor::Caret::at(offset)]);
                if !self.dumb_mode_admits(crate::actions::Action::ChangeSignature) {
                    return;
                }
                self.open_change_signature();
                if let Dialog::ChangeSignature(state) = &mut self.dialog {
                    if !state
                        .model
                        .params
                        .iter()
                        .any(|parameter| parameter.name == name)
                    {
                        let mut parameter = SigParam::added(&name, Some("None"), None);
                        parameter.kind = if keyword_only {
                            ParamKind::KeywordOnly
                        } else {
                            ParamKind::Normal
                        };
                        let position = state
                            .model
                            .params
                            .iter()
                            .position(|parameter| {
                                parameter.kind == ParamKind::KwArgs
                                    || (!keyword_only
                                        && matches!(
                                            parameter.kind,
                                            ParamKind::KeywordOnly | ParamKind::VarArgs
                                        ))
                            })
                            .unwrap_or(state.model.params.len());
                        state.model.params.insert(position, parameter);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(source: &str) -> (IdeApp, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        (app, root)
    }
    #[test]
    fn definition_parameter_actions_edit_project_signature_model_not_document() {
        let source = "def f(value):\n    return value\nf(1)\n";
        let (mut app, _root) = app(source);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(6))]);
        app.apply_definition_request(RequestAction::Parameter {
            name: "value".into(),
            change: ParameterChange::KeywordOnly,
        });
        let Dialog::ChangeSignature(state) = &app.dialog else {
            panic!("signature dialog missing")
        };
        assert_eq!(state.model.params[0].kind, ParamKind::KeywordOnly);
        assert_eq!(app.tabs[index].state.doc.text(), source);
    }
    #[test]
    fn definition_refactorings_wait_for_indexing_at_the_caret() {
        let source = "def f(value):\n    return value\nf(1)\n";
        let (mut app, _root) = app(source);
        let index = app.active.unwrap();
        let caret = TextSize::new(6);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(caret)]);
        app.dumb_mode.forced = Some(true);
        app.apply_definition_request(RequestAction::Parameter {
            name: "value".into(),
            change: ParameterChange::KeywordOnly,
        });
        assert!(!matches!(app.dialog, Dialog::ChangeSignature(_)));
        assert_eq!(
            app.dumb_mode.notice(),
            Some(crate::actions::Action::ChangeSignature)
        );
        app.dumb_mode.queue_notice();
        app.apply_definition_request(RequestAction::Rename(None));
        assert_eq!(app.dumb_mode.notice(), Some(crate::actions::Action::Rename));
        app.dumb_mode.dismiss();
        // Indexing finished: Change Signature opens for the definition at the caret.
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        app.dumb_mode.forced = Some(false);
        app.index_tick(&egui::Context::default());
        assert_eq!(app.tabs[index].state.primary().head(), caret);
        assert!(matches!(app.dialog, Dialog::ChangeSignature(_)));
        assert_eq!(app.tabs[index].state.doc.text(), source);
    }
    #[test]
    fn call_site_parameter_seed_uses_actual_definition_and_keeps_existing_calls() {
        let source = "def f(value):\n    return value\nf(1, flag=True)\n";
        let (mut app, root) = app(source);
        app.apply_definition_request(RequestAction::CallParameter {
            path: root.path().join("main.py"),
            offset: TextSize::new(0),
            name: "flag".into(),
            keyword_only: true,
        });
        let Dialog::ChangeSignature(state) = &app.dialog else {
            panic!("signature dialog missing")
        };
        let parameter = state
            .model
            .params
            .iter()
            .find(|parameter| parameter.name == "flag")
            .unwrap();
        assert_eq!(parameter.kind, ParamKind::KeywordOnly);
        assert_eq!(parameter.default.as_deref(), Some("None"));
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
    }
}

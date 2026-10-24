//! One validated policy shared by scheduled Python, prose and commit checks.
use rustpy_grammar::policy::{Context, Policy, PolicyError};
use rustpy_settings::InspectionSettings;

pub(super) fn configured(settings: &InspectionSettings) -> Result<Policy, PolicyError> {
    Policy::from_preferences(
        &settings.grammar_language,
        settings
            .grammar_disabled_categories
            .iter()
            .map(String::as_str),
    )
}

pub(super) fn check(
    settings: &InspectionSettings,
    text: &str,
    context: Context,
) -> Result<Vec<rustpy_grammar::Issue>, PolicyError> {
    if !settings.grammar
        || settings
            .disabled_inspections
            .iter()
            .any(|code| code == "Grammar")
    {
        return Ok(Vec::new());
    }
    Ok(rustpy_grammar::policy::check(
        text,
        context,
        configured(settings)?,
    ))
}

impl super::IdeApp {
    pub(super) fn configure_git_grammar(&mut self) {
        self.git_panel.check_grammar = self.settings.inspections.grammar
            && !self
                .settings
                .inspections
                .disabled_inspections
                .iter()
                .any(|code| code == "Grammar");
        self.git_panel.grammar_policy = configured(&self.settings.inspections);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_policy_reaches_all_surfaces_and_preserves_disabled_toggle() {
        let mut settings = InspectionSettings {
            grammar_language: "en-GB".into(),
            grammar_disabled_categories: vec!["grammar".into()],
            ..Default::default()
        };
        for (context, text) in [
            (Context::Python, "# the the answer happens alot"),
            (Context::Markdown, "the the answer happens alot"),
            (Context::Prose, "the the answer happens alot"),
            (Context::CommitMessage, "the the answer happens alot"),
        ] {
            let issues = check(&settings, text, context).unwrap();
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].rule, "common-mistake");
        }
        settings.grammar_language = "unsupported".into();
        assert!(check(&settings, "the the", Context::Prose).is_err());
        settings.grammar = false;
        assert!(check(&settings, "the the", Context::Prose)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn actual_app_inputs_invalidate_cache_and_actual_commit_checker_obeys_settings() {
        let mut app = super::super::IdeApp::new(rustpy_settings::Settings::default(), None);
        let original = app.analysis_inputs();
        app.settings.inspections.grammar_disabled_categories = vec!["grammar".into()];
        let filtered = app.analysis_inputs();
        assert_ne!(original.fingerprint(), filtered.fingerprint());
        app.settings.inspections.grammar_language = "en-GB".into();
        assert_ne!(filtered.fingerprint(), app.analysis_inputs().fingerprint());
        app.configure_git_grammar();
        app.git_panel.message = "the the answer happens alot".into();
        assert_eq!(app.git_panel.grammar_issues().unwrap().len(), 1);
        assert_eq!(
            app.git_panel.grammar_issues().unwrap()[0].rule,
            "common-mistake"
        );
        app.settings.inspections.grammar_language = "fr-FR".into();
        app.configure_git_grammar();
        assert!(app.git_panel.grammar_issues().is_err());
        assert!(!app.analysis_inputs().grammar);
        app.settings.inspections.grammar = false;
        app.configure_git_grammar();
        assert!(app.git_panel.grammar_issues().unwrap().is_empty());
    }

    #[test]
    fn actual_app_prose_diagnostics_refresh_without_editing_markdown_or_plain_text() {
        for extension in ["md", "txt"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(format!("prose.{extension}"));
            let source = "the the answer happens alot";
            std::fs::write(&path, source).unwrap();
            let mut settings = rustpy_settings::Settings::default();
            settings.inspections.spelling = false;
            settings.trust_project(root.path());
            let mut app = super::super::IdeApp::new(settings, Some(root.path().to_path_buf()));
            app.open_file(&path);
            let index = app.active.unwrap();
            let version = app.tabs[index].state.doc.version();
            app.last_edit_time = 0.0;
            app.sync_analysis(1.0);
            assert_eq!(
                app.tabs[index]
                    .state
                    .extra_diagnostics
                    .iter()
                    .filter(|d| d.code == "Grammar")
                    .count(),
                2
            );
            app.settings.inspections.grammar_disabled_categories = vec!["grammar".into()];
            app.tabs[index].state.semantic_version = None;
            app.sync_analysis(2.0);
            let state = &app.tabs[index].state;
            assert_eq!(
                state
                    .extra_diagnostics
                    .iter()
                    .filter(|d| d.code == "Grammar")
                    .count(),
                1
            );
            assert_eq!(state.quick_fixes.len(), 1);
            assert_eq!(state.doc.version(), version);
            assert!(!state.doc.can_undo());
            assert_eq!(state.doc.text(), source);
            app.settings.inspections.grammar_language = "unsupported".into();
            app.tabs[index].state.semantic_version = None;
            app.sync_analysis(3.0);
            assert!(app
                .status
                .as_ref()
                .is_some_and(|(message, _)| message.contains("does not support language")));
            assert!(app.tabs[index].state.extra_diagnostics.is_empty());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        }
    }
}

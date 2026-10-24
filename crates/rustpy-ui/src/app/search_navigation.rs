//! Search Everywhere's App-owned inputs and activation. Providers never mutate editors.
use super::*;
use crate::search_ui::Destination;
impl IdeApp {
    pub(super) fn prepare_search_navigation(&mut self) {
        self.everywhere.memory = self.settings.navigation_search.clone();
        self.everywhere.clear_providers();
        if let Some(project) = &self.project {
            let root = project.root.clone();
            let files = self.project_files();
            let buffers = self
                .tabs
                .iter()
                .filter(|tab| tab.state.doc.len() <= TextSize::new(128 * 1024))
                .take(32)
                .filter_map(|tab| Some((tab.state.path.clone()?, tab.state.doc.text())))
                .collect();
            self.everywhere.start_providers(&root, files, buffers);
        }
    }
    pub(super) fn handle_search_destination(
        &mut self,
        destination: Destination,
        ctx: &egui::Context,
    ) {
        match destination {
            Destination::File(path, line) => self.handle_nav_event(
                NavEvent::OpenFile {
                    path,
                    line: Some(line),
                    column: None,
                },
                ctx,
            ),
            Destination::Copy(value) => {
                ctx.copy_text(value.clone());
                self.set_status(format!("Copied {value}"));
            }
            Destination::Settings(page) => {
                self.settings_ui.page = match page {
                    "keymap" => settings_ui::SettingsPage::Keymap,
                    "font" => settings_ui::SettingsPage::EditorFont,
                    "scheme" => settings_ui::SettingsPage::ColorScheme,
                    "inspections" => settings_ui::SettingsPage::Inspections,
                    "interpreter" => settings_ui::SettingsPage::Interpreter,
                    "templates" => settings_ui::SettingsPage::LiveTemplates,
                    "plugins" => settings_ui::SettingsPage::Plugins,
                    "completion" => settings_ui::SettingsPage::CodeCompletion,
                    "style" => settings_ui::SettingsPage::CodeStyle,
                    _ => settings_ui::SettingsPage::Appearance,
                };
                self.dialog = Dialog::Settings;
            }
            Destination::Git(reference) => {
                if let Some(repo) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                {
                    self.git_panel.all_branches = true;
                    self.git_panel.log_filter = None;
                    self.git_panel.refresh(repo, 0.0);
                    self.git_panel.log_query = reference.clone();
                    self.git_panel.selected_commit = Some(reference);
                    self.git_panel.tab = crate::git_panel::GitTab::Log;
                    self.bottom = Some(BottomTool::Git);
                }
            }
            Destination::Tab(tab) => self.open_everywhere(tab),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_settings_and_toggle_destinations_use_actual_app_routes() {
        let mut app = IdeApp::new_welcome(Settings::default());
        let context = egui::Context::default();
        app.handle_nav_event(
            NavEvent::SearchDestination(Destination::Settings("inspections")),
            &context,
        );
        assert_eq!(app.settings_ui.page, settings_ui::SettingsPage::Inspections);
        assert!(matches!(app.dialog, Dialog::Settings));
        app.dialog = Dialog::None;
        let before = app.settings.editor.show_line_numbers;
        app.handle_nav_event(NavEvent::RunAction(Action::ToggleLineNumbers), &context);
        assert_eq!(app.settings.editor.show_line_numbers, !before);
    }
    #[test]
    fn directory_result_reveals_actual_project_tree_without_opening_editor() {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.open_project(directory.path());
        let count = app.tabs.len();
        app.handle_nav_event(
            NavEvent::OpenFile {
                path: child.clone(),
                line: None,
                column: None,
            },
            &egui::Context::default(),
        );
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_ref(),
            Some(&child)
        );
        assert_eq!(app.tabs.len(), count);
        assert!(app.layout.left_visible);
    }
}

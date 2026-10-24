//! Effective configuration preview, source navigation and EditorConfig lookup.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::editorconfig::{self, Resolution};

pub(super) struct PreviewState {
    target: String,
    overlay: Option<(PathBuf, String, rustpy_text::DocumentId, u64)>,
    resolution: Option<Resolution>,
    resolved_target: PathBuf,
}
impl IdeApp {
    pub(super) fn open_editorconfig_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if !tab
            .state
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .is_some_and(|name| name == ".editorconfig")
        {
            return false;
        }
        let text = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let context = editorconfig::document::complete(&text, usize::from(caret));
        self.cancel_basic_completion();
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.completion_ui.reset_help();
        let items: Vec<_> = context
            .as_ref()
            .map(|context| {
                context
                    .items
                    .iter()
                    .map(|item| CompletionItem {
                        label: item.label.clone().into(),
                        kind: CompletionKind::Property,
                        detail: item.detail.clone(),
                        insert: item.insert.clone(),
                        caret_offset: None,
                        priority: 0,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: context
                .map_or(caret, |context| TextSize::new(context.range.start as u32)),
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        self.refresh_completion_order();
        true
    }
    pub(super) fn editorconfig_preview_state(&self) -> PreviewState {
        let current = self.active.and_then(|index| self.tabs.get(index));
        let path = current.and_then(|tab| tab.state.path.clone());
        let overlay = current
            .filter(|tab| {
                tab.state
                    .path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .is_some_and(|name| name == ".editorconfig")
            })
            .map(|tab| {
                (
                    tab.state.path.clone().unwrap(),
                    tab.state.doc.text(),
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                )
            });
        let target = if let Some((path, _, _, _)) = &overlay {
            path.parent().unwrap_or(Path::new(".")).join("example.py")
        } else {
            path.unwrap_or_else(|| {
                self.project
                    .as_ref()
                    .map(|project| project.root.join("example.py"))
                    .unwrap_or_else(|| PathBuf::from("example.py"))
            })
        };
        PreviewState {
            target: target.display().to_string(),
            overlay,
            resolution: None,
            resolved_target: PathBuf::new(),
        }
    }
    pub(super) fn draw_editorconfig_preview(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<PreviewState>,
    ) {
        let mut target = PathBuf::from(&state.target);
        if target.is_relative() {
            if let Some(project) = &self.project {
                target = project.root.join(target);
            }
        }
        let stale = state.overlay.as_ref().is_some_and(|(_, _, id, version)| {
            !self
                .tabs
                .iter()
                .any(|tab| tab.state.doc.identity() == *id && tab.state.doc.version() == *version)
        });
        if state.resolution.is_none() || state.resolved_target != target {
            state.resolution = Some(editorconfig::resolve_with_overlay(
                &target,
                state
                    .overlay
                    .as_ref()
                    .map(|(path, text, _, _)| (path.as_path(), text.as_str())),
            ));
            state.resolved_target = target;
        }
        let mut open = true;
        let mut refresh = false;
        let mut navigate = None;
        egui::Window::new("EditorConfig Preview").open(&mut open).resizable(true).default_size([740.0, 520.0]).show_modal(ctx, |ui| {
            ui.horizontal(|ui| { ui.label("Preview for file:"); ui.text_edit_singleline(&mut state.target); refresh = ui.button("Refresh").clicked(); });
            if !self.settings.editor.editorconfig_enabled { ui.colored_label(self.theme.warning, "EditorConfig is disabled; these settings take effect when enabled."); }
            if stale { ui.colored_label(self.theme.warning, "The captured configuration changed. Refresh to preview its current editor text."); }
            if state.overlay.is_some() { ui.label("Includes the captured unsaved EditorConfig text."); }
            if let Some(resolution) = &state.resolution {
                for error in &resolution.errors { ui.colored_label(self.theme.error, error); }
                egui::ScrollArea::vertical().id_salt("editorconfig_properties").show(ui, |ui| {
                    egui::Grid::new("editorconfig_origins").striped(true).num_columns(3).show(ui, |ui| {
                        ui.strong("Property"); ui.strong("Effective value"); ui.strong("Defined in"); ui.end_row();
                        for (key, value) in &resolution.values {
                            ui.monospace(key); ui.monospace(&value.value);
                            if ui.link(value.config.display().to_string()).clicked() { navigate = Some((value.config.clone(), value.range.clone())); }
                            ui.end_row();
                        }
                    });
                    if resolution.values.is_empty() { ui.label("No matching properties. IDE defaults apply."); }
                    if !resolution.props.unsupported_ide.is_empty() {
                        ui.separator(); ui.label("IDE extension properties without a consumer in this version:");
                        for (key, value) in &resolution.props.unsupported_ide { ui.monospace(format!("{key} = {value}")); }
                    }
                    if resolution.props.spelling_language.as_deref().is_some_and(|language| !matches!(language, "en" | "en-US" | "en-GB")) {
                        ui.label("The configured spelling language has no bundled checker. English checks are disabled for this file.");
                    }
                    ui.separator(); ui.label("Configuration search (nearest first):");
                    for path in &resolution.configs { ui.monospace(path.display().to_string()); }
                    ui.label("Export the current scheme through Settings → Code Style → Export → EditorConfig.");
                });
            }
        });
        if refresh {
            if let Some((path, text, id, version)) = &mut state.overlay {
                if let Some(tab) = self
                    .tabs
                    .iter()
                    .find(|tab| tab.state.path.as_ref() == Some(path))
                {
                    *text = tab.state.doc.text();
                    *id = tab.state.doc.identity();
                    *version = tab.state.doc.version();
                }
            }
            state.resolution = None;
        }
        if let Some((path, range)) = navigate {
            self.navigate_to(
                &path,
                TextRange::new(
                    TextSize::new(range.start as u32),
                    TextSize::new(range.end as u32),
                ),
            );
        } else if open {
            self.dialog = Dialog::EditorConfig(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_editorconfig_lookup_inserts_a_property_and_a_hyphenated_value() {
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("[*.py]\nchar", Language::Ini);
        state.path = Some(PathBuf::from("/editorconfig-fixture/.editorconfig"));
        state.move_doc_end(false);
        app.push_tab(state);
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "charset")
            .unwrap();
        app.accept_completion(false);
        assert_eq!(app.tabs[0].state.doc.text(), "[*.py]\ncharset");
        let options = app.edit_options_for(0);
        app.tabs[0].state.insert_text(" = utf-", &options);
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "utf-8-bom")
            .unwrap();
        app.accept_completion(true);
        assert_eq!(app.tabs[0].state.doc.text(), "[*.py]\ncharset = utf-8-bom");
    }
    #[test]
    fn actual_preview_keeps_unsaved_configuration_in_memory_and_reports_source_origin() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".editorconfig");
        let original = "root = true\n[*]\nindent_size = 4\n";
        std::fs::write(&config, original).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state =
            EditorState::from_text("root = true\n[*]\nindent_size = 2\n", Language::Ini);
        state.path = Some(config.clone());
        app.push_tab(state);
        let preview = Box::new(app.editorconfig_preview_state());
        let mut preview = Some(preview);
        egui::Context::default()
            .run_ui(egui::RawInput::default(), |ui| {
                app.draw_editorconfig_preview(ui.ctx(), preview.take().unwrap())
            })
            .drop_without_applying_deltas();
        let Dialog::EditorConfig(preview) = &app.dialog else {
            panic!("preview stays open");
        };
        let resolution = preview.resolution.as_ref().unwrap();
        assert_eq!(resolution.props.indent_size, Some(2));
        assert_eq!(resolution.values["indent_size"].config, config);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    }
}

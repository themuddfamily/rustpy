//! Declared template block and canonical Django URL-name rename entry points.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_refactor::{
    refactor2::FileOp,
    rename_framework::{self, FrameworkTarget},
};

pub(super) struct FrameworkRenameState {
    target: FrameworkTarget,
    path: PathBuf,
    original: String,
    name: String,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_framework_rename(&mut self, index: usize) {
        self.sync_open_tabs_now();
        self.sync_rename_text_sources();
        let Some(analysis) = self.analysis_handle() else {
            return;
        };
        let Some(path) = self.tabs[index].state.path.clone() else {
            return;
        };
        let source = self.tabs[index].state.doc.text();
        let offset = self.tabs[index].state.primary().head();
        let mut paths = self.project_files();
        paths.extend(self.tabs.iter().filter_map(|tab| tab.state.path.clone()));
        paths.sort();
        paths.dedup();
        let Some(target) = rename_framework::target_at(&analysis, &path, &source, offset, &paths)
        else {
            self.set_status(
                "No renamable symbol, declared template block, or canonical URL name at the caret",
            );
            return;
        };
        let name = target.name().to_owned();
        self.dialog = Dialog::FrameworkRename(Box::new(FrameworkRenameState {
            target,
            path,
            original: source,
            name,
            error: None,
        }));
    }
    pub(super) fn framework_rename_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<FrameworkRenameState>,
    ) {
        let mut open = true;
        let mut preview = false;
        let mut cancel = false;
        egui::Window::new("Rename template / URL name").id(egui::Id::new("framework_rename")).collapsible(false).open(&mut open).show_modal(ctx, |ui| {
            ui.label(format!("Rename '{}'", state.target.name()));
            ui.text_edit_singleline(&mut state.name);
            ui.label("Only declared template inheritance and canonical Django URL references are changed.");
            if let Some(error) = &state.error { ui.colored_label(Color32::RED, error); }
            preview = ui.button("Preview rename").clicked();
            if ui.button("Cancel").clicked() { cancel = true; }
        });
        if preview {
            let result = (|| -> Result<Vec<FileOp>, String> {
                let current = self
                    .tabs
                    .iter()
                    .find(|tab| tab.state.path.as_ref() == Some(&state.path))
                    .map(|tab| tab.state.doc.text())
                    .or_else(|| std::fs::read_to_string(&state.path).ok())
                    .ok_or("Source is unavailable")?;
                if current != state.original {
                    return Err("Source changed; reopen Rename".into());
                }
                self.sync_open_tabs_now();
                self.sync_rename_text_sources();
                let analysis = self.analysis_handle().ok_or("No project analysis")?;
                let mut paths = self.project_files();
                let overrides = self
                    .tabs
                    .iter()
                    .filter_map(|tab| {
                        tab.state
                            .path
                            .clone()
                            .map(|path| (path, tab.state.doc.text()))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                paths.extend(overrides.keys().cloned());
                paths.sort();
                paths.dedup();
                let edits = rename_framework::rename_target(
                    &analysis,
                    &state.target,
                    state.name.trim(),
                    &paths,
                    &overrides,
                )?;
                if edits.is_empty() {
                    return Err("No declared references to rename".into());
                }
                Ok(edits
                    .into_iter()
                    .map(|(path, edits)| FileOp::Edit(path, edits))
                    .collect())
            })();
            match result {
                Ok(operations) => {
                    self.preview_file_ops(operations, "Rename template / URL name");
                    return;
                }
                Err(error) => state.error = Some(error),
            }
        }
        if open && !cancel {
            self.dialog = Dialog::FrameworkRename(state);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shift_f6_on_template_block_opens_source_bound_declared_target_dialog() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("base.html");
        let source = "{% block content %}hello{% endblock content %}";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.find("content").unwrap() as u32,
            ))]);
        app.open_rename(true, None);
        let Dialog::FrameworkRename(state) = std::mem::replace(&mut app.dialog, Dialog::None)
        else {
            panic!("template rename was not routed")
        };
        assert_eq!(state.name, "content");
        assert_eq!(state.original, source);
        assert_eq!(state.path, path);
        let mut state = Some(state);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut shapes = Vec::new();
        // The Area sizing pass retains state but does not paint visible labels.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 800.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.framework_rename_dialog(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::FrameworkRename(retained) =
                    std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("rename dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.name, "content");
                assert_eq!(retained.original, source);
                assert_eq!(retained.path, path);
                state = Some(retained);
            }
        }
        assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains("Preview rename"))));
        assert_eq!(app.tabs[index].state.doc.text(), source);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}

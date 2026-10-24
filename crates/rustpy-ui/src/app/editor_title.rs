//! Exact-path editor title and breadcrumb popup. App owns navigation/clipboard.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    CopyPath(PathBuf),
    SelectIn(PathBuf),
}

/// Never invent a filesystem path for an unsaved editor.
pub(super) fn window_title(
    project: Option<&str>,
    path: Option<&Path>,
    title: Option<&str>,
) -> String {
    let file = path
        .map(|path| path.display().to_string())
        .or_else(|| title.map(str::to_owned));
    match (project, file) {
        (Some(project), Some(file)) => format!("{project} – {file} – RustPy"),
        (Some(project), None) => format!("{project} – RustPy"),
        (None, Some(file)) => format!("{file} – RustPy"),
        (None, None) => "RustPy".into(),
    }
}

/// File first, then every real ancestor, including the filesystem root. Preserve
/// lexical path identity; no disk access or canonicalization during painting.
pub(super) fn hierarchy(path: &Path) -> Vec<PathBuf> {
    path.ancestors()
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect()
}

fn actions(ui: &mut egui::Ui, path: &Path) -> Option<Intent> {
    if ui.button("Copy Path").clicked() {
        ui.close();
        return Some(Intent::CopyPath(path.to_path_buf()));
    }
    if ui.button("Select In…").clicked() {
        ui.close();
        return Some(Intent::SelectIn(path.to_path_buf()));
    }
    None
}

/// Replace only the FILE crumb; existing class/function crumbs remain App-owned.
/// Returned path is the clicked popup target, never the current active tab later.
pub(super) fn show(ui: &mut egui::Ui, path: Option<&Path>, title: &str) -> Option<Intent> {
    let Some(path) = path else {
        ui.add_enabled(false, egui::Button::new(title))
            .on_disabled_hover_text("Unsaved editor: no filesystem path");
        return None;
    };
    let mut intent = None;
    ui.menu_button(title, |ui| {
        ui.label(path.display().to_string());
        intent = actions(ui, path);
        ui.separator();
        ui.label("Path hierarchy");
        for ancestor in hierarchy(path).into_iter().skip(1) {
            ui.push_id(&ancestor, |ui| {
                ui.menu_button(ancestor.display().to_string(), |ui| {
                    if let Some(selected) = actions(ui, &ancestor) {
                        intent = Some(selected);
                    }
                });
            });
        }
    })
    .response
    .on_hover_text(path.display().to_string());
    intent
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        ctx: &egui::Context,
        path: Option<&Path>,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (egui::FullOutput, Option<Intent>) {
        let mut intent = None;
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 450.0),
                )),
                ..Default::default()
            },
            |ui| {
                intent = show(ui, path, "module π.py");
            },
        );
        (output, intent)
    }
    fn rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("popup did not paint {label:?}"))
    }
    fn click(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    #[test]
    fn full_title_and_hierarchy_preserve_unicode_spaces_and_exact_roots() {
        let path = Path::new("/owned project/π folder/module π.py");
        assert_eq!(
            window_title(Some("project"), Some(path), Some("ignored")),
            "project – /owned project/π folder/module π.py – RustPy"
        );
        assert_eq!(
            hierarchy(path),
            vec![
                path.to_path_buf(),
                PathBuf::from("/owned project/π folder"),
                PathBuf::from("/owned project"),
                PathBuf::from("/")
            ]
        );
        assert_eq!(
            window_title(None, None, Some("Untitled")),
            "Untitled – RustPy"
        );
        assert_eq!(
            hierarchy(Path::new("module.py")),
            vec![PathBuf::from("module.py")]
        );
    }
    #[test]
    fn actual_file_crumb_opens_full_path_popup_and_copy_emits_exact_target() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let path = Path::new("/owned project/π folder/module π.py");
        let (output, result) = frame(&ctx, Some(path), vec![], 0.0);
        assert!(result.is_none());
        let crumb = rect(&output, "module π.py").center();
        output.drop_without_applying_deltas();
        frame(&ctx, Some(path), click(crumb, true), 0.02)
            .0
            .drop_without_applying_deltas();
        frame(&ctx, Some(path), click(crumb, false), 0.04)
            .0
            .drop_without_applying_deltas();
        let (output, _) = frame(&ctx, Some(path), vec![], 0.06);
        rect(&output, &path.display().to_string());
        rect(&output, "/owned project/π folder");
        let copy = rect(&output, "Copy Path").center();
        output.drop_without_applying_deltas();
        frame(&ctx, Some(path), click(copy, true), 0.08)
            .0
            .drop_without_applying_deltas();
        let (output, result) = frame(&ctx, Some(path), click(copy, false), 0.10);
        assert_eq!(result, Some(Intent::CopyPath(path.to_path_buf())));
        assert!(
            !output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::CopyText(_))),
            "App owns single clipboard publication"
        );
        output.drop_without_applying_deltas();
    }
    #[test]
    fn actual_ancestor_submenu_select_in_returns_parent_not_active_file() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let path = Path::new("/owned project/π folder/module π.py");
        let (output, _) = frame(&ctx, Some(path), vec![], 0.0);
        let crumb = rect(&output, "module π.py").center();
        output.drop_without_applying_deltas();
        frame(&ctx, Some(path), click(crumb, true), 0.02)
            .0
            .drop_without_applying_deltas();
        frame(&ctx, Some(path), click(crumb, false), 0.04)
            .0
            .drop_without_applying_deltas();
        let (output, _) = frame(&ctx, Some(path), vec![], 0.06);
        let parent = rect(&output, "/owned project/π folder").center();
        output.drop_without_applying_deltas();
        frame(&ctx, Some(path), click(parent, true), 0.08)
            .0
            .drop_without_applying_deltas();
        frame(&ctx, Some(path), click(parent, false), 0.10)
            .0
            .drop_without_applying_deltas();
        let (output, _) = frame(
            &ctx,
            Some(path),
            vec![egui::Event::PointerMoved(parent)],
            0.20,
        );
        let select = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Select In…" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .next_back()
            .expect("ancestor submenu must paint Select In");
        output.drop_without_applying_deltas();
        frame(&ctx, Some(path), click(select, true), 0.22)
            .0
            .drop_without_applying_deltas();
        let (output, result) = frame(&ctx, Some(path), click(select, false), 0.24);
        assert_eq!(
            result,
            Some(Intent::SelectIn(PathBuf::from("/owned project/π folder")))
        );
        output.drop_without_applying_deltas();
    }
    #[test]
    fn actual_pathless_crumb_has_no_popup_or_filesystem_intent() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let (output, result) = frame(&ctx, None, vec![], 0.0);
        assert!(result.is_none());
        let crumb = rect(&output, "module π.py").center();
        output.drop_without_applying_deltas();
        frame(&ctx, None, click(crumb, true), 0.02)
            .0
            .drop_without_applying_deltas();
        let (output, result) = frame(&ctx, None, click(crumb, false), 0.04);
        assert!(result.is_none());
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "Copy Path")));
        output.drop_without_applying_deltas();
    }
}

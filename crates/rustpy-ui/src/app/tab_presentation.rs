//! Layout geometry shared by the tab-strip renderer and real editor-group panels.
use super::*;
use rustpy_settings::tab_policy::TabPlacement;

pub(super) const ROW_HEIGHT: f32 = 30.0;
pub(super) const VERTICAL_WIDTH: f32 = 220.0;

/// A wrap plan preserves source order and never emits a zero-width or runaway
/// row for a very long path. The scroll viewport bounds large tab collections.
pub(super) fn wrapped_rows(width: f32, widths: &[f32]) -> usize {
    if widths.is_empty() {
        return 1;
    }
    let width = width.max(1.0);
    let mut rows = 1;
    let mut used = 0.0;
    for &item in widths {
        let item = item.clamp(1.0, width);
        if used > 0.0 && used + item > width {
            rows += 1;
            used = 0.0;
        }
        used += item;
    }
    rows
}
pub(super) fn vertical(placement: TabPlacement) -> bool {
    matches!(placement, TabPlacement::Left | TabPlacement::Right)
}
impl IdeApp {
    fn tab_policy_horizontal_height(&self, ui: &egui::Ui, group: u8) -> f32 {
        if !self.settings.editor.tabs.multiple_rows {
            return ROW_HEIGHT;
        }
        let titles = super::tab_strip::unique_titles(
            &self
                .tabs
                .iter()
                .map(|t| t.state.path.as_deref())
                .collect::<Vec<_>>(),
        );
        let widths: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group == group)
            .map(|(i, t)| {
                let title = self.tab_policy_title(i, &titles[i]);
                ui.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(title, egui::FontId::proportional(13.0), self.theme.text)
                })
                .size()
                .x + 40.0
                    + if t.pinned { 14.0 } else { 0.0 }
            })
            .collect();
        (wrapped_rows(ui.available_width(), &widths) as f32 * ROW_HEIGHT)
            .min((ui.available_height() - 80.0).max(ROW_HEIGHT))
    }
    /// The Root bridge renames the existing editor-only body to
    /// group_editor_area; this wrapper leaves one rendering owner per editor.
    pub(super) fn tab_policy_group_area(&mut self, ui: &mut egui::Ui, group: u8) {
        let policy = self.settings.editor.tabs.clone();
        self.observe_tab_policy();
        match policy.placement {
            TabPlacement::None => self.group_editor_area(ui, group),
            TabPlacement::Top => {
                self.tab_bar(ui, group);
                self.group_editor_area(ui, group);
            }
            TabPlacement::Bottom => {
                let height = self.tab_policy_horizontal_height(ui, group);
                egui::Panel::bottom(ui.id().with(("tabs-bottom", group)))
                    .resizable(false)
                    .exact_size(height)
                    .show(ui, |ui| self.tab_bar(ui, group));
                self.group_editor_area(ui, group);
            }
            TabPlacement::Left | TabPlacement::Right => {
                let id = ui.id().with(("tabs-side", group));
                let panel = if policy.placement == TabPlacement::Left {
                    egui::Panel::left(id)
                } else {
                    egui::Panel::right(id)
                };
                panel
                    .resizable(false)
                    .exact_size(VERTICAL_WIDTH)
                    .show(ui, |ui| self.tab_bar(ui, group));
                self.group_editor_area(ui, group);
            }
        }
        self.observe_tab_policy();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_plan_wraps_long_unicode_titles_without_unbounded_width() {
        assert_eq!(wrapped_rows(200.0, &[100.0, 100.0, 70.0]), 2);
        assert_eq!(wrapped_rows(200.0, &[10000.0, 70.0]), 2);
        assert_eq!(wrapped_rows(0.0, &[10.0, 10.0]), 2);
        assert!(vertical(TabPlacement::Right));
        assert!(!vertical(TabPlacement::Bottom));
    }
    #[test]
    fn actual_group_renderer_all_placements_preserves_source_and_active_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "π = 1\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let original = app.tabs[0].view.id;
        let source = app.tabs[0].state.doc.text();
        let mut time = 0.0;
        for placement in [
            TabPlacement::Top,
            TabPlacement::Bottom,
            TabPlacement::Left,
            TabPlacement::Right,
            TabPlacement::None,
        ] {
            app.settings.editor.tabs.placement = placement;
            let mut panel_id = None;
            for _ in 0..2 {
                time += 0.1;
                let output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(time),
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1000.0, 700.0),
                        )),
                        focused: true,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            panel_id = Some(ui.id().with((
                                if vertical(placement) {
                                    "tabs-side"
                                } else {
                                    "tabs-bottom"
                                },
                                0u8,
                            )));
                            app.group_area(ui, 0);
                        });
                    },
                );
                output.drop_without_applying_deltas();
            }
            assert_eq!(app.tabs[app.active.unwrap()].view.id, original);
            assert_eq!(app.tabs[0].state.doc.text(), source);
            assert!(!app.tabs[0].state.doc.can_undo());
            if vertical(placement) {
                let rect = egui::PanelState::load(&ctx, panel_id.unwrap())
                    .unwrap()
                    .outer_rect;
                assert!((rect.width() - VERTICAL_WIDTH).abs() < 2.0);
            }
            if placement == TabPlacement::Bottom {
                let rect = egui::PanelState::load(&ctx, panel_id.unwrap())
                    .unwrap()
                    .outer_rect;
                assert!(rect.height() >= ROW_HEIGHT - 2.0);
            }
        }
    }
}

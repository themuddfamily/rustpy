//! The bottom tool window docked under the editor (roadmap §5.2 tool windows
//! framework) and Maximize Tool Window: maximized, the tool window takes the
//! editor's place; restored, it gets back the height it had before.

use super::*;

/// egui id of the docked bottom tool window panel (its height lives in egui's
/// persisted panel state).
const BOTTOM_PANEL: &str = "bottom_tools";

/// Height of the docked tool window before anything resized it.
const DEFAULT_HEIGHT: f32 = 300.0;

/// Where the docked height is kept while the tool window is maximized
/// (persisted like the panel state, so a restart while maximized restores it).
fn restore_height_id() -> egui::Id {
    egui::Id::new("bottom_tools_height_before_maximize")
}

/// Shows the docked bottom tool window panel. `maximized` fills the whole
/// height that is left (the caller hides the editor); egui only applies a
/// panel's default size before its first show, so the maximized height is
/// enforced and the previous height is written back into the panel state on
/// restore.
pub(super) fn show_bottom_dock<R>(
    ui: &mut egui::Ui,
    maximized: bool,
    default_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let default_height = if default_height.is_finite() && default_height >= 80.0 {
        default_height
    } else {
        DEFAULT_HEIGHT
    };
    let id = egui::Id::new(BOTTOM_PANEL);
    let ctx = ui.ctx().clone();
    let saved: Option<f32> = ctx.data_mut(|d| d.get_persisted(restore_height_id()));
    let panel = egui::Panel::bottom(id).min_size(80.0);
    if maximized {
        if saved.is_none() {
            let height = egui::PanelState::load(&ctx, id)
                .map(|s| s.size().y)
                .unwrap_or(default_height);
            ctx.data_mut(|d| d.insert_persisted(restore_height_id(), height));
        }
        return panel
            .resizable(false)
            .exact_size(ui.available_height())
            .show(ui, add_contents);
    }
    if let Some(height) = saved {
        let outer_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(ui.available_width(), height));
        ctx.data_mut(|d| {
            d.remove::<f32>(restore_height_id());
            d.insert_persisted(id, egui::PanelState { outer_rect });
        });
    }
    panel
        .resizable(true)
        .default_size(default_height)
        .show(ui, add_contents)
}

/// An explicit docked resize supersedes a pending maximize restoration. This
/// changes the same PanelState used by pointer resizing and layout capture.
pub(super) fn set_keyboard_height(ctx: &egui::Context, height: f32) {
    let id = egui::Id::new(BOTTOM_PANEL);
    let mut rect = egui::PanelState::load(ctx, id)
        .map(|state| state.outer_rect)
        .unwrap_or_else(|| ctx.content_rect());
    rect.max.y = rect.min.y + height;
    ctx.data_mut(|data| {
        data.remove::<f32>(restore_height_id());
        data.insert_persisted(id, egui::PanelState { outer_rect: rect });
    });
}

impl IdeApp {
    /// Whether a maximized docked tool window covers the editor area.
    pub(super) fn editor_behind_tool_window(&self) -> bool {
        self.layout
            .tools
            .saved
            .maximized
            .as_ref()
            .is_some_and(|id| self.framework_visible().contains(id))
            || (self.bottom.is_some() && !self.floating_bottom && self.bottom_maximized)
    }

    /// A maximized tool window gives the editor back when it is hidden, and when
    /// something wants the editor (navigation to a file, Escape to the editor).
    pub(super) fn sync_tool_window_maximized(&mut self) {
        if self.focus_editor && !self.tabs.is_empty() {
            self.layout.tools.saved.maximized = None;
        }
        if self.bottom.is_none()
            || (self.editor_behind_tool_window() && self.focus_editor && !self.tabs.is_empty())
        {
            self.bottom_maximized = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame of a 1000×800 window that shows only the dock (its
    /// content fills it, like the tool windows do); returns its height.
    fn frame(ctx: &egui::Context, maximized: bool) -> f32 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
            ..Default::default()
        };
        let mut height = 0.0;
        ctx.run_ui(input, |ui| {
            let dock = show_bottom_dock(ui, maximized, DEFAULT_HEIGHT, |ui| {
                ui.label("tool window");
                ui.allocate_space(ui.available_size());
            });
            height = dock.response.rect.height();
        })
        .drop_without_applying_deltas();
        height
    }

    #[test]
    fn maximize_fills_the_height_and_restore_brings_the_old_height_back() {
        let ctx = egui::Context::default();
        let normal = frame(&ctx, false);
        assert!((normal - DEFAULT_HEIGHT).abs() < 2.0, "{normal}");
        // Maximize takes effect at once (not only on the panel's first show).
        assert!(frame(&ctx, true) > 790.0);
        assert!(frame(&ctx, true) > 790.0);
        assert!((frame(&ctx, false) - normal).abs() < 1.0);
        assert!((frame(&ctx, false) - normal).abs() < 1.0);
    }

    #[test]
    fn restore_keeps_a_height_the_user_chose() {
        let ctx = egui::Context::default();
        let _ = frame(&ctx, false);
        let id = egui::Id::new(BOTTOM_PANEL);
        ctx.data_mut(|d| {
            d.insert_persisted(
                id,
                egui::PanelState {
                    outer_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 450.0)),
                },
            )
        });
        assert!((frame(&ctx, false) - 450.0).abs() < 1.0);
        assert!(frame(&ctx, true) > 790.0);
        assert!((frame(&ctx, false) - 450.0).abs() < 1.0);
    }
}

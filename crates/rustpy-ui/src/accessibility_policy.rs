//! Shared policy for ordinary egui widgets and all native viewports.
use egui::{Color32, Stroke};
use rustpy_settings::accessibility::{AccessibilitySettings, ScreenReader};
use std::sync::atomic::{AtomicBool, Ordering};
static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}
pub fn configure(ctx: &egui::Context, settings: &AccessibilitySettings) {
    REDUCED_MOTION.store(settings.reduced_motion, Ordering::Relaxed);
    let key = egui::Id::new("accessibility-reader-policy");
    let previous = ctx.data(|data| data.get_temp::<ScreenReader>(key));
    match settings.screen_reader {
        ScreenReader::Always => ctx.enable_accesskit(),
        ScreenReader::Disabled => ctx.disable_accesskit(),
        ScreenReader::Automatic if previous == Some(ScreenReader::Disabled) => {
            ctx.enable_accesskit()
        }
        _ => {}
    }
    ctx.data_mut(|data| data.insert_temp(key, settings.screen_reader));
    ctx.data_mut(|data| {
        data.insert_temp(egui::Id::new("accessibility-settings"), settings.clone())
    });
    apply_reversible_style(ctx, settings);
}
pub fn apply_style(style: &mut egui::Style, settings: &AccessibilitySettings) {
    if settings.reduced_motion {
        style.animation_time = 0.0;
        style.scroll_animation = egui::style::ScrollAnimation::none();
    }
    if settings.focus_indicators {
        let accent = style.visuals.selection.stroke.color;
        style.visuals.widgets.active.bg_stroke = Stroke::new(settings.ring_width(), accent);
        style.visuals.widgets.open.bg_stroke = Stroke::new(settings.ring_width(), accent);
    }
    if settings.contrast_scrollbars {
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_width = style.spacing.scroll.bar_width.max(12.0);
        style.visuals.widgets.inactive.bg_fill = if style.visuals.dark_mode {
            Color32::from_gray(130)
        } else {
            Color32::from_gray(80)
        };
    }
}
pub fn settings(ctx: &egui::Context) -> AccessibilitySettings {
    ctx.data(|data| data.get_temp::<AccessibilitySettings>(egui::Id::new("accessibility-settings")))
        .unwrap_or_default()
}
pub fn focus_indicator(ctx: &egui::Context) {
    let settings = settings(ctx);
    if !settings.focus_indicators || ctx.input(|input| input.viewport().focused == Some(false)) {
        return;
    }
    let Some(id) = ctx.memory(|memory| memory.focused()) else {
        return;
    };
    let Some(response) = ctx.read_response(id) else {
        return;
    };
    if !response.rect.is_finite() || !response.rect.is_positive() {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        id.with("accessible-focus-ring"),
    ));
    painter.rect_stroke(
        response.rect.expand(2.0),
        3.0,
        Stroke::new(
            settings.ring_width(),
            ctx.global_style().visuals.selection.stroke.color,
        ),
        egui::StrokeKind::Outside,
    );
}
/// A reduced-motion progress indicator retains the same role and never steals focus.
pub fn spinner(ui: &mut egui::Ui) -> egui::Response {
    let size = ui.spacing().interact_size.y;
    spinner_sized(ui, size)
}
pub fn spinner_sized(ui: &mut egui::Ui, size: f32) -> egui::Response {
    if !reduced_motion() {
        return ui.add(egui::Spinner::new().size(size));
    }
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator));
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_label(crate::i18n::tr_static("Working"));
        node.set_role(egui::accesskit::Role::ProgressIndicator);
    });
    ui.painter().circle_stroke(
        rect.center(),
        (size * 0.3).max(2.0),
        Stroke::new(2.0, ui.visuals().strong_text_color()),
    );
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reduced_motion_and_contrast_are_actual_style_consumers() {
        let mut style = egui::Style::default();
        let settings = AccessibilitySettings {
            reduced_motion: true,
            contrast_scrollbars: true,
            ..Default::default()
        };
        apply_style(&mut style, &settings);
        assert_eq!(style.animation_time, 0.0);
        assert_eq!(style.scroll_animation.duration.min, 0.0);
        assert!(!style.spacing.scroll.floating);
        assert!(style.spacing.scroll.bar_width >= 12.0);
    }
}

#[derive(Clone)]
struct StyleState {
    base: egui::Style,
    applied: egui::Style,
}
fn apply_reversible_style(ctx: &egui::Context, settings: &AccessibilitySettings) {
    let key = egui::Id::new("accessible-style-base").with(ctx.viewport_id());
    let previous = ctx.data(|data| data.get_temp::<StyleState>(key));
    let mut next = None;
    ctx.global_style_mut(|style|{if let Some(previous)=previous{
macro_rules! restore{($($field:ident).+)=>{if style.$($field).+ == previous.applied.$($field).+ { style.$($field).+ = (previous.base.$($field).+).clone(); }};}
restore!(animation_time);restore!(scroll_animation);restore!(spacing.scroll);restore!(visuals.widgets.inactive.bg_fill);restore!(visuals.widgets.active.bg_stroke);restore!(visuals.widgets.open.bg_stroke);
}let base=style.clone();apply_style(style,settings);next=Some(StyleState{base,applied:style.clone()});});
    if let Some(next) = next {
        ctx.data_mut(|data| data.insert_temp(key, next));
    }
}

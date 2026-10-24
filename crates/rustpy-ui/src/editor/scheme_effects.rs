//! Extra scheme effects rendered over the actual laid-out glyph positions.
use egui::{Color32, Painter, Pos2, Rect, Stroke};
use rustpy_highlight::TextStyle;

pub(super) fn paint(painter: &Painter, rect: Rect, style: TextStyle, fallback: Color32) {
    let color = style
        .effect_color
        .map(|c| Color32::from_rgb(c.0, c.1, c.2))
        .unwrap_or(fallback);
    if style.border {
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    }
    if style.wave {
        let mut points = Vec::new();
        let mut x = rect.left();
        let mut high = false;
        while x < rect.right() && points.len() < 8192 {
            points.push(Pos2::new(x, rect.bottom() - if high { 2.0 } else { 0.0 }));
            high = !high;
            x += 2.0;
        }
        points.push(Pos2::new(rect.right(), rect.bottom()));
        painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_painter_emits_wave_and_border_shapes_for_custom_effects() {
        let context = egui::Context::default();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("scheme-effects"),
            ));
            let mut style = TextStyle::fg(0x123456);
            style.wave = true;
            style.border = true;
            paint(
                &painter,
                Rect::from_min_size(Pos2::new(10.0, 10.0), egui::vec2(40.0, 16.0)),
                style,
                Color32::WHITE,
            );
        });
        let border = output
            .shapes
            .iter()
            .any(|shape| matches!(shape.shape, egui::Shape::Rect(_)));
        let wave = output
            .shapes
            .iter()
            .any(|shape| matches!(shape.shape, egui::Shape::Path(_)));
        output.drop_without_applying_deltas();
        assert!(border);
        assert!(wave);
    }
}

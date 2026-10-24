//! Bounded source context for the editor's existing overview stripe.
use egui::{InputOptions, MouseWheelUnit, TouchPhase, Vec2};
use rustpy_core::{TextRange, TextSize};
use rustpy_text::Document;

pub(super) fn context(doc: &Document, offset: TextSize) -> String {
    let line = doc.line_of(doc.clamp(offset));
    let first = line.saturating_sub(2);
    let last = (line + 2).min(doc.line_count().saturating_sub(1));
    (first..=last)
        .map(|row| {
            let range = doc.line_range(row);
            let end = doc.clamp((range.start() + TextSize::new(720)).min(range.end()));
            let source = doc.slice(TextRange::new(range.start(), end));
            let shortened: String = source.chars().take(180).collect();
            format!(
                "{} {:>5}  {}{}",
                if row == line { "›" } else { " " },
                row + 1,
                shortened,
                if end < range.end() || source.chars().count() > 180 {
                    "…"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Same unit/modifier convention as egui's wheel input, without easing. Zoom
/// remains owned by the existing Ctrl/Cmd-wheel route, not editor scrolling.
#[derive(Default)]
pub(super) struct WheelGesture {
    in_touch: bool,
    modifiers: egui::Modifiers,
}
impl WheelGesture {
    pub(super) fn immediate_wheel(
        &mut self,
        events: &[egui::Event],
        options: &InputOptions,
        viewport_height: f32,
    ) -> Vec2 {
        let mut sum = Vec2::ZERO;
        for event in events {
            let egui::Event::MouseWheel {
                unit,
                delta,
                phase,
                modifiers,
            } = event
            else {
                continue;
            };
            match phase {
                TouchPhase::Start => {
                    self.in_touch = true;
                    self.modifiers = *modifiers;
                    continue;
                }
                TouchPhase::End | TouchPhase::Cancel => {
                    self.in_touch = false;
                    self.modifiers = Default::default();
                    continue;
                }
                TouchPhase::Move => {
                    if self.in_touch {
                        self.modifiers |= *modifiers;
                    } else {
                        self.modifiers = *modifiers;
                    }
                }
            }
            if self.modifiers.matches_any(options.zoom_modifier) {
                continue;
            }
            let mut delta = match unit {
                MouseWheelUnit::Point => *delta,
                MouseWheelUnit::Line => *delta * options.line_scroll_speed,
                MouseWheelUnit::Page => *delta * viewport_height,
            };
            let horizontal = self
                .modifiers
                .matches_any(options.horizontal_scroll_modifier);
            let vertical = self.modifiers.matches_any(options.vertical_scroll_modifier);
            if horizontal && !vertical {
                delta = Vec2::new(delta.x + delta.y, 0.0);
            }
            if vertical && !horizontal {
                delta = Vec2::new(0.0, delta.x + delta.y);
            }
            sum += delta;
        }
        sum
    }
}

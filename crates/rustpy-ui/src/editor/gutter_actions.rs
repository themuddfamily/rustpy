//! Named gutter intentions; source authority and destinations belong to App.
use egui::{Color32, Id, Rect, Sense, Ui};
use rustpy_core::TextSize;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Super,
    Implementations,
    QuickFix,
    ReviewComment(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    pub offset: TextSize,
    pub kind: Kind,
}
#[derive(Default)]
pub(crate) struct Output {
    pub intent: Option<Intent>,
    pub consumed_pointer: bool,
}
/// Root supplies a separate lane, never a breakpoint/fold/run hit rectangle.
pub(crate) fn show(ui: &mut Ui, rect: Rect, id: Id, intent: Intent, color: Color32) -> Output {
    let rect = rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        return Output::default();
    }
    let response = ui.interact(rect, id, Sense::CLICK);
    let (glyph, label) = match intent.kind {
        Kind::Super => ("↑", "Go to super method"),
        Kind::Implementations => ("↓", "Go to implementations"),
        Kind::ReviewComment(_) => ("☵", "Open code review comment"),
        Kind::QuickFix => ("💡", "Show context actions"),
    };
    ui.painter().with_clip_rect(rect).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(13.0),
        color,
    );
    crate::a11y::button(&response, label);
    response.clone().on_hover_text(label);
    Output {
        intent: response.clicked().then_some(intent),
        consumed_pointer: response.contains_pointer()
            && ui.input(|i| {
                i.pointer.primary_down() || i.pointer.button_released(egui::PointerButton::Primary)
            }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_pointer_control_emits_source_intent_without_focus() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let rect = Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(18.0, 18.0));
        let id = Id::new("gutter-intent");
        let target = Intent {
            offset: TextSize::new(17),
            kind: Kind::QuickFix,
        };
        let mut clicked = false;
        for (time, pressed) in [
            (0.0, None),
            (0.02, None),
            (0.04, Some(true)),
            (0.06, Some(false)),
        ] {
            let events = pressed
                .map(|pressed| {
                    vec![
                        egui::Event::PointerMoved(rect.center()),
                        egui::Event::PointerButton {
                            pos: rect.center(),
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                })
                .unwrap_or_default();
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(200.0, 100.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let out = show(ui, rect, id, target, Color32::WHITE);
                    if pressed.is_some() {
                        assert!(out.consumed_pointer);
                    }
                    if let Some(intent) = out.intent {
                        assert_eq!(intent, target);
                        clicked = true;
                    }
                    assert!(!ui.memory(|m| m.has_focus(id)));
                },
            );
            output.drop_without_applying_deltas();
        }
        assert!(clicked);
    }
}

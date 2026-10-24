//! Parameter Info popup (Ctrl+P): every signature of the call (overloads,
//! constructors, conditional definitions) stacked in declaration order; the
//! one the arguments typed so far select is drawn normally and the others
//! dimmed, with the parameter at the caret in bold in each.

use std::ops::Range;

use rustpy_semantic::features::SignatureLabel;

use super::*;

/// Signatures shown at most; the rest are summarized as "+N more".
const MAX_SHOWN: usize = 10;

/// Text sections of a signature label: `(byte range, is the caret's parameter)`.
fn label_sections(sig: &SignatureLabel) -> Vec<(Range<usize>, bool)> {
    let len = sig.label.len();
    match sig.active.and_then(|a| sig.params.get(a).copied()) {
        Some((s, e))
            if s <= e
                && e <= len
                && sig.label.is_char_boundary(s)
                && sig.label.is_char_boundary(e) =>
        {
            [(0..s, false), (s..e, true), (e..len, false)]
                .into_iter()
                .filter(|(r, _)| !r.is_empty())
                .collect()
        }
        _ => vec![(0..len, false)],
    }
}

/// The signatures to list: a window of at most `max` that contains `active`.
fn shown_range(count: usize, active: usize, max: usize) -> Range<usize> {
    if count <= max {
        return 0..count;
    }
    let start = active.saturating_sub(max / 2).min(count - max);
    start..start + max
}

/// Where the popup goes: above the caret line when it fits there, else below
/// when it fits there, else on the roomier side. Returns `(anchor, pivot)`.
fn placement(caret: Rect, height: f32, screen: Rect) -> (Pos2, Align2) {
    let x = caret.left() - 8.0;
    let room_above = caret.top() - 4.0 - screen.top();
    let room_below = screen.bottom() - caret.bottom() - 4.0;
    if height <= room_above || (height > room_below && room_above >= room_below) {
        (Pos2::new(x, caret.top() - 4.0), Align2::LEFT_BOTTOM)
    } else {
        (Pos2::new(x, caret.bottom() + 4.0), Align2::LEFT_TOP)
    }
}

impl IdeApp {
    pub(super) fn draw_param_info(&mut self, ui: &mut egui::Ui, caret: Rect, h: &SignatureHelp) {
        let theme = self.theme.clone();
        let screen = ui.ctx().content_rect();
        let max_width = (screen.width() - 40.0).clamp(200.0, 900.0);
        let font = egui::FontId::new(13.0, crate::theme::mono_family(false, false));
        let bold = egui::FontId::new(13.0, crate::theme::mono_family(true, false));
        let range = shown_range(h.signatures.len(), h.active_signature, MAX_SHOWN);
        let hidden = h.signatures.len() - range.len();
        // Laid out up front: the labels wrap at `max_width` (not at the area's
        // first-frame width) and the popup's height decides where it goes.
        let galleys: Vec<Arc<egui::Galley>> = range
            .clone()
            .map(|i| {
                let sig = &h.signatures[i];
                let selected = i == h.active_signature;
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = max_width;
                let color = if selected {
                    theme.text
                } else {
                    theme.text_disabled
                };
                for (r, active) in label_sections(sig) {
                    let format = match (active, selected) {
                        (true, true) => egui::text::TextFormat {
                            font_id: bold.clone(),
                            color: theme.accent,
                            background: theme.hover_bg,
                            ..Default::default()
                        },
                        (true, false) => egui::text::TextFormat {
                            font_id: bold.clone(),
                            color,
                            ..Default::default()
                        },
                        _ => egui::text::TextFormat {
                            font_id: font.clone(),
                            color,
                            ..Default::default()
                        },
                    };
                    job.append(&sig.label[r], 0.0, format);
                }
                ui.painter().layout_job(job)
            })
            .collect();
        let gap = 3.0;
        let separator = 6.0 + gap;
        let small_line = 16.0 + gap;
        let extra_lines = usize::from(hidden > 0) + usize::from(h.docstring.is_some());
        let height = galleys.iter().map(|g| g.size().y + gap).sum::<f32>()
            + separator * galleys.len().saturating_sub(1) as f32
            + small_line * extra_lines as f32
            + if h.signatures.len() > 1 {
                24.0 + gap
            } else {
                0.0
            }
            + 12.0;
        let (pos, pivot) = placement(caret, height, screen);
        let area = egui::Area::new(ui.id().with("param_info"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = gap;
                        self.parameter_info_cycle_controls(ui, h);
                        for (n, galley) in galleys.into_iter().enumerate() {
                            if n > 0 {
                                ui.separator();
                            }
                            ui.label(galley);
                        }
                        if hidden > 0 {
                            ui.label(
                                egui::RichText::new(format!("+{hidden} more"))
                                    .color(theme.text_dim)
                                    .small(),
                            );
                        }
                        if let Some(d) = &h.docstring {
                            ui.label(egui::RichText::new(d).color(theme.text_dim).small());
                        }
                    });
            });
        area.response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Parameter information")
        });
        if let Some(sig) = h.signatures.get(h.active_signature) {
            let parameter = sig
                .active
                .and_then(|n| sig.params.get(n))
                .and_then(|&(start, end)| sig.label.get(start..end));
            crate::a11y::announce(
                ui.ctx(),
                area.response.id,
                &format!(
                    "Signature {} of {}: {}. Current parameter: {}",
                    h.active_signature + 1,
                    h.signatures.len(),
                    sig.label,
                    parameter.unwrap_or("none")
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_mark_the_caret_parameter() {
        let sig = SignatureLabel {
            label: "f(a, b) -> int".into(),
            params: vec![(2, 3), (5, 6)],
            active: Some(1),
        };
        assert_eq!(
            label_sections(&sig),
            vec![(0..5, false), (5..6, true), (6..14, false)]
        );
        let none = SignatureLabel {
            active: None,
            ..sig.clone()
        };
        assert_eq!(label_sections(&none), vec![(0..14, false)]);
        // Out-of-range parameter ranges never panic.
        let bad = SignatureLabel {
            params: vec![(2, 99)],
            active: Some(0),
            ..sig
        };
        assert_eq!(label_sections(&bad), vec![(0..14, false)]);
    }

    #[test]
    fn popup_goes_above_the_caret_line_unless_only_below_fits() {
        let screen = Rect::from_min_max(Pos2::ZERO, Pos2::new(1400.0, 900.0));
        let caret = Rect::from_min_max(Pos2::new(500.0, 240.0), Pos2::new(502.0, 258.0));
        assert_eq!(
            placement(caret, 100.0, screen),
            (Pos2::new(492.0, 236.0), Align2::LEFT_BOTTOM)
        );
        assert_eq!(
            placement(caret, 450.0, screen),
            (Pos2::new(492.0, 262.0), Align2::LEFT_TOP)
        );
        // Too tall for either side: the roomier one.
        assert_eq!(placement(caret, 2000.0, screen).1, Align2::LEFT_TOP);
        let low = Rect::from_min_max(Pos2::new(500.0, 800.0), Pos2::new(502.0, 818.0));
        assert_eq!(placement(low, 2000.0, screen).1, Align2::LEFT_BOTTOM);
    }

    #[test]
    fn long_overload_lists_keep_the_selected_one_visible() {
        assert_eq!(shown_range(3, 2, 10), 0..3);
        assert_eq!(shown_range(30, 0, 10), 0..10);
        assert_eq!(shown_range(30, 17, 10), 12..22);
        assert_eq!(shown_range(30, 29, 10), 20..30);
    }
}

//! Bounded editor preference helpers; no settings UI or filesystem access.
use super::state::EditorState;
use rustpy_core::{TextRange, TextSize};
use rustpy_highlight::{HighlightTag, Span};
pub fn keywords(state: &EditorState) -> Vec<Span> {
    let Some(association) = state.preferences.association(
        state
            .path
            .as_deref()
            .unwrap_or(std::path::Path::new("untitled")),
        state.doc.line_text(0).trim_end(),
    ) else {
        return Vec::new();
    };
    if association.keywords.is_empty() || state.is_large_file() {
        return Vec::new();
    }
    let text = state.doc.text();
    let mut spans = Vec::new();
    let mut start = None;
    for (offset, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if ch.is_alphanumeric() || ch == '_' {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            if association
                .keywords
                .iter()
                .take(512)
                .any(|key| key == &text[begin..offset])
            {
                spans.push(Span {
                    range: TextRange::new(
                        TextSize::new(begin as u32),
                        TextSize::new(offset as u32),
                    ),
                    tag: HighlightTag::Keyword,
                });
                if spans.len() >= 20_000 {
                    break;
                }
            }
        }
    }
    spans
}
pub fn gutter_visible(settings: &rustpy_settings::EditorSettings, name: &str) -> bool {
    !settings
        .preferences
        .gutter_disabled
        .iter()
        .any(|disabled| disabled == name)
}

pub fn filter_hints(state: &EditorState, hints: &mut Vec<rustpy_semantic::features::InlayHint>) {
    let p = &state.preferences;
    if p.inlay_disabled_languages
        .iter()
        .any(|language| language.eq_ignore_ascii_case(&language_key(state)))
    {
        hints.clear();
        return;
    }
    if p.inlay_parameter_blacklist.is_empty() {
        return;
    }
    let source = state.doc.text();
    hints.retain(|hint| {
        if hint.kind != rustpy_semantic::features::InlayKind::Parameter {
            return true;
        }
        let label = hint.label.trim().trim_end_matches(':');
        let offset = usize::from(hint.offset).min(source.len());
        let before = &source[..rustpy_core::floor_char_boundary(&source, offset)];
        let function = before
            .rfind('(')
            .map(|at| {
                before[..at]
                    .trim_end()
                    .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                    .next()
                    .unwrap_or("")
            })
            .unwrap_or("");
        let qualified = format!("{function}({label})");
        !p.inlay_parameter_blacklist.iter().take(256).any(|pattern| {
            rustpy_settings::editor_pages::mask(pattern, label)
                || rustpy_settings::editor_pages::mask(pattern, &qualified)
        })
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "Consume actual editor row geometry and captured gutter authority"
)]
pub fn paint_inheritors(
    ui: &mut egui::Ui,
    state: &EditorState,
    markers: &[super::view::GutterMarker],
    rows: &super::code_vision_view::HeaderRows,
    origin: egui::Pos2,
    line_h: f32,
    line_ends: &[(usize, egui::Pos2)],
    color: egui::Color32,
) -> Vec<super::gutter_actions::Intent> {
    let mut events = Vec::new();
    for marker in markers
        .iter()
        .filter(|marker| marker.has_implementations && marker.offset <= state.doc.len())
    {
        let line = state.doc.line_of(marker.offset);
        let Some(header) = rows.headers.iter().find(|header| header.line == line) else {
            continue;
        };
        let galley = ui.painter().layout_no_wrap(
            "Implementations / inheritors".into(),
            egui::FontId::proportional(11.0),
            color,
        );
        let position = if rows.inline {
            let Some((_, end)) = line_ends.iter().find(|(item, _)| *item == line) else {
                continue;
            };
            *end + egui::Vec2::new(260.0, 0.0)
        } else {
            egui::pos2(
                (ui.clip_rect().right() - galley.size().x - 8.0).max(origin.x),
                origin.y + header.row as f32 * line_h,
            )
        };
        let rect = egui::Rect::from_min_size(position, galley.size());
        let response = ui.interact(
            rect,
            ui.id().with(("inheritor-lens", marker.offset)),
            egui::Sense::CLICK,
        );
        ui.painter().galley(position, galley, color);
        if response.clicked() {
            events.push(super::gutter_actions::Intent {
                offset: marker.offset,
                kind: super::gutter_actions::Kind::Implementations,
            });
        }
    }
    events
}

pub fn language_key(state: &EditorState) -> String {
    let extension = state
        .path
        .as_deref()
        .and_then(std::path::Path::extension)
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "ts" | "tsx" | "mts" | "cts" => "TypeScript".into(),
        "xml" => "XML".into(),
        "sh" | "bash" | "zsh" => "Shell".into(),
        _ => state.language_label(),
    }
}

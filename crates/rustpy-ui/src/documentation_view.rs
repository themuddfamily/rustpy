//! Shared QuickDoc/inline block rendering, with source-owned image resolution.
use crate::editor::doc_render::{self, DocStyle};
use rustpy_semantic::doc_blocks::{BlockKind, DocBlock};
use std::path::Path;
pub fn show(
    ui: &mut egui::Ui,
    blocks: &[DocBlock],
    source: Option<&Path>,
    scheme: &rustpy_highlight::ColorScheme,
    font: f32,
) -> Option<String> {
    let mut section = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("Sections:");
        for block in blocks
            .iter()
            .filter(|block| matches!(block.kind, BlockKind::Heading(_)))
            .take(32)
        {
            if let BlockKind::Heading(title) = &block.kind {
                if ui.small_button(title).clicked() {
                    section = Some(block.line);
                }
            }
        }
    });
    let style = DocStyle::new(scheme, font);
    let doc =
        doc_render::layout_with_source(ui, blocks, ui.available_width().max(40.0), &style, source);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(doc.width, doc.height), egui::Sense::click());
    doc.paint(ui.painter(), rect.min, &style);
    if let Some(line) = section {
        if let Some(item) = doc.items.iter().find(|item| item.line == line) {
            ui.scroll_to_rect(
                egui::Rect::from_min_size(rect.min + item.pos, item.size),
                Some(egui::Align::TOP),
            );
        }
    }
    if let Some(pointer) = response.hover_pos() {
        if let Some(link) = doc.link_at(ui.ctx(), pointer - rect.min) {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            response.clone().on_hover_text(link);
            if response.clicked() {
                return Some(link.to_string());
            }
        }
    }
    None
}

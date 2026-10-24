//! Geometry for scalar-column virtual positions after the final physical glyph.
use egui::{Pos2, Vec2};
pub(super) fn extend(end: Pos2, column: usize, physical: usize, cell: f32) -> Pos2 {
    end + Vec2::new(column.saturating_sub(physical) as f32 * cell, 0.0)
}
pub(super) fn pointer_column(
    physical: usize,
    end: Pos2,
    pointer: Pos2,
    cell: f32,
    row_height: f32,
    enabled: bool,
) -> Option<usize> {
    if !enabled
        || cell <= 0.0
        || pointer.x <= end.x
        || pointer.y < end.y
        || pointer.y >= end.y + row_height
    {
        return None;
    }
    Some((physical + ((pointer.x - end.x) / cell).round().max(0.0) as usize).min(100_000))
}

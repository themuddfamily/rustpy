//! Viewport-sized page movement and one-shot caret centering.
use super::{EditOptions, EditorState};

/// Preserve file-aware edit options while deriving a page from actual visible
/// height. At least one row moves, including a viewport shorter than one row.
pub fn options_for_viewport(opts: &EditOptions, height: f32, row_height: f32) -> EditOptions {
    let mut out = opts.clone();
    if height.is_finite() && row_height.is_finite() && row_height > 0.0 {
        out.page_lines = (height.max(0.0) / row_height)
            .floor()
            .clamp(1.0, u32::MAX as f32) as usize;
    }
    out
}

/// Stored per EditorView, rather than per document: split views have independent
/// scrolling. A request reveals folded source but never moves a caret/selection.
#[derive(Default, Debug)]
pub struct CenterRequest {
    pending: bool,
}
impl CenterRequest {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request(&mut self, state: &mut EditorState) {
        state.reveal_offset(state.primary().head());
        self.pending = true;
    }
    /// Coordinates are content-local. Caller maps the actual source caret into
    /// its folded/soft-wrapped visual row first. Invalid/unlaid-out frames retain
    /// the request; a valid frame consumes it exactly once (manual scroll wins).
    pub fn take_offset(
        &mut self,
        caret_top: f32,
        row_height: f32,
        viewport_height: f32,
        content_height: f32,
    ) -> Option<f32> {
        if !self.pending
            || ![caret_top, row_height, viewport_height, content_height]
                .iter()
                .all(|v| v.is_finite())
            || row_height <= 0.0
            || viewport_height <= 0.0
            || content_height < 0.0
        {
            return None;
        }
        self.pending = false;
        Some(
            (caret_top + row_height / 2.0 - viewport_height / 2.0)
                .clamp(0.0, (content_height - viewport_height).max(0.0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Caret, Language};
    use rustpy_core::{Position, TextSize};
    #[test]
    fn actual_python_page_movement_tracks_viewport_goal_unicode_and_selection() {
        let source="@decorate(\n    'α',\n)\ndef outer():\n    if True:\n        value = (\n            '界'\n        )\n        return value\noutside = 0";
        let mut e = EditorState::from_text(source, Language::Python);
        e.move_primary_to(Position::new(6, 13));
        let before = e.selections();
        let opts = options_for_viewport(&EditOptions::default(), 45.0, 20.0);
        assert_eq!(opts.page_lines, 2);
        e.page(true, true, &opts);
        assert_eq!(e.primary_position(), Position::new(8, 13));
        assert_eq!(e.primary().sel.anchor, before[0].anchor);
        e.page(false, false, &opts);
        assert_eq!(e.primary_position(), Position::new(6, 13));
        assert!(e.primary().sel.is_empty());
        assert_eq!(e.doc.text(), source);
        assert!(!e.doc.can_undo());
        e.doc.set_read_only(true);
        e.page(true, false, &opts);
        assert_eq!(e.primary_position().line, 8);
        assert!(!e.take_read_only_refusal());
    }
    #[test]
    fn page_options_are_file_aware_and_do_not_mutate_shared_options() {
        let opts = EditOptions {
            page_lines: 30,
            right_margin: 79,
            tab_size: 8,
            ..Default::default()
        };
        let small = options_for_viewport(&opts, 10.0, 20.0);
        assert_eq!(small.page_lines, 1);
        assert_eq!((small.right_margin, small.tab_size), (79, 8));
        assert_eq!(options_for_viewport(&opts, 400.0, 20.0).page_lines, 20);
        assert_eq!(opts.page_lines, 30);
        assert_eq!(options_for_viewport(&opts, 400.0, 0.0).page_lines, 30);
    }
    #[test]
    fn page_rows_follow_existing_soft_wrap_movement() {
        let mut e = EditorState::from_text("abcdefghijklmno", Language::PlainText);
        e.soft_wrap = Some(crate::editor::soft_wrap::SoftWrap {
            original_indent: false,
            cols: 5,
            tab_size: 4,
        });
        e.move_primary_to(Position::new(0, 2));
        let opts = options_for_viewport(&EditOptions::default(), 40.0, 20.0);
        e.page(true, false, &opts);
        assert_eq!(e.primary_position(), Position::new(0, 12));
        e.page(false, false, &opts);
        assert_eq!(e.primary_position(), Position::new(0, 2));
    }
    #[test]
    fn explicit_center_preserves_carets_text_history_and_consumes_once() {
        let mut e = EditorState::from_text("αβ\nvalue", Language::PlainText);
        e.set_carets(vec![Caret::new(TextSize::new(4), TextSize::new(0))]);
        let before = e.selections();
        let mut request = CenterRequest::default();
        request.request(&mut e);
        assert_eq!(request.take_offset(600.0, 20.0, 200.0, 1000.0), Some(510.0));
        assert_eq!(request.take_offset(600.0, 20.0, 200.0, 1000.0), None);
        assert_eq!(e.selections(), before);
        assert_eq!(e.doc.text(), "αβ\nvalue");
        assert!(!e.doc.can_undo());
    }
    #[test]
    fn centering_clamps_edges_and_retains_request_until_layout_exists() {
        let mut e = EditorState::from_text("text", Language::PlainText);
        let mut request = CenterRequest::default();
        request.request(&mut e);
        assert_eq!(request.take_offset(0.0, 20.0, 0.0, 1000.0), None);
        assert_eq!(request.take_offset(0.0, 20.0, 200.0, 1000.0), Some(0.0));
        request.request(&mut e);
        assert_eq!(request.take_offset(980.0, 20.0, 200.0, 1000.0), Some(800.0));
        request.request(&mut e);
        assert_eq!(request.take_offset(20.0, 20.0, 200.0, 60.0), Some(0.0));
    }
}

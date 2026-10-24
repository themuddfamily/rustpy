//! Independent editor instances over one Document authority.
use super::state::{Caret, EditorState, LastOp};
use rustpy_text::{
    Affinity, Document, DocumentId, MarkerId, MarkerPosition, PointDeletion, Selection,
};

#[derive(Debug)]
pub(super) struct Anchors {
    document: Document,
    identity: DocumentId,
    version: u64,
    created_depth: usize,
    carets: Vec<Caret>,
    path: Option<std::path::PathBuf>,
    language: super::state::Language,
    markers: Vec<(MarkerId, MarkerId)>,
    retired: Vec<Vec<(MarkerId, MarkerId)>>,
}
impl Drop for Anchors {
    fn drop(&mut self) {
        for (anchor, head) in &self.markers {
            let _ = self.document.remove_marker(*anchor);
            let _ = self.document.remove_marker(*head);
        }
        for generation in &self.retired {
            for (anchor, head) in generation {
                let _ = self.document.remove_marker(*anchor);
                let _ = self.document.remove_marker(*head);
            }
        }
    }
}
impl EditorState {
    fn remember_shared_carets(&mut self) {
        let mut document = self.doc.share();
        let identity = document.identity();
        let version = document.version();
        let mut retired = Vec::new();
        if let Some(mut previous) = self.shared_anchors.take() {
            if document.shares_authority(&previous.document)
                && document.identity() == previous.identity
            {
                retired.append(&mut previous.retired);
                // Rollback may restore removed handles from the transaction snapshot.
                // Retain their cleanup authority until the outermost transaction ends.
                if document.transaction_depth() > previous.created_depth {
                    retired.push(std::mem::take(&mut previous.markers));
                }
            }
        }
        if document.transaction_depth() == 0 {
            for generation in retired.drain(..) {
                for (anchor, head) in generation {
                    let _ = document.remove_marker(anchor);
                    let _ = document.remove_marker(head);
                }
            }
        }
        let mut markers = Vec::new();
        for caret in &self.carets {
            let anchor = document
                .point_marker(
                    document.clamp(caret.sel.anchor),
                    if caret.has_selection() {
                        Affinity::Before
                    } else {
                        Affinity::After
                    },
                    PointDeletion::Clamp,
                )
                .expect("clamped anchor");
            let head = document
                .point_marker(
                    document.clamp(caret.sel.head),
                    Affinity::After,
                    PointDeletion::Clamp,
                )
                .expect("clamped head");
            markers.push((anchor, head));
        }
        let created_depth = document.transaction_depth();
        self.shared_anchors = Some(Anchors {
            document,
            identity,
            version,
            created_depth,
            carets: self.carets.clone(),
            path: self.path.clone(),
            language: self.language,
            markers,
            retired,
        });
    }
    /// Public path/language edits (rename, scratch language changes) are forwarded at the App boundary.
    pub fn shared_file_change(
        &self,
    ) -> Option<(
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
        super::state::Language,
    )> {
        self.shared_anchors
            .as_ref()
            .filter(|previous| {
                self.doc.shares_authority(&previous.document)
                    && (self.path != previous.path || self.language != previous.language)
            })
            .map(|previous| (previous.path.clone(), self.path.clone(), self.language))
    }

    /// Refresh inactive-view positions after another instance edits the shared document.
    /// Explicit caret movement wins over automatic remapping; views never share caret arrays.
    pub fn sync_shared_view(&mut self) {
        let Some(previous) = self.shared_anchors.as_ref() else {
            return;
        };
        let same_document = self.doc.shares_authority(&previous.document)
            && self.doc.identity() == previous.identity;
        if same_document
            && self.doc.version() == previous.version
            && self.carets == previous.carets
            && self.path == previous.path
            && self.language == previous.language
            && (previous.retired.is_empty() || self.doc.transaction_depth() > 0)
        {
            return;
        }
        let reveal = self.scroll_to_caret;
        if same_document && self.doc.version() != previous.version && self.carets == previous.carets
        {
            let mapped = previous
                .carets
                .iter()
                .zip(&previous.markers)
                .enumerate()
                .map(|(index, (caret, (anchor, head)))| {
                    let point = |id, is_anchor| {
                        let at = |id| match self.doc.marker(id) {
                            Ok(Some(MarkerPosition::Point(offset))) => Some(offset),
                            _ => None,
                        };
                        at(id)
                            .or_else(|| {
                                previous.retired.iter().find_map(|generation| {
                                    generation.get(index).and_then(|(anchor, head)| {
                                        at(if is_anchor { *anchor } else { *head })
                                    })
                                })
                            })
                            .unwrap_or_else(|| self.doc.clamp(caret.head()))
                    };
                    let mut moved = *caret;
                    moved.sel = Selection::new(point(*anchor, true), point(*head, false));
                    moved
                })
                .collect();
            self.set_carets(mapped);
            self.last_op = LastOp::Other;
        } else if !same_document {
            self.set_carets(self.carets.clone());
        }
        self.scroll_to_caret = reveal;
        self.remember_shared_carets();
    }
    /// New caret/folding/semantic/render caches, sharing actual text/Undo/save/format state.
    pub fn split_view(&mut self) -> Self {
        self.sync_shared_view();
        self.remember_shared_carets();
        let mut view = Self::new(self.doc.share(), self.path.clone());
        view.language = self.language;
        if let Some(grammar) = self.textmate_grammar() {
            view.language = self.base_language();
            view.set_textmate_grammar(Some((
                std::sync::Arc::clone(grammar),
                self.language_label(),
            )));
        }
        view.carets = self.carets.clone();
        view.overwrite_mode = self.overwrite_mode;
        view.semantic_highlighting = self.semantic_highlighting;
        view.rainbow_brackets = self.rainbow_brackets;
        view.indent_override = self.indent_override;
        view.detected_indent = self.detected_indent;
        view.last_op = LastOp::Other;
        self.last_op = LastOp::Other;
        self.doc.break_undo_group();
        view.remember_shared_carets();
        view
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditOptions, Language};
    use rustpy_text::{LineEnding, TextSize};
    #[test]
    fn real_editor_instances_share_unicode_edits_undo_and_format_but_not_carets_or_reveal() {
        let mut first = EditorState::from_text("π = 1\nsecond\n", Language::Python);
        first.set_carets(vec![Caret::at(TextSize::new(7))]);
        let original_parse = first.syntax().syntax_node().to_string();
        let mut second = first.split_view();
        assert_eq!(second.syntax().syntax_node().to_string(), original_parse);
        second.set_carets(vec![Caret::at(TextSize::new(0))]);
        second.sync_shared_view();
        second.scroll_to_caret = false;
        first.insert_text("🙂", &EditOptions::default());
        second.sync_shared_view();
        assert_eq!(first.doc.text(), second.doc.text());
        assert_eq!(second.syntax().syntax_node().to_string(), first.doc.text());
        assert_ne!(second.syntax().syntax_node().to_string(), original_parse);
        assert_eq!(second.primary().head(), TextSize::new(0));
        assert!(!second.scroll_to_caret);
        assert_ne!(first.primary().head(), second.primary().head());
        assert!(second.undo());
        first.sync_shared_view();
        assert_eq!(first.doc.text(), "π = 1\nsecond\n");
        assert!(first.redo());
        assert_eq!(second.doc.text(), first.doc.text());
        first.doc.set_line_ending(LineEnding::CrLf);
        assert_eq!(second.doc.line_ending(), LineEnding::CrLf);
        first.doc.mark_saved();
        assert!(!second.doc.is_modified());
        second.doc.set_read_only(true);
        let before = first.doc.text();
        first.insert_text("bad", &EditOptions::default());
        assert_eq!(second.doc.text(), before);
    }
    #[test]
    fn inactive_caret_tracks_shared_edit_marker_and_closed_view_does_not_lose_history() {
        let mut first = EditorState::from_text("alpha\nbeta\n", Language::PlainText);
        first.set_carets(vec![Caret::at(TextSize::new(8))]);
        let mut second = first.split_view();
        second.set_carets(vec![Caret::at(TextSize::new(0))]);
        second.sync_shared_view();
        second.insert_text("π\n", &EditOptions::default());
        first.sync_shared_view();
        assert_eq!(first.primary().head(), TextSize::new(11));
        assert_eq!(first.primary_position().line, 2);
        drop(second);
        assert!(first.undo());
        assert_eq!(first.doc.text(), "alpha\nbeta\n");
    }
    #[test]
    fn view_anchor_cleanup_survives_real_shared_transaction_rollback() {
        let mut first = EditorState::from_text("alpha\nbeta\n", Language::PlainText);
        let mut second = first.split_view();
        let original = first.shared_anchors.as_ref().unwrap().markers[0].0;
        let token = second.begin_transaction("Shared transaction").unwrap();
        second.insert_text("π\n", &EditOptions::default());
        first.sync_shared_view();
        assert!(!first.shared_anchors.as_ref().unwrap().retired.is_empty());
        second.rollback_transaction(token).unwrap();
        first.sync_shared_view();
        assert!(first.shared_anchors.as_ref().unwrap().retired.is_empty());
        assert_eq!(
            first.doc.marker(original),
            Err(rustpy_text::MarkerError::UnknownMarker)
        );
        assert_eq!(first.doc.text(), "alpha\nbeta\n");
        assert_eq!(first.primary().head(), TextSize::new(0));
    }
}

//! General virtual positions, separate from byte-addressed source selections.
//! Columns are Unicode scalar columns, matching the existing column editor.
use super::{map_offset, Caret, EditorState, LastOp};
use rustpy_core::{Position, TextSize};
use rustpy_text::{DocumentId, Edit, EditGroup};

const MAX_VIRTUAL_COLUMN: u32 = 100_000;
type BlockStamp = (DocumentId, u64, Vec<Caret>, Position, Position);
type VirtualHistory = (
    DocumentId,
    rustpy_text::HistoryId,
    Vec<Caret>,
    Vec<VirtualCaret>,
    bool,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualCaret {
    pub anchor: Position,
    pub head: Position,
}

#[derive(Debug)]
pub struct VirtualSpace {
    pub enabled: bool,
    pub allow_in_block: bool,
    stamp: Option<(DocumentId, u64, Vec<Caret>)>,
    positions: Vec<VirtualCaret>,
    history: std::collections::VecDeque<VirtualHistory>,
    block_stamp: Option<BlockStamp>,
    block_positions: bool,
}

impl Default for VirtualSpace {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_in_block: true,
            stamp: None,
            positions: Vec::new(),
            history: Default::default(),
            block_stamp: None,
            block_positions: false,
        }
    }
}

impl EditorState {
    pub(in crate::editor) fn remember_virtual_block(&mut self, anchor: Position, head: Position) {
        self.virtual_space.block_stamp = Some((
            self.doc.identity(),
            self.doc.version(),
            self.carets.clone(),
            anchor,
            head,
        ));
    }
    fn virtual_block_active(&self) -> bool {
        self.virtual_space.allow_in_block
            && self
                .virtual_space
                .block_stamp
                .as_ref()
                .is_some_and(|(id, version, carets, a, h)| {
                    *id == self.doc.identity()
                        && *version == self.doc.version()
                        && *carets == self.carets
                        && self.block == Some((*a, *h))
                })
    }
    pub(in crate::editor) fn virtual_positions_enabled(&self) -> bool {
        self.virtual_space.enabled
            || self.virtual_block_active()
            || (self.virtual_space.allow_in_block
                && self.virtual_space.block_positions
                && self
                    .virtual_space
                    .stamp
                    .as_ref()
                    .is_some_and(|(id, version, carets)| {
                        *id == self.doc.identity()
                            && *version == self.doc.version()
                            && *carets == self.carets
                    }))
    }

    pub fn set_virtual_space_policy(&mut self, general: bool, block: bool) {
        self.virtual_space.allow_in_block = block;
        self.set_virtual_space_enabled(general || (self.column_mode && block));
    }

    /// Root's view/settings bridge sets this before handling editor input.
    pub fn set_virtual_space_enabled(&mut self, enabled: bool) {
        if self.virtual_space.enabled != enabled {
            self.virtual_space.enabled = enabled;
            self.virtual_space.stamp = None;
            self.virtual_space.positions.clear();
            self.virtual_space.history.clear();
            if !enabled {
                for caret in &mut self.carets {
                    caret.goal_column = None;
                }
            }
        }
    }

    /// Actual source offset plus any authorized beyond-EOL scalar column.
    pub fn virtual_caret(&self, index: usize) -> VirtualCaret {
        let caret = &self.carets[index];
        if self.virtual_positions_enabled()
            && self
                .virtual_space
                .stamp
                .as_ref()
                .is_some_and(|(id, version, carets)| {
                    *id == self.doc.identity()
                        && *version == self.doc.version()
                        && carets.get(index) == Some(caret)
                })
        {
            return self.virtual_space.positions[index];
        }
        if self.virtual_block_active() {
            let (_, _, _, anchor, head) = self.virtual_space.block_stamp.as_ref().unwrap();
            let line = self.doc.line_of(caret.head()) as u32;
            return VirtualCaret {
                anchor: Position::new(line, anchor.column),
                head: Position::new(line, head.column),
            };
        }
        let anchor = self.doc.offset_to_position(caret.sel.anchor);
        let mut head = self.doc.offset_to_position(caret.head());
        if self.virtual_space.enabled
            && self.soft_wrap.is_none()
            && head.column == self.doc.line_text(head.line as usize).chars().count() as u32
        {
            head.column = head
                .column
                .max(caret.goal_column.unwrap_or(head.column))
                .min(MAX_VIRTUAL_COLUMN);
        }
        VirtualCaret {
            anchor: if caret.sel.is_empty() { head } else { anchor },
            head,
        }
    }

    /// Pointer/navigation bridge. Virtual positions are clipped only when disabled;
    /// neither navigation nor selection inserts whitespace into the document.
    pub fn set_virtual_carets(&mut self, positions: Vec<VirtualCaret>) {
        let authorized = self.virtual_positions_enabled();
        self.set_virtual_carets_authorized(positions, authorized);
    }
    fn set_virtual_carets_authorized(&mut self, positions: Vec<VirtualCaret>, authorized: bool) {
        let last = self.doc.line_count().saturating_sub(1) as u32;
        let positions: Vec<_> = positions
            .into_iter()
            .map(|mut position| {
                for p in [&mut position.anchor, &mut position.head] {
                    p.line = p.line.min(last);
                    let end = self.doc.line_text(p.line as usize).chars().count() as u32;
                    p.column = p
                        .column
                        .min(if authorized { MAX_VIRTUAL_COLUMN } else { end });
                }
                position
            })
            .collect();
        let carets: Vec<_> = positions
            .iter()
            .map(|position| Caret {
                sel: rustpy_text::Selection::new(
                    self.doc.position_to_offset(position.anchor),
                    self.doc.position_to_offset(position.head),
                ),
                goal_column: authorized.then(|| {
                    if let Some(wrap) = self.soft_wrap {
                        let length = self
                            .doc
                            .line_text(position.head.line as usize)
                            .chars()
                            .count() as u32;
                        let layout = self.wrapped_line(position.head.line as usize, wrap);
                        layout.row_col(position.head.column.min(length) as usize).1 as u32
                            + position.head.column.saturating_sub(length)
                    } else {
                        position.head.column
                    }
                }),
            })
            .collect();
        self.set_carets(carets.clone());
        // Normalization can merge true overlaps; retain the surviving caret's
        // position authority, never attach an old index to another caret.
        self.virtual_space.positions = self
            .carets
            .iter()
            .map(|caret| {
                carets
                    .iter()
                    .position(|candidate| candidate == caret)
                    .map(|i| positions[i])
                    .unwrap_or_else(|| VirtualCaret {
                        anchor: self.doc.offset_to_position(caret.sel.anchor),
                        head: self.doc.offset_to_position(caret.head()),
                    })
            })
            .collect();
        self.virtual_space.stamp =
            Some((self.doc.identity(), self.doc.version(), self.carets.clone()));
        self.virtual_space.block_positions = authorized && !self.virtual_space.enabled;
    }

    pub(in crate::editor) fn move_virtual_horizontal(&mut self, delta: i32, select: bool) -> bool {
        if !self.virtual_space.enabled {
            return false;
        }
        let select = select || self.sticky_selection;
        let positions = (0..self.carets.len())
            .map(|i| {
                let p = self.virtual_caret(i);
                let mut head = p.head;
                if !select && p.anchor != p.head {
                    let key = |p: Position| (p.line, p.column);
                    head = if (delta < 0) == (key(p.anchor) < key(p.head)) {
                        p.anchor
                    } else {
                        p.head
                    };
                } else {
                    let length = self.doc.line_text(head.line as usize).chars().count() as u32;
                    if delta > 0 && head.column >= length {
                        head.column = head.column.saturating_add(1).min(MAX_VIRTUAL_COLUMN);
                    } else if delta < 0 && head.column > length {
                        head.column -= 1;
                    } else {
                        let offset = self.doc.position_to_offset(head);
                        head = self.doc.offset_to_position(if delta < 0 {
                            self.doc.prev_boundary(offset)
                        } else {
                            self.doc.next_boundary(offset)
                        });
                    }
                }
                VirtualCaret {
                    anchor: if select { p.anchor } else { head },
                    head,
                }
            })
            .collect();
        self.set_virtual_carets(positions);
        self.last_op = LastOp::Other;
        true
    }

    pub(in crate::editor) fn virtual_excess(&self, index: usize) -> u32 {
        if !self.virtual_positions_enabled() {
            return 0;
        }
        let head = self.virtual_caret(index).head;
        head.column
            .saturating_sub(self.doc.line_text(head.line as usize).chars().count() as u32)
    }

    pub(in crate::editor) fn restore_virtual_gaps(&mut self, carets: &[Caret], gaps: &[u32]) {
        if !self.virtual_space.enabled
            && !(self.virtual_space.allow_in_block && gaps.iter().any(|gap| *gap > 0))
        {
            return;
        }
        let positions = carets
            .iter()
            .zip(gaps)
            .map(|(caret, gap)| {
                let mut head = self.doc.offset_to_position(self.doc.clamp(caret.head()));
                head.column = head.column.saturating_add(*gap);
                VirtualCaret { anchor: head, head }
            })
            .collect();
        self.set_virtual_carets_authorized(positions, true);
    }

    pub(in crate::editor) fn selected_caret_text(&self, index: usize) -> String {
        if !self.virtual_positions_enabled() {
            return self.doc.slice(self.carets[index].range());
        }
        self.virtual_selection_text(self.virtual_caret(index))
    }

    pub(in crate::editor) fn virtual_selection_text(&self, position: VirtualCaret) -> String {
        let (start, end) = if (position.anchor.line, position.anchor.column)
            <= (position.head.line, position.head.column)
        {
            (position.anchor, position.head)
        } else {
            (position.head, position.anchor)
        };
        let mut text = self.doc.slice(rustpy_core::TextRange::new(
            self.doc.position_to_offset(start),
            self.doc.position_to_offset(end),
        ));
        let length = self.doc.line_text(end.line as usize).chars().count() as u32;
        let excess = if start.line == end.line {
            end.column.saturating_sub(length.max(start.column))
        } else {
            end.column.saturating_sub(length)
        };
        text.push_str(&" ".repeat(excess as usize));
        text
    }

    pub fn toggle_sticky_selection(&mut self) {
        self.sticky_selection = !self.sticky_selection;
    }

    /// Escape ends sticky mode and clears selection without changing the text.
    pub fn cancel_sticky_selection(&mut self) -> bool {
        if !self.sticky_selection {
            return false;
        }
        self.sticky_selection = false;
        let positions = (0..self.carets.len())
            .map(|i| {
                let head = self.virtual_caret(i).head;
                VirtualCaret { anchor: head, head }
            })
            .collect();
        self.set_virtual_carets(positions);
        true
    }

    pub(in crate::editor) fn restore_virtual_history(&mut self) {
        if !self.virtual_space.enabled && !self.virtual_space.allow_in_block {
            return;
        }
        let saved = self
            .virtual_space
            .history
            .iter()
            .rev()
            .find(|(id, history, carets, _, block)| {
                (self.virtual_space.enabled || *block)
                    && *id == self.doc.identity()
                    && *history == self.doc.current_history()
                    && carets
                        .iter()
                        .all(|saved| self.carets.iter().any(|current| current.sel == saved.sel))
                    && self
                        .carets
                        .iter()
                        .all(|current| carets.iter().any(|saved| current.sel == saved.sel))
            })
            .map(|entry| (entry.2.clone(), entry.3.clone(), entry.4));
        if let Some((carets, positions, block)) = saved {
            self.restore_virtual_state(carets, positions, block);
        }
    }

    fn restore_virtual_state(
        &mut self,
        carets: Vec<Caret>,
        positions: Vec<VirtualCaret>,
        block: bool,
    ) {
        // Restore the recorded caret metadata as well as its virtual columns.
        // Reconstructing carets from positions invents vertical goal columns,
        // changing the pre-edit rectangle after Undo or a refused transaction.
        self.set_carets(carets);
        self.virtual_space.positions = positions;
        self.virtual_space.block_positions = block;
        self.virtual_space.stamp =
            Some((self.doc.identity(), self.doc.version(), self.carets.clone()));
    }

    pub(in crate::editor) fn virtual_history_snapshot(&self) -> Option<VirtualHistory> {
        self.has_virtual_padding().then(|| {
            (
                self.doc.identity(),
                self.doc.current_history(),
                self.carets.clone(),
                (0..self.carets.len())
                    .map(|i| self.virtual_caret(i))
                    .collect(),
                self.virtual_space.block_positions || self.virtual_block_active(),
            )
        })
    }
    pub(in crate::editor) fn remember_virtual_history(&mut self, snapshot: Option<VirtualHistory>) {
        if let Some(snapshot) = snapshot {
            self.virtual_space.history.push_back(snapshot);
            while self.virtual_space.history.len() > 128 {
                self.virtual_space.history.pop_front();
            }
        }
    }

    pub(in crate::editor) fn clear_virtual_history(&mut self) {
        self.virtual_space.history.clear();
        self.virtual_space.stamp = None;
        self.virtual_space.block_stamp = None;
    }

    pub(crate) fn has_virtual_padding(&self) -> bool {
        self.virtual_positions_enabled()
            && (0..self.carets.len()).any(|i| {
                let p = self.virtual_caret(i);
                [p.anchor, p.head]
                    .iter()
                    .any(|p| p.column > self.doc.line_text(p.line as usize).chars().count() as u32)
            })
    }

    /// Materialize once per line, inside the same undo transaction as the actual
    /// command. Multiple carets on a short line share one padding insertion.
    pub(in crate::editor) fn edit_in_virtual_space(
        &mut self,
        label: &str,
        edit: impl FnOnce(&mut Self),
    ) -> bool {
        self.try_edit_in_virtual_space(label, |state| {
            edit(state);
            true
        })
        .is_some()
    }

    pub(crate) fn try_edit_in_virtual_space(
        &mut self,
        label: &str,
        edit: impl FnOnce(&mut Self) -> bool,
    ) -> Option<bool> {
        if !self.has_virtual_padding() {
            return None;
        }
        let positions: Vec<_> = (0..self.carets.len())
            .map(|i| self.virtual_caret(i))
            .collect();
        let before = self
            .virtual_history_snapshot()
            .expect("virtual positions own this edit");
        let token = match self.begin_transaction(label) {
            Ok(token) => token,
            Err(_) => {
                self.read_only_refused = true;
                return Some(false);
            }
        };
        let mut columns = std::collections::BTreeMap::<u32, u32>::new();
        for position in &positions {
            for p in [position.anchor, position.head] {
                columns
                    .entry(p.line)
                    .and_modify(|column| *column = (*column).max(p.column))
                    .or_insert(p.column);
            }
        }
        let edits: Vec<_> = columns
            .into_iter()
            .filter_map(|(line, column)| {
                let text = self.doc.line_text(line as usize);
                let length = text.chars().count() as u32;
                (column > length).then(|| {
                    Edit::insert(
                        self.doc.line_range(line as usize).start()
                            + TextSize::new(text.len() as u32),
                        " ".repeat((column - length) as usize),
                    )
                })
            })
            .collect();
        let offset = |p: Position| {
            let actual = self.doc.position_to_offset(p);
            let length = self.doc.line_text(p.line as usize).chars().count() as u32;
            map_offset(&edits, actual, false) + TextSize::new(p.column.saturating_sub(length))
        };
        let carets = positions
            .iter()
            .map(|p| Caret::new(offset(p.anchor), offset(p.head)))
            .collect();
        if !self.apply(edits, carets, EditGroup::Open, Some(label)) {
            self.rollback_transaction(token)
                .expect("virtual edit owns transaction");
            self.restore_virtual_state(before.2.clone(), positions, before.4);
            return Some(false);
        }
        if !edit(self) {
            self.rollback_transaction(token)
                .expect("virtual edit owns transaction");
            self.restore_virtual_state(before.2.clone(), positions, before.4);
            return Some(false);
        }
        self.commit_transaction(token)
            .expect("virtual edit owns transaction");
        self.remember_virtual_history(Some(before));
        self.last_op = LastOp::Other;
        Some(true)
    }
}

#[cfg(test)]
#[path = "virtual_space_tests.rs"]
mod tests;

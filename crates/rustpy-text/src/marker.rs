//! Document-owned markers, mapped eagerly through every accepted edit batch.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Edit, TextRange, TextSize};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_id() -> u64 {
    let mut current = NEXT_ID.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("document marker identity space exhausted");
        match NEXT_ID.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return current,
            Err(observed) => current = observed,
        }
    }
}

/// Identity of one document incarnation, independent of its path and version.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentId(u64);

/// A document-bound marker identity. Removed/rolled-back IDs are never reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerId {
    document: DocumentId,
    serial: u64,
}

/// Position at an insertion boundary, including all inserts at the same offset.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PointDeletion {
    /// Keep the point, clamping inside a replacement to its chosen affinity edge.
    Clamp,
    /// Invalidate only strictly inside removed text. Boundary zero is preserved.
    InvalidateInterior,
    /// Invalidate in the removed half-open interval, including its start.
    InvalidateCovered,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RangeDeletion {
    /// Map both endpoints. Crossed endpoints collapse to an empty range at mapped start.
    Clamp,
    /// Invalidate if any original range bytes are removed/replaced. Empty ranges survive.
    InvalidateOverlap,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MarkerPosition {
    Point(TextSize),
    Range(TextRange),
}

/// A cursor into chronological, normalized, pre-step-coordinate document edits.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct JournalCursor {
    pub(crate) document: DocumentId,
    pub(crate) version: u64,
}
impl JournalCursor {
    pub fn document(self) -> DocumentId {
        self.document
    }
    pub fn version(self) -> u64 {
        self.version
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MarkerError {
    #[error("marker or journal cursor belongs to another document incarnation")]
    ForeignDocument,
    #[error("marker was removed or created in a rolled-back transaction")]
    UnknownMarker,
    #[error("marker offset {0:?} is out of bounds")]
    OutOfBounds(TextSize),
    #[error("marker offset {0:?} is not a UTF-8 character boundary")]
    NotCharBoundary(TextSize),
    #[error("marker line {0} is out of bounds")]
    LineOutOfBounds(usize),
    #[error("edit journal no longer covers this cursor")]
    JournalExpired,
}

#[derive(Clone, Debug)]
enum Policy {
    Line(TrackedLineSpan),
    Point {
        affinity: Affinity,
        deletion: PointDeletion,
    },
    Range {
        start: Affinity,
        end: Affinity,
        deletion: RangeDeletion,
    },
}
#[derive(Clone, Debug)]
struct Entry {
    position: Option<MarkerPosition>,
    policy: Policy,
}

#[derive(Clone, Debug)]
pub(crate) struct MarkerStore {
    document: DocumentId,
    entries: BTreeMap<MarkerId, Entry>,
}
impl MarkerStore {
    pub fn new() -> Self {
        Self {
            document: DocumentId(next_id()),
            entries: BTreeMap::new(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn document(&self) -> DocumentId {
        self.document
    }
    pub fn point(&mut self, at: TextSize, affinity: Affinity, deletion: PointDeletion) -> MarkerId {
        self.insert(
            MarkerPosition::Point(at),
            Policy::Point { affinity, deletion },
        )
    }
    pub fn range(
        &mut self,
        range: TextRange,
        start: Affinity,
        end: Affinity,
        deletion: RangeDeletion,
    ) -> MarkerId {
        self.insert(
            MarkerPosition::Range(range),
            Policy::Range {
                start,
                end,
                deletion,
            },
        )
    }
    pub fn line(&mut self, range: TextRange) -> MarkerId {
        self.insert(
            MarkerPosition::Point(range.start()),
            Policy::Line(TrackedLineSpan { range }),
        )
    }
    fn insert(&mut self, position: MarkerPosition, policy: Policy) -> MarkerId {
        let id = MarkerId {
            document: self.document,
            serial: next_id(),
        };
        self.entries.insert(
            id,
            Entry {
                position: Some(position),
                policy,
            },
        );
        id
    }
    fn check(&self, id: MarkerId) -> Result<(), MarkerError> {
        if id.document != self.document {
            return Err(MarkerError::ForeignDocument);
        }
        if !self.entries.contains_key(&id) {
            return Err(MarkerError::UnknownMarker);
        }
        Ok(())
    }
    pub fn get(&self, id: MarkerId) -> Result<Option<MarkerPosition>, MarkerError> {
        self.check(id)?;
        Ok(self.entries[&id].position)
    }
    pub fn remove(&mut self, id: MarkerId) -> Result<(), MarkerError> {
        self.check(id)?;
        self.entries.remove(&id);
        Ok(())
    }
    /// New logical rows introduced by a replacement do not inherit the owner.
    /// Cap the mapped owned content at its first row after the accepted rope edit.
    pub fn finish_lines(&mut self, rope: &ropey::Rope) {
        for entry in self.entries.values_mut() {
            if let Policy::Line(span) = &mut entry.policy {
                if entry.position.is_some() {
                    let start = usize::from(span.range.start());
                    let line = rope.byte_to_line(start);
                    let slice = rope.line(line);
                    let lf = slice.len_chars() > 0 && slice.char(slice.len_chars() - 1) == '\n';
                    let end = rope.line_to_byte(line) + slice.len_bytes() - usize::from(lf);
                    span.range = TextRange::new(
                        span.range.start(),
                        span.range.end().min(TextSize::new(end as u32)),
                    );
                }
            }
        }
    }

    pub fn apply(&mut self, edits: &[&Edit], rope: &ropey::Rope) {
        for entry in self.entries.values_mut() {
            if let Policy::Line(span) = &mut entry.policy {
                if entry.position.is_some() {
                    entry.position = span
                        .apply(edits, rope)
                        .then_some(MarkerPosition::Point(span.range.start()));
                }
                continue;
            }
            entry.position = match (entry.position, &entry.policy) {
                (Some(MarkerPosition::Point(at)), Policy::Point { affinity, deletion }) => {
                    let invalid = edits.iter().any(|edit| {
                        let (s, e) = (edit.range.start(), edit.range.end());
                        match deletion {
                            PointDeletion::Clamp => false,
                            PointDeletion::InvalidateInterior => s < at && at < e,
                            PointDeletion::InvalidateCovered => s <= at && at < e,
                        }
                    });
                    (!invalid).then(|| MarkerPosition::Point(map_point(at, *affinity, edits)))
                }
                (
                    Some(MarkerPosition::Range(range)),
                    Policy::Range {
                        start,
                        end,
                        deletion,
                    },
                ) => {
                    let invalid = *deletion == RangeDeletion::InvalidateOverlap
                        && !range.is_empty()
                        && edits.iter().any(|edit| {
                            !edit.range.is_empty()
                                && edit.range.start() < range.end()
                                && range.start() < edit.range.end()
                        });
                    (!invalid).then(|| {
                        let s = map_point(range.start(), *start, edits);
                        let e = map_point(range.end(), *end, edits).max(s);
                        MarkerPosition::Range(TextRange::new(s, e))
                    })
                }
                (None, _) => None,
                _ => unreachable!("marker policy and position always have matching kinds"),
            };
        }
    }
}

/// Accepted batches are sorted, non-overlapping edits in the same original coordinates.
/// Keep accumulating at a boundary: it may be a replacement end AND several insertion starts.
fn map_point(at: TextSize, affinity: Affinity, edits: &[&Edit]) -> TextSize {
    let at = u32::from(at) as i64;
    let mut delta = 0_i64;
    for edit in edits {
        let s = u32::from(edit.range.start()) as i64;
        let e = u32::from(edit.range.end()) as i64;
        let n = edit.text.len() as i64;
        if e < at || (e == at && s < e) {
            delta += n - (e - s);
            continue;
        }
        if s > at {
            break;
        }
        if s == e {
            if affinity == Affinity::After {
                delta += n;
            }
        } else {
            return TextSize::new(
                (s + delta + if affinity == Affinity::After { n } else { 0 }) as u32,
            );
        }
    }
    TextSize::new((at + delta) as u32)
}

/// Content owned by an original logical line, even after joining a neighboring row.
/// Boundary ownership is derived from the pre-step rope, never a bounded edit journal.
#[derive(Clone, Debug)]
struct TrackedLineSpan {
    range: TextRange,
}
impl TrackedLineSpan {
    fn apply(&mut self, edits: &[&Edit], rope: &ropey::Rope) -> bool {
        let start = usize::from(self.range.start());
        let end = usize::from(self.range.end());
        let line = rope.byte_to_line(start);
        let physical_start = rope.line_to_byte(line);
        let slice = rope.line(line);
        let has_newline = slice.len_chars() > 0 && slice.char(slice.len_chars() - 1) == '\n';
        let physical_end = physical_start + slice.len_bytes() - usize::from(has_newline);
        let ending = has_newline.then(|| {
            TextRange::new(
                TextSize::new(physical_end as u32),
                TextSize::new(physical_end as u32 + 1),
            )
        });
        let preceding = (physical_start > 0).then(|| {
            TextRange::new(
                TextSize::new(physical_start as u32 - 1),
                TextSize::new(physical_start as u32),
            )
        });
        let replacement_body = edits.iter().any(|edit| {
            !edit.text.is_empty()
                && !edit.range.is_empty()
                && if self.range.is_empty() {
                    edit.range.start() <= self.range.start()
                        && self.range.start() < edit.range.end()
                } else {
                    edit.range.start() < self.range.end() && self.range.start() < edit.range.end()
                }
        });
        let invalid = !replacement_body
            && if self.range.is_empty() {
                // A blank middle row owns its LF; the trailing empty row owns the
                // preceding LF. An empty document has no removable line boundary.
                ending
                    .or(preceding)
                    .is_some_and(|guard| pure_deletion_covers(guard, edits))
            } else {
                let joined = start != physical_start || end != physical_end;
                pure_deletion_covers(self.range, edits)
                    && (joined
                        || ending.is_some_and(|guard| pure_deletion_covers(guard, edits))
                        || (!has_newline
                            && preceding.is_some_and(|guard| pure_deletion_covers(guard, edits)))
                        || (!has_newline && physical_start == 0))
            };
        if invalid {
            return false;
        }
        let empty = self.range.is_empty();
        let replaces_start = pure_deletion_covers(self.range, edits) && replacement_body;
        let follows_insert = !replaces_start
            && (!empty
                || edits.iter().any(|edit| {
                    edit.range.is_empty()
                        && edit.range.start() == self.range.start()
                        && edit.text.contains('\n')
                }));
        let start = map_line_boundary(self.range.start(), true, follows_insert, edits);
        let end = map_line_boundary(self.range.end(), false, false, edits).max(start);
        self.range = TextRange::new(start, end);
        true
    }
}

/// All edits have pre-step coordinates. Adjacent pure deletions may collectively
/// remove an owned span; replacement text cannot prove the line was deleted.
fn pure_deletion_covers(range: TextRange, edits: &[&Edit]) -> bool {
    let mut covered = range.start();
    for edit in edits
        .iter()
        .filter(|edit| edit.text.is_empty() && !edit.range.is_empty())
    {
        if edit.range.end() <= covered {
            continue;
        }
        if edit.range.start() > covered {
            return false;
        }
        covered = edit.range.end();
        if covered >= range.end() {
            return true;
        }
    }
    false
}

/// Line starts follow inserted prefixes but retain replacement bodies. Line ends
/// include replacement bodies and appended characters up to the first new LF.
fn map_line_boundary(at: TextSize, start: bool, follows_insert: bool, edits: &[&Edit]) -> TextSize {
    let at = i64::from(u32::from(at));
    let mut delta = 0_i64;
    let mut end_insert_ended = false;
    for edit in edits {
        let s = i64::from(u32::from(edit.range.start()));
        let e = i64::from(u32::from(edit.range.end()));
        let n = edit.text.len() as i64;
        if e < at || (e == at && s < e) {
            delta += n - (e - s);
            continue;
        }
        if s > at {
            break;
        }
        if s == e {
            if start && follows_insert {
                delta += n;
            } else if !start && !end_insert_ended {
                let prefix = edit.text.find('\n').unwrap_or(edit.text.len());
                delta += prefix as i64;
                end_insert_ended = prefix != edit.text.len();
            }
        } else {
            return TextSize::new((s + delta + if start { 0 } else { n }) as u32);
        }
    }
    TextSize::new((at + delta) as u32)
}

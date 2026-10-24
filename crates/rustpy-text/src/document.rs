//! The [`Document`]: a rope-backed, undoable text buffer addressed by byte
//! offsets ([`TextSize`]) with line/column ([`Position`]) conversions.

use std::collections::VecDeque;
use std::sync::Arc;

use ropey::{Rope, RopeSlice};
use rustpy_core::{Position, TextRange, TextSize};

use crate::encoding::{Encoding, EncodingError, LineEnding, SourcePolicy};
use crate::marker::{
    Affinity, DocumentId, JournalCursor, MarkerError, MarkerId, MarkerPosition, MarkerStore,
    PointDeletion, RangeDeletion,
};
use crate::undo::{HistoryEntry, HistoryError, HistoryId, Step, TransactionToken, UndoHistory};

/// A caret with an anchor; `anchor == head` means no selection.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    pub anchor: TextSize,
    pub head: TextSize,
}

impl Selection {
    pub fn caret(at: TextSize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }
    pub fn new(anchor: TextSize, head: TextSize) -> Self {
        Self { anchor, head }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn range(&self) -> TextRange {
        TextRange::new(self.anchor.min(self.head), self.anchor.max(self.head))
    }
    pub fn start(&self) -> TextSize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> TextSize {
        self.anchor.max(self.head)
    }
}

/// Replace `range` with `text`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edit {
    pub range: TextRange,
    pub text: String,
}

impl Edit {
    pub fn replace(range: TextRange, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }
    pub fn insert(at: TextSize, text: impl Into<String>) -> Self {
        Self {
            range: TextRange::empty(at),
            text: text.into(),
        }
    }
    pub fn delete(range: TextRange) -> Self {
        Self {
            range,
            text: String::new(),
        }
    }
}

/// How an edit relates to the previous undo transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EditGroup {
    /// Start a new undo transaction that cannot absorb later edits.
    Separate,
    /// Start a new transaction that later `Merge` edits may join (e.g. first typed char).
    Open,
    /// Join the previous transaction if it is open (consecutive typing).
    Merge,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error("edit range {0:?} is out of bounds (document length {1:?})")]
    OutOfBounds(TextRange, TextSize),
    #[error("edits overlap at {0:?}")]
    Overlapping(TextRange),
    #[error("document is read-only")]
    ReadOnly,
}

const CHANGE_LOG_LIMIT: usize = 64;
const UNDO_LIMIT: usize = 10_000;

/// A shared text snapshot and its undo identity, for an edit session that may
/// restore its original text. It does not capture save state or file format.
#[derive(Debug)]
pub struct ContentCheckpoint {
    rope: Rope,
    content_id: u64,
    save_generation: u64,
    incarnation: Arc<()>,
}

impl ContentCheckpoint {
    pub(crate) fn quit_guard_state(&self) -> (u64, u64, *const ()) {
        (
            self.content_id,
            self.save_generation,
            Arc::as_ptr(&self.incarnation),
        )
    }
}

/// A fully validated conversion prepared without changing the document.
/// Bytes are the exact disk representation; edits use normalized document
/// coordinates. Publication consumes the plan and rejects stale revisions.
#[derive(Debug)]
pub struct EncodingConversionPlan {
    incarnation: Arc<()>,
    version: u64,
    format: (LineEnding, Encoding, bool),
    read_only: bool,
    bytes: Vec<u8>,
    edits: Vec<Edit>,
    encoding: Encoding,
    affects_shebang: bool,
}

impl EncodingConversionPlan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }
    pub fn affects_shebang(&self) -> bool {
        self.affects_shebang
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EncodingConversionError {
    #[error(transparent)]
    Encoding(#[from] EncodingError),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error("the document changed after the encoding conversion was prepared")]
    StalePlan,
    #[error("the document is read-only")]
    ReadOnly,
}

/// File-format metadata restored only by explicit prepared-save history steps.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DocumentFormat {
    pub line_ending: LineEnding,
    pub encoding: Encoding,
    pub mixed_line_endings: bool,
}

/// Immutable, prevalidated save output. Stages use each intermediate text's
/// byte coordinates; the live document remains untouched until publication.
#[derive(Debug)]
pub struct PreparedSavePlan {
    incarnation: Arc<()>,
    version: u64,
    original_format: DocumentFormat,
    read_only: bool,
    history: HistoryId,
    scopes: Vec<TransactionToken>,
    bytes: Vec<u8>,
    text: String,
    stages: Vec<Vec<Edit>>,
    steps: Vec<Step>,
    format: DocumentFormat,
}

impl PreparedSavePlan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn stages(&self) -> &[Vec<Edit>] {
        &self.stages
    }
    pub fn encoding(&self) -> Encoding {
        self.format.encoding
    }
    pub fn line_ending(&self) -> LineEnding {
        self.format.line_ending
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreparedSaveError {
    #[error(transparent)]
    Encoding(#[from] EncodingError),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error("the document or transaction changed after the save was prepared")]
    StalePlan,
    #[error("document is read-only")]
    ReadOnly,
}

/// Shared document authority. Handles share text, history, markers, format and save identity.
/// Read snapshots own their storage so no reference can outlive the authority lock.
#[derive(Debug)]
pub struct Document {
    data: Arc<std::sync::RwLock<DocumentData>>,
}
impl Default for Document {
    fn default() -> Self {
        Self::new("")
    }
}
impl Document {
    fn from_data(data: DocumentData) -> Self {
        Self {
            data: Arc::new(std::sync::RwLock::new(data)),
        }
    }
    fn read(&self) -> std::sync::RwLockReadGuard<'_, DocumentData> {
        self.data.read().expect("document authority poisoned")
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, DocumentData> {
        self.data.write().expect("document authority poisoned")
    }
    /// An editor-instance handle, never a second independently editable document.
    pub fn share(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
        }
    }
    pub fn shares_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
    #[cfg(test)]
    fn format(&self) -> (LineEnding, Encoding, bool) {
        self.read().format()
    }
    pub fn new(text: &str) -> Self {
        Self::from_data(DocumentData::new(text))
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EncodingError> {
        DocumentData::from_bytes(bytes).map(Self::from_data)
    }
    pub fn from_bytes_with_policy(
        bytes: &[u8],
        policy: SourcePolicy,
        selected: Option<Encoding>,
    ) -> Result<Self, EncodingError> {
        DocumentData::from_bytes_with_policy(bytes, policy, selected).map(Self::from_data)
    }
    pub fn reload_from_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodingError> {
        self.write().reload_from_bytes(bytes)
    }
    pub fn reload_from(&mut self, loaded: Document) {
        let snapshot = loaded.read().reload_snapshot();
        self.write().reload_from(snapshot);
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodingError> {
        self.read().to_bytes()
    }
    pub fn to_bytes_with_policy(&self, policy: SourcePolicy) -> Result<Vec<u8>, EncodingError> {
        self.read().to_bytes_with_policy(policy)
    }
    pub fn prepare_encoding_conversion(
        &self,
        destination: Encoding,
        policy: SourcePolicy,
    ) -> Result<EncodingConversionPlan, EncodingConversionError> {
        self.read().prepare_encoding_conversion(destination, policy)
    }
    pub fn commit_encoding_conversion(
        &mut self,
        plan: EncodingConversionPlan,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), EncodingConversionError> {
        self.write().commit_encoding_conversion(plan, before, after)
    }
    pub fn prepare_save(
        &self,
        stages: Vec<Vec<Edit>>,
        encoding: Encoding,
        line_ending: LineEnding,
        policy: SourcePolicy,
    ) -> Result<PreparedSavePlan, PreparedSaveError> {
        self.read()
            .prepare_save(stages, encoding, line_ending, policy)
    }
    pub fn check_prepared_save(&self, plan: &PreparedSavePlan) -> Result<(), PreparedSaveError> {
        self.read().check_prepared_save(plan)
    }
    pub fn commit_prepared_save(
        &mut self,
        plan: PreparedSavePlan,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), PreparedSaveError> {
        self.write().commit_prepared_save(plan, before, after)
    }
    pub fn quit_guard_state(&self) -> String {
        self.read().quit_guard_state()
    }
    pub fn version(&self) -> u64 {
        self.read().version()
    }
    pub fn is_modified(&self) -> bool {
        self.read().is_modified()
    }
    pub fn content_checkpoint(&self) -> ContentCheckpoint {
        self.read().content_checkpoint()
    }
    pub fn recognize_restored_content(&mut self, checkpoint: &ContentCheckpoint) -> bool {
        self.write().recognize_restored_content(checkpoint)
    }
    pub fn mark_saved(&mut self) {
        self.write().mark_saved()
    }
    pub fn line_ending(&self) -> LineEnding {
        self.read().line_ending()
    }
    pub fn set_line_ending(&mut self, le: LineEnding) {
        self.write().set_line_ending(le)
    }
    pub fn has_mixed_line_endings(&self) -> bool {
        self.read().has_mixed_line_endings()
    }
    pub fn encoding(&self) -> Encoding {
        self.read().encoding()
    }
    pub fn set_encoding(&mut self, enc: Encoding) {
        self.write().set_encoding(enc)
    }
    pub fn is_read_only(&self) -> bool {
        self.read().is_read_only()
    }
    pub fn set_read_only(&mut self, ro: bool) {
        self.write().set_read_only(ro)
    }
    pub fn rope(&self) -> Rope {
        self.read().rope().clone()
    }
    pub fn text(&self) -> String {
        self.read().text()
    }
    pub fn len(&self) -> TextSize {
        self.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }
    pub fn len_chars(&self) -> usize {
        self.read().len_chars()
    }
    pub fn line_count(&self) -> usize {
        self.read().line_count()
    }
    pub fn slice(&self, range: TextRange) -> String {
        self.read().slice(range)
    }
    pub fn line_text(&self, line: usize) -> String {
        self.read().line_text(line)
    }
    pub fn line_slice(&self, line: usize) -> Rope {
        self.read().line_slice(line).into()
    }
    pub fn line_range(&self, line: usize) -> TextRange {
        self.read().line_range(line)
    }
    pub fn line_range_with_newline(&self, line: usize) -> TextRange {
        self.read().line_range_with_newline(line)
    }
    pub fn line_of(&self, offset: TextSize) -> usize {
        self.read().line_of(offset)
    }
    pub fn char_at(&self, offset: TextSize) -> Option<char> {
        self.read().char_at(offset)
    }
    pub fn char_before(&self, offset: TextSize) -> Option<char> {
        self.read().char_before(offset)
    }
    pub fn next_boundary(&self, offset: TextSize) -> TextSize {
        self.read().next_boundary(offset)
    }
    pub fn prev_boundary(&self, offset: TextSize) -> TextSize {
        self.read().prev_boundary(offset)
    }
    pub fn offset_to_position(&self, offset: TextSize) -> Position {
        self.read().offset_to_position(offset)
    }
    pub fn position_to_offset(&self, pos: Position) -> TextSize {
        self.read().position_to_offset(pos)
    }
    pub fn offset_to_encoded_position(
        &self,
        offset: TextSize,
        units: crate::ColumnUnits,
    ) -> crate::TextPosition {
        self.read().offset_to_encoded_position(offset, units)
    }
    pub fn encoded_position_to_offset(
        &self,
        position: crate::TextPosition,
        units: crate::ColumnUnits,
    ) -> TextSize {
        self.read().encoded_position_to_offset(position, units)
    }
    pub fn word_range_at(&self, offset: TextSize) -> Option<TextRange> {
        self.read().word_range_at(offset)
    }
    pub fn clamp(&self, offset: TextSize) -> TextSize {
        self.read().clamp(offset)
    }
    pub fn clamp_range(&self, range: TextRange) -> TextRange {
        self.read().clamp_range(range)
    }
    pub fn edit(
        &mut self,
        edits: Vec<Edit>,
        before: &[Selection],
        after: &[Selection],
        group: EditGroup,
        label: Option<&str>,
    ) -> Result<(), EditError> {
        self.write().edit(edits, before, after, group, label)
    }
    pub fn reset(&mut self, text: &str) {
        self.write().reset(text)
    }
    pub fn set_text(
        &mut self,
        text: &str,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), EditError> {
        self.write().set_text(text, before, after)
    }
    pub fn can_undo(&self) -> bool {
        self.read().can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.read().can_redo()
    }
    pub fn undo_label(&self) -> Option<String> {
        self.read().undo_label().map(str::to_owned)
    }
    pub fn undo_is_refactoring(&self) -> bool {
        self.read().undo_is_refactoring()
    }
    /// Marks the current committed undo unit, shared by all views of this document.
    pub fn mark_refactoring(&mut self) {
        self.write().mark_refactoring()
    }
    /// An explicit workspace operation can change paths/bytes without editing
    /// this open buffer. Its history boundary routes Undo to the shared owner.
    pub fn record_refactoring_boundary(
        &mut self,
        selections: &[Selection],
        label: &str,
    ) -> Result<(), HistoryError> {
        self.write().record_refactoring_boundary(selections, label)
    }

    pub fn redo_label(&self) -> Option<String> {
        self.read().redo_label().map(str::to_owned)
    }
    pub fn break_undo_group(&mut self) {
        self.write().break_undo_group()
    }
    pub fn undo(&mut self) -> Option<Vec<Selection>> {
        self.write().undo()
    }
    pub fn redo(&mut self) -> Option<Vec<Selection>> {
        self.write().redo()
    }
    pub fn undo_history(&self) -> Vec<HistoryEntry> {
        self.read().undo_history()
    }
    pub fn current_history(&self) -> HistoryId {
        self.read().current_history()
    }
    pub fn select_history(&mut self, target: HistoryId) -> Result<Vec<Selection>, HistoryError> {
        self.write().select_history(target)
    }
    pub fn begin_transaction(
        &mut self,
        before: &[Selection],
        label: Option<&str>,
    ) -> Result<TransactionToken, HistoryError> {
        self.write().begin_transaction(before, label)
    }
    pub fn commit_transaction(
        &mut self,
        token: TransactionToken,
        after: &[Selection],
    ) -> Result<(), HistoryError> {
        self.write().commit_transaction(token, after)
    }
    pub fn rollback_transaction(
        &mut self,
        token: TransactionToken,
    ) -> Result<Vec<Selection>, HistoryError> {
        self.write().rollback_transaction(token)
    }
    pub fn transaction_depth(&self) -> usize {
        self.read().transaction_depth()
    }
    pub fn set_undo_limit(&mut self, limit: usize) -> Result<(), HistoryError> {
        self.write().set_undo_limit(limit)
    }
    pub fn identity(&self) -> DocumentId {
        self.read().identity()
    }
    pub fn point_marker(
        &mut self,
        at: TextSize,
        affinity: Affinity,
        deletion: PointDeletion,
    ) -> Result<MarkerId, MarkerError> {
        self.write().point_marker(at, affinity, deletion)
    }
    pub fn line_marker(&mut self, line0: usize) -> Result<MarkerId, MarkerError> {
        self.write().line_marker(line0)
    }
    pub fn range_marker(
        &mut self,
        range: TextRange,
        start: Affinity,
        end: Affinity,
        deletion: RangeDeletion,
    ) -> Result<MarkerId, MarkerError> {
        self.write().range_marker(range, start, end, deletion)
    }
    pub fn marker(&self, id: MarkerId) -> Result<Option<MarkerPosition>, MarkerError> {
        self.read().marker(id)
    }
    pub fn remove_marker(&mut self, id: MarkerId) -> Result<(), MarkerError> {
        self.write().remove_marker(id)
    }
    pub fn journal_cursor(&self) -> JournalCursor {
        self.read().journal_cursor()
    }
    pub fn edits_since(&self, cursor: JournalCursor) -> Result<Vec<Vec<Edit>>, MarkerError> {
        self.read()
            .edits_since(cursor)
            .map(|batches| batches.into_iter().map(<[Edit]>::to_vec).collect())
    }
    pub fn changes_since(&self, version: u64) -> Option<Vec<Vec<Edit>>> {
        self.read()
            .changes_since(version)
            .map(|batches| batches.into_iter().map(<[Edit]>::to_vec).collect())
    }
}

/// Rope-backed text document. Text is always LF-normalized UTF-8 internally.
#[derive(Debug)]
pub(crate) struct DocumentData {
    rope: Rope,
    saved_rope: Rope,
    incarnation: Arc<()>,
    version: u64,
    /// Identity of the current *content*; restored by undo/redo so that
    /// returning to the saved state reports "not modified".
    content_id: u64,
    next_content_id: u64,
    saved_content_id: u64,
    save_generation: u64,
    line_ending: LineEnding,
    mixed_line_endings: bool,
    encoding: Encoding,
    /// Line ending, encoding and mixed-endings flag of the file as last loaded
    /// or saved: changing the line separator or encoding modifies the document.
    saved_format: (LineEnding, Encoding, bool),
    read_only: bool,
    undo: UndoHistory,
    markers: MarkerStore,
    marker_frames: Vec<(TransactionToken, MarkerStore)>,
    /// Recent `(version_after, edits)` pairs for incremental consumers.
    change_log: VecDeque<(u64, Vec<Edit>)>,
}

impl Default for DocumentData {
    fn default() -> Self {
        Self::new("")
    }
}

impl DocumentData {
    /// Creates a document from text (line endings are normalized to LF).
    pub fn new(text: &str) -> Self {
        let (line_ending, mixed) = LineEnding::detect(text);
        let normalized = LineEnding::normalize_to_lf(text);
        let rope = Rope::from_str(&normalized);
        Self {
            saved_rope: rope.clone(),
            rope,
            incarnation: Arc::new(()),
            version: 0,
            content_id: 0,
            next_content_id: 1,
            saved_content_id: 0,
            save_generation: 0,
            line_ending,
            mixed_line_endings: mixed,
            encoding: Encoding::Utf8,
            saved_format: (line_ending, Encoding::Utf8, mixed),
            read_only: false,
            undo: UndoHistory::new(UNDO_LIMIT),
            markers: MarkerStore::new(),
            marker_frames: Vec::new(),
            change_log: VecDeque::new(),
        }
    }

    /// Creates a document from raw file bytes, detecting encoding and line endings.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EncodingError> {
        let (text, encoding) = Encoding::decode(bytes)?;
        let mut doc = Self::new(&text);
        doc.encoding = encoding;
        doc.saved_format.1 = encoding;
        Ok(doc)
    }

    /// Load using the explicit source policy and optional selected codec.
    /// Selected encoding metadata is retained rather than autodetected again.
    pub fn from_bytes_with_policy(
        bytes: &[u8],
        policy: SourcePolicy,
        selected: Option<Encoding>,
    ) -> Result<Self, EncodingError> {
        let (text, encoding) = Encoding::decode_with_policy(bytes, policy, selected)?;
        let mut document = Self::new(&text);
        document.encoding = encoding;
        document.saved_format.1 = encoding;
        Ok(document)
    }

    /// Replaces the content with a file's bytes (reload from disk): encoding and
    /// line endings are detected again (so a CRLF file stays CRLF), the undo
    /// history is cleared and the document is unmodified. The version keeps
    /// increasing so caches keyed on it are refreshed.
    pub fn reload_from_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodingError> {
        let loaded = Self::from_bytes(bytes)?;
        self.reload_from(loaded);
        Ok(())
    }

    /// [`DocumentData::reload_from_bytes`] with a document decoded elsewhere (a
    /// background task reloading a large file): `loaded` replaces the
    /// content, the version keeps increasing and read-only stays as it was.
    /// Source and destination history, transactions, markers and cursors expire.
    /// The loaded content, format and saved/dirty identities are preserved.
    pub fn reload_from(&mut self, loaded: DocumentData) {
        let (version, read_only) = (self.version, self.read_only);
        *self = loaded;
        self.version = version + 1;
        self.read_only = read_only;
        // A reload is a new incarnation, even if its source was edited or marked.
        // Preserve loaded content/save identities, but expire all external handles.
        self.incarnation = Arc::new(());
        self.markers = MarkerStore::new();
        self.marker_frames.clear();
        self.undo = UndoHistory::new(UNDO_LIMIT);
        self.undo.restore_content_identity(0, self.content_id);
        self.change_log.clear();
    }

    fn reload_snapshot(&self) -> Self {
        let mut snapshot = Self::new("");
        snapshot.rope = self.rope.clone();
        snapshot.saved_rope = self.saved_rope.clone();
        snapshot.content_id = self.content_id;
        snapshot.next_content_id = self.next_content_id;
        snapshot.saved_content_id = self.saved_content_id;
        snapshot.save_generation = self.save_generation;
        snapshot.line_ending = self.line_ending;
        snapshot.mixed_line_endings = self.mixed_line_endings;
        snapshot.encoding = self.encoding;
        snapshot.saved_format = self.saved_format;
        snapshot.read_only = self.read_only;
        snapshot
    }

    /// Serializes the document using its encoding and line-ending convention.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EncodingError> {
        let text = self.text();
        self.encoding.encode(&self.line_ending.apply(&text))
    }

    // ----- metadata -------------------------------------------------------

    /// Serialize under the same explicit Python/generic policy used for load.
    pub fn to_bytes_with_policy(&self, policy: SourcePolicy) -> Result<Vec<u8>, EncodingError> {
        self.encoding
            .encode_with_policy(&self.line_ending.apply(&self.text()), policy)
    }

    /// Plan a conversion without mutating text, format, markers or history.
    pub fn prepare_encoding_conversion(
        &self,
        destination: Encoding,
        policy: SourcePolicy,
    ) -> Result<EncodingConversionPlan, EncodingConversionError> {
        if self.read_only {
            return Err(EncodingConversionError::ReadOnly);
        }
        let original = self.text();
        let raw = self.line_ending.apply(&original);
        let conversion = destination.plan_conversion(&raw, policy)?;
        let proposed = LineEnding::normalize_to_lf(&conversion.text);
        let edits = minimal_conversion_edit(&original, &proposed);
        Ok(EncodingConversionPlan {
            incarnation: Arc::clone(&self.incarnation),
            version: self.version,
            format: self.format(),
            read_only: self.read_only,
            bytes: conversion.bytes,
            edits,
            encoding: conversion.encoding,
            affects_shebang: conversion.affects_shebang,
        })
    }

    /// Publish a validated plan after the caller's disk write succeeds. This
    /// records one text-and-encoding undo unit (or joins an active transaction),
    /// but does not mark saved. Failed/stale publication changes nothing.
    pub fn commit_encoding_conversion(
        &mut self,
        plan: EncodingConversionPlan,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), EncodingConversionError> {
        if self.read_only {
            return Err(EncodingConversionError::ReadOnly);
        }
        if !Arc::ptr_eq(&self.incarnation, &plan.incarnation)
            || self.version != plan.version
            || self.format() != plan.format
            || self.read_only != plan.read_only
        {
            return Err(EncodingConversionError::StalePlan);
        }
        if plan.edits.is_empty() && self.encoding == plan.encoding {
            return Ok(());
        }
        let encoding_before = self.encoding;
        let mut step = self.apply_step(plan.edits)?;
        let content_before = self.content_id;
        if step.edits.is_empty() {
            self.apply_raw(&[]); // Format-only steps still have version/change-log identity.
        } else {
            self.content_id = self.next_content_id;
            self.next_content_id += 1;
        }
        self.encoding = plan.encoding;
        step.encoding = Some((encoding_before, plan.encoding));
        self.undo.push(
            step,
            before.to_vec(),
            after.to_vec(),
            false,
            false,
            Some("Convert encoding".into()),
            content_before,
            self.content_id,
        );
        Ok(())
    }

    /// Validate ordered edits and the exact output bytes without changing this
    /// document, its markers, history, selections or saved identity.
    pub fn prepare_save(
        &self,
        stages: Vec<Vec<Edit>>,
        encoding: Encoding,
        line_ending: LineEnding,
        policy: SourcePolicy,
    ) -> Result<PreparedSavePlan, PreparedSaveError> {
        if self.read_only {
            return Err(PreparedSaveError::ReadOnly);
        }
        let mut scratch = Self::new("");
        scratch.rope = self.rope.clone();
        let mut steps = Vec::new();
        for stage in stages {
            let step = scratch.apply_step(stage)?;
            if !step.edits.is_empty() {
                steps.push(step);
            }
        }
        let text = scratch.text();
        let bytes = encoding.encode_with_policy(&line_ending.apply(&text), policy)?;
        // Temporary edits that restore the original text must not disturb its
        // marker ownership, even when the save also changes the file format.
        if scratch.rope == self.rope {
            steps.clear();
        }
        let stages = steps.iter().map(|step| step.edits.clone()).collect();
        Ok(PreparedSavePlan {
            incarnation: Arc::clone(&self.incarnation),
            version: self.version,
            original_format: self.document_format(),
            read_only: self.read_only,
            history: self.undo.current(),
            scopes: self.marker_frames.iter().map(|(token, _)| *token).collect(),
            bytes,
            text,
            stages,
            steps,
            format: DocumentFormat {
                line_ending,
                encoding,
                mixed_line_endings: false,
            },
        })
    }

    /// Check immediately before writing, and again when publishing. Callers
    /// must keep exclusive access between a successful write and publication.
    pub fn check_prepared_save(&self, plan: &PreparedSavePlan) -> Result<(), PreparedSaveError> {
        if self.read_only {
            return Err(PreparedSaveError::ReadOnly);
        }
        if !Arc::ptr_eq(&self.incarnation, &plan.incarnation)
            || self.version != plan.version
            || self.document_format() != plan.original_format
            || self.read_only != plan.read_only
            || self.undo.current() != plan.history
            || !self
                .marker_frames
                .iter()
                .map(|(token, _)| *token)
                .eq(plan.scopes.iter().copied())
        {
            return Err(PreparedSaveError::StalePlan);
        }
        Ok(())
    }

    /// Publish a successfully written save as one undo unit, or join the active
    /// transaction. This does not mark saved; the writer calls `mark_saved`.
    pub fn commit_prepared_save(
        &mut self,
        mut plan: PreparedSavePlan,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), PreparedSaveError> {
        self.check_prepared_save(&plan)?;
        if plan.steps.is_empty() && plan.format == plan.original_format {
            return Ok(());
        }
        let content_before = self.content_id;
        if plan.steps.is_empty() {
            plan.steps.push(Step {
                edits: Vec::new(),
                inverse: Vec::new(),
                encoding: None,
                format: None,
            });
        } else {
            self.content_id = self.next_content_id;
            self.next_content_id += 1;
        }
        for step in &plan.steps {
            self.apply_raw(&step.edits);
        }
        self.restore_format(plan.format);
        plan.steps.last_mut().unwrap().format = Some((plan.original_format, plan.format));
        self.undo.push_batch(
            plan.steps,
            before.to_vec(),
            after.to_vec(),
            Some("Save Actions".into()),
            content_before,
            self.content_id,
        );
        Ok(())
    }

    fn document_format(&self) -> DocumentFormat {
        DocumentFormat {
            line_ending: self.line_ending,
            encoding: self.encoding,
            mixed_line_endings: self.mixed_line_endings,
        }
    }

    fn restore_format(&mut self, format: DocumentFormat) {
        self.line_ending = format.line_ending;
        self.encoding = format.encoding;
        self.mixed_line_endings = format.mixed_line_endings;
    }

    /// Observational identity/history state for staged close coordination; no text copy.
    pub fn quit_guard_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.identity(),
                self.version,
                self.content_id,
                self.next_content_id,
                self.saved_content_id,
                self.save_generation,
                self.format(),
                self.saved_format,
                self.read_only,
                self.undo.quit_guard_state(),
                &self.markers,
                self.marker_frames
                    .iter()
                    .map(|(token, markers)| (token, markers))
                    .collect::<Vec<_>>()
            )
        )
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    /// The text differs from the file as last loaded or saved, or saving would
    /// write other line separators or another encoding.
    pub fn is_modified(&self) -> bool {
        self.content_id != self.saved_content_id || self.format() != self.saved_format
    }
    /// Captures the content before a reversible editing session. Rope cloning
    /// shares its storage; it does not copy the editor or its undo history.
    pub fn content_checkpoint(&self) -> ContentCheckpoint {
        ContentCheckpoint {
            rope: self.rope.clone(),
            content_id: self.content_id,
            save_generation: self.save_generation,
            incarnation: Arc::clone(&self.incarnation),
        }
    }

    /// Recognizes an exact restoration of a session's original text without
    /// saving it. Unrelated edits prevent restoration; format changes and any
    /// save made during the session retain their existing dirty-state meaning.
    pub fn recognize_restored_content(&mut self, checkpoint: &ContentCheckpoint) -> bool {
        if !Arc::ptr_eq(&self.incarnation, &checkpoint.incarnation) || self.rope != checkpoint.rope
        {
            return false;
        }
        // An explicit undo may already have returned to the saved identity.
        // Keep it clean without changing redo of the originally dirty state.
        if self.save_generation == checkpoint.save_generation
            && self.content_id == self.saved_content_id
        {
            return true;
        }
        // A net-zero edit may already have been dirty when the session began.
        // Only a save during the session may replace that original identity.
        let restored =
            if self.save_generation != checkpoint.save_generation && self.rope == self.saved_rope {
                self.saved_content_id
            } else {
                checkpoint.content_id
            };
        if self.content_id != restored {
            self.undo
                .restore_content_identity(self.content_id, restored);
            self.content_id = restored;
        }
        if checkpoint.content_id != restored {
            self.undo
                .restore_content_identity(checkpoint.content_id, restored);
        }
        true
    }

    /// Marks the current content as persisted. [`DocumentData::to_bytes`] writes one
    /// line separator throughout, so a file with mixed separators is no longer
    /// mixed once saved.
    pub fn mark_saved(&mut self) {
        self.undo.break_merge();
        self.save_generation += 1;
        self.mixed_line_endings = false;
        self.saved_content_id = self.content_id;
        self.saved_rope = self.rope.clone();
        self.saved_format = self.format();
    }
    fn format(&self) -> (LineEnding, Encoding, bool) {
        (self.line_ending, self.encoding, self.mixed_line_endings)
    }
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
    /// Converts the line separators (all lines, also of a mixed file) on the next save.
    pub fn set_line_ending(&mut self, le: LineEnding) {
        self.line_ending = le;
        self.mixed_line_endings = false;
        self.version += 1;
    }
    pub fn has_mixed_line_endings(&self) -> bool {
        self.mixed_line_endings
    }
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }
    pub fn set_encoding(&mut self, enc: Encoding) {
        self.encoding = enc;
        self.version += 1;
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub fn set_read_only(&mut self, ro: bool) {
        self.read_only = ro;
    }

    // ----- reading --------------------------------------------------------

    pub fn rope(&self) -> &Rope {
        &self.rope
    }
    pub fn text(&self) -> String {
        self.rope.to_string()
    }
    pub fn len(&self) -> TextSize {
        TextSize::new(self.rope.len_bytes() as u32)
    }
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }
    /// Number of lines; a trailing newline yields an extra empty last line.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }
    pub fn slice(&self, range: TextRange) -> String {
        let range = self.clamp_range(range);
        let s = self.rope.byte_to_char(range.start().into());
        let e = self.rope.byte_to_char(range.end().into());
        self.rope.slice(s..e).to_string()
    }
    /// The line's text *without* its trailing newline.
    pub fn line_text(&self, line: usize) -> String {
        self.line_slice(line).to_string()
    }
    /// The line's content as a rope slice without its trailing newline.
    pub fn line_slice(&self, line: usize) -> RopeSlice<'_> {
        let line = line.min(self.line_count().saturating_sub(1));
        let slice = self.rope.line(line);
        let mut end = slice.len_chars();
        if end > 0 && slice.char(end - 1) == '\n' {
            end -= 1;
        }
        slice.slice(..end)
    }
    /// Byte range of the line content excluding the newline.
    pub fn line_range(&self, line: usize) -> TextRange {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_byte(line);
        let end = start + self.line_slice(line).len_bytes();
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    }
    /// Byte range of the line including its newline (if any).
    pub fn line_range_with_newline(&self, line: usize) -> TextRange {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_byte(line);
        let end = if line + 1 < self.line_count() {
            self.rope.line_to_byte(line + 1)
        } else {
            self.rope.len_bytes()
        };
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    }
    pub fn line_of(&self, offset: TextSize) -> usize {
        self.rope.byte_to_line(self.clamp(offset).into())
    }
    pub fn char_at(&self, offset: TextSize) -> Option<char> {
        let off: usize = offset.into();
        if off >= self.rope.len_bytes() {
            return None;
        }
        Some(self.rope.char(self.rope.byte_to_char(off)))
    }
    pub fn char_before(&self, offset: TextSize) -> Option<char> {
        let off: usize = self.clamp(offset).into();
        if off == 0 {
            return None;
        }
        let ci = self.rope.byte_to_char(off);
        Some(self.rope.char(ci - 1))
    }
    /// Next character boundary after `offset`.
    pub fn next_boundary(&self, offset: TextSize) -> TextSize {
        match self.char_at(offset) {
            Some(c) => offset + TextSize::new(c.len_utf8() as u32),
            None => self.len(),
        }
    }
    /// Previous character boundary before `offset`.
    pub fn prev_boundary(&self, offset: TextSize) -> TextSize {
        match self.char_before(offset) {
            Some(c) => offset - TextSize::new(c.len_utf8() as u32),
            None => TextSize::new(0),
        }
    }
    pub fn offset_to_position(&self, offset: TextSize) -> Position {
        let off: usize = self.clamp(offset).into();
        let line = self.rope.byte_to_line(off);
        let line_start = self.rope.line_to_char(line);
        let col = self.rope.byte_to_char(off) - line_start;
        Position::new(line as u32, col as u32)
    }
    /// Converts a position to a byte offset, clamping line and column.
    pub fn position_to_offset(&self, pos: Position) -> TextSize {
        let line = (pos.line as usize).min(self.line_count().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        let len = self.line_slice(line).len_chars();
        let col = (pos.column as usize).min(len);
        TextSize::new(self.rope.char_to_byte(line_start + col) as u32)
    }
    /// Zero-based encoded position in the normalized document. Unlike the
    /// legacy scalar API, positions stop before line endings. Rope indexes
    /// provide conversions without flattening or scanning the document.
    pub fn offset_to_encoded_position(
        &self,
        offset: TextSize,
        units: crate::ColumnUnits,
    ) -> crate::TextPosition {
        let off = usize::from(self.clamp(offset));
        let line = self.rope.byte_to_line(off);
        let start = self.rope.line_to_byte(line);
        let content = self.line_slice(line);
        let byte = (off - start).min(content.len_bytes());
        let character = content.byte_to_char(byte);
        let column = match units {
            crate::ColumnUnits::Bytes => content.char_to_byte(character),
            crate::ColumnUnits::Scalars => character,
            crate::ColumnUnits::Utf16 => content.char_to_utf16_cu(character),
        };
        crate::TextPosition::new(line, column)
    }

    /// Convert an encoded position to a byte offset. Lines past EOF map to
    /// EOF, oversized columns stop before the newline, and byte/surrogate
    /// interiors round down. Existing scalar API behavior is unchanged.
    pub fn encoded_position_to_offset(
        &self,
        position: crate::TextPosition,
        units: crate::ColumnUnits,
    ) -> TextSize {
        if position.line >= self.line_count() {
            return self.len();
        }
        let content = self.line_slice(position.line);
        let character = match units {
            crate::ColumnUnits::Bytes => {
                content.byte_to_char(position.column.min(content.len_bytes()))
            }
            crate::ColumnUnits::Scalars => position.column.min(content.len_chars()),
            crate::ColumnUnits::Utf16 => {
                content.utf16_cu_to_char(position.column.min(content.len_utf16_cu()))
            }
        };
        TextSize::new(
            (self.rope.line_to_byte(position.line) + content.char_to_byte(character)) as u32,
        )
    }
    /// Byte range of the identifier-like word containing or touching `offset`.
    pub fn word_range_at(&self, offset: TextSize) -> Option<TextRange> {
        let is_word = |c: char| rustpy_core::is_ident_continue(c);
        let mut start = offset;
        let mut end = offset;
        while let Some(c) = self.char_before(start) {
            if !is_word(c) {
                break;
            }
            start -= TextSize::new(c.len_utf8() as u32);
        }
        while let Some(c) = self.char_at(end) {
            if !is_word(c) {
                break;
            }
            end += TextSize::new(c.len_utf8() as u32);
        }
        (start < end).then(|| TextRange::new(start, end))
    }
    /// `offset` limited to the text and moved back to a character boundary
    /// (a stale offset can point into a multi-byte character).
    pub fn clamp(&self, offset: TextSize) -> TextSize {
        let off = usize::from(offset.min(self.len()));
        TextSize::new(self.rope.char_to_byte(self.rope.byte_to_char(off)) as u32)
    }
    pub fn clamp_range(&self, range: TextRange) -> TextRange {
        TextRange::new(self.clamp(range.start()), self.clamp(range.end()))
    }

    // ----- editing --------------------------------------------------------

    /// Applies `edits` (any order, non-overlapping, in current coordinates)
    /// as one step and records it for undo.
    pub fn edit(
        &mut self,
        edits: Vec<Edit>,
        before: &[Selection],
        after: &[Selection],
        group: EditGroup,
        label: Option<&str>,
    ) -> Result<(), EditError> {
        if self.read_only {
            return Err(EditError::ReadOnly);
        }
        let step = self.apply_step(edits)?;
        if step.edits.is_empty() {
            return Ok(());
        }
        let (merge, mergeable) = match group {
            EditGroup::Separate => (false, false),
            EditGroup::Open => (false, true),
            EditGroup::Merge => (true, true),
        };
        let content_before = self.content_id;
        self.content_id = self.next_content_id;
        self.next_content_id += 1;
        self.undo.push(
            step,
            before.to_vec(),
            after.to_vec(),
            merge,
            mergeable,
            label.map(str::to_owned),
            content_before,
            self.content_id,
        );
        Ok(())
    }

    /// Replaces the whole content without recording undo history (used for
    /// reload; [`DocumentData::reload_from_bytes`] also keeps the file's line
    /// separators and encoding). The version keeps increasing so caches keyed
    /// on it are refreshed.
    pub fn reset(&mut self, text: &str) {
        let (version, read_only) = (self.version, self.read_only);
        *self = Self::new(text);
        self.version = version + 1;
        self.read_only = read_only;
    }

    /// Replaces the entire text as a single undoable step.
    pub fn set_text(
        &mut self,
        text: &str,
        before: &[Selection],
        after: &[Selection],
    ) -> Result<(), EditError> {
        let normalized = LineEnding::normalize_to_lf(text).into_owned();
        let whole = TextRange::new(TextSize::new(0), self.len());
        self.edit(
            vec![Edit::replace(whole, normalized)],
            before,
            after,
            EditGroup::Separate,
            Some("Replace All"),
        )
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.undo_label()
    }
    pub fn undo_is_refactoring(&self) -> bool {
        self.undo.undo_is_refactoring()
    }
    pub fn mark_refactoring(&mut self) {
        self.undo.mark_refactoring()
    }
    pub fn record_refactoring_boundary(
        &mut self,
        selections: &[Selection],
        label: &str,
    ) -> Result<(), HistoryError> {
        if self.read_only {
            return Err(HistoryError::ReadOnly);
        }
        if self.undo.depth() != 0 {
            return Err(HistoryError::TransactionActive);
        }
        self.undo.break_merge();
        self.undo.push(
            Step {
                edits: Vec::new(),
                inverse: Vec::new(),
                encoding: None,
                format: None,
            },
            selections.to_vec(),
            selections.to_vec(),
            false,
            false,
            Some(label.into()),
            self.content_id,
            self.content_id,
        );
        self.undo.mark_refactoring();
        Ok(())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.undo.redo_label()
    }
    /// Ends the current typing group so the next edit becomes a new undo unit.
    pub fn break_undo_group(&mut self) {
        self.undo.break_merge();
    }

    /// Undoes the last transaction; returns the selections to restore.
    pub fn undo(&mut self) -> Option<Vec<Selection>> {
        let t = self.undo.undo()?;
        for step in t.steps.iter().rev() {
            self.apply_history_step(step, false);
        }
        self.content_id = t.content_before;
        let sel = t.before.clone();
        Some(sel)
    }

    /// Redoes the last undone transaction; returns the selections to restore.
    pub fn redo(&mut self) -> Option<Vec<Selection>> {
        let t = self.undo.redo()?;
        for step in &t.steps {
            self.apply_history_step(step, true);
        }
        self.content_id = t.content_after;
        let sel = t.after.clone();
        Some(sel)
    }

    /// Retained states for a history picker, including alternate redo branches.
    pub fn undo_history(&self) -> Vec<HistoryEntry> {
        self.undo.entries()
    }

    /// The current retained cursor. Edits in active scopes are not represented until commit.
    pub fn current_history(&self) -> HistoryId {
        self.undo.current()
    }

    /// Visits a retained state, restoring its text and selections. Versions keep increasing.
    /// History IDs expire when pruned or when the document is reset/reloaded.
    pub fn select_history(&mut self, target: HistoryId) -> Result<Vec<Selection>, HistoryError> {
        if self.read_only {
            return Err(HistoryError::ReadOnly);
        }
        let (backward, forward) = self.undo.route(target)?;
        for &id in &backward {
            let t = self.undo.transaction(id);
            for step in t.steps.iter().rev() {
                self.apply_history_step(step, false);
            }
        }
        for &id in &forward {
            let t = self.undo.transaction(id);
            for step in &t.steps {
                self.apply_history_step(step, true);
            }
        }
        let (content, selections) = self.undo.finish_selection(target, &backward, &forward);
        self.content_id = content;
        Ok(selections)
    }

    /// Begins a scope whose edits form one undo unit when the outermost scope commits.
    /// Ordinary undo/redo and history selection are unavailable while scopes are active.
    pub fn begin_transaction(
        &mut self,
        before: &[Selection],
        label: Option<&str>,
    ) -> Result<TransactionToken, HistoryError> {
        if self.read_only {
            return Err(HistoryError::ReadOnly);
        }
        let token = self.undo.begin(
            before.to_vec(),
            label.map(str::to_owned),
            self.content_id,
            self.content_checkpoint(),
        );
        self.marker_frames.push((token, self.markers.clone()));
        Ok(token)
    }

    /// Commits the innermost scope. Nested commits join their parent; empty scopes add no unit.
    pub fn commit_transaction(
        &mut self,
        token: TransactionToken,
        after: &[Selection],
    ) -> Result<(), HistoryError> {
        self.undo.commit(token, after.to_vec())?;
        let (frame_token, _) = self
            .marker_frames
            .pop()
            .expect("marker scope matches undo scope");
        debug_assert_eq!(frame_token, token);
        Ok(())
    }

    /// Reverts the innermost scope and restores its initial selections. Save state and
    /// legacy format setters remain unchanged; explicit conversion steps restore encoding.
    /// Rollback is available even if read-only was enabled.
    pub fn rollback_transaction(
        &mut self,
        token: TransactionToken,
    ) -> Result<Vec<Selection>, HistoryError> {
        let (t, checkpoint) = self.undo.rollback(token)?;
        for step in t.steps.iter().rev() {
            self.apply_history_step(step, false);
        }
        self.content_id = t.content_before;
        self.recognize_restored_content(&checkpoint);
        let (frame_token, markers) = self
            .marker_frames
            .pop()
            .expect("marker scope matches undo scope");
        debug_assert_eq!(frame_token, token);
        self.markers = markers;
        Ok(t.before)
    }

    pub fn transaction_depth(&self) -> usize {
        self.undo.depth()
    }

    /// Bounds retained edit units across all branches. Old alternate leaves are removed
    /// first, then the oldest active ancestors. The current state always remains valid.
    pub fn set_undo_limit(&mut self, limit: usize) -> Result<(), HistoryError> {
        self.undo.set_limit(limit)
    }

    /// Identity changes on reset/reload, even when two documents have equal versions/text.
    pub fn identity(&self) -> DocumentId {
        self.markers.document()
    }

    /// Attaches a point in UTF-8 byte coordinates. Metadata attachment is allowed read-only.
    pub fn point_marker(
        &mut self,
        at: TextSize,
        affinity: Affinity,
        deletion: PointDeletion,
    ) -> Result<MarkerId, MarkerError> {
        self.check_marker_offset(at)?;
        Ok(self.markers.point(at, affinity, deletion))
    }

    /// Attaches to an original logical line, following joins and body replacements.
    /// A true whole-line deletion invalidates it permanently through ordinary undo;
    /// explicit transaction rollback restores the pre-scope marker membership.
    /// Blank middle rows own their LF, trailing empty rows the preceding LF, and
    /// an empty document its EOF. Nonempty replacements preserve the first new
    /// logical row; inserting an interior LF likewise leaves ownership on the
    /// first split row. Separate deletion and insertion do not transfer ownership.
    /// The position is the owned content span's start.
    /// Metadata attachment is allowed read-only and does not change undo/version.
    pub fn line_marker(&mut self, line0: usize) -> Result<MarkerId, MarkerError> {
        if line0 >= self.line_count() {
            return Err(MarkerError::LineOutOfBounds(line0));
        }
        Ok(self.markers.line(self.line_range(line0)))
    }

    /// Attaches a range with independent boundary insertion affinities and deletion policy.
    /// Crossed endpoints collapse at mapped start. No text, version or undo unit changes.
    pub fn range_marker(
        &mut self,
        range: TextRange,
        start: Affinity,
        end: Affinity,
        deletion: RangeDeletion,
    ) -> Result<MarkerId, MarkerError> {
        self.check_marker_offset(range.start())?;
        self.check_marker_offset(range.end())?;
        Ok(self.markers.range(range, start, end, deletion))
    }

    /// Current position, or None after deletion invalidates the marker. Ordinary undo does
    /// not revive invalidated markers. Explicit rollback restores exact pre-scope metadata,
    /// including removed/invalidated markers; markers created in that scope expire instead.
    pub fn marker(&self, id: MarkerId) -> Result<Option<MarkerPosition>, MarkerError> {
        self.markers.get(id)
    }

    /// Removes marker metadata. Transaction rollback can restore a pre-scope removed marker.
    pub fn remove_marker(&mut self, id: MarkerId) -> Result<(), MarkerError> {
        self.markers.remove(id)
    }

    pub fn journal_cursor(&self) -> JournalCursor {
        JournalCursor {
            document: self.identity(),
            version: self.version,
        }
    }

    /// Identity-guarded edit batches after a cursor, oldest first, with original coordinates
    /// per step (including undo/redo/rollback). Eager markers do not depend on log retention.
    pub fn edits_since(&self, cursor: JournalCursor) -> Result<Vec<&[Edit]>, MarkerError> {
        if cursor.document != self.identity() {
            return Err(MarkerError::ForeignDocument);
        }
        self.changes_since(cursor.version)
            .ok_or(MarkerError::JournalExpired)
    }

    fn check_marker_offset(&self, at: TextSize) -> Result<(), MarkerError> {
        if at > self.len() {
            return Err(MarkerError::OutOfBounds(at));
        }
        if self.rope.char_to_byte(self.rope.byte_to_char(at.into())) != usize::from(at) {
            return Err(MarkerError::NotCharBoundary(at));
        }
        Ok(())
    }

    /// Changes applied after `version`, oldest first, or `None` if the log no
    /// longer covers that version (caller must do a full refresh).
    pub fn changes_since(&self, version: u64) -> Option<Vec<&[Edit]>> {
        if version >= self.version {
            return Some(Vec::new());
        }
        let oldest = self.change_log.front().map(|(v, _)| *v)?;
        if oldest > version + 1 {
            return None;
        }
        Some(
            self.change_log
                .iter()
                .filter(|(v, _)| *v > version)
                .map(|(_, e)| e.as_slice())
                .collect(),
        )
    }

    fn apply_step(&mut self, mut edits: Vec<Edit>) -> Result<Step, EditError> {
        let len = self.len();
        for e in &mut edits {
            if e.range.end() > len {
                return Err(EditError::OutOfBounds(e.range, len));
            }
            // Snap to char boundaries.
            let s = self
                .rope
                .char_to_byte(self.rope.byte_to_char(e.range.start().into()));
            let en = self
                .rope
                .char_to_byte(self.rope.byte_to_char(e.range.end().into()));
            e.range = TextRange::new(TextSize::new(s as u32), TextSize::new(en as u32));
            if e.text.contains('\r') {
                e.text = LineEnding::normalize_to_lf(&e.text).into_owned();
            }
        }
        edits.retain(|e| !(e.range.is_empty() && e.text.is_empty()));
        edits.sort_by_key(|e| (e.range.start(), e.range.end()));
        for w in edits.windows(2) {
            if w[1].range.start() < w[0].range.end() {
                return Err(EditError::Overlapping(w[1].range));
            }
        }
        if edits.is_empty() {
            return Ok(Step {
                edits,
                inverse: Vec::new(),
                encoding: None,
                format: None,
            });
        }
        // Build inverse edits in post-edit coordinates.
        let mut inverse = Vec::with_capacity(edits.len());
        let mut delta: i64 = 0;
        for e in &edits {
            let old_text = self.slice(e.range);
            let new_start = (u32::from(e.range.start()) as i64 + delta) as u32;
            let new_len = e.text.len() as u32;
            inverse.push(Edit {
                range: TextRange::at(TextSize::new(new_start), TextSize::new(new_len)),
                text: old_text,
            });
            delta += e.text.len() as i64 - u32::from(e.range.len()) as i64;
        }
        self.apply_raw(&edits);
        Ok(Step {
            edits,
            inverse,
            encoding: None,
            format: None,
        })
    }

    fn apply_history_step(&mut self, step: &Step, forward: bool) {
        self.apply_raw(if forward { &step.edits } else { &step.inverse });
        if let Some((before, after)) = step.encoding {
            self.encoding = if forward { after } else { before };
        }
        if let Some((before, after)) = step.format {
            self.restore_format(if forward { after } else { before });
        }
    }

    /// Applies sorted, non-overlapping edits from last to first.
    fn apply_raw(&mut self, edits: &[Edit]) {
        if !self.markers.is_empty() {
            // An accepted identical replacement still has history/version semantics,
            // but it must not delete or move metadata attached to unchanged text.
            let changed: Vec<&Edit> = edits
                .iter()
                .filter(|edit| {
                    let start = self.rope.byte_to_char(edit.range.start().into());
                    let end = self.rope.byte_to_char(edit.range.end().into());
                    !self.rope.slice(start..end).chars().eq(edit.text.chars())
                })
                .collect();
            self.markers.apply(&changed, &self.rope);
        }
        for e in edits.iter().rev() {
            let s = self.rope.byte_to_char(e.range.start().into());
            let en = self.rope.byte_to_char(e.range.end().into());
            if s < en {
                self.rope.remove(s..en);
            }
            if !e.text.is_empty() {
                self.rope.insert(s, &e.text);
            }
        }
        self.markers.finish_lines(&self.rope);
        self.version += 1;
        self.change_log.push_back((self.version, edits.to_vec()));
        if self.change_log.len() > CHANGE_LOG_LIMIT {
            self.change_log.pop_front();
        }
    }
}

/// Preserve untouched owners around the one contiguous cookie/shebang change.
/// Prefix and suffix boundaries never split UTF-8 code points.
fn minimal_conversion_edit(original: &str, proposed: &str) -> Vec<Edit> {
    if original == proposed {
        return Vec::new();
    }
    let common = original
        .bytes()
        .zip(proposed.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let start = rustpy_core::floor_char_boundary(original, common);
    let suffix = original[start..]
        .bytes()
        .rev()
        .zip(proposed[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut old_end = original.len() - suffix;
    let mut new_end = proposed.len() - suffix;
    while !original.is_char_boundary(old_end) || !proposed.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    vec![Edit::replace(
        TextRange::new(TextSize::new(start as u32), TextSize::new(old_end as u32)),
        &proposed[start..new_end],
    )]
}

#[cfg(test)]
mod tests {
    #[test]
    fn restored_content_recognizes_a_later_save_of_the_original_text() {
        let mut doc = Document::new("A");
        let checkpoint = doc.content_checkpoint();
        let replace = |doc: &mut Document, text: &str| doc.set_text(text, &[], &[]).unwrap();
        replace(&mut doc, "B");
        replace(&mut doc, "A");
        doc.mark_saved();
        replace(&mut doc, "C");
        replace(&mut doc, "A");
        assert!(doc.recognize_restored_content(&checkpoint));
        assert!(!doc.is_modified());
        doc.undo();
        assert_eq!(doc.text(), "C");
        assert!(doc.is_modified());
        doc.redo();
        assert_eq!(doc.text(), "A");
        assert!(!doc.is_modified());
    }

    #[test]
    fn recognizing_a_dirty_checkpoint_after_undo_does_not_redirty_saved_text() {
        let mut doc = Document::new("A");
        for text in ["B", "A"] {
            doc.set_text(text, &[], &[]).unwrap();
        }
        let checkpoint = doc.content_checkpoint();
        assert!(doc.is_modified());
        doc.undo();
        doc.undo();
        assert!(!doc.is_modified());
        assert!(doc.recognize_restored_content(&checkpoint));
        assert!(!doc.is_modified());
        doc.redo();
        doc.redo();
        assert_eq!(doc.text(), "A");
        assert!(
            doc.is_modified(),
            "redo preserves the originally dirty identity"
        );
    }

    #[test]
    fn saving_an_old_saved_identity_after_checkpoint_is_still_a_save() {
        let mut doc = Document::new("A");
        for text in ["B", "A"] {
            doc.set_text(text, &[], &[]).unwrap();
        }
        let checkpoint = doc.content_checkpoint();
        assert!(doc.is_modified());
        doc.undo();
        doc.undo();
        doc.mark_saved();
        assert!(doc.recognize_restored_content(&checkpoint));
        assert!(!doc.is_modified());
        doc.redo();
        doc.redo();
        assert_eq!(doc.text(), "A");
        assert!(!doc.is_modified());
    }

    #[test]
    fn recognizing_a_saved_original_alias_keeps_deeper_undo_clean() {
        let mut doc = Document::new("A");
        let checkpoint = doc.content_checkpoint();
        for text in ["B", "A"] {
            doc.set_text(text, &[], &[]).unwrap();
        }
        doc.mark_saved();
        assert!(doc.recognize_restored_content(&checkpoint));
        assert!(!doc.is_modified());
        doc.undo();
        assert_eq!(doc.text(), "B");
        assert!(doc.is_modified());
        doc.undo();
        assert_eq!(doc.text(), "A");
        assert!(!doc.is_modified());
    }

    #[test]
    fn recognizing_original_text_after_undo_keeps_redo_identity_coherent() {
        let mut doc = Document::new("A");
        let checkpoint = doc.content_checkpoint();
        for text in ["B", "A", "C"] {
            doc.set_text(text, &[], &[]).unwrap();
        }
        doc.undo();
        assert!(doc.recognize_restored_content(&checkpoint));
        assert!(!doc.is_modified());
        assert!(doc.can_redo());
        doc.redo();
        assert_eq!(doc.text(), "C");
        assert!(doc.is_modified());
        doc.undo();
        assert_eq!(doc.text(), "A");
        assert!(!doc.is_modified());
    }

    #[test]
    fn a_content_checkpoint_cannot_clean_another_document_or_a_reload() {
        let original = Document::new("A");
        let checkpoint = original.content_checkpoint();
        let mut different = Document::new("B");
        different.set_text("A", &[], &[]).unwrap();
        assert!(!different.recognize_restored_content(&checkpoint));
        assert!(different.is_modified());
        let mut reset = Document::new("A");
        let checkpoint = reset.content_checkpoint();
        reset.reset("B");
        reset.set_text("A", &[], &[]).unwrap();
        assert!(!reset.recognize_restored_content(&checkpoint));
        assert!(reset.is_modified());
    }

    #[test]
    fn clamp_moves_stale_offsets_to_character_boundaries() {
        let d = Document::new("aé b");
        assert_eq!(d.clamp(TextSize::new(2)), TextSize::new(1), "inside 'é'");
        assert_eq!(d.clamp(TextSize::new(3)), TextSize::new(3));
        assert_eq!(d.clamp(TextSize::new(99)), d.len());
    }

    use super::*;
    use pretty_assertions::assert_eq;

    fn ts(n: u32) -> TextSize {
        TextSize::new(n)
    }

    #[test]
    fn positions_roundtrip_with_multibyte() {
        let d = Document::new("héllo\nwörld\n");
        assert_eq!(d.line_count(), 3);
        assert_eq!(d.offset_to_position(ts(0)), Position::new(0, 0));
        assert_eq!(d.offset_to_position(ts(3)), Position::new(0, 2)); // after 'é' (2 bytes)
        assert_eq!(d.position_to_offset(Position::new(1, 2)), ts(10));
        assert_eq!(d.position_to_offset(Position::new(1, 99)), ts(13));
        assert_eq!(d.position_to_offset(Position::new(99, 99)), d.len());
        assert_eq!(d.line_text(1), "wörld");
        assert_eq!(d.line_range(1), TextRange::new(ts(7), ts(13)));
    }

    #[test]
    fn edit_undo_redo() {
        let mut d = Document::new("abc");
        let c = [Selection::caret(ts(1))];
        d.edit(
            vec![Edit::insert(ts(1), "XY")],
            &c,
            &[Selection::caret(ts(3))],
            EditGroup::Open,
            None,
        )
        .unwrap();
        d.edit(
            vec![Edit::insert(ts(3), "Z")],
            &c,
            &[Selection::caret(ts(4))],
            EditGroup::Merge,
            None,
        )
        .unwrap();
        assert_eq!(d.text(), "aXYZbc");
        assert!(d.is_modified());
        assert_eq!(d.undo(), Some(vec![Selection::caret(ts(1))]));
        assert_eq!(d.text(), "abc");
        assert_eq!(d.redo(), Some(vec![Selection::caret(ts(4))]));
        assert_eq!(d.text(), "aXYZbc");
    }

    #[test]
    fn multi_edit_inverse() {
        let mut d = Document::new("0123456789");
        let edits = vec![
            Edit::replace(TextRange::new(ts(8), ts(9)), "EIGHT"),
            Edit::delete(TextRange::new(ts(1), ts(3))),
            Edit::insert(ts(5), "-"),
        ];
        d.edit(edits, &[], &[], EditGroup::Separate, Some("Multi"))
            .unwrap();
        assert_eq!(d.text(), "034-567EIGHT9");
        assert_eq!(d.undo_label().as_deref(), Some("Multi"));
        d.undo();
        assert_eq!(d.text(), "0123456789");
        assert!(!d.is_modified());
    }

    #[test]
    fn rejects_bad_edits() {
        let mut d = Document::new("abc");
        assert!(matches!(
            d.edit(
                vec![Edit::delete(TextRange::new(ts(2), ts(9)))],
                &[],
                &[],
                EditGroup::Separate,
                None
            ),
            Err(EditError::OutOfBounds(..))
        ));
        let overlapping = vec![
            Edit::delete(TextRange::new(ts(0), ts(2))),
            Edit::delete(TextRange::new(ts(1), ts(3))),
        ];
        assert!(matches!(
            d.edit(overlapping, &[], &[], EditGroup::Separate, None),
            Err(EditError::Overlapping(..))
        ));
        d.set_read_only(true);
        assert_eq!(
            d.edit(
                vec![Edit::insert(ts(0), "x")],
                &[],
                &[],
                EditGroup::Separate,
                None
            ),
            Err(EditError::ReadOnly)
        );
    }

    #[test]
    fn ascii_latin1_cookie_controls_later_edit_and_save() {
        let original = b"#!/usr/bin/python\r\n# coding: latin-1\r\ns = \"ascii\"\r\n";
        let mut d = Document::from_bytes(original).unwrap();
        assert_eq!(d.encoding(), Encoding::Latin1);
        assert_eq!(d.line_ending(), LineEnding::CrLf);
        assert!(!d.is_modified());
        let at = d.text().find("ascii").unwrap() as u32;
        d.edit(
            vec![Edit::replace(TextRange::new(ts(at), ts(at + 5)), "é")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        assert!(d.is_modified());
        let saved = b"#!/usr/bin/python\r\n# coding: latin-1\r\ns = \"\xe9\"\r\n";
        assert_eq!(d.to_bytes().unwrap(), saved);
        d.mark_saved();
        d.reload_from_bytes(saved).unwrap();
        assert_eq!(d.encoding(), Encoding::Latin1);
        assert!(d.text().contains("é"));
        assert!(!d.is_modified());
        assert_eq!(d.to_bytes().unwrap(), saved);
    }

    #[test]
    fn latin1_cookie_preserves_utf8_shaped_bytes_on_load_and_reload() {
        let bytes = b"# coding: latin-1\r\ns = \"\xc3\xa9\"\r\n";
        let mut d = Document::from_bytes(bytes).unwrap();
        assert!(d.text().contains("Ã©"));
        assert_eq!(d.to_bytes().unwrap(), bytes);
        d.reload_from_bytes(bytes).unwrap();
        assert_eq!(d.encoding(), Encoding::Latin1);
        assert_eq!(d.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn cookies_use_physical_lines_for_cr_crlf_and_mixed_files() {
        // Expected values verified with CPython compile(bytes), whose universal
        // newline handling differs from tokenize(BytesIO.readline) for lone CR.
        for first_end in ["\r", "\r\n", "\n"] {
            for second_end in ["\r", "\r\n", "\n"] {
                for (lines, encoding, value) in [
                    (vec!["# coding: latin-1", "x = 1"], Encoding::Latin1, "Ã©"),
                    (
                        vec!["# comment", "# coding: latin-1"],
                        Encoding::Latin1,
                        "Ã©",
                    ),
                    (vec!["", "# coding: latin-1"], Encoding::Latin1, "Ã©"),
                    (
                        vec!["# comment", "x = 1", "# coding: latin-1"],
                        Encoding::Utf8,
                        "é",
                    ),
                    (vec!["x = 1", "# coding: latin-1"], Encoding::Utf8, "é"),
                    (
                        vec!["x = '# coding: latin-1'", "# comment"],
                        Encoding::Utf8,
                        "é",
                    ),
                ] {
                    let source = format!(
                        "{}{}{}{}{}s = \"é\"\r",
                        lines[0],
                        first_end,
                        lines[1],
                        second_end,
                        if lines.len() > 2 {
                            format!("{}\r", lines[2])
                        } else {
                            String::new()
                        }
                    );
                    let mut d = Document::from_bytes(source.as_bytes()).unwrap();
                    assert_eq!(d.encoding(), encoding, "{source:?}");
                    assert!(
                        d.text().contains(&format!("s = \"{value}\"")),
                        "{source:?}: {}",
                        d.text()
                    );
                    assert!(!d.is_modified());
                    d.reload_from_bytes(source.as_bytes()).unwrap();
                    assert_eq!(d.encoding(), encoding);
                    // Saves can normalize mixed separators, but retain each byte
                    // representing the Python string's declared encoding.
                    let reloaded = Document::from_bytes(&d.to_bytes().unwrap()).unwrap();
                    assert_eq!(reloaded.text(), d.text());
                    assert_eq!(reloaded.encoding(), encoding);
                }
            }
        }
    }

    #[test]
    fn failed_reload_preserves_document_and_history() {
        let mut d = Document::from_bytes(b"# coding: latin-1\r\nx=1\r\n").unwrap();
        d.edit(
            vec![Edit::insert(d.len(), "é")],
            &[Selection::caret(ts(0))],
            &[Selection::caret(ts(1))],
            EditGroup::Separate,
            Some("Local"),
        )
        .unwrap();
        d.set_read_only(true);
        let (text, version, format) = (d.text(), d.version(), d.format());
        assert!(d.reload_from_bytes(b"\xff\xfea").is_err());
        assert_eq!((d.text(), d.version(), d.format()), (text, version, format));
        assert!(d.is_read_only() && d.is_modified() && d.can_undo());
        assert_eq!(d.undo_label().as_deref(), Some("Local"));
        assert_eq!(d.undo(), Some(vec![Selection::caret(ts(0))]));
        assert!(!d.is_modified());
    }

    #[test]
    fn failed_latin1_serialization_preserves_dirty_buffer() {
        let mut d = Document::from_bytes(b"# coding: latin-1\nx=1\n").unwrap();
        d.edit(
            vec![Edit::insert(d.len(), "😀")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        let (text, version) = (d.text(), d.version());
        assert!(d.to_bytes().is_err());
        assert_eq!((d.text(), d.version()), (text, version));
        assert!(d.is_modified() && d.can_undo());
    }

    #[test]
    fn crlf_roundtrip() {
        let mut d = Document::from_bytes(b"a\r\nb\r\n").unwrap();
        assert_eq!(d.line_ending(), LineEnding::CrLf);
        assert_eq!(d.text(), "a\nb\n");
        d.edit(
            vec![Edit::insert(ts(1), "\r\nX")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        assert_eq!(d.text(), "a\nX\nb\n");
        assert_eq!(d.to_bytes().unwrap(), b"a\r\nX\r\nb\r\n");
    }

    #[test]
    fn reset_keeps_the_version_increasing() {
        // A freshly opened (version 0) document reloaded from disk must not
        // look unchanged to caches keyed by the version.
        let mut d = Document::new("old\n");
        assert_eq!(d.version(), 0);
        d.reset("new text\n");
        assert_eq!(d.version(), 1);
        assert_eq!(d.text(), "new text\n");
        assert!(!d.is_modified());
        assert!(!d.can_undo());
        // The change log does not cover the reload: consumers refresh fully.
        assert_eq!(d.changes_since(0), None);
        d.edit(
            vec![Edit::insert(ts(0), "x")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        assert_eq!(d.version(), 2);
        assert_eq!(d.changes_since(1).map(|c| c.len()), Some(1));
    }

    #[test]
    fn changing_line_separator_or_encoding_modifies_the_document() {
        let mut d = Document::from_bytes(b"a\nb\n").unwrap();
        assert!(!d.is_modified());
        d.set_line_ending(LineEnding::CrLf);
        assert!(
            d.is_modified(),
            "Save All and the close prompt must see the conversion"
        );
        d.set_line_ending(LineEnding::Lf);
        assert!(!d.is_modified(), "back to the file's separator");
        d.set_line_ending(LineEnding::CrLf);
        d.mark_saved();
        assert!(!d.is_modified());
        // Undoing back to the saved text does not hide a pending conversion.
        d.set_line_ending(LineEnding::Lf);
        d.edit(
            vec![Edit::insert(ts(0), "x")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        d.undo();
        assert_eq!(d.text(), "a\nb\n");
        assert!(d.is_modified());
        let mut e = Document::from_bytes(b"x\n").unwrap();
        e.set_encoding(Encoding::Utf8Bom);
        assert!(e.is_modified());
        // Normalizing a mixed file to its dominant separator is a change too.
        let mut m = Document::from_bytes(b"a\r\nb\r\nc\n").unwrap();
        assert!(m.has_mixed_line_endings() && !m.is_modified());
        m.set_line_ending(LineEnding::CrLf);
        assert!(!m.has_mixed_line_endings() && m.is_modified());
        assert_eq!(m.to_bytes().unwrap(), b"a\r\nb\r\nc\r\n");
        // Saving a mixed file writes one separator: no stale "mixed" warning afterwards.
        let mut saved = Document::from_bytes(b"a\r\nb\r\nc\n").unwrap();
        assert_eq!(saved.to_bytes().unwrap(), b"a\r\nb\r\nc\r\n");
        saved.mark_saved();
        assert!(!saved.has_mixed_line_endings() && !saved.is_modified());
        saved.set_line_ending(LineEnding::CrLf);
        assert!(
            !saved.is_modified(),
            "converting to the separator already on disk changes nothing"
        );
    }

    #[test]
    fn reload_keeps_separators_and_advances_the_version() {
        let mut d = Document::from_bytes(b"a\r\nb\r\n").unwrap();
        let v = d.version();
        d.reload_from_bytes(b"x\r\ny\r\nz\r\n").unwrap();
        assert_eq!(d.text(), "x\ny\nz\n");
        assert_eq!(d.line_ending(), LineEnding::CrLf);
        assert!(!d.is_modified() && !d.can_undo());
        assert!(d.version() > v, "caches keyed on the version must refresh");
        assert_eq!(d.changes_since(v), None, "consumers do a full refresh");
        let mut r = Document::new("old");
        r.set_read_only(true);
        let v = r.version();
        r.reset("new");
        assert!(r.version() > v && r.is_read_only());
        assert_eq!(r.text(), "new");
        // A document decoded elsewhere: same rules.
        let v = r.version();
        r.reload_from(Document::from_bytes(b"p\r\nq\r\n").unwrap());
        assert!(r.version() > v && r.is_read_only() && !r.is_modified());
        assert_eq!(
            (r.text().as_str(), r.line_ending()),
            ("p\nq\n", LineEnding::CrLf)
        );
    }

    #[test]
    fn word_at() {
        let d = Document::new("foo_bar + baz1");
        assert_eq!(d.word_range_at(ts(2)), Some(TextRange::new(ts(0), ts(7))));
        assert_eq!(d.word_range_at(ts(7)), Some(TextRange::new(ts(0), ts(7))));
        assert_eq!(d.word_range_at(ts(8)), None);
        assert_eq!(
            d.word_range_at(ts(14)),
            Some(TextRange::new(ts(10), ts(14)))
        );
    }
}

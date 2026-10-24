//! RustPy IDE — rope-backed text documents with undo/redo, line/column mapping,
//! line-ending and encoding handling. (Roadmap §0.2)

mod document;
mod encoding;
mod marker;
mod position;
mod undo;
pub mod util;

pub use document::{
    ContentCheckpoint, Document, DocumentFormat, Edit, EditError, EditGroup,
    EncodingConversionError, EncodingConversionPlan, PreparedSaveError, PreparedSavePlan,
    Selection,
};
pub use encoding::{
    is_python_source_path, Encoding, EncodingConversion, EncodingError, LineEnding, SourcePolicy,
};
pub use position::{line_column, line_offset, text_position, ColumnUnits, TextIndex, TextPosition};
pub use rustpy_core::{Position, TextRange, TextSize};
pub use undo::{HistoryEntry, HistoryError, HistoryId, Transaction, TransactionToken};

pub use marker::{
    Affinity, DocumentId, JournalCursor, MarkerError, MarkerId, MarkerPosition, PointDeletion,
    RangeDeletion,
};

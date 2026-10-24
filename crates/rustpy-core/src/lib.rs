//! RustPy IDE — shared foundation types used by every other crate.
//!
//! This crate deliberately stays small: identifiers, text offsets, and the
//! shared infrastructure of long operations — cancellation tokens
//! ([`cancel`]), background tasks with progress ([`tasks`]), the thread pool
//! of background scans ([`parallel`]), cancellable external commands
//! ([`process`]) and a typed event bus ([`events`]).
//! Anything heavier lives in a dedicated crate.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

pub use text_size::{TextLen, TextRange, TextSize};

pub mod cancel;
pub mod events;
pub mod interpreter_launch;
pub mod memory;
pub mod orphan_guard;
pub mod parallel;
pub mod process;
pub mod process_tree;
pub mod tasks;
pub mod vfs;

pub use cancel::{CancellationToken, Cancelled};
pub use events::EventBus;
pub use tasks::{ProgressReporter, TaskError, TaskHandle, TaskManager, TaskResult};

/// Stable identifier of a file known to the IDE (open document, indexed file, …).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct FileId(pub u32);

impl fmt::Debug for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileId({})", self.0)
    }
}

/// Allocates unique [`FileId`]s.
#[derive(Default)]
pub struct FileIdAllocator {
    next: AtomicU32,
}

impl FileIdAllocator {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
        }
    }
    pub fn alloc(&self) -> FileId {
        FileId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// A line/column position. `line` is zero-based; `column` is a zero-based
/// **character** (Unicode scalar value) index within the line.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line + 1, self.column + 1)
    }
}

/// Severity of a diagnostic, mirroring the inspection profile severities.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Severity {
    Info,
    Typo,
    WeakWarning,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "Information",
            Severity::Typo => "Typo",
            Severity::WeakWarning => "Weak Warning",
            Severity::Warning => "Warning",
            Severity::Error => "Error",
        }
    }
}

/// A diagnostic message anchored to a byte range of a file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub range: TextRange,
    pub severity: Severity,
    pub message: String,
    /// Inspection id such as `PyUnresolvedReferences`, or `syntax`.
    pub code: smol_str::SmolStr,
}

impl Diagnostic {
    pub fn new(
        range: TextRange,
        severity: Severity,
        code: impl Into<smol_str::SmolStr>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            range,
            severity,
            message: message.into(),
            code: code.into(),
        }
    }
    pub fn error(range: TextRange, code: &str, message: impl Into<String>) -> Self {
        Self::new(range, Severity::Error, code, message)
    }
    pub fn warning(range: TextRange, code: &str, message: impl Into<String>) -> Self {
        Self::new(range, Severity::Warning, code, message)
    }
}

/// Returns true for characters that may start a Python identifier.
/// The largest character boundary of `s` at or before `i` (clamped to the
/// length): `str::floor_char_boundary` for the workspace's minimum Rust version.
pub fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// Returns true for characters that may continue a Python identifier.
pub fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_display_is_one_based() {
        assert_eq!(Position::new(0, 0).to_string(), "1:1");
    }
}

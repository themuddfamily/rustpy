//! Independent caret and viewport positions persisted with an editor tab.

use rustpy_core::Position;

/// Logical source lines, independent of wrapping, folding and rendered documentation.
/// Only the top logical line is persisted, not its wrapped sub-row or pixel offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionPosition {
    pub caret: Position,
    pub scroll_line: u32,
}

impl From<&rustpy_settings::OpenFileState> for SessionPosition {
    fn from(file: &rustpy_settings::OpenFileState) -> Self {
        Self {
            caret: Position::new(file.caret_line, file.caret_column),
            scroll_line: file.scroll_line,
        }
    }
}

impl SessionPosition {
    pub fn into_open_file_state(
        self,
        path: std::path::PathBuf,
        pinned: bool,
    ) -> rustpy_settings::OpenFileState {
        rustpy_settings::OpenFileState {
            path,
            caret_line: self.caret.line,
            caret_column: self.caret.column,
            scroll_line: self.scroll_line,
            pinned,
        }
    }
}

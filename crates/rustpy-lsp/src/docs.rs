//! Open documents: the client's text with incremental changes applied.

use std::path::PathBuf;
use std::sync::Arc;

use lsp_types::{TextDocumentContentChangeEvent, Uri};

use crate::line_index::{Encoding, LineIndex};

#[derive(Clone, Debug)]
pub struct Doc {
    pub uri: Uri,
    /// Real path, or the `<typeshed>/…` path of a bundled stub.
    pub path: PathBuf,
    pub version: i32,
    pub is_python: bool,
    /// The text and its line starts.
    pub index: Arc<LineIndex>,
}

impl Doc {
    pub fn new(uri: Uri, path: PathBuf, version: i32, language_id: &str, text: String) -> Self {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let is_python =
            matches!(language_id, "python" | "py" | "pyi") || matches!(ext, "py" | "pyi" | "pyw");
        Self {
            uri,
            path,
            version,
            is_python,
            index: Arc::new(LineIndex::new(text)),
        }
    }

    pub fn text(&self) -> &str {
        self.index.text()
    }

    /// Applies `changes` in order (ranged edits, or a full replacement for a
    /// change without a range). Ranges are clamped to the text.
    pub fn apply_changes(
        &mut self,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
        enc: Encoding,
    ) {
        // A single full replacement is the common "full sync" fallback.
        let mut text: Option<String> = None;
        for change in changes {
            match change.range {
                None => {
                    text = Some(change.text);
                    self.index = Arc::new(LineIndex::new(text.as_deref().unwrap_or_default()));
                }
                Some(range) => {
                    let r = self.index.text_range(range, enc);
                    let mut t = text.take().unwrap_or_else(|| self.index.text().to_string());
                    let (s, e) = (
                        usize::from(r.start()).min(t.len()),
                        usize::from(r.end()).min(t.len()),
                    );
                    t.replace_range(s..e, &change.text);
                    self.index = Arc::new(LineIndex::new(t.as_str()));
                    text = Some(t);
                }
            }
        }
        self.version = version;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};
    use std::str::FromStr;

    #[test]
    fn incremental_and_full_changes() {
        let uri = Uri::from_str("file:///p/m.py").unwrap();
        let mut d = Doc::new(
            uri,
            "/p/m.py".into(),
            1,
            "python",
            "def f():\n    return 1\n".into(),
        );
        assert!(d.is_python);
        let change = |l1, c1, l2, c2, t: &str| TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(l1, c1), Position::new(l2, c2))),
            range_length: None,
            text: t.into(),
        };
        d.apply_changes(
            2,
            vec![change(1, 11, 1, 12, "42"), change(0, 4, 0, 5, "g")],
            Encoding::Utf16,
        );
        assert_eq!(d.text(), "def g():\n    return 42\n");
        assert_eq!(d.version, 2);
        // Out-of-range positions clamp to the end.
        d.apply_changes(3, vec![change(9, 0, 9, 0, "x = 1\n")], Encoding::Utf16);
        assert_eq!(d.text(), "def g():\n    return 42\nx = 1\n");
        d.apply_changes(
            4,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "y = 2\n".into(),
            }],
            Encoding::Utf16,
        );
        assert_eq!(d.text(), "y = 2\n");
    }
}

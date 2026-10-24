//! Console input history with up/down navigation and on-disk persistence.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "console_history.txt";
const MAX_ENTRIES: usize = 1000;
/// Multi-line entries are stored on one line with this separator.
const NEWLINE_MARK: &str = "\u{241E}";

/// Command history (newest last). `prev`/`next` walk from the end like a shell.
#[derive(Debug, Default, Clone)]
pub struct History {
    entries: Vec<String>,
    limit: Option<usize>,
    /// Index into `entries` while navigating; `None` = at the fresh prompt.
    cursor: Option<usize>,
    path: Option<PathBuf>,
}

impl History {
    /// Loads `<dir>/console_history.txt` (missing file = empty history).
    pub fn load(dir: &Path) -> History {
        let path = dir.join(FILE_NAME);
        let entries = std::fs::read_to_string(&path)
            .map(|t| {
                t.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.replace(NEWLINE_MARK, "\n"))
                    .collect()
            })
            .unwrap_or_default();
        History {
            entries,
            limit: None,
            cursor: None,
            path: Some(path),
        }
    }

    /// In-memory history that is never written to disk.
    pub fn in_memory() -> History {
        History::default()
    }

    pub fn set_limit(&mut self, limit: usize) {
        let limit = limit.clamp(1, 100_000);
        self.limit = Some(limit);
        if self.entries.len() > limit {
            self.entries.drain(..self.entries.len() - limit);
            self.cursor = None;
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records an executed input (blank input and consecutive duplicates are skipped).
    pub fn push(&mut self, entry: &str) {
        self.cursor = None;
        let entry = entry.trim_end_matches('\n');
        if entry.trim().is_empty() {
            return;
        }
        if self.entries.last().map(String::as_str) == Some(entry) {
            return;
        }
        self.entries.push(entry.to_string());
        let limit = self.limit.unwrap_or(MAX_ENTRIES);
        if self.entries.len() > limit {
            let excess = self.entries.len() - limit;
            self.entries.drain(0..excess);
        }
        self.save();
    }

    /// Older entry (Up arrow); `None` when already at the oldest.
    pub fn prev(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => self.entries.len() - 1,
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// Newer entry (Down arrow); `None` (fresh prompt) after the newest one.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&str> {
        let i = self.cursor?;
        if i + 1 >= self.entries.len() {
            self.cursor = None;
            return None;
        }
        self.cursor = Some(i + 1);
        self.entries.get(i + 1).map(String::as_str)
    }

    /// Leaves navigation mode (e.g. after the user edits the prompt).
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = None;
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text: String = self
            .entries
            .iter()
            .map(|e| e.replace('\n', NEWLINE_MARK) + "\n")
            .collect();
        if let Err(e) = std::fs::write(path, text) {
            log::warn!("cannot save console history to {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_and_dedupe() {
        let mut h = History::in_memory();
        assert!(h.prev().is_none());
        h.push("a = 1");
        h.push("a = 1");
        h.push("print(a)");
        h.push("   ");
        assert_eq!(h.len(), 2);
        assert_eq!(h.prev(), Some("print(a)"));
        assert_eq!(h.prev(), Some("a = 1"));
        assert_eq!(h.prev(), None);
        assert_eq!(h.next(), Some("print(a)"));
        assert_eq!(h.next(), None);
        assert_eq!(h.prev(), Some("print(a)"));
        h.reset_cursor();
        assert_eq!(h.prev(), Some("print(a)"));
    }

    #[test]
    fn persists_multiline_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load(dir.path());
        h.push("def f():\n    return 1");
        h.push("f()");
        let again = History::load(dir.path());
        assert_eq!(
            again.entries(),
            &["def f():\n    return 1".to_string(), "f()".to_string()]
        );
    }
}

#[cfg(test)]
mod configured_limit_fixture {
    use super::*;
    #[test]
    fn configured_history_limit_bounds_real_push_and_navigation() {
        let mut history = History::in_memory();
        history.set_limit(2);
        history.push("first");
        history.push("second");
        history.push("third");
        assert_eq!(history.entries(), &["second", "third"]);
        assert_eq!(history.prev(), Some("third"));
        history.set_limit(1);
        assert_eq!(history.entries(), &["third"]);
        assert_eq!(history.prev(), Some("third"));
    }
}

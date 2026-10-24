//! Navigation history behind Back/Forward (Ctrl+Alt+←/→) and Recent
//! Locations (Ctrl+Shift+E), roadmap §2.7.
//!
//! The editor reports the active caret once per frame ([`NavHistory::observe`]).
//! A place is recorded whenever the active file changes or the caret jumps
//! [`NavHistory::jump_lines`] lines or more without a document edit, so every
//! way of getting somewhere counts: Search Everywhere, Go to File/Class/Symbol,
//! Find in Files, Go to Line, File Structure, bookmarks, tab switches, the
//! project tree and long caret moves. Explicit jumps
//! ([`NavHistory::mark_jump`]) are recorded however short. Places are kept as
//! line/column and follow line insertions and deletions
//! ([`NavHistory::shift_lines`]); edited places are collected separately for
//! *Show changed only*.

use std::path::{Path, PathBuf};

/// The visible tool-window selection and dimensions at a navigation place.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NavContext {
    /// Serialized existing ToolWindowLayout (including floating/maximized state).
    pub layout: String,
    /// Selection/visibility identity; resizing panels is not another navigation jump.
    pub identity: String,
}

/// A place in a file: 0-based line and character column.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NavPoint {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub context: Option<NavContext>,
}

impl NavPoint {
    pub fn new(path: impl Into<PathBuf>, line: usize, col: usize) -> Self {
        Self {
            path: path.into(),
            line,
            col,
            context: None,
        }
    }

    pub fn same_place(&self, other: &Self) -> bool {
        self.same_line(other)
            && self.context.as_ref().map(|context| &context.identity)
                == other.context.as_ref().map(|context| &context.identity)
    }

    /// Same file and line (the column does not make a different place).
    pub fn same_line(&self, other: &NavPoint) -> bool {
        self.path == other.path && self.line == other.line
    }
}

const BACK_LIMIT: usize = 100;
const CHANGED_LIMIT: usize = 50;
const VISITED_LIMIT: usize = 60;

#[derive(Clone, Debug)]
pub struct NavHistory {
    back: Vec<NavPoint>,
    forward: Vec<NavPoint>,
    /// Caret position at the last observation and its document's version.
    current: Option<(NavPoint, u64)>,
    /// Places where text was edited, oldest first.
    changed: Vec<NavPoint>,
    /// Every place left, oldest first, one entry per line (Recent Locations).
    visited: Vec<NavPoint>,
    /// Caret moves of at least this many lines without an edit are navigation.
    pub jump_lines: usize,
}

impl Default for NavHistory {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
            current: None,
            changed: Vec::new(),
            visited: Vec::new(),
            jump_lines: 10,
        }
    }
}

impl NavHistory {
    /// Feeds the active editor's caret (call once per frame). Records the
    /// previous place when the file changed or the caret jumped far without
    /// an edit; an edit records a changed place instead.
    pub fn observe(&mut self, at: NavPoint, doc_version: u64) {
        if let Some((prev, version)) = self.current.take() {
            if prev.path != at.path {
                self.record(prev);
            } else if version != doc_version {
                // Typing, undo or a refactoring moved the caret: an edit, not navigation.
                self.note_edit(at.clone());
            } else if prev.context.as_ref().map(|context| &context.identity)
                != at.context.as_ref().map(|context| &context.identity)
                || prev.line.abs_diff(at.line) >= self.jump_lines.max(1)
            {
                self.record(prev);
            }
        }
        self.current = Some((at, doc_version));
    }

    /// An explicit jump (Go to Declaration, a search result, Go to Line, …)
    /// is about to leave `from`: recorded however short the distance.
    pub fn mark_jump(&mut self, from: NavPoint) {
        self.record(from);
        // The next observation is the jump's target: nothing more to record.
        self.current = None;
    }

    /// A new place: clears the forward history.
    fn record(&mut self, p: NavPoint) {
        self.visit(p.clone());
        self.push_back(p);
        self.forward.clear();
    }

    /// Notes a place that was left (for Recent Locations).
    fn visit(&mut self, p: NavPoint) {
        self.visited.retain(|v| !v.same_line(&p));
        self.visited.push(p);
        if self.visited.len() > VISITED_LIMIT {
            self.visited.remove(0);
        }
    }

    fn push_back(&mut self, p: NavPoint) {
        if self.back.last().is_some_and(|l| l.same_place(&p)) {
            self.back.pop();
        }
        self.back.push(p);
        if self.back.len() > BACK_LIMIT {
            self.back.remove(0);
        }
    }

    /// Back: the previous place (skipping places `valid` rejects, such as
    /// deleted files, and the current line). `current` moves to the forward list.
    pub fn back(
        &mut self,
        current: Option<NavPoint>,
        valid: impl Fn(&NavPoint) -> bool,
    ) -> Option<NavPoint> {
        while let Some(p) = self.back.pop() {
            if !valid(&p) || current.as_ref().is_some_and(|c| c.same_place(&p)) {
                continue;
            }
            if let Some(c) = current {
                self.visit(c.clone());
                if !self.forward.last().is_some_and(|f| f.same_place(&c)) {
                    self.forward.push(c);
                }
            }
            self.current = None;
            return Some(p);
        }
        None
    }

    /// Forward: undoes the last [`back`](Self::back).
    pub fn forward(
        &mut self,
        current: Option<NavPoint>,
        valid: impl Fn(&NavPoint) -> bool,
    ) -> Option<NavPoint> {
        while let Some(p) = self.forward.pop() {
            if !valid(&p) || current.as_ref().is_some_and(|c| c.same_place(&p)) {
                continue;
            }
            if let Some(c) = current {
                self.visit(c.clone());
                self.push_back(c);
            }
            self.current = None;
            return Some(p);
        }
        None
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Records an edit at `at`. Edits close to the previous one (typing) move
    /// that place; elsewhere they add a new place.
    pub fn note_edit(&mut self, at: NavPoint) {
        let near =
            |p: &NavPoint| p.path == at.path && p.line.abs_diff(at.line) < self.jump_lines.max(1);
        self.changed.retain(|p| !near(p));
        self.changed.push(at);
        if self.changed.len() > CHANGED_LIMIT {
            self.changed.remove(0);
        }
    }

    /// Visited places for Recent Locations, most recently left first, one
    /// per line (places reached by Back/Forward included).
    pub fn recent_places(&self) -> Vec<NavPoint> {
        self.visited.iter().rev().cloned().collect()
    }

    /// Edited places, most recent first.
    pub fn changed_places(&self) -> Vec<NavPoint> {
        self.changed.iter().rev().cloned().collect()
    }

    /// Keeps places in `path` on their text after `delta` lines were inserted
    /// (positive) or deleted (negative) below 0-based line `first_line`.
    /// Places inside a deleted block move to its start.
    pub fn shift_lines(&mut self, path: &Path, first_line: usize, delta: i64) {
        if delta == 0 {
            return;
        }
        let shift = |p: &mut NavPoint| {
            if p.path == path && p.line > first_line {
                p.line = (p.line as i64 + delta).max(first_line as i64) as usize;
            }
        };
        self.back.iter_mut().for_each(shift);
        self.forward.iter_mut().for_each(shift);
        self.changed.iter_mut().for_each(shift);
        self.visited.iter_mut().for_each(shift);
        if let Some((p, _)) = &mut self.current {
            shift(p);
        }
    }

    /// Follows a file or directory rename (or drops the places in it when `to` is `None`).
    pub fn rename_file(&mut self, from: &Path, to: Option<&Path>) {
        let moved = |p: &Path| -> Option<PathBuf> {
            let rel = p.strip_prefix(from).ok()?;
            let to = to?;
            Some(if rel.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rel)
            })
        };
        for list in [
            &mut self.back,
            &mut self.forward,
            &mut self.changed,
            &mut self.visited,
        ] {
            match to {
                Some(_) => list.iter_mut().for_each(|p| {
                    if let Some(n) = moved(&p.path) {
                        p.path = n;
                    }
                }),
                None => list.retain(|p| !p.path.starts_with(from)),
            }
        }
        // The editor reports the new path next: not a move to another file.
        if self
            .current
            .as_ref()
            .is_some_and(|(p, _)| p.path.starts_with(from))
        {
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(file: &str, line: usize) -> NavPoint {
        NavPoint::new(format!("/p/{file}"), line, 0)
    }

    #[test]
    fn switching_files_records_the_place_left() {
        let mut h = NavHistory::default();
        h.observe(p("a.py", 3), 1);
        h.observe(p("a.py", 3), 1);
        assert!(!h.can_go_back(), "standing still records nothing");
        h.observe(p("b.py", 0), 7);
        assert_eq!(h.recent_places(), vec![p("a.py", 3)]);
        assert_eq!(h.back(Some(p("b.py", 0)), |_| true), Some(p("a.py", 3)));
    }

    #[test]
    fn long_caret_jumps_count_but_small_moves_and_typing_do_not() {
        let mut h = NavHistory::default();
        h.observe(p("a.py", 10), 1);
        // Arrow keys: one line per frame, never a jump however far in total.
        for line in 11..40 {
            h.observe(p("a.py", line), 1);
        }
        assert!(!h.can_go_back());
        // Typing Enter fifteen times moves the caret with edits: a changed place only.
        for (i, line) in (40..55).enumerate() {
            h.observe(p("a.py", line), 2 + i as u64);
        }
        assert!(!h.can_go_back());
        assert_eq!(
            h.changed_places(),
            vec![p("a.py", 54)],
            "typing merges into one changed place"
        );
        // Ctrl+End / a click far away without an edit: navigation.
        h.observe(p("a.py", 200), 16);
        assert_eq!(h.recent_places(), vec![p("a.py", 54)]);
        // A short jump below the threshold is not.
        h.observe(p("a.py", 205), 16);
        assert_eq!(h.recent_places().len(), 1);
    }

    #[test]
    fn back_and_forward_are_symmetric() {
        let mut h = NavHistory::default();
        h.observe(p("a.py", 1), 1);
        h.observe(p("b.py", 5), 1);
        h.observe(p("b.py", 50), 1);
        // back: b:50 → b:5 → a:1
        assert_eq!(h.back(Some(p("b.py", 50)), |_| true), Some(p("b.py", 5)));
        h.observe(p("b.py", 5), 1);
        assert_eq!(h.back(Some(p("b.py", 5)), |_| true), Some(p("a.py", 1)));
        h.observe(p("a.py", 1), 1);
        assert_eq!(
            h.back(Some(p("a.py", 1)), |_| true),
            None,
            "nothing further back"
        );
        // forward: a:1 → b:5 → b:50
        assert_eq!(h.forward(Some(p("a.py", 1)), |_| true), Some(p("b.py", 5)));
        h.observe(p("b.py", 5), 1);
        assert_eq!(h.forward(Some(p("b.py", 5)), |_| true), Some(p("b.py", 50)));
        h.observe(p("b.py", 50), 1);
        assert!(!h.can_go_forward());
        // And back again walks the same places.
        assert_eq!(h.back(Some(p("b.py", 50)), |_| true), Some(p("b.py", 5)));
        // A new navigation drops the forward list.
        h.observe(p("b.py", 5), 1);
        h.observe(p("c.py", 0), 1);
        assert!(!h.can_go_forward());
    }

    #[test]
    fn recent_places_include_places_left_by_back_and_forward() {
        let mut h = NavHistory::default();
        h.observe(p("a.py", 2), 1);
        h.observe(p("a.py", 90), 1);
        // Ctrl+End then Back: the end of the file is still a recent location.
        assert_eq!(h.back(Some(p("a.py", 90)), |_| true), Some(p("a.py", 2)));
        h.observe(p("a.py", 2), 1);
        assert_eq!(h.recent_places(), vec![p("a.py", 90), p("a.py", 2)]);
        // Forward again: the place left by Forward moves to the top.
        assert_eq!(h.forward(Some(p("a.py", 2)), |_| true), Some(p("a.py", 90)));
        assert_eq!(
            h.recent_places(),
            vec![p("a.py", 2), p("a.py", 90)],
            "one entry per line, most recent first"
        );
    }

    #[test]
    fn explicit_jumps_are_recorded_however_short() {
        let mut h = NavHistory::default();
        h.observe(p("a.py", 10), 1);
        h.mark_jump(p("a.py", 10));
        h.observe(p("a.py", 12), 1);
        assert_eq!(h.recent_places(), vec![p("a.py", 10)]);
        // The jump target is not recorded a second time on the next frame.
        h.observe(p("b.py", 0), 1);
        assert_eq!(h.recent_places(), vec![p("a.py", 12), p("a.py", 10)]);
    }

    #[test]
    fn back_skips_deleted_files_and_the_current_line() {
        let mut h = NavHistory::default();
        h.mark_jump(p("gone.py", 1));
        h.mark_jump(p("a.py", 3));
        h.mark_jump(p("b.py", 8));
        let target = h.back(Some(p("b.py", 8)), |q| !q.path.ends_with("gone.py"));
        assert_eq!(target, Some(p("a.py", 3)), "b:8 is where we are");
        assert_eq!(
            h.back(Some(p("a.py", 3)), |q| !q.path.ends_with("gone.py")),
            None
        );
    }

    #[test]
    fn places_follow_inserted_and_deleted_lines() {
        let mut h = NavHistory::default();
        h.mark_jump(p("a.py", 5));
        h.mark_jump(p("a.py", 40));
        h.mark_jump(p("b.py", 40));
        h.note_edit(p("a.py", 60));
        // Three lines inserted at line 10 of a.py.
        h.shift_lines(Path::new("/p/a.py"), 10, 3);
        assert_eq!(
            h.recent_places(),
            vec![p("b.py", 40), p("a.py", 43), p("a.py", 5)]
        );
        assert_eq!(h.changed_places(), vec![p("a.py", 63)]);
        // Lines 30..=50 deleted: places inside move to the start of the gap.
        h.shift_lines(Path::new("/p/a.py"), 30, -21);
        assert_eq!(
            h.recent_places(),
            vec![p("b.py", 40), p("a.py", 30), p("a.py", 5)]
        );
        assert_eq!(h.changed_places(), vec![p("a.py", 42)]);
        h.rename_file(Path::new("/p/a.py"), Some(Path::new("/p/c.py")));
        assert_eq!(h.recent_places()[1], p("c.py", 30));
        h.rename_file(Path::new("/p/c.py"), None);
        assert_eq!(h.recent_places(), vec![p("b.py", 40)]);
    }

    #[test]
    fn renames_move_places_and_are_not_navigation() {
        let mut h = NavHistory::default();
        h.mark_jump(p("pkg/a.py", 4));
        h.mark_jump(p("pkg/sub/b.py", 9));
        h.mark_jump(p("pkgx.py", 1));
        h.observe(p("pkg/a.py", 20), 3);
        // The package directory is renamed while pkg/a.py is open.
        h.rename_file(Path::new("/p/pkg"), Some(Path::new("/p/lib")));
        assert_eq!(
            h.recent_places(),
            vec![p("pkgx.py", 1), p("lib/sub/b.py", 9), p("lib/a.py", 4)],
            "only paths inside the directory move"
        );
        // The editor now reports the new path: the old one is not recorded as a place left.
        h.observe(p("lib/a.py", 20), 3);
        assert_eq!(h.recent_places().len(), 3);
        assert_eq!(
            h.back(Some(p("lib/a.py", 20)), |_| true),
            Some(p("pkgx.py", 1))
        );
        // Deleting the directory drops every place inside it.
        h.rename_file(Path::new("/p/lib"), None);
        assert_eq!(h.recent_places(), vec![p("pkgx.py", 1)]);
        assert!(!h.can_go_forward(), "lib/a.py:20 went with it");
    }

    #[test]
    fn changed_places_keep_distinct_regions_most_recent_first() {
        let mut h = NavHistory::default();
        h.note_edit(p("a.py", 10));
        h.note_edit(p("a.py", 100));
        h.note_edit(p("b.py", 1));
        h.note_edit(p("a.py", 12));
        assert_eq!(
            h.changed_places(),
            vec![p("a.py", 12), p("b.py", 1), p("a.py", 100)]
        );
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn tool_state_changes_are_places_but_size_changes_do_not_create_jumps() {
        let mut history = NavHistory::default();
        let mut editor = NavPoint::new("/a.py", 2, 4);
        editor.context = Some(NavContext {
            layout: "260px".into(),
            identity: "Project".into(),
        });
        history.observe(editor.clone(), 1);
        let mut resized = editor.clone();
        resized.context.as_mut().unwrap().layout = "320px".into();
        history.observe(resized.clone(), 1);
        assert!(!history.can_go_back());
        let mut tool = resized.clone();
        tool.context.as_mut().unwrap().identity = "Git".into();
        history.observe(tool.clone(), 1);
        assert_eq!(history.back(Some(tool.clone()), |_| true), Some(resized));
        assert_eq!(history.forward(Some(editor), |_| true), Some(tool));
        assert!(history.changed_places().is_empty());
    }
}

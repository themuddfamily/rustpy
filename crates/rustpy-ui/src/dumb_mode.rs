//! Dumb mode: while the symbol indexes are being
//! built, features that need complete indexes either wait — the action is
//! refused with a status-bar notice offering "Run when indexing finishes" —
//! or run with their results flagged as possibly incomplete.

use crate::actions::Action;

/// How an action behaves while indexing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumbPolicy {
    /// Does not depend on the indexes (dumb-aware).
    Available,
    /// Runs on what is indexed so far; results are flagged as incomplete.
    Partial,
    /// Not available until indexing finishes; can be queued to run then.
    Wait,
}

/// The policy of `action` (actions not listed are dumb-aware).
pub fn policy(action: Action) -> DumbPolicy {
    use Action::*;
    match action {
        GoToClass | GoToSymbol => DumbPolicy::Partial,
        FindUsages | ShowUsages | GotoImplementation | TypeHierarchy | MethodHierarchy
        | CallHierarchy | Rename | RefactorThis | MoveRefactoring | MoveSymbol | RenameModule
        | SafeDelete | ChangeSignature | Inline | PullMembersUp | PushMembersDown
        | ExtractSuperclass => DumbPolicy::Wait,
        _ => DumbPolicy::Available,
    }
}

/// What to do with an action fired now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Run,
    /// Run, telling the user the results may be incomplete.
    RunPartial,
    /// Refused; [`DumbMode::notice`] offers to queue it.
    Blocked,
}

/// The editor an action was invoked in: its document incarnation, version
/// and carets/selections, with the text around each caret so that a queued
/// action can tell whether later edits left its target in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorTarget {
    pub document: rustpy_text::DocumentId,
    pub version: u64,
    pub carets: Vec<crate::editor::Caret>,
    /// Per caret: the selection, or the identifier around the caret, and its text.
    contexts: Vec<(rustpy_core::TextRange, String)>,
}

impl EditorTarget {
    pub fn capture(
        document: rustpy_text::DocumentId,
        version: u64,
        carets: Vec<crate::editor::Caret>,
        text: &str,
    ) -> Self {
        let contexts = carets
            .iter()
            .map(|caret| {
                let range = if caret.has_selection() {
                    caret.range()
                } else {
                    word_around(text, usize::from(caret.head()))
                };
                let snippet = text
                    .get(usize::from(range.start())..usize::from(range.end()))
                    .unwrap_or_default()
                    .to_string();
                (range, snippet)
            })
            .collect();
        Self {
            document,
            version,
            carets,
            contexts,
        }
    }

    /// Whether the target is still where it was in the document now at
    /// `version` with `text`: unchanged, or every caret's selection or
    /// identifier still reads the same at the same offsets.
    pub fn still_valid(&self, document: rustpy_text::DocumentId, version: u64, text: &str) -> bool {
        if document != self.document {
            return false;
        }
        version == self.version
            || self.contexts.iter().all(|(range, snippet)| {
                !snippet.is_empty()
                    && text.get(usize::from(range.start())..usize::from(range.end()))
                        == Some(snippet.as_str())
            })
    }
}

/// The identifier (letters, digits, `_`) touching byte `offset` of `text`.
fn word_around(text: &str, offset: usize) -> rustpy_core::TextRange {
    let offset = offset.min(text.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(offset, |(i, _)| i);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(i, _)| offset + i);
    rustpy_core::TextRange::new((start as u32).into(), (end as u32).into())
}

/// Where a refused action was invoked, to replay it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// No particular target (the action uses whatever is current).
    None,
    Editor(EditorTarget),
    /// A project-tree or navigation-bar path.
    Path(std::path::PathBuf),
    /// Several project-tree paths (Safe Delete of a selection).
    Paths(Vec<std::path::PathBuf>),
}

/// An action refused while indexing, with where it was invoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub action: Action,
    pub target: Target,
}

/// The refused action offered for "Run when indexing finishes", and the
/// actions queued to run then.
#[derive(Debug, Default)]
pub struct DumbMode {
    notice: Option<Pending>,
    queued: Vec<Pending>,
    /// Tests: dumb mode on or off regardless of the indexes.
    #[cfg(test)]
    pub(crate) forced: Option<bool>,
}

impl DumbMode {
    /// Decides about `action` fired while the indexes are `dumb` (being
    /// built); `target` is captured only when the action is refused.
    pub fn gate(&mut self, action: Action, dumb: bool, target: impl FnOnce() -> Target) -> Gate {
        if !dumb {
            return Gate::Run;
        }
        match policy(action) {
            DumbPolicy::Available => Gate::Run,
            DumbPolicy::Partial => Gate::RunPartial,
            DumbPolicy::Wait => {
                self.notice = Some(Pending {
                    action,
                    target: target(),
                });
                Gate::Blocked
            }
        }
    }

    /// The last refused action, while it can still be queued.
    pub fn notice(&self) -> Option<Action> {
        self.notice.as_ref().map(|pending| pending.action)
    }

    pub fn dismiss(&mut self) {
        self.notice = None;
    }

    /// "Run when indexing finishes" for the refused action (at its target).
    pub fn queue_notice(&mut self) {
        if let Some(pending) = self.notice.take() {
            if !self.queued.contains(&pending) {
                self.queued.push(pending);
            }
        }
    }

    pub fn queued(&self) -> impl Iterator<Item = Action> + '_ {
        self.queued.iter().map(|pending| pending.action)
    }

    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Once indexing finished: the queued actions to run now, in order (and
    /// the notice is retired). Nothing while still `dumb`.
    pub fn take_ready(&mut self, dumb: bool) -> Vec<Pending> {
        if dumb {
            return Vec::new();
        }
        self.notice = None;
        std::mem::take(&mut self.queued)
    }
}

/// The status-bar text for a queued action whose target is gone.
pub fn skipped_text(action: Action) -> String {
    format!(
        "{} was not run: its target changed while indexing",
        action.text().trim_end_matches('…')
    )
}

/// The status-bar text for a refused action.
pub fn unavailable_text(action: Action) -> String {
    format!(
        "{} is not available while indexing is in progress",
        action.text().trim_end_matches('…')
    )
}

/// The status-bar text for an action run on incomplete indexes.
pub fn partial_text(action: Action) -> String {
    format!(
        "{}: indexing is in progress, results may be incomplete",
        action.text().trim_end_matches('…')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    use rustpy_core::TextSize;

    fn none() -> Target {
        Target::None
    }

    #[test]
    fn smart_mode_runs_everything() {
        let mut dumb = DumbMode::default();
        for action in [
            Action::FindUsages,
            Action::GoToClass,
            Action::Rename,
            Action::Save,
        ] {
            assert_eq!(dumb.gate(action, false, || unreachable!()), Gate::Run);
        }
        assert_eq!(dumb.notice(), None);
    }

    #[test]
    fn index_dependent_actions_wait_or_run_partially_while_indexing() {
        let mut dumb = DumbMode::default();
        assert_eq!(dumb.gate(Action::Save, true, none), Gate::Run);
        assert_eq!(dumb.gate(Action::GoToClass, true, none), Gate::RunPartial);
        assert_eq!(dumb.gate(Action::GoToSymbol, true, none), Gate::RunPartial);
        for action in [
            Action::FindUsages,
            Action::GotoImplementation,
            Action::TypeHierarchy,
            Action::MethodHierarchy,
            Action::CallHierarchy,
            Action::Rename,
            Action::MoveRefactoring,
            Action::SafeDelete,
            Action::ChangeSignature,
        ] {
            assert_eq!(dumb.gate(action, true, none), Gate::Blocked, "{action:?}");
            assert_eq!(dumb.notice(), Some(action));
        }
        assert_eq!(
            unavailable_text(Action::FindUsages),
            "Find Usages is not available while indexing is in progress"
        );
        assert_eq!(
            partial_text(Action::GoToClass),
            "Go to Class: indexing is in progress, results may be incomplete"
        );
    }

    #[test]
    fn queued_actions_run_once_indexing_finishes_at_their_targets() {
        let mut dumb = DumbMode::default();
        let path = || Target::Path("/p/a.py".into());
        assert_eq!(dumb.gate(Action::FindUsages, true, path), Gate::Blocked);
        dumb.queue_notice();
        assert_eq!(dumb.notice(), None);
        assert_eq!(dumb.gate(Action::TypeHierarchy, true, none), Gate::Blocked);
        dumb.queue_notice();
        // Queued once per target, however often it is asked for.
        assert_eq!(dumb.gate(Action::FindUsages, true, path), Gate::Blocked);
        dumb.queue_notice();
        assert_eq!(
            dumb.queued().collect::<Vec<_>>(),
            [Action::FindUsages, Action::TypeHierarchy]
        );
        // Still indexing: nothing runs yet.
        assert!(dumb.take_ready(true).is_empty());
        // A refused action that was not queued is dropped with its notice.
        assert_eq!(dumb.gate(Action::Rename, true, none), Gate::Blocked);
        assert_eq!(
            dumb.take_ready(false),
            [
                Pending {
                    action: Action::FindUsages,
                    target: path()
                },
                Pending {
                    action: Action::TypeHierarchy,
                    target: Target::None
                }
            ]
        );
        assert_eq!(dumb.notice(), None);
        assert!(!dumb.has_queued());
        // Replayed in smart mode, they run.
        assert_eq!(dumb.gate(Action::FindUsages, false, none), Gate::Run);
    }

    #[test]
    fn dismissed_notice_queues_nothing() {
        let mut dumb = DumbMode::default();
        dumb.gate(Action::GotoImplementation, true, none);
        dumb.dismiss();
        dumb.queue_notice();
        assert!(!dumb.has_queued());
    }

    #[test]
    fn editor_targets_survive_edits_that_keep_them_in_place() {
        let document = rustpy_text::Document::new("x");
        let id = document.identity();
        let text = "def total(order):\n    return order.sum\n";
        let caret = Caret::at(TextSize::new(6)); // in `total`
        let target = EditorTarget::capture(id, 3, vec![caret], text);
        assert!(target.still_valid(id, 3, text));
        // Edited after the identifier: still `total` at the same offsets.
        assert!(target.still_valid(id, 4, "def total(order):\n    return order.total\n"));
        // Renamed or shifted: the target is gone.
        assert!(!target.still_valid(id, 4, "def summed(order):\n    return order.sum\n"));
        assert!(!target.still_valid(id, 4, "\ndef total(order):\n    return order.sum\n"));
        // Another incarnation of the document (reloaded, reopened).
        let other = rustpy_text::Document::new("x").identity();
        assert!(!target.still_valid(other, 3, text));
        // A selection must read the same; a caret in whitespace needs the same version.
        let selection = EditorTarget::capture(
            id,
            3,
            vec![Caret::new(TextSize::new(22), TextSize::new(28))],
            text,
        );
        assert!(selection.still_valid(id, 9, "def total(order):\n    return order.max\n"));
        let blank = EditorTarget::capture(id, 3, vec![Caret::at(TextSize::new(19))], text);
        assert!(!blank.still_valid(id, 4, text));
        assert!(blank.still_valid(id, 3, text));
    }
}

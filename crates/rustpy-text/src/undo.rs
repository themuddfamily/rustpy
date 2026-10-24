//! Retained branching undo history and explicit, nested edit transactions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::document::{ContentCheckpoint, Edit, Selection};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// An opaque history identity, valid only while its node remains in its document.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryId(u64);

/// An opaque transaction scope. Scopes must be committed or rolled back in LIFO order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TransactionToken(u64);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistoryError {
    #[error("history node no longer exists or belongs to another document")]
    UnknownHistory,
    #[error("finish the active transaction before navigating or pruning history")]
    TransactionActive,
    #[error("transaction token is no longer valid or belongs to another document")]
    InvalidTransaction,
    #[error("finish the innermost transaction first")]
    TransactionOrder,
    #[error("document is read-only")]
    ReadOnly,
}

/// A retained history state, in parent-before-child order. The root has no edit label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: HistoryId,
    pub parent: Option<HistoryId>,
    pub depth: usize,
    pub label: Option<String>,
    pub is_current: bool,
    /// The child that ordinary redo will visit.
    pub preferred_child: Option<HistoryId>,
}

/// One atomic batch of edits applied to a document, plus its inverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Forward edits in coordinates of the text *before* the step.
    pub edits: Vec<Edit>,
    /// Edits that revert the step, in coordinates of the text *after* the step.
    pub inverse: Vec<Edit>,
    /// Only explicit encoding conversions participate in format undo. Legacy
    /// format setters remain independent of text transactions and rollback.
    pub encoding: Option<(crate::Encoding, crate::Encoding)>,
    /// Full format metadata belongs only to explicit prepared-save units.
    pub format: Option<(crate::DocumentFormat, crate::DocumentFormat)>,
}

/// A user-visible undo unit. Several steps may be merged into one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub steps: Vec<Step>,
    pub before: Vec<Selection>,
    pub after: Vec<Selection>,
    pub mergeable: bool,
    pub label: Option<String>,
    pub content_before: u64,
    pub content_after: u64,
}

#[derive(Debug)]
struct Node {
    parent: Option<HistoryId>,
    children: Vec<HistoryId>,
    preferred_child: Option<HistoryId>,
    transaction: Option<Transaction>,
    refactoring: bool,
}

#[derive(Debug)]
struct Pending {
    token: TransactionToken,
    checkpoint: ContentCheckpoint,
    transaction: Transaction,
}

#[derive(Debug)]
pub(crate) struct UndoHistory {
    nodes: BTreeMap<HistoryId, Node>,
    leaves: BTreeSet<HistoryId>,
    root: HistoryId,
    current: HistoryId,
    root_content: u64,
    root_selections: Vec<Selection>,
    pending: Vec<Pending>,
    limit: usize,
}

impl UndoHistory {
    pub fn new(limit: usize) -> Self {
        let root = HistoryId(next_id());
        let node = Node {
            parent: None,
            children: Vec::new(),
            preferred_child: None,
            transaction: None,
            refactoring: false,
        };
        Self {
            nodes: BTreeMap::from([(root, node)]),
            leaves: BTreeSet::new(),
            root,
            current: root,
            root_content: 0,
            root_selections: Vec::new(),
            pending: Vec::new(),
            limit,
        }
    }

    pub(crate) fn quit_guard_state(&self) -> String {
        let nodes = self
            .nodes
            .iter()
            .map(|(id, node)| {
                (
                    id,
                    &node.parent,
                    &node.children,
                    &node.preferred_child,
                    node.refactoring,
                    node.transaction.as_ref().map(|transaction| {
                        (
                            &transaction.label,
                            transaction.mergeable,
                            transaction.content_before,
                            transaction.content_after,
                            transaction.steps.len(),
                            &transaction.before,
                            &transaction.after,
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();
        let pending = self
            .pending
            .iter()
            .map(|scope| {
                (
                    scope.token,
                    scope.checkpoint.quit_guard_state(),
                    &scope.transaction.label,
                    scope.transaction.mergeable,
                    scope.transaction.content_before,
                    scope.transaction.content_after,
                    scope.transaction.steps.len(),
                    &scope.transaction.before,
                    &scope.transaction.after,
                )
            })
            .collect::<Vec<_>>();
        format!(
            "{:?}",
            (
                self.root,
                self.current,
                self.root_content,
                &self.root_selections,
                self.limit,
                nodes,
                pending
            )
        )
    }
    pub fn current(&self) -> HistoryId {
        self.current
    }
    pub fn depth(&self) -> usize {
        self.pending.len()
    }
    pub fn can_undo(&self) -> bool {
        self.pending.is_empty() && self.current != self.root
    }
    pub fn can_redo(&self) -> bool {
        self.pending.is_empty() && self.nodes[&self.current].preferred_child.is_some()
    }
    pub fn undo_label(&self) -> Option<&str> {
        if !self.can_undo() {
            return None;
        }
        self.nodes[&self.current]
            .transaction
            .as_ref()?
            .label
            .as_deref()
    }
    pub fn undo_is_refactoring(&self) -> bool {
        self.can_undo() && self.nodes[&self.current].refactoring
    }
    pub fn mark_refactoring(&mut self) {
        if self.can_undo() {
            self.break_merge();
            self.nodes.get_mut(&self.current).unwrap().refactoring = true;
        }
    }
    pub fn redo_label(&self) -> Option<&str> {
        if !self.can_redo() {
            return None;
        }
        let child = self.nodes[&self.current].preferred_child?;
        self.nodes[&child].transaction.as_ref()?.label.as_deref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        step: Step,
        before: Vec<Selection>,
        after: Vec<Selection>,
        merge: bool,
        mergeable: bool,
        label: Option<String>,
        content_before: u64,
        content_after: u64,
    ) {
        if let Some(frame) = self.pending.last_mut() {
            let t = &mut frame.transaction;
            t.steps.push(step);
            t.after = after;
            t.content_after = content_after;
            if t.label.is_none() {
                t.label = label;
            }
            return;
        }
        self.record(
            Transaction {
                steps: vec![step],
                before,
                after,
                mergeable,
                label,
                content_before,
                content_after,
            },
            merge,
        );
    }

    pub fn push_batch(
        &mut self,
        steps: Vec<Step>,
        before: Vec<Selection>,
        after: Vec<Selection>,
        label: Option<String>,
        content_before: u64,
        content_after: u64,
    ) {
        if let Some(frame) = self.pending.last_mut() {
            let t = &mut frame.transaction;
            t.steps.extend(steps);
            t.after = after;
            t.content_after = content_after;
            if t.label.is_none() {
                t.label = label;
            }
        } else {
            self.record(
                Transaction {
                    steps,
                    before,
                    after,
                    mergeable: false,
                    label,
                    content_before,
                    content_after,
                },
                false,
            );
        }
    }

    fn record(&mut self, transaction: Transaction, merge: bool) {
        let node = self.nodes.get_mut(&self.current).unwrap();
        if merge && node.children.is_empty() {
            if let Some(last) = &mut node.transaction {
                if last.mergeable && last.content_after == transaction.content_before {
                    last.steps.extend(transaction.steps);
                    last.after = transaction.after;
                    last.mergeable = transaction.mergeable;
                    last.content_after = transaction.content_after;
                    return;
                }
            }
        }
        if self.current == self.root && node.children.is_empty() {
            self.root_selections = transaction.before.clone();
        }
        let id = HistoryId(next_id());
        node.children.push(id);
        node.preferred_child = Some(id);
        self.leaves.remove(&self.current);
        self.nodes.insert(
            id,
            Node {
                parent: Some(self.current),
                children: Vec::new(),
                preferred_child: None,
                transaction: Some(transaction),
                refactoring: false,
            },
        );
        self.current = id;
        self.leaves.insert(id);
        self.prune();
    }

    pub fn undo(&mut self) -> Option<Transaction> {
        if !self.can_undo() {
            return None;
        }
        self.break_merge();
        let node = &self.nodes[&self.current];
        let transaction = node.transaction.clone().unwrap();
        let parent = node.parent.unwrap();
        self.nodes.get_mut(&parent).unwrap().preferred_child = Some(self.current);
        self.current = parent;
        self.break_merge();
        Some(transaction)
    }

    pub fn redo(&mut self) -> Option<Transaction> {
        if !self.can_redo() {
            return None;
        }
        self.break_merge();
        self.current = self.nodes[&self.current].preferred_child.unwrap();
        self.break_merge();
        self.nodes[&self.current].transaction.clone()
    }

    pub fn entries(&self) -> Vec<HistoryEntry> {
        let mut result = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(self.root, 0)];
        while let Some((id, depth)) = stack.pop() {
            let node = &self.nodes[&id];
            result.push(HistoryEntry {
                id,
                parent: node.parent,
                depth,
                label: node.transaction.as_ref().and_then(|t| t.label.clone()),
                is_current: id == self.current,
                preferred_child: node.preferred_child,
            });
            stack.extend(node.children.iter().rev().map(|&child| (child, depth + 1)));
        }
        result
    }

    /// IDs of transactions to undo, then redo. Validates before any edit occurs.
    pub fn route(
        &self,
        target: HistoryId,
    ) -> Result<(Vec<HistoryId>, Vec<HistoryId>), HistoryError> {
        if !self.pending.is_empty() {
            return Err(HistoryError::TransactionActive);
        }
        if !self.nodes.contains_key(&target) {
            return Err(HistoryError::UnknownHistory);
        }
        let mut ancestors = BTreeSet::new();
        let mut cursor = Some(self.current);
        while let Some(id) = cursor {
            ancestors.insert(id);
            cursor = self.nodes[&id].parent;
        }
        let mut forward = Vec::new();
        let mut common = target;
        while !ancestors.contains(&common) {
            forward.push(common);
            common = self.nodes[&common].parent.unwrap();
        }
        let mut backward = Vec::new();
        let mut cursor = self.current;
        while cursor != common {
            backward.push(cursor);
            cursor = self.nodes[&cursor].parent.unwrap();
        }
        forward.reverse();
        Ok((backward, forward))
    }

    pub fn transaction(&self, id: HistoryId) -> Transaction {
        self.nodes[&id].transaction.clone().unwrap()
    }
    pub fn finish_selection(
        &mut self,
        target: HistoryId,
        backward: &[HistoryId],
        forward: &[HistoryId],
    ) -> (u64, Vec<Selection>) {
        self.break_merge();
        for &id in backward.iter().chain(forward) {
            let parent = self.nodes[&id].parent.unwrap();
            self.nodes.get_mut(&parent).unwrap().preferred_child = Some(id);
        }
        self.current = target;
        self.break_merge();
        match &self.nodes[&target].transaction {
            Some(t) => (t.content_after, t.after.clone()),
            None => (self.root_content, self.root_selections.clone()),
        }
    }

    pub fn begin(
        &mut self,
        before: Vec<Selection>,
        label: Option<String>,
        content: u64,
        checkpoint: ContentCheckpoint,
    ) -> TransactionToken {
        self.break_merge();
        let token = TransactionToken(next_id());
        self.pending.push(Pending {
            token,
            checkpoint,
            transaction: Transaction {
                steps: Vec::new(),
                after: before.clone(),
                before,
                mergeable: false,
                label,
                content_before: content,
                content_after: content,
            },
        });
        token
    }

    fn validate_token(&self, token: TransactionToken) -> Result<(), HistoryError> {
        if self.pending.last().is_some_and(|p| p.token == token) {
            return Ok(());
        }
        if self.pending.iter().any(|p| p.token == token) {
            return Err(HistoryError::TransactionOrder);
        }
        Err(HistoryError::InvalidTransaction)
    }

    pub fn commit(
        &mut self,
        token: TransactionToken,
        after: Vec<Selection>,
    ) -> Result<(), HistoryError> {
        self.validate_token(token)?;
        let mut t = self.pending.pop().unwrap().transaction;
        if t.steps.is_empty() {
            return Ok(());
        }
        t.after = after;
        if let Some(parent) = self.pending.last_mut() {
            parent.transaction.steps.extend(t.steps);
            parent.transaction.after = t.after;
            parent.transaction.content_after = t.content_after;
            if parent.transaction.label.is_none() {
                parent.transaction.label = t.label;
            }
        } else {
            self.record(t, false);
        }
        Ok(())
    }

    pub fn rollback(
        &mut self,
        token: TransactionToken,
    ) -> Result<(Transaction, ContentCheckpoint), HistoryError> {
        self.validate_token(token)?;
        let frame = self.pending.pop().unwrap();
        Ok((frame.transaction, frame.checkpoint))
    }

    pub fn restore_content_identity(&mut self, current: u64, restored: u64) {
        if self.root_content == current {
            self.root_content = restored;
        }
        let retained = self
            .nodes
            .values_mut()
            .filter_map(|n| n.transaction.as_mut());
        for t in retained.chain(self.pending.iter_mut().map(|p| &mut p.transaction)) {
            if t.content_before == current {
                t.content_before = restored;
            }
            if t.content_after == current {
                t.content_after = restored;
            }
        }
    }

    pub fn break_merge(&mut self) {
        if let Some(t) = &mut self.nodes.get_mut(&self.current).unwrap().transaction {
            t.mergeable = false;
        }
    }

    pub fn set_limit(&mut self, limit: usize) -> Result<(), HistoryError> {
        if !self.pending.is_empty() {
            return Err(HistoryError::TransactionActive);
        }
        self.limit = limit;
        self.prune();
        Ok(())
    }

    fn prune(&mut self) {
        while self.nodes.len() - 1 > self.limit {
            if let Some(id) = self.leaves.iter().copied().find(|&id| id != self.current) {
                self.leaves.remove(&id);
                let parent = self.nodes.remove(&id).unwrap().parent.unwrap();
                let node = self.nodes.get_mut(&parent).unwrap();
                node.children.retain(|&child| child != id);
                if node.preferred_child == Some(id) {
                    node.preferred_child = node.children.last().copied();
                }
                if node.children.is_empty() && parent != self.root {
                    self.leaves.insert(parent);
                }
            } else {
                // Only the current path remains: rebase onto its oldest retained state.
                let old = self.nodes.remove(&self.root).unwrap();
                let root = old.children[0];
                let node = self.nodes.get_mut(&root).unwrap();
                let t = node.transaction.take().unwrap();
                node.parent = None;
                self.root_content = t.content_after;
                self.root_selections = t.after;
                self.root = root;
                self.leaves.remove(&root);
            }
        }
    }
}

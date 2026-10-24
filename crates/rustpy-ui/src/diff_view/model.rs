use rustpy_settings::diff_merge::Ignore;
use similar::{ChangeTag, TextDiff};
use std::ops::Range;
#[derive(Clone, Debug)]
pub struct Row {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub changed: bool,
}
#[derive(Clone, Debug)]
pub struct Hunk {
    pub left: Range<usize>,
    pub right: Range<usize>,
    pub row: usize,
}
pub struct Model {
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub rows: Vec<Row>,
    pub hunks: Vec<Hunk>,
}
pub fn normalized(text: &str, ignore: Ignore) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let ending = if line.ends_with('\n') { "\n" } else { "" };
            let content = line.trim_end_matches(['\r', '\n']);
            let content = match ignore {
                Ignore::None => content.into(),
                Ignore::Leading => content.trim_start().to_owned(),
                Ignore::Trailing => content.trim_end().to_owned(),
                Ignore::All => content
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect(),
            };
            format!("{content}{ending}")
        })
        .collect()
}
impl Model {
    pub fn new(left: &str, right: &str, ignore: Ignore, align: bool) -> Self {
        let old = normalized(left, ignore);
        let new = normalized(right, ignore);
        let diff = TextDiff::from_lines(&old, &new);
        let mut rows = Vec::new();
        let mut hunks = Vec::new();
        for group in diff.grouped_ops(0) {
            let first = group.first().unwrap();
            let last = group.last().unwrap();
            hunks.push(Hunk {
                left: first.old_range().start..last.old_range().end,
                right: first.new_range().start..last.new_range().end,
                row: 0,
            });
        }
        for op in diff.ops() {
            let mut removed = Vec::new();
            let mut added = Vec::new();
            for change in diff.iter_changes(op) {
                match change.tag() {
                    ChangeTag::Equal => rows.push(Row {
                        left: change.old_index(),
                        right: change.new_index(),
                        changed: false,
                    }),
                    ChangeTag::Delete => removed.push(change.old_index().unwrap()),
                    ChangeTag::Insert => added.push(change.new_index().unwrap()),
                }
            }
            if align {
                for index in 0..removed.len().max(added.len()) {
                    rows.push(Row {
                        left: removed.get(index).copied(),
                        right: added.get(index).copied(),
                        changed: true,
                    });
                }
            } else {
                rows.extend(removed.into_iter().map(|index| Row {
                    left: Some(index),
                    right: None,
                    changed: true,
                }));
                rows.extend(added.into_iter().map(|index| Row {
                    left: None,
                    right: Some(index),
                    changed: true,
                }));
            }
        }
        for hunk in &mut hunks {
            hunk.row = rows
                .iter()
                .position(|row| {
                    row.changed
                        && (row.left.is_some_and(|index| hunk.left.contains(&index))
                            || row.right.is_some_and(|index| hunk.right.contains(&index)))
                })
                .unwrap_or(0);
        }
        Self {
            left: left.split_inclusive('\n').map(str::to_owned).collect(),
            right: right.split_inclusive('\n').map(str::to_owned).collect(),
            rows,
            hunks,
        }
    }
    pub fn apply(&self, right: &str, hunk: usize) -> Option<String> {
        let hunk = self.hunks.get(hunk)?;
        let mut lines: Vec<String> = right.split_inclusive('\n').map(str::to_owned).collect();
        if hunk.right.end > lines.len() {
            return None;
        }
        lines.splice(
            hunk.right.clone(),
            self.left[hunk.left.clone()].iter().cloned(),
        );
        Some(lines.concat())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignore_policies_and_partial_application() {
        let model = Model::new("a\nb\nc\n", "A\nb\nC\n", Ignore::None, true);
        assert_eq!(model.hunks.len(), 2);
        assert_eq!(model.apply("A\nb\nC\n", 0).as_deref(), Some("a\nb\nC\n"));
        assert!(Model::new(" x \n", "x\n", Ignore::All, true)
            .hunks
            .is_empty());
        assert!(!Model::new(" x \n", "x\n", Ignore::Trailing, true)
            .hunks
            .is_empty());
    }
    #[test]
    fn eof_and_unicode_are_preserved() {
        let model = Model::new("λ🙂\n", "λx", Ignore::None, true);
        assert_eq!(model.apply("λx", 0).as_deref(), Some("λ🙂\n"));
    }
}

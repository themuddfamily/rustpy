//! Stable cell boundaries for semantic analysis and source navigation; offsets are UTF-8 bytes.
use crate::{CellKind, Notebook};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    pub cell: usize,
    pub id: String,
    pub start: usize,
    pub end: usize,
    pub source: String,
}
#[derive(Clone, Debug)]
pub struct VirtualFile {
    pub text: String,
    pub boundaries: Vec<Boundary>,
}
impl VirtualFile {
    pub fn new(notebook: &Notebook) -> Self {
        let mut text = String::new();
        let mut boundaries = Vec::new();
        for (cell, source) in notebook
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.kind == CellKind::Code)
        {
            let id = source
                .id
                .clone()
                .unwrap_or_else(|| format!("legacy-{cell}"));
            let label = id
                .chars()
                .filter(|character| {
                    character.is_ascii_alphanumeric() || matches!(*character, '_' | '-')
                })
                .take(64)
                .collect::<String>();
            text.push_str(&format!("# %% cell {label}\n"));
            let start = text.len();
            // Replace magic/shell lines with equal-length comments so neighboring Python offsets remain exact.
            for line in source.source.split_inclusive('\n') {
                let trimmed = line.trim_start();
                if trimmed.starts_with('%') || trimmed.starts_with('!') {
                    text.push('#');
                    text.extend(std::iter::repeat_n(
                        ' ',
                        line.len()
                            .saturating_sub(1 + usize::from(line.ends_with('\n'))),
                    ));
                    if line.ends_with('\n') {
                        text.push('\n')
                    }
                } else {
                    text.push_str(line)
                }
            }
            boundaries.push(Boundary {
                cell,
                id,
                start,
                end: text.len(),
                source: source.source.clone(),
            });
            text.push_str("\n\n");
        }
        Self { text, boundaries }
    }
    pub fn offset(&self, cell: usize, offset: usize) -> Option<usize> {
        let boundary = self
            .boundaries
            .iter()
            .find(|boundary| boundary.cell == cell)?;
        Some(boundary.start + offset.min(boundary.end - boundary.start))
    }
    pub fn locate(&self, offset: usize) -> Option<(usize, usize)> {
        self.boundaries
            .iter()
            .find(|boundary| boundary.start <= offset && offset <= boundary.end)
            .map(|boundary| (boundary.cell, offset - boundary.start))
    }
    pub fn apply_edits(
        &self,
        notebook: &mut Notebook,
        edits: &[(std::ops::Range<usize>, String)],
    ) -> Result<(), String> {
        let mut grouped =
            std::collections::BTreeMap::<usize, Vec<(std::ops::Range<usize>, String)>>::new();
        for (range, replacement) in edits {
            let (cell, start) = self
                .locate(range.start)
                .ok_or("Edit begins outside a code cell")?;
            let (end_cell, end) = self
                .locate(range.end)
                .ok_or("Edit ends outside a code cell")?;
            if cell != end_cell {
                return Err("Edit crosses notebook cell boundary".into());
            }
            let snapshot = self
                .boundaries
                .iter()
                .find(|boundary| boundary.cell == cell)
                .ok_or("Missing cell boundary")?;
            if notebook.cells.get(cell).is_none_or(|cell| {
                cell.source != snapshot.source
                    || cell.id.as_ref().is_some_and(|id| id != &snapshot.id)
            }) {
                return Err("Notebook cell changed since semantic analysis".into());
            }
            grouped
                .entry(cell)
                .or_default()
                .push((start..end, replacement.clone()));
        }
        let mut updated = Vec::new();
        for (cell, mut edits) in grouped {
            let mut source = notebook.cells[cell].source.clone();
            edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
            let mut previous = source.len();
            for (range, replacement) in edits {
                if range.end > previous || source.get(range.clone()).is_none() {
                    return Err("Overlapping or stale notebook edit".into());
                }
                previous = range.start;
                source.replace_range(range, &replacement);
            }
            updated.push((cell, source));
        }
        for (cell, source) in updated {
            notebook.cells[cell].source = source;
        }
        Ok(())
    }
    pub fn headings(notebook: &Notebook) -> Vec<(usize, usize, String)> {
        notebook
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.kind == CellKind::Markdown)
            .flat_map(|(index, cell)| {
                cell.source.lines().filter_map(move |line| {
                    let depth = line.bytes().take_while(|byte| *byte == b'#').count();
                    (depth > 0 && depth <= 6 && line.as_bytes().get(depth) == Some(&b' '))
                        .then(|| (index, depth, line[depth..].trim().into()))
                })
            })
            .collect()
    }
}

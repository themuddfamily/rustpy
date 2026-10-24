//! Cached Code Vision headers integrated into the editor's actual visual rows.
//! No source edits and no independent approximation of fold/wrap geometry.
use super::EditorState;
use egui::{Color32, FontId, Pos2, Rect, Sense};
use rustpy_core::TextRange;
use rustpy_search::code_vision::UsageLens;
use rustpy_semantic::features::InlayHint;
use rustpy_text::DocumentId;
use rustpy_vcs::code_vision::AuthorLens;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub document: DocumentId,
    pub version: u64,
    pub path: PathBuf,
    pub source: Arc<str>,
    pub returns: Vec<InlayHint>,
    pub usages: Vec<UsageLens>,
    pub authors: Vec<AuthorLens>,
    pub repo_head: Option<String>,
}
impl Display {
    pub fn matches(&self, state: &EditorState) -> bool {
        self.document == state.doc.identity()
            && self.version == state.doc.version()
            && state.path.as_ref() == Some(&self.path)
    }
    /// Pass this merged list to BOTH glyph layout and soft-wrap row counting.
    /// It is a view input; the analysis-owned base hint list is never overwritten.
    pub fn merged_hints(&self, state: &EditorState) -> Vec<InlayHint> {
        let mut hints = state.inlay_hints.clone();
        if self.matches(state) {
            hints.extend(self.returns.iter().cloned());
        }
        hints.sort_by_key(|hint| hint.offset);
        hints
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindUsagesTarget {
    pub document: DocumentId,
    pub version: u64,
    pub path: PathBuf,
    pub range: TextRange,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct HeaderRow {
    pub row: usize,
    pub line: usize,
}
/// The reserved entries use u32::MAX as a sub-row sentinel. View must skip
/// them in its source-glyph loop and paint headers using `paint`. The line to
/// first-source-row map remains authoritative for all caret/source geometry.
#[derive(Default)]
pub struct HeaderRows {
    pub inline: bool,
    pub headers: Vec<HeaderRow>,
}
impl HeaderRows {
    pub fn is_header(&self, row: usize) -> bool {
        if self.inline {
            return false;
        }
        self.headers
            .binary_search_by_key(&row, |header| header.row)
            .is_ok()
    }
}
/// Decorate the ACTUAL build_rows output after folds/docs/soft-wrap are applied.
/// Hidden declarations receive no header. Repeated wrapped subrows receive one.
pub fn reserve_rows(
    state: &EditorState,
    display: Option<&Display>,
    rows: &mut Vec<usize>,
    subrows: &mut Vec<u32>,
    line_to_row: &mut [Option<usize>],
) -> HeaderRows {
    reserve_rows_with_extra(state, display, rows, subrows, line_to_row, &[])
}
pub fn reserve_rows_with_extra(
    state: &EditorState,
    display: Option<&Display>,
    rows: &mut Vec<usize>,
    subrows: &mut Vec<u32>,
    line_to_row: &mut [Option<usize>],
    extra: &[usize],
) -> HeaderRows {
    let display = display.filter(|display| display.matches(state));
    if display.is_none() && extra.is_empty() {
        return HeaderRows::default();
    }
    if state.preferences.code_vision_position == "hidden" {
        return HeaderRows::default();
    }
    let mut lines = BTreeMap::new();
    for line in extra {
        lines.insert(*line, ());
    }
    for usage in display.into_iter().flat_map(|display| &display.usages) {
        lines.insert(usage.line, ());
    }
    for author in display.into_iter().flat_map(|display| &display.authors) {
        lines.insert(author.lines.start, ());
    }
    if state.preferences.code_vision_position == "inline" {
        let headers = lines
            .keys()
            .filter_map(|line| {
                line_to_row
                    .get(*line)
                    .copied()
                    .flatten()
                    .map(|row| HeaderRow { row, line: *line })
            })
            .collect();
        return HeaderRows {
            headers,
            inline: true,
        };
    }
    let mut headers = Vec::new();
    let mut output_rows = Vec::with_capacity(rows.len() + lines.len());
    let mut output_subrows = Vec::with_capacity(subrows.len() + lines.len());
    for (&line, &subrow) in rows.iter().zip(subrows.iter()) {
        if subrow == 0 {
            if lines.contains_key(&line) {
                headers.push(HeaderRow {
                    row: output_rows.len(),
                    line,
                });
                output_rows.push(line);
                output_subrows.push(u32::MAX);
            }
            if let Some(slot) = line_to_row.get_mut(line) {
                *slot = Some(output_rows.len());
            }
        }
        output_rows.push(line);
        output_subrows.push(subrow);
    }
    *rows = output_rows;
    *subrows = output_subrows;
    HeaderRows {
        headers,
        inline: false,
    }
}
fn author_text(author: &AuthorLens) -> String {
    let mut names: Vec<_> = author
        .contributors
        .iter()
        .map(|contributor| contributor.name.clone())
        .collect();
    if author.uncommitted_lines > 0 {
        names.push("Uncommitted".into());
    }
    if names.is_empty() {
        "Author unavailable".into()
    } else {
        names.join(", ")
    }
}
fn author_tooltip(author: &AuthorLens, head: Option<&str>) -> String {
    let mut text = format!(
        "Git blame of the current editor buffer\nSource lines {}–{}",
        author.lines.start + 1,
        author.lines.end
    );
    if let Some(head) = head {
        text.push_str(&format!("\nHEAD {head}"));
    }
    for contributor in &author.contributors {
        text.push_str(&format!(
            "\n{}: {} committed line(s)",
            contributor.name, contributor.committed_lines
        ));
    }
    if author.uncommitted_lines > 0 {
        text.push_str(&format!(
            "\n{} uncommitted line(s); no author assigned",
            author.uncommitted_lines
        ));
    }
    text
}
/// Called inside the same ScrollArea content UI as the source. `origin` is
/// text_origin; `line_h` and the reserved row numbers are the real View values.
/// Returns an exact document/version/declaration target; never moves a caret.
#[expect(
    clippy::too_many_arguments,
    reason = "Use the editor's actual source, row map and paint geometry together"
)]
pub fn paint(
    ui: &mut egui::Ui,
    state: &EditorState,
    display: Option<&Display>,
    rows: &HeaderRows,
    origin: Pos2,
    line_h: f32,
    font: FontId,
    color: Color32,
    line_ends: &[(usize, Pos2)],
) -> Vec<FindUsagesTarget> {
    let Some(display) = display.filter(|display| display.matches(state)) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for header in &rows.headers {
        let inline = rows
            .inline
            .then(|| {
                line_ends
                    .iter()
                    .find(|(line, _)| *line == header.line)
                    .map(|(_, position)| *position)
            })
            .flatten();
        if rows.inline && inline.is_none() {
            continue;
        }
        let y = inline.map_or(origin.y + header.row as f32 * line_h, |position| position.y);
        if y + line_h < ui.clip_rect().top() || y > ui.clip_rect().bottom() {
            continue;
        }
        let mut x = inline.map_or(origin.x, |position| position.x);
        for usage in display
            .usages
            .iter()
            .filter(|usage| usage.line == header.line)
        {
            let label = format!(
                "{} usage{}",
                usage.usages,
                if usage.usages == 1 { "" } else { "s" }
            );
            let galley = ui.painter().layout_no_wrap(label, font.clone(), color);
            let rect = Rect::from_min_size(Pos2::new(x, y), galley.size());
            let response = ui.interact(
                rect,
                ui.id().with((
                    display.document,
                    display.version,
                    usage.range.start(),
                    "code-vision-usages",
                )),
                Sense::CLICK,
            );
            ui.painter().galley(rect.min, galley, color);
            let response = response.on_hover_text(format!(
                "Resolver-backed project usages of {}\nClick to find usages of this declaration",
                usage.name
            ));
            if response.clicked() {
                events.push(FindUsagesTarget {
                    document: display.document,
                    version: display.version,
                    path: display.path.clone(),
                    range: usage.range,
                    name: usage.name.clone(),
                });
            }
            x = rect.right() + 16.0;
        }
        for author in display
            .authors
            .iter()
            .filter(|author| author.lines.start == header.line)
        {
            let galley = ui
                .painter()
                .layout_no_wrap(author_text(author), font.clone(), color);
            let rect = Rect::from_min_size(Pos2::new(x, y), galley.size());
            ui.interact(
                rect,
                ui.id().with((
                    display.document,
                    display.version,
                    header.line,
                    "code-vision-author",
                )),
                Sense::hover(),
            )
            .on_hover_text(author_tooltip(author, display.repo_head.as_deref()));
            ui.painter().galley(rect.min, galley, color);
            x = rect.right() + 16.0;
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Language;
    use rustpy_core::TextSize;
    fn fixture() -> (EditorState, Display) {
        let mut state = EditorState::from_text(
            "def π():\n\treturn 1\n\ndef other():\n    return 2\n",
            Language::Python,
        );
        state.path = Some(PathBuf::from("/vision.py"));
        let source: Arc<str> = state.doc.text().into();
        let range = TextRange::new(TextSize::new(4), TextSize::new(6));
        let display = Display {
            document: state.doc.identity(),
            version: state.doc.version(),
            path: state.path.clone().unwrap(),
            source,
            returns: vec![InlayHint {
                offset: TextSize::new(8),
                label: " -> int".into(),
                kind: rustpy_semantic::features::InlayKind::Type,
            }],
            usages: vec![UsageLens {
                range,
                declaration: TextRange::new(TextSize::new(0), TextSize::new(22)),
                name: "π".into(),
                line: 0,
                usages: 2,
            }],
            authors: vec![],
            repo_head: None,
        };
        (state, display)
    }
    #[test]
    fn actual_fold_wrap_row_map_reserves_one_header_and_preserves_source_mapping() {
        let (state, mut display) = fixture();
        display.usages.push(UsageLens {
            line: 3,
            name: "other".into(),
            ..display.usages[0].clone()
        });
        // Actual View build_rows format: line0 wraps twice, folded body1–2 absent.
        let mut rows = vec![0, 0, 3, 4];
        let mut subs = vec![0, 1, 0, 0];
        let mut map = vec![Some(0), None, None, Some(2), Some(3)];
        let headers = reserve_rows(&state, Some(&display), &mut rows, &mut subs, &mut map);
        assert_eq!(rows, [0, 0, 0, 3, 3, 4]);
        assert_eq!(subs, [u32::MAX, 0, 1, u32::MAX, 0, 0]);
        assert_eq!(map, [Some(1), None, None, Some(4), Some(5)]);
        assert_eq!(
            headers
                .headers
                .iter()
                .map(|header| header.row)
                .collect::<Vec<_>>(),
            [0, 3]
        );
        let hints = display.merged_hints(&state);
        assert_eq!(hints.len(), 1);
        assert!(state.inlay_hints.is_empty());
    }
    #[test]
    fn real_egui_header_click_targets_unicode_declaration_without_source_or_caret_edit() {
        let (state, display) = fixture();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let before = state.doc.text();
        let before_carets = state.carets.clone();
        let rows = HeaderRows {
            inline: false,
            headers: vec![HeaderRow { row: 0, line: 0 }],
        };
        let mut events = Vec::new();
        for (time, pointer_events) in [
            (0.0, vec![]),
            (
                0.1,
                vec![
                    egui::Event::PointerMoved(Pos2::new(35.0, 32.0)),
                    egui::Event::PointerButton {
                        pos: Pos2::new(35.0, 32.0),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            ),
            (
                0.2,
                vec![egui::Event::PointerButton {
                    pos: Pos2::new(35.0, 32.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            ),
        ] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events: pointer_events,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0))),
                    ..Default::default()
                },
                |ui| {
                    events.extend(paint(
                        ui,
                        &state,
                        Some(&display),
                        &rows,
                        Pos2::new(20.0, 25.0),
                        20.0,
                        FontId::monospace(13.0),
                        Color32::LIGHT_BLUE,
                        &[],
                    ));
                },
            );
            output.drop_without_applying_deltas();
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].range, display.usages[0].range);
        assert_eq!(state.doc.slice(events[0].range), "π");
        assert_eq!(state.doc.text(), before);
        assert_eq!(state.carets, before_carets);
    }
    #[test]
    fn stale_version_hides_rows_hints_and_targets_and_author_provenance_is_real() {
        let (mut state, display) = fixture();
        state.insert_text("x", &crate::editor::EditOptions::default());
        assert!(!display.matches(&state));
        let mut rows = vec![0];
        let mut subs = vec![0];
        let mut map = vec![Some(0)];
        assert!(
            reserve_rows(&state, Some(&display), &mut rows, &mut subs, &mut map)
                .headers
                .is_empty()
        );
        assert!(display.merged_hints(&state).is_empty());
        let author = AuthorLens {
            lines: 0..2,
            contributors: vec![rustpy_vcs::code_vision::Contributor {
                name: "Actual Author".into(),
                committed_lines: 1,
            }],
            uncommitted_lines: 1,
        };
        let tooltip = author_tooltip(&author, Some("immutable HEAD"));
        assert!(tooltip.contains("Actual Author: 1 committed"));
        assert!(tooltip.contains("1 uncommitted"));
        assert!(tooltip.contains("immutable HEAD"));
    }
}

#[cfg(test)]
mod placement_fixture {
    use super::*;
    #[test]
    fn inline_and_hidden_modes_do_not_reserve_source_rows() {
        let mut state =
            EditorState::from_text("class Base:\n    pass\n", super::super::Language::Python);
        state.preferences.code_vision_position = "inline".into();
        let mut rows = vec![0, 1];
        let mut subrows = vec![0, 0];
        let mut map = vec![Some(0), Some(1)];
        let headers =
            reserve_rows_with_extra(&state, None, &mut rows, &mut subrows, &mut map, &[0]);
        assert!(headers.inline);
        assert_eq!(headers.headers.len(), 1);
        assert!(!headers.is_header(0));
        assert_eq!(rows, vec![0, 1]);
        state.preferences.code_vision_position = "hidden".into();
        assert!(
            reserve_rows_with_extra(&state, None, &mut rows, &mut subrows, &mut map, &[0])
                .headers
                .is_empty()
        );
        state.preferences.code_vision_position = "top".into();
        assert!(
            !reserve_rows_with_extra(&state, None, &mut rows, &mut subrows, &mut map, &[0])
                .headers
                .is_empty()
        );
        assert_eq!(rows.len(), 3);
    }
}

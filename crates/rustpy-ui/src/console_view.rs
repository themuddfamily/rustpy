//! Console output view of the Run tool window (roadmap §3.2 "Console view"):
//! virtualized rows over a [`ConsoleBuffer`] with soft wraps, mouse selection
//! and copy, find (Ctrl+F) and clickable `File "x.py", line N` links.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Color32, FontId, Galley, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_run::ansi::SgrState;
use rustpy_run::console_buffer::{ConsoleBuffer, ConsoleLine};
use rustpy_run::process::file_links;
use rustpy_run::OutputKind;

use crate::a11y::Accessible;
use crate::text_selection::{unit_span, SelectUnit, TextPos, TextSelection};
use crate::theme::UiTheme;

const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x00, 0x00, 0x00),
    Color32::from_rgb(0xFF, 0x60, 0x60),
    Color32::from_rgb(0x4F, 0xC4, 0x14),
    Color32::from_rgb(0xC8, 0xC0, 0x14),
    Color32::from_rgb(0x35, 0x93, 0xFF),
    Color32::from_rgb(0xC6, 0x4F, 0xC6),
    Color32::from_rgb(0x00, 0xB8, 0xC6),
    Color32::from_rgb(0xBB, 0xBB, 0xBB),
    Color32::from_rgb(0x55, 0x55, 0x55),
    Color32::from_rgb(0xFF, 0x80, 0x80),
    Color32::from_rgb(0x70, 0xE0, 0x50),
    Color32::from_rgb(0xF0, 0xE0, 0x60),
    Color32::from_rgb(0x60, 0xB0, 0xFF),
    Color32::from_rgb(0xE0, 0x70, 0xE0),
    Color32::from_rgb(0x50, 0xD0, 0xE0),
    Color32::from_rgb(0xFF, 0xFF, 0xFF),
];

/// Color of ANSI palette entry `idx` (16 colors, 6×6×6 cube, gray ramp).
pub fn ansi_color(idx: u8, dark: bool) -> Color32 {
    if idx < 16 {
        if !dark && idx == 7 {
            Color32::from_gray(80)
        } else {
            ANSI[idx as usize]
        }
    } else if idx < 232 {
        let i = idx - 16;
        let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
        let f = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        Color32::from_rgb(f(r), f(g), f(b))
    } else {
        Color32::from_gray(8 + (idx - 232) * 10)
    }
}

/// Without soft wraps, at most this many characters of a line are drawn.
const MAX_ROW_CHARS: usize = 20_000;

/// A screen row of the console: characters `start..end` of line `line`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Splits a line of `len` characters into rows of at most `cols` characters
/// (an empty line is one empty row).
pub fn wrap_spans(len: usize, cols: usize) -> impl Iterator<Item = (usize, usize)> {
    let cols = cols.max(1);
    (0..len.div_ceil(cols).max(1)).map(move |i| (i * cols, ((i + 1) * cols).min(len)))
}

/// The visual rows of a console, kept in step with its buffer incrementally:
/// only lines added since the last sync, plus the previously newest line
/// (the only one that can change), are wrapped again.
#[derive(Debug, Default)]
pub struct RowIndex {
    /// Columns per row when soft-wrapping; `None`: one row per line.
    wrap: Option<usize>,
    hidden: std::collections::BTreeSet<usize>,
    rows: VecDeque<VisualRow>,
    synced_end: usize,
    /// Oldest line that could still change at the last sync.
    synced_open: Option<usize>,
    synced_revision: Option<u64>,
    /// Longest line seen, in characters (horizontal extent without wraps).
    max_chars: usize,
}

impl RowIndex {
    pub fn set_hidden(&mut self, hidden: std::collections::BTreeSet<usize>) {
        if self.hidden != hidden {
            self.hidden = hidden;
            self.rows.clear();
            self.synced_end = 0;
            self.synced_open = None;
            self.synced_revision = None;
        }
    }
    pub fn sync(&mut self, buf: &ConsoleBuffer, wrap: Option<usize>) {
        if wrap != self.wrap || self.synced_end > buf.end_line() {
            *self = RowIndex {
                hidden: std::mem::take(&mut self.hidden),
                wrap,
                ..Default::default()
            };
        }
        if self.synced_revision == Some(buf.revision()) {
            return;
        }
        while self.rows.front().is_some_and(|r| r.line < buf.first_line()) {
            self.rows.pop_front();
        }
        let from = self
            .synced_end
            .saturating_sub(1)
            .min(self.synced_open.unwrap_or(usize::MAX))
            .max(buf.first_line());
        while self.rows.back().is_some_and(|r| r.line >= from) {
            self.rows.pop_back();
        }
        for (n, line) in buf.lines_from(from) {
            if self.hidden.contains(&n) {
                continue;
            }
            let len = line.char_len();
            self.max_chars = self.max_chars.max(len.min(MAX_ROW_CHARS));
            if wrap.is_some() || !self.hidden.is_empty() {
                self.rows
                    .extend(
                        wrap_spans(len, wrap.unwrap_or(MAX_ROW_CHARS)).map(|(start, end)| {
                            VisualRow {
                                line: n,
                                start,
                                end,
                            }
                        }),
                    );
            }
        }
        self.synced_end = buf.end_line();
        self.synced_open = buf.oldest_open_line();
        self.synced_revision = Some(buf.revision());
    }

    pub fn len(&self, buf: &ConsoleBuffer) -> usize {
        if self.wrap.is_some() || !self.hidden.is_empty() {
            self.rows.len()
        } else {
            buf.len()
        }
    }

    pub fn row(&self, buf: &ConsoleBuffer, i: usize) -> Option<VisualRow> {
        match self.wrap {
            _ if self.wrap.is_some() || !self.hidden.is_empty() => self.rows.get(i).copied(),
            _ => {
                let line = buf.first_line() + i;
                buf.line(line).map(|l| VisualRow {
                    line,
                    start: 0,
                    end: l.char_len().min(MAX_ROW_CHARS),
                })
            }
        }
    }

    /// Index of the row that shows character `col` of `line`.
    pub fn row_of(&self, buf: &ConsoleBuffer, line: usize, col: usize) -> Option<usize> {
        match self.wrap {
            _ if self.wrap.is_some() || !self.hidden.is_empty() => {
                let i = self
                    .rows
                    .partition_point(|r| (r.line, r.start) <= (line, col));
                (i > 0 && self.rows[i - 1].line == line).then(|| i - 1)
            }
            _ => line
                .checked_sub(buf.first_line())
                .filter(|&i| i < buf.len()),
        }
    }
}

/// A search hit: characters `start..end` of line `line`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Non-overlapping occurrences of `needle` in `text`, as character ranges.
pub fn find_in_line(text: &str, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    // One-to-one case folding keeps character indices aligned with `text`.
    let fold = |c: char| {
        if case_sensitive {
            c
        } else {
            c.to_lowercase().next().unwrap_or(c)
        }
    };
    let needle: Vec<char> = needle.chars().map(fold).collect();
    let hay: Vec<char> = text.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Find-in-console: the query, its options and the hits, which are brought
/// up to date incrementally as output arrives.
#[derive(Debug, Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub case_sensitive: bool,
    matches: VecDeque<Match>,
    /// Index of the current hit in `matches`.
    current: Option<usize>,
    /// Query, case option and buffer revision the hits were computed for.
    synced: Option<(String, bool, u64)>,
    synced_end: usize,
    /// Oldest line that could still change at the last sync.
    synced_open: Option<usize>,
    focus_query: bool,
}

impl FindState {
    /// Updates the hits for the buffer's current text; returns true when the
    /// query or its options changed since the last call.
    pub fn sync(&mut self, buf: &ConsoleBuffer) -> bool {
        let query_changed = self
            .synced
            .as_ref()
            .is_none_or(|(q, c, _)| *q != self.query || *c != self.case_sensitive);
        if !query_changed && self.synced.as_ref().is_some_and(|s| s.2 == buf.revision()) {
            return false;
        }
        let current = self.current();
        let from = if query_changed || self.synced_end > buf.end_line() {
            self.matches.clear();
            buf.first_line()
        } else {
            while self
                .matches
                .front()
                .is_some_and(|m| m.line < buf.first_line())
            {
                self.matches.pop_front();
            }
            let from = self
                .synced_end
                .saturating_sub(1)
                .min(self.synced_open.unwrap_or(usize::MAX))
                .max(buf.first_line());
            while self.matches.back().is_some_and(|m| m.line >= from) {
                self.matches.pop_back();
            }
            from
        };
        if !self.query.is_empty() {
            for (n, line) in buf.lines_from(from) {
                self.matches.extend(
                    find_in_line(&line.text, &self.query, self.case_sensitive)
                        .into_iter()
                        .map(|(start, end)| Match {
                            line: n,
                            start,
                            end,
                        }),
                );
            }
        }
        self.synced = Some((self.query.clone(), self.case_sensitive, buf.revision()));
        self.synced_end = buf.end_line();
        self.synced_open = buf.oldest_open_line();
        // Keep the current hit on the same text while it exists.
        self.current = current.and_then(|m| {
            self.matches
                .binary_search_by(|x| (x.line, x.start).cmp(&(m.line, m.start)))
                .ok()
        });
        query_changed
    }

    pub fn count(&self) -> usize {
        self.matches.len()
    }

    pub fn current(&self) -> Option<Match> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Index of the current hit (0-based) for the "n/m" counter.
    pub fn current_index(&self) -> Option<usize> {
        self.current.filter(|&i| i < self.matches.len())
    }

    /// Makes the first hit on or after `line` current (else the last hit).
    pub fn select_from(&mut self, line: usize) {
        self.current = if self.matches.is_empty() {
            None
        } else {
            Some(
                self.matches
                    .partition_point(|m| m.line < line)
                    .min(self.matches.len() - 1),
            )
        };
    }

    /// Moves to the next or previous hit, wrapping around.
    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        self.current = match (n, self.current) {
            (0, _) => None,
            (_, None) => Some(if forward { 0 } else { n - 1 }),
            (_, Some(i)) => Some(if forward {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            }),
        };
    }

    /// Hits on `line`, each with whether it is the current one.
    pub fn on_line(&self, line: usize) -> impl Iterator<Item = (Match, bool)> + '_ {
        let i = self.matches.partition_point(|m| m.line < line);
        let current = self.current;
        self.matches
            .range(i..)
            .take_while(move |m| m.line == line)
            .enumerate()
            .map(move |(k, m)| (*m, current == Some(i + k)))
    }
}

/// Text between two positions (`end` exclusive), lines joined with `\n`.
pub fn text_between(buf: &ConsoleBuffer, start: TextPos, end: TextPos) -> String {
    let mut out = String::new();
    for (n, line) in buf.lines_from(start.line) {
        if n > end.line {
            break;
        }
        let len = line.char_len();
        let a = if n == start.line {
            start.col.min(len)
        } else {
            0
        };
        let b = if n == end.line { end.col.min(len) } else { len };
        out.extend(line.text.chars().skip(a).take(b.saturating_sub(a)));
        if n < end.line {
            out.push('\n');
        }
    }
    out
}

/// Colors and font for laying out console rows.
pub struct RowStyle {
    pub ansi_palette: Vec<[u8; 3]>,
    pub font: FontId,
    pub dark: bool,
    pub high_contrast: bool,
    pub text: Color32,
    pub stderr: Color32,
    pub system: Color32,
    pub link: Color32,
    pub selection: Color32,
    pub hit: Color32,
    pub current_hit: Color32,
}

impl RowStyle {
    fn ansi_color(&self, index: u8) -> Color32 {
        self.ansi_palette
            .get(index as usize)
            .map(|rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
            .unwrap_or_else(|| ansi_color(index, self.dark))
    }
    pub fn new(theme: &UiTheme, font: FontId) -> Self {
        let (hit, current_hit, selection) = if theme.high_contrast {
            (theme.selection_bg, theme.selection_bg, theme.selection_bg)
        } else if theme.dark {
            (
                Color32::from_rgb(0x3D, 0x5A, 0x3E),
                Color32::from_rgb(0x8C, 0x6A, 0x14),
                theme.selection_bg,
            )
        } else {
            (
                Color32::from_rgb(0xF5, 0xE8, 0xA0),
                Color32::from_rgb(0xFF, 0xB8, 0x4D),
                Color32::from_rgb(0xA6, 0xD2, 0xFF),
            )
        };
        Self {
            ansi_palette: Vec::new(),
            font,
            dark: theme.dark,
            high_contrast: theme.high_contrast,
            text: theme.scheme_color("console.text", theme.text),
            stderr: theme.scheme_color("console.stderr", theme.error),
            system: theme.scheme_color("console.system", theme.text_dim),
            link: theme.scheme_color("console.link", theme.accent),
            selection: theme.scheme_color("console.selection", selection),
            hit: theme.scheme_color("console.search", hit),
            current_hit: theme.scheme_color("console.search.current", current_hit),
        }
    }
}

/// Layout of the characters of `row` with styles, links (`links`: character
/// ranges), search hits (`hits`: ranges and whether current) and the selected
/// columns.
pub fn row_job(
    line: &ConsoleLine,
    row: VisualRow,
    links: &[(usize, usize)],
    hits: &[(usize, usize, bool)],
    selected: Option<(usize, usize)>,
    st: &RowStyle,
) -> LayoutJob {
    let mut cuts = vec![row.start, row.end];
    cuts.extend(line.runs.iter().map(|r| r.start));
    cuts.extend(links.iter().flat_map(|&(a, b)| [a, b]));
    cuts.extend(hits.iter().flat_map(|&(a, b, _)| [a, b]));
    cuts.extend(selected.iter().flat_map(|&(a, b)| [a, b]));
    cuts.retain(|&c| c >= row.start && c <= row.end);
    cuts.sort_unstable();
    cuts.dedup();
    let ascii = line.text.is_ascii();
    let byte = |c: usize| {
        if ascii {
            c.min(line.text.len())
        } else {
            line.text
                .char_indices()
                .nth(c)
                .map_or(line.text.len(), |(b, _)| b)
        }
    };
    let inside = |c: usize, (a, b): (usize, usize)| c >= a && c < b;
    let mut job = LayoutJob::default();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (style, kind) = line
            .runs
            .iter()
            .rev()
            .find(|r| r.start <= a)
            .map_or((SgrState::default(), line.kind), |r| (r.style, r.kind));
        let base = match kind {
            OutputKind::Stdout => st.text,
            OutputKind::Stderr => st.stderr,
            OutputKind::System => st.system,
        };
        let mut color = if st.high_contrast {
            base
        } else {
            style
                .fg_rgb
                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                .unwrap_or_else(|| {
                    style.fg.map_or(base, |c| {
                        st.ansi_color(if style.bold && c < 8 { c + 8 } else { c })
                    })
                })
        };
        let link = links.iter().any(|&r| inside(a, r));
        if link {
            color = st.link;
        }
        let hit = hits
            .iter()
            .find(|&&(s, e, _)| inside(a, (s, e)))
            .map(|&(_, _, current)| current);
        let background = if selected.is_some_and(|r| inside(a, r)) {
            st.selection
        } else if let Some(current) = hit {
            if current {
                st.current_hit
            } else {
                st.hit
            }
        } else if st.high_contrast {
            Color32::TRANSPARENT
        } else {
            style
                .bg_rgb
                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                .unwrap_or_else(|| style.bg.map_or(Color32::TRANSPARENT, |c| st.ansi_color(c)))
        };
        let underline = if link || style.underline {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        };
        let font_id = FontId::new(
            st.font.size,
            if matches!(&st.font.family,egui::FontFamily::Name(name)if name.starts_with("console:"))
            {
                crate::fonts::console_family(style.bold, style.italic)
            } else {
                crate::theme::mono_family(style.bold, style.italic)
            },
        );
        job.append(
            &line.text[byte(a)..byte(b)],
            0.0,
            TextFormat {
                font_id,
                color,
                background,
                underline,
                ..Default::default()
            },
        );
    }
    job
}

/// What the user asked the console for.
pub enum ConsoleAction {
    OpenFile(PathBuf, usize),
    /// "Clear All" from the context menu.
    ClearAll,
    OpenUrl(String),
    SelectInProject(PathBuf),
}

/// A row on screen, for hit testing.
struct ShownRow {
    row: VisualRow,
    rect: Rect,
    origin: Pos2,
    galley: Arc<Galley>,
}

/// The caret position nearest to `p` and the character under it.
fn hit_test(shown: &[ShownRow], p: Pos2) -> Option<(TextPos, TextPos)> {
    let (first, last) = (shown.first()?, shown.last()?);
    let r = if p.y < first.rect.top() {
        first
    } else {
        shown.iter().find(|r| p.y < r.rect.bottom()).unwrap_or(last)
    };
    let rel = p - r.origin;
    let caret = r
        .galley
        .cursor_from_pos(rel)
        .index
        .0
        .min(r.row.end - r.row.start);
    let caret_x = r.galley.pos_from_cursor(CCursor::new(caret)).min.x;
    let cell = if rel.x < caret_x && caret > 0 {
        caret - 1
    } else {
        caret
    };
    Some((
        TextPos::new(r.row.line, r.row.start + caret),
        TextPos::new(r.row.line, r.row.start + cell),
    ))
}

/// File links on a line as `(character range, path, line number)`.
fn links_of(line: &ConsoleLine) -> Vec<((usize, usize), PathBuf, usize)> {
    let to_char = |b: usize| {
        if line.text.is_ascii() {
            b
        } else {
            line.text[..b].chars().count()
        }
    };
    file_links(&line.text)
        .into_iter()
        .map(|(r, path, n)| ((to_char(r.start), to_char(r.end)), path, n))
        .collect()
}

#[derive(Clone)]
enum LinkTarget {
    File(PathBuf, usize),
    Url(String),
}
fn all_links(line: &ConsoleLine, filters: &[regex::Regex]) -> Vec<((usize, usize), LinkTarget)> {
    let mut output = links_of(line)
        .into_iter()
        .map(|(range, path, line)| (range, LinkTarget::File(path, line)))
        .collect::<Vec<_>>();
    let to_char = |byte: usize| line.text[..byte].chars().count();
    static URL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    for found in URL
        .get_or_init(|| regex::Regex::new(r#"https?://[^\s<>"']+"#).unwrap())
        .find_iter(&line.text)
    {
        let value = found.as_str().trim_end_matches(['.', ',', ')', ']', ';']);
        output.push((
            (to_char(found.start()), to_char(found.start() + value.len())),
            LinkTarget::Url(value.into()),
        ));
    }
    for filter in filters {
        for captures in filter.captures_iter(&line.text) {
            let Some(path) = captures.name("path").or_else(|| captures.get(1)) else {
                continue;
            };
            let number = captures
                .name("line")
                .or_else(|| captures.get(2))
                .and_then(|n| n.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            output.push((
                (to_char(path.start()), to_char(path.end())),
                LinkTarget::File(PathBuf::from(path.as_str()), number),
            ));
        }
    }
    output
}

/// The console of one run session at a time (the Run window's active tab).
#[derive(Default)]
pub struct ConsoleView {
    /// Session the state below belongs to.
    session: Option<u64>,
    rows: RowIndex,
    selection: Option<TextSelection>,
    pub find: FindState,
    /// Find hit to scroll into view on this frame.
    reveal: Option<Match>,
    /// Whether the console area itself has keyboard focus.
    area_focused: bool,
    /// Whether the console area or its find field has keyboard focus.
    pub focused: bool,
    announcement_blocked: bool,
    options: rustpy_settings::run_console::Settings,
    link_filters: Vec<regex::Regex>,
    folded_revision: Option<u64>,
    folded_active: Option<bool>,
    expanded_folds: bool,
}

impl ConsoleView {
    pub fn terminal_input_focus(&self) -> bool {
        self.area_focused
            && !self.find.open
            && !self
                .selection
                .is_some_and(|selection| !selection.is_empty())
    }
    pub fn configure(&mut self, options: &rustpy_settings::run_console::Settings) {
        if &self.options != options {
            if self.options.fold_output != options.fold_output
                || self.options.fold_contains != options.fold_contains
            {
                self.expanded_folds = false;
            }
            self.options = options.clone();
            self.link_filters = options
                .link_filters
                .iter()
                .take(32)
                .filter_map(|pattern| {
                    regex::RegexBuilder::new(pattern)
                        .size_limit(64 * 1024)
                        .build()
                        .ok()
                })
                .collect();
            self.folded_revision = None;
        }
    }
    pub(crate) fn claims_context_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
        active: Option<u64>,
    ) -> bool {
        self.session == active
            && key == egui::Key::F10
            && mods == egui::Modifiers::SHIFT
            && !egui::Popup::is_any_open(ctx)
            && self.session.is_some_and(|session| {
                ctx.memory(|memory| memory.has_focus(egui::Id::new(("run_console_area", session))))
            })
    }

    pub(crate) fn set_announcement_owner(&mut self, available: bool) {
        self.announcement_blocked = !available;
    }

    /// Opens the find bar (Ctrl+F), seeded with a one-line selection.
    pub fn open_find(&mut self, buf: &ConsoleBuffer) {
        if let Some(s) = self.selection.filter(|s| !s.is_empty()) {
            let (a, b) = s.range();
            if a.line == b.line {
                self.find.query = text_between(buf, a, b);
            }
        }
        self.find.open = true;
        self.find.focus_query = true;
    }

    fn selected_text(&self, buf: &ConsoleBuffer) -> Option<String> {
        let s = self.selection.filter(|s| !s.is_empty())?;
        let (a, b) = s.range();
        Some(text_between(buf, a, b))
    }

    fn select_all(&mut self, buf: &ConsoleBuffer) {
        self.selection = (!buf.is_empty()).then(|| {
            TextSelection::new(
                SelectUnit::Char,
                (
                    TextPos::new(buf.first_line(), 0),
                    TextPos::new(buf.end_line() - 1, usize::MAX),
                ),
            )
        });
    }

    /// Shows the console of session `session`, leaving `reserve_below` points
    /// of height for what follows. `keys` says whether the Run window has
    /// keyboard focus (Ctrl+F, F3 and Shift+F3 then go to the console).
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: FontId,
        session: u64,
        buf: &ConsoleBuffer,
        soft_wrap: bool,
        stick_to_end: bool,
        reserve_below: f32,
        keys: bool,
    ) -> Vec<ConsoleAction> {
        let mut mono = mono;
        mono.size = self.options.font_size.clamp(8.0, 40.0);
        if !self.options.font_family.is_empty() {
            mono.family = crate::fonts::console_family(false, false);
        }
        if let Some(background) = theme.scheme_override("console.background") {
            ui.painter().rect_filled(ui.max_rect(), 0.0, background);
        }
        if self.options.custom_colors {
            ui.painter().rect_filled(
                ui.max_rect(),
                0.0,
                Color32::from_rgb(
                    self.options.background[0],
                    self.options.background[1],
                    self.options.background[2],
                ),
            );
        }
        let mut actions = Vec::new();
        if self.session != Some(session) {
            let find = std::mem::take(&mut self.find);
            let announcement_blocked = self.announcement_blocked;
            let options = self.options.clone();
            let filters = std::mem::take(&mut self.link_filters);
            *self = ConsoleView {
                options,
                link_filters: filters,
                announcement_blocked,
                session: Some(session),
                find: FindState {
                    open: find.open,
                    query: find.query,
                    case_sensitive: find.case_sensitive,
                    ..Default::default()
                },
                ..Default::default()
            };
        }
        // Lines cleared or dropped by the cycle buffer take the selection with them.
        if self
            .selection
            .is_some_and(|s| s.range().1.line < buf.first_line())
        {
            self.selection = None;
        }
        let area_id = egui::Id::new(("run_console_area", session));
        let query_id = egui::Id::new("run_console_find");
        // Keyboard shortcuts (`consume_key` ignores extra Shift/Alt, so Ctrl+Shift+F
        // and Ctrl+Shift+A stay with their own actions).
        let plain_command = |i: &egui::InputState| !i.modifiers.shift && !i.modifiers.alt;
        if keys
            && ui.input_mut(|i| {
                plain_command(i) && i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)
            })
        {
            self.open_find(buf);
        }
        let step = if keys && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3))
        {
            Some(false)
        } else if keys && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F3)) {
            Some(true)
        } else {
            None
        };
        if self.area_focused {
            if ui.input_mut(|i| {
                plain_command(i) && i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
            }) {
                self.select_all(buf);
            }
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
                if let Some(text) = self.selected_text(buf) {
                    ui.ctx().copy_text(text);
                }
            }
            if !egui::Popup::is_any_open(ui.ctx())
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.selection = None;
                self.find.open = false;
            }
        }
        // Find bar.
        let mut query_focused = false;
        let mut step = step;
        if self.find.open {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Find:").color(theme.text_dim));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.find.query)
                        .id(query_id)
                        .desired_width(260.0)
                        .hint_text("Search console output"),
                );
                if std::mem::take(&mut self.find.focus_query) {
                    resp.request_focus();
                }
                if resp.lost_focus() {
                    let (enter, shift, escape) = ui.input(|i| {
                        (
                            i.key_pressed(egui::Key::Enter),
                            i.modifiers.shift,
                            i.key_pressed(egui::Key::Escape),
                        )
                    });
                    if enter {
                        step = Some(!shift);
                        resp.request_focus();
                    } else if escape {
                        self.find.open = false;
                        ui.memory_mut(|m| m.request_focus(area_id));
                    }
                }
                query_focused = resp.has_focus();
                ui.toggle_value(&mut self.find.case_sensitive, "Aa")
                    .on_hover_text("Match case");
                if ui
                    .button("⬆")
                    .icon_hint("Previous occurrence (Shift+Enter, Shift+F3)")
                    .clicked()
                {
                    step = Some(false);
                }
                if ui
                    .button("⬇")
                    .icon_hint("Next occurrence (Enter, F3)")
                    .clicked()
                {
                    step = Some(true);
                }
                let count = self.find.count();
                if !self.find.query.is_empty() {
                    let (text, color) = match self.find.current_index() {
                        _ if count == 0 => ("0 results".to_string(), theme.error),
                        Some(i) => (format!("{}/{count}", i + 1), theme.text_dim),
                        None => (format!("{count} results"), theme.text_dim),
                    };
                    ui.label(egui::RichText::new(text).color(color));
                }
                if ui
                    .button("×")
                    .icon_hint("Close console search (Escape)")
                    .clicked()
                {
                    self.find.open = false;
                }
            });
        }
        // Rows and search hits for the current text.
        let char_w = ui.fonts_mut(|f| f.glyph_width(&mono, 'M')).max(1.0);
        let row_h = ui.fonts_mut(|f| f.row_height(&mono)) + 2.0;
        let wrap =
            soft_wrap.then(|| (((ui.available_width() - 24.0) / char_w).floor() as usize).max(10));
        let fold = self.options.fold_output && !self.expanded_folds && !self.find.open;
        if self.folded_revision != Some(buf.revision())
            || self.find.open
            || !fold
            || self.folded_active != Some(fold)
        {
            let hidden = if fold {
                buf.lines_from(buf.first_line())
                    .filter(|(_, line)| {
                        self.options
                            .fold_contains
                            .iter()
                            .any(|pattern| !pattern.is_empty() && line.text.contains(pattern))
                    })
                    .map(|(line, _)| line)
                    .collect()
            } else {
                Default::default()
            };
            self.rows.set_hidden(hidden);
            self.folded_revision = Some(buf.revision());
            self.folded_active = Some(fold);
        }
        if !self.rows.hidden.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} lines folded", self.rows.hidden.len()));
                if ui.small_button("Expand all").clicked() {
                    self.expanded_folds = true;
                    self.rows.set_hidden(Default::default());
                }
            });
        }
        self.rows.sync(buf, wrap);
        let mut first_visible = buf.first_line();
        if self.find.open {
            if self.find.sync(buf) {
                // A new query: start from what is on screen.
                let top_row = ui
                    .data(|d| d.get_temp::<usize>(area_id.with("top_row")))
                    .unwrap_or(0);
                first_visible = self
                    .rows
                    .row(buf, top_row)
                    .map_or(first_visible, |r| r.line);
                self.find.select_from(first_visible);
                self.reveal = self.find.current();
            }
            if let Some(forward) = step {
                self.find.step(forward);
                self.reveal = self.find.current();
            }
        }
        let reveal_row = self
            .reveal
            .take()
            .and_then(|m| self.rows.row_of(buf, m.line, m.start).map(|r| (r, m)));
        // The console.
        let height = (ui.available_height() - reserve_below).max(40.0);
        let n_rows = self.rows.len(buf);
        let content_w = if soft_wrap {
            0.0
        } else {
            self.rows.max_chars as f32 * char_w + 16.0
        };
        let dragging = ui.ctx().is_being_dragged(area_id);
        let mut area = egui::ScrollArea::new([!soft_wrap, true])
            .id_salt(("run_console", session))
            .auto_shrink([false, false])
            .max_height(height)
            .stick_to_bottom(stick_to_end && reveal_row.is_none() && !dragging);
        if let Some((row, m)) = reveal_row {
            area =
                area.vertical_scroll_offset((row as f32 * row_h - (height - row_h) / 2.0).max(0.0));
            if !soft_wrap {
                area = area.horizontal_scroll_offset((m.start as f32 * char_w - 80.0).max(0.0));
            }
        }
        let mut style = RowStyle::new(theme, mono.clone());
        style.ansi_palette = self.options.ansi_palette.clone();
        if !self.options.font_family.is_empty() {
            style.font.family = crate::fonts::console_family(false, false);
        }
        if self.options.custom_colors {
            let rgb = |color: [u8; 3]| Color32::from_rgb(color[0], color[1], color[2]);
            style.text = rgb(self.options.foreground);
            style.stderr = rgb(self.options.stderr);
            style.system = rgb(self.options.system);
        }
        let filters = self.link_filters.clone();
        let selection = self.selection;
        let find_open = self.find.open;
        let (find, rows) = (&self.find, &self.rows);
        let (resp, shown, top_row) = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_rows(ui, row_h, n_rows, |ui, range| {
                    let top_row = range.start;
                    let row_w = ui.available_width().max(content_w);
                    let mut shown: Vec<ShownRow> = Vec::with_capacity(range.len());
                    // Links of the current line, shared by its wrapped rows.
                    let mut links: (Option<usize>, Vec<(usize, usize)>) = (None, Vec::new());
                    for i in range {
                        let Some(row) = rows.row(buf, i) else { break };
                        let Some(line) = buf.line(row.line) else {
                            break;
                        };
                        if links.0 != Some(row.line) {
                            links = (
                                Some(row.line),
                                all_links(line, &filters)
                                    .into_iter()
                                    .map(|(range, _)| range)
                                    .collect(),
                            );
                        }
                        let links = &links.1;
                        let hits: Vec<(usize, usize, bool)> = if find_open {
                            find.on_line(row.line)
                                .map(|(m, cur)| (m.start, m.end, cur))
                                .collect()
                        } else {
                            Vec::new()
                        };
                        let sel = selection.and_then(|s| s.cols_on(row.line, line.char_len()));
                        let galley = ui.fonts_mut(|f| {
                            f.layout_job(row_job(line, row, links, &hits, sel, &style))
                        });
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(row_w, row_h), Sense::hover());
                        let origin = Pos2::new(
                            rect.left() + 4.0,
                            rect.top() + (row_h - galley.size().y) / 2.0,
                        );
                        // A selection that continues on the next line shows its line break.
                        if row.end == line.char_len()
                            && selection.is_some_and(|s| s.includes_break_after(row.line))
                        {
                            let x = origin.x + galley.size().x;
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(x, rect.top()),
                                    Vec2::new(char_w * 0.5, row_h),
                                ),
                                0.0,
                                style.selection,
                            );
                        }
                        ui.painter().galley(origin, galley.clone(), style.text);
                        shown.push(ShownRow {
                            row,
                            rect,
                            origin,
                            galley,
                        });
                    }
                    let resp = ui.interact(ui.clip_rect(), area_id, Sense::click_and_drag());
                    // Selecting past the top or bottom edge scrolls.
                    if resp.dragged_by(PointerButton::Primary) {
                        if let Some(p) = resp.interact_pointer_pos() {
                            let view = ui.clip_rect();
                            let dy = if p.y < view.top() {
                                row_h
                            } else if p.y > view.bottom() {
                                -row_h
                            } else {
                                0.0
                            };
                            if dy != 0.0 {
                                ui.scroll_with_delta(Vec2::new(0.0, dy));
                                ui.ctx().request_repaint();
                            }
                        }
                    }
                    (resp, shown, top_row)
                })
            })
            .inner
            .inner;
        ui.data_mut(|d| d.insert_temp(area_id.with("top_row"), top_row));
        // Mouse: drag selects, double click a word, triple click a line,
        // Shift+click extends; a click on a file link opens it.
        if resp.clicked() || resp.drag_started() {
            ui.memory_mut(|m| m.request_focus(area_id));
        }
        let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
        let (press, shift) = ui.input(|i| (i.pointer.press_origin(), i.modifiers.shift));
        let chars_of = |line: usize| -> Vec<char> {
            buf.line(line)
                .map(|l| l.text.chars().collect())
                .unwrap_or_default()
        };
        let span_at = |unit: SelectUnit, p: Pos2| {
            let (caret, cell) = hit_test(&shown, p)?;
            let pos = if unit == SelectUnit::Char {
                caret
            } else {
                cell
            };
            Some(unit_span(unit, pos, &chars_of(pos.line)))
        };
        let link_at = |p: Pos2| {
            let (_, cell) = hit_test(&shown, p)?;
            let line = buf.line(cell.line)?;
            all_links(line, &filters)
                .into_iter()
                .find(|((a, b), _)| cell.col >= *a && cell.col < *b)
                .map(|(_, target)| target)
        };
        if let Some(p) = resp.hover_pos() {
            let icon = if link_at(p).is_some() {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::Text
            };
            ui.ctx().set_cursor_icon(icon);
        }
        if resp.triple_clicked() {
            self.selection = pointer
                .and_then(|p| span_at(SelectUnit::Line, p))
                .map(|s| TextSelection::new(SelectUnit::Line, s));
        } else if resp.double_clicked() {
            self.selection = pointer
                .and_then(|p| span_at(SelectUnit::Word, p))
                .map(|s| TextSelection::new(SelectUnit::Word, s));
        } else if resp.clicked() {
            match (self.selection.as_mut(), pointer) {
                (Some(sel), Some(p)) if shift => {
                    if let Some(s) = span_at(sel.unit, p) {
                        sel.extend_to(s);
                    }
                }
                _ => {
                    self.selection = None;
                    if let Some(target) = pointer.and_then(link_at) {
                        match target {
                            LinkTarget::File(path, line) => {
                                actions.push(ConsoleAction::OpenFile(path, line))
                            }
                            LinkTarget::Url(url) => actions.push(ConsoleAction::OpenUrl(url)),
                        }
                    }
                }
            }
        } else if resp.drag_started_by(PointerButton::Primary) {
            self.selection = press
                .and_then(|p| span_at(SelectUnit::Char, p))
                .map(|s| TextSelection::new(SelectUnit::Char, s));
        }
        if resp.dragged_by(PointerButton::Primary) {
            if let (Some(sel), Some(p)) = (self.selection.as_mut(), pointer) {
                if let Some(s) = span_at(sel.unit, p) {
                    sel.extend_to(s);
                }
            }
        }
        if resp.drag_stopped() && self.selection.is_some_and(|s| s.is_empty()) {
            self.selection = None;
        }
        // Context menu.
        crate::a11y::keyboard_context_menu(&resp, !self.announcement_blocked).show(|ui| {
            let has_selection = self.selection.is_some_and(|s| !s.is_empty());
            if ui
                .add_enabled(
                    has_selection,
                    egui::Button::new("Copy").shortcut_text("Ctrl+C"),
                )
                .clicked()
            {
                if let Some(text) = self.selected_text(buf) {
                    ui.ctx().copy_text(text);
                }
                ui.close();
            }
            if ui.button("Copy All").clicked() {
                ui.ctx().copy_text(buf.text());
                ui.close();
            }
            if ui
                .add(egui::Button::new("Select All").shortcut_text("Ctrl+A"))
                .clicked()
            {
                self.select_all(buf);
                ui.close();
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Find…").shortcut_text("Ctrl+F"))
                .clicked()
            {
                self.open_find(buf);
                ui.close();
            }
            ui.separator();
            if let Some(LinkTarget::File(path, _)) = pointer.and_then(link_at) {
                if ui.button("Select in Project").clicked() {
                    actions.push(ConsoleAction::SelectInProject(path));
                    ui.close();
                }
            }
            if ui.button("Clear All").clicked() {
                actions.push(ConsoleAction::ClearAll);
                ui.close();
            }
        });
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Run console output")
        });
        resp.ctx
            .accesskit_node_builder(resp.id, |node| node.set_role(egui::accesskit::Role::Group));
        self.area_focused = ui.memory(|m| m.has_focus(area_id));
        self.focused = self.area_focused || query_focused;
        if !self.announcement_blocked && !egui::Popup::is_any_open(ui.ctx()) {
            if query_focused && self.find.open {
                let detail = self
                    .find
                    .current()
                    .and_then(|hit| {
                        buf.line(hit.line).map(|line| {
                            let source: String = line.text.chars().take(160).collect();
                            format!(
                                "Result {} of {}. Line {}, column {}. {source}",
                                self.find.current_index().unwrap_or(0) + 1,
                                self.find.count(),
                                hit.line + 1,
                                hit.start + 1
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        if self.find.query.is_empty() {
                            "Type to search retained console output".into()
                        } else {
                            "No results".into()
                        }
                    });
                crate::a11y::announce(ui.ctx(), query_id, &format!("Console search. {detail}"));
            } else if self.area_focused {
                let line = shown
                    .last()
                    .and_then(|row| {
                        buf.line(row.row.line).map(|line| {
                            let text: String = line.text.chars().take(160).collect();
                            format!("Visible line {}: {text}", row.row.line + 1)
                        })
                    })
                    .unwrap_or_else(|| "No console output".into());
                crate::a11y::announce(
                    ui.ctx(),
                    resp.id,
                    &format!(
                        "Console output. {} retained lines. {}{line}. Ctrl F searches; Ctrl A selects all; Ctrl C copies; Tab moves to controls",
                        buf.len(),
                        if self
                            .selection
                            .is_some_and(|selection| !selection.is_empty())
                        {
                            "Selection active. "
                        } else {
                            ""
                        }
                    ),
                );
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OutputKind::{Stderr, Stdout};

    fn buffer(text: &str) -> ConsoleBuffer {
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, text);
        b
    }

    #[test]
    fn wraps_lines_into_rows_of_the_given_width() {
        assert_eq!(wrap_spans(0, 4).collect::<Vec<_>>(), [(0, 0)]);
        assert_eq!(wrap_spans(4, 4).collect::<Vec<_>>(), [(0, 4)]);
        assert_eq!(
            wrap_spans(9, 4).collect::<Vec<_>>(),
            [(0, 4), (4, 8), (8, 9)]
        );
    }

    #[test]
    fn row_index_follows_the_buffer_incrementally() {
        let mut b = ConsoleBuffer::with_max_lines(3);
        b.push(Stdout, "abcdefghij\nxy");
        let mut idx = RowIndex::default();
        idx.sync(&b, Some(4));
        let rows = |idx: &RowIndex, b: &ConsoleBuffer| {
            (0..idx.len(b))
                .map(|i| idx.row(b, i).map(|r| (r.line, r.start, r.end)))
                .collect::<Option<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            rows(&idx, &b),
            [(0, 0, 4), (0, 4, 8), (0, 8, 10), (1, 0, 2)]
        );
        // The open last line grows: only it is wrapped again.
        b.push(Stdout, "zzzz\n\n");
        idx.sync(&b, Some(4));
        assert_eq!(
            rows(&idx, &b),
            [
                (0, 0, 4),
                (0, 4, 8),
                (0, 8, 10),
                (1, 0, 4),
                (1, 4, 6),
                (2, 0, 0)
            ]
        );
        assert_eq!(idx.row_of(&b, 1, 5), Some(4));
        // The cycle buffer drops line 0, and a new width re-wraps everything.
        b.push(Stdout, "end");
        idx.sync(&b, Some(4));
        assert_eq!(rows(&idx, &b)[0], (1, 0, 4));
        idx.sync(&b, None);
        assert_eq!(idx.len(&b), 3);
        assert_eq!(
            idx.row(&b, 0),
            Some(VisualRow {
                line: 1,
                start: 0,
                end: 6
            })
        );
        assert_eq!(idx.row_of(&b, 3, 0), Some(2));
        assert_eq!(idx.row_of(&b, 0, 0), None, "dropped line");
        assert_eq!(idx.max_chars, 6, "rebuilt from the lines still kept");

        // A stream's open line that is no longer the newest still re-wraps
        // (a stderr progress bar while stdout prints).
        let mut b = ConsoleBuffer::default();
        b.push(Stderr, "\r1%");
        b.push(Stdout, "out\n");
        let mut idx = RowIndex::default();
        idx.sync(&b, Some(4));
        assert_eq!(rows(&idx, &b), [(0, 0, 2), (1, 0, 3)]);
        b.push(Stderr, "\r100% done");
        idx.sync(&b, Some(4));
        assert_eq!(rows(&idx, &b), [(0, 0, 4), (0, 4, 8), (0, 8, 9), (1, 0, 3)]);
        let mut f = FindState {
            open: true,
            query: "done".into(),
            ..Default::default()
        };
        b.push(Stderr, "\r50%");
        f.sync(&b);
        assert_eq!(f.count(), 0);
        b.push(Stderr, "\rall done\n");
        f.sync(&b);
        assert_eq!(
            f.count(),
            1,
            "hits on the older line are found once it changes"
        );
    }

    #[test]
    fn find_tracks_new_output_and_steps_around() {
        assert_eq!(
            find_in_line("Error: error ERROR", "error", false),
            [(0, 5), (7, 12), (13, 18)]
        );
        assert_eq!(find_in_line("Error: error ERROR", "error", true), [(7, 12)]);
        assert_eq!(
            find_in_line("ééé", "éé", true),
            [(0, 2)],
            "non-overlapping, character offsets"
        );
        let mut b = buffer("one error\ntwo\nthree err");
        let mut f = FindState {
            open: true,
            query: "err".into(),
            ..Default::default()
        };
        assert!(f.sync(&b), "first sync counts as a new query");
        assert_eq!(f.count(), 2);
        f.select_from(1);
        assert_eq!(
            f.current(),
            Some(Match {
                line: 2,
                start: 6,
                end: 9
            })
        );
        // More output: the open last line is searched again, the rest stays.
        b.push(Stdout, "or err\n");
        assert!(!f.sync(&b));
        assert_eq!(f.count(), 3);
        assert_eq!(
            f.current(),
            Some(Match {
                line: 2,
                start: 6,
                end: 9
            }),
            "current hit kept"
        );
        f.step(true);
        assert_eq!(
            f.current().map(|m| (m.line, m.start)),
            Some((2, 12)),
            "in `three error err`"
        );
        f.step(true);
        assert_eq!(
            f.current().map(|m| (m.line, m.start)),
            Some((0, 4)),
            "wraps around"
        );
        f.step(false);
        assert_eq!(f.current_index(), Some(2));
        assert_eq!(
            f.on_line(2)
                .map(|(m, cur)| (m.start, cur))
                .collect::<Vec<_>>(),
            [(6, false), (12, true)]
        );
        f.case_sensitive = true;
        f.query = "ERR".into();
        assert!(f.sync(&b));
        assert_eq!(f.count(), 0);
        assert_eq!(f.current(), None);
    }

    #[test]
    fn selected_text_spans_lines_and_ignores_soft_wraps() {
        let b = buffer("first line\nsecond\nthird line\n");
        assert_eq!(
            text_between(&b, TextPos::new(0, 6), TextPos::new(2, 5)),
            "line\nsecond\nthird"
        );
        assert_eq!(
            text_between(&b, TextPos::new(1, 0), TextPos::new(1, usize::MAX)),
            "second"
        );
        assert_eq!(text_between(&b, TextPos::new(2, 3), TextPos::new(2, 3)), "");
        let mut view = ConsoleView::default();
        view.select_all(&b);
        assert_eq!(
            view.selected_text(&b).as_deref(),
            Some("first line\nsecond\nthird line")
        );
        view.selection = Some(TextSelection::new(
            SelectUnit::Char,
            (TextPos::new(2, 0), TextPos::new(2, 5)),
        ));
        view.open_find(&b);
        assert!(view.find.open);
        assert_eq!(
            view.find.query, "third",
            "a one-line selection seeds the search"
        );
    }

    #[test]
    fn row_layout_marks_links_hits_and_selection() {
        let mut b = ConsoleBuffer::default();
        b.push(Stderr, "  File \"/tmp/a.py\", line 3, in <module>\n");
        let line = b.line(0).unwrap();
        let links: Vec<(usize, usize)> = links_of(line).into_iter().map(|(r, ..)| r).collect();
        assert_eq!(links, [(2, 26)]);
        let st = RowStyle::new(&UiTheme::dark(), FontId::monospace(13.0));
        let row = VisualRow {
            line: 0,
            start: 0,
            end: line.char_len(),
        };
        let job = row_job(line, row, &links, &[(28, 30, true)], Some((0, 4)), &st);
        assert_eq!(job.text, line.text);
        let section = |byte: usize| {
            job.sections
                .iter()
                .find(|s| s.byte_range.contains(&egui::text::ByteIndex(byte)))
                .unwrap()
        };
        assert_eq!(section(0).format.background, st.selection);
        assert_eq!(
            section(3).format.background,
            st.selection,
            "the selection wins over the link"
        );
        assert_eq!(section(10).format.color, st.link);
        assert!(section(10).format.underline.width > 0.0);
        assert_eq!(section(28).format.background, st.current_hit);
        assert_eq!(section(35).format.color, st.stderr);
        assert_eq!(section(35).format.background, Color32::TRANSPARENT);
        // A wrapped row lays out only its own characters.
        let job = row_job(
            line,
            VisualRow {
                line: 0,
                start: 7,
                end: 17,
            },
            &links,
            &[],
            None,
            &st,
        );
        assert_eq!(job.text, "\"/tmp/a.py");
    }
    #[test]
    fn urls_custom_unicode_file_filters_and_folded_rows_remain_navigable() {
        let mut buffer = buffer(
            "μ /project/test.py:23 https://example.com/docs.\ninternal/site-packages/hidden.py\nvisible\n",
        );
        let pattern = regex::Regex::new(r"(?P<path>/[^ :]+):(?P<line>\d+)").unwrap();
        let links = all_links(buffer.line(0).unwrap(), &[pattern]);
        assert!(links.iter().any(|(_,target)|matches!(target,LinkTarget::File(path,23)if path==&PathBuf::from("/project/test.py"))));
        assert!(links.iter().any(
            |(_, target)| matches!(target,LinkTarget::Url(url)if url=="https://example.com/docs")
        ));
        let mut rows = RowIndex::default();
        rows.set_hidden([1].into());
        rows.sync(&buffer, None);
        assert_eq!(rows.len(&buffer), 2);
        assert_eq!(rows.row(&buffer, 1).unwrap().line, 2);
        buffer.push(OutputKind::Stdout, "new\n");
        rows.sync(&buffer, None);
        assert_eq!(rows.len(&buffer), 3);
    }
    #[test]
    fn truecolor_layout_preserves_exact_foreground_and_background() {
        let buffer = buffer("\x1b[38;2;12;34;56;48;2;65;43;21;3;4mX");
        let line = buffer.line(0).unwrap();
        let style = RowStyle::new(&UiTheme::dark(), FontId::monospace(13.0));
        let job = row_job(
            line,
            VisualRow {
                line: 0,
                start: 0,
                end: 1,
            },
            &[],
            &[],
            None,
            &style,
        );
        assert_eq!(job.sections[0].format.color, Color32::from_rgb(12, 34, 56));
        assert_eq!(
            job.sections[0].format.background,
            Color32::from_rgb(65, 43, 21)
        );
        assert!(job.sections[0].format.underline.width > 0.0);
    }
}

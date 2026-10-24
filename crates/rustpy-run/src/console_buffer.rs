//! Console output model for the Run tool window (roadmap §3.2 "Console
//! view"): streamed process output becomes styled lines incrementally, with
//! partial lines, carriage returns (progress bars), ANSI colors and a cycle
//! buffer that drops the oldest lines.

use std::collections::VecDeque;
use std::ops::Range;

use crate::ansi::{apply_sgr, SgrState};
use crate::process::OutputKind;

/// Lines a console keeps before it drops the oldest ("Override console cycle
/// buffer size").
pub const DEFAULT_MAX_LINES: usize = 100_000;

/// Tab stops every this many columns.
const TAB_WIDTH: usize = 8;

/// An escape sequence still unterminated after this many bytes is dropped.
const MAX_ESCAPE_LEN: usize = 512;

/// A run of characters with the same style from the same stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleRun {
    /// Character index where the run starts.
    pub start: usize,
    pub style: SgrState,
    pub kind: OutputKind,
}

/// One console line: the text without escape sequences, plus style runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLine {
    /// Stream that started the line.
    pub kind: OutputKind,
    /// Plain text: tabs expanded, control characters removed.
    pub text: String,
    /// Style runs ordered by `start`; the first starts at 0.
    pub runs: Vec<StyleRun>,
    chars: usize,
}

impl ConsoleLine {
    fn new(kind: OutputKind) -> Self {
        Self {
            kind,
            text: String::new(),
            runs: Vec::new(),
            chars: 0,
        }
    }

    /// Length in characters.
    pub fn char_len(&self) -> usize {
        self.chars
    }

    /// Each style run as `(character range, style, stream)`.
    pub fn spans(&self) -> impl Iterator<Item = (Range<usize>, SgrState, OutputKind)> + '_ {
        self.runs.iter().enumerate().map(|(i, r)| {
            (
                r.start..self.runs.get(i + 1).map_or(self.chars, |n| n.start),
                r.style,
                r.kind,
            )
        })
    }

    fn push(&mut self, c: char, style: SgrState, kind: OutputKind) {
        if self
            .runs
            .last()
            .is_none_or(|r| r.style != style || r.kind != kind)
        {
            self.runs.push(StyleRun {
                start: self.chars,
                style,
                kind,
            });
        }
        self.text.push(c);
        self.chars += 1;
    }

    fn pop(&mut self) {
        if self.text.pop().is_some() {
            self.chars -= 1;
            if self.runs.last().is_some_and(|r| r.start == self.chars) {
                self.runs.pop();
            }
        }
    }

    fn write_at(&mut self, column: usize, c: char, style: SgrState, kind: OutputKind) {
        if column >= self.chars {
            while self.chars < column {
                self.push(' ', style, kind);
            }
            self.push(c, style, kind);
            return;
        }
        let mut cells = Vec::with_capacity(self.chars);
        let mut run = 0;
        for (index, ch) in self.text.chars().enumerate() {
            while self.runs.get(run + 1).is_some_and(|r| r.start <= index) {
                run += 1;
            }
            let original = self
                .runs
                .get(run)
                .map(|r| (r.style, r.kind))
                .unwrap_or((SgrState::default(), self.kind));
            cells.push(if index == column {
                (c, style, kind)
            } else {
                (ch, original.0, original.1)
            });
        }
        self.reset(kind);
        for (ch, style, kind) in cells {
            self.push(ch, style, kind);
        }
    }
    fn truncate_at(&mut self, column: usize) {
        while self.chars > column {
            self.pop();
        }
    }

    fn reset(&mut self, kind: OutputKind) {
        self.text.clear();
        self.runs.clear();
        self.chars = 0;
        self.kind = kind;
    }
}

/// Decoding state of one output stream. Stdout and stderr each have their
/// own: a carriage return, line break or cut-off escape sequence on one stream
/// never touches the other stream's line (unbuffered `print()` writes the text
/// and the `\n` separately, and the streams' reader threads race).
#[derive(Clone, Debug, Default)]
struct StreamState {
    /// SGR state (colors, bold, …) carried from chunk to chunk.
    style: SgrState,
    /// Escape sequence cut off at the end of the previous chunk.
    pending_escape: String,
    /// A `\r` was read: the next character rewrites the line (unless it is `\n`).
    pending_cr: bool,
    cursor: Option<usize>,
    vertical_cursor: bool,
    /// Absolute number of this stream's line that has no line break yet.
    open: Option<usize>,
}

/// Styled console lines, built incrementally from output chunks.
///
/// Lines have absolute numbers that keep counting when old lines are dropped
/// (cycle buffer) or cleared, so a view can keep positions such as a
/// selection or search results across updates. A line can change only while a
/// stream still writes to it (it has no line break yet): every line before
/// [`ConsoleBuffer::oldest_open_line`] is final.
#[derive(Clone, Debug)]
pub struct ConsoleBuffer {
    lines: VecDeque<ConsoleLine>,
    /// Absolute number of `lines[0]`.
    first: usize,
    max_lines: usize,
    max_bytes: usize,
    bytes: usize,
    /// Stdout's and stderr's state; system messages (echoed input, exit
    /// status) continue whichever stream's line is newest.
    streams: [StreamState; 2],
    revision: u64,
}

impl Default for ConsoleBuffer {
    fn default() -> Self {
        Self::with_max_lines(DEFAULT_MAX_LINES)
    }
}

/// An escape sequence at the start of a string.
enum Escape<'a> {
    /// The text ends before the sequence does.
    Incomplete,
    /// `ESC [ params final`.
    Csi {
        params: &'a str,
        final_byte: u8,
        len: usize,
    },
    /// Any other sequence (OSC titles, charset selection, …) to skip: its length.
    Other(usize),
}

/// Parses the escape sequence `s` starts with (`s` starts with ESC).
fn parse_escape(s: &str) -> Escape<'_> {
    let b = s.as_bytes();
    match b.get(1) {
        None => Escape::Incomplete,
        Some(b'[') => {
            // Parameter bytes 0x30–0x3F, intermediates 0x20–0x2F, final byte 0x40–0x7E.
            for (j, &x) in b.iter().enumerate().skip(2) {
                match x {
                    0x20..=0x3F => {}
                    0x40..=0x7E => {
                        return Escape::Csi {
                            params: &s[2..j],
                            final_byte: x,
                            len: j + 1,
                        };
                    }
                    // Malformed: drop the introducer and parameters, keep the rest as text.
                    _ => return Escape::Other(j),
                }
            }
            Escape::Incomplete
        }
        // OSC, DCS, APC, PM: up to BEL or ST (`ESC \`).
        Some(b']' | b'P' | b'_' | b'^') => {
            for j in 2..b.len() {
                if b[j] == 0x07 {
                    return Escape::Other(j + 1);
                }
                if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
                    return Escape::Other(j + 2);
                }
            }
            Escape::Incomplete
        }
        Some(b'(' | b')' | b'*' | b'+') if b.len() < 3 => Escape::Incomplete,
        Some(b'(' | b')' | b'*' | b'+') => Escape::Other(3),
        Some(_) => Escape::Other(1 + s[1..].chars().next().map_or(1, char::len_utf8)),
    }
}

impl ConsoleBuffer {
    pub fn with_max_lines(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            first: 0,
            max_lines: max_lines.max(1),
            max_bytes: 4 * 1024 * 1024,
            bytes: 0,
            streams: Default::default(),
            revision: 0,
        }
    }

    pub fn set_max_bytes(&mut self, bytes: usize) {
        self.max_bytes = bytes.clamp(1024, 256 * 1024 * 1024);
        self.revision = self.revision.wrapping_add(1);
        self.trim();
    }
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// Changes the cycle buffer size, dropping the oldest lines beyond it.
    pub fn set_max_lines(&mut self, max_lines: usize) {
        self.max_lines = max_lines.max(1);
        self.revision = self.revision.wrapping_add(1);
        self.trim();
    }

    /// Absolute number of the oldest line kept.
    pub fn first_line(&self) -> usize {
        self.first
    }

    /// One past the absolute number of the newest line.
    pub fn end_line(&self) -> usize {
        self.first + self.lines.len()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line with absolute number `n`, if it is still kept.
    pub fn line(&self, n: usize) -> Option<&ConsoleLine> {
        n.checked_sub(self.first).and_then(|i| self.lines.get(i))
    }

    /// The lines from absolute number `from` on, with their numbers.
    pub fn lines_from(&self, from: usize) -> impl Iterator<Item = (usize, &ConsoleLine)> + '_ {
        let first = self.first;
        self.lines
            .iter()
            .enumerate()
            .skip(from.saturating_sub(first))
            .map(move |(i, l)| (first + i, l))
    }

    /// Increases whenever the text changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the newest line has no line break yet and may still change.
    pub fn last_line_open(&self) -> bool {
        let end = self.end_line();
        end > self.first && self.streams.iter().any(|st| st.open == Some(end - 1))
    }

    /// The oldest line a stream still writes to (no line break yet): it and
    /// any later line may change; every earlier line is final.
    pub fn oldest_open_line(&self) -> Option<usize> {
        self.streams
            .iter()
            .filter_map(|st| st.open)
            .filter(|n| (self.first..self.end_line()).contains(n))
            .min()
    }

    /// Plain text of all kept lines; terminated lines end with `\n`.
    pub fn text(&self) -> String {
        let last_open = self.last_line_open();
        let mut out = String::new();
        for (i, l) in self.lines.iter().enumerate() {
            out.push_str(&l.text);
            if i + 1 < self.lines.len() || !last_open {
                out.push('\n');
            }
        }
        out
    }

    /// Removes all lines; numbering continues after them.
    pub fn clear(&mut self) {
        self.first = self.end_line();
        self.lines.clear();
        self.bytes = 0;
        for st in &mut self.streams {
            st.open = None;
            st.pending_cr = false;
            st.cursor = None;
            st.vertical_cursor = false;
        }
        self.revision += 1;
    }

    /// Stream state used for output of `kind`.
    fn slot(&self, kind: OutputKind) -> usize {
        match kind {
            OutputKind::Stdout => 0,
            OutputKind::Stderr => 1,
            // Echoed input and status messages continue the newest open line.
            OutputKind::System => (0..self.streams.len())
                .filter(|&s| self.open_index(s).is_some())
                .max_by_key(|&s| self.streams[s].open)
                .unwrap_or(0),
        }
    }

    /// Index into `lines` of stream `s`'s open line, if it is still kept.
    fn open_index(&self, s: usize) -> Option<usize> {
        self.streams[s]
            .open
            .and_then(|n| n.checked_sub(self.first))
            .filter(|&i| i < self.lines.len())
    }

    /// Appends a chunk of output from `kind`'s stream. Chunks may end anywhere,
    /// even inside an escape sequence.
    pub fn push(&mut self, kind: OutputKind, text: &str) {
        if text.is_empty() {
            return;
        }
        self.revision += 1;
        let s = self.slot(kind);
        let joined;
        let text = if self.streams[s].pending_escape.is_empty() {
            text
        } else {
            joined = std::mem::take(&mut self.streams[s].pending_escape) + text;
            joined.as_str()
        };
        let mut i = 0;
        while let Some(c) = text[i..].chars().next() {
            match c {
                '\x1b' => match parse_escape(&text[i..]) {
                    Escape::Incomplete => {
                        if text.len() - i <= MAX_ESCAPE_LEN {
                            self.streams[s].pending_escape = text[i..].to_string();
                        }
                        break;
                    }
                    Escape::Csi {
                        params,
                        final_byte,
                        len,
                    } => {
                        self.csi(s, params, final_byte, kind);
                        i += len;
                        continue;
                    }
                    Escape::Other(len) => {
                        i += len;
                        continue;
                    }
                },
                '\n' => self.newline(s, kind),
                '\r' => self.streams[s].pending_cr = true,
                '\t' => {
                    for _ in 0..TAB_WIDTH - self.column(s) % TAB_WIDTH {
                        self.put(s, ' ', kind);
                    }
                }
                // Backspace: progress output steps back to overwrite.
                '\x08' => {
                    if !self.streams[s].pending_cr {
                        if let Some(li) = self.open_index(s) {
                            let before = self.lines[li].text.len();
                            self.lines[li].pop();
                            self.bytes -= before - self.lines[li].text.len();
                            self.streams[s].cursor = None;
                        }
                    }
                }
                c if c.is_control() => {}
                c => self.put(s, c, kind),
            }
            i += c.len_utf8();
        }
        self.trim();
    }

    /// Cursor column of stream `s`.
    fn column(&self, s: usize) -> usize {
        match self.open_index(s) {
            Some(li) if !self.streams[s].pending_cr => {
                self.streams[s].cursor.unwrap_or(self.lines[li].chars)
            }
            _ => 0,
        }
    }

    fn move_row(&mut self, s: usize, row: usize, column: usize, kind: OutputKind) {
        let row = row.clamp(self.first, self.first.saturating_add(4095));
        while self.end_line() <= row {
            self.lines.push_back(ConsoleLine::new(kind));
        }
        self.streams[s].open = Some(row);
        self.streams[s].vertical_cursor = true;
        self.streams[s].cursor = Some(column.min(4095));
        self.streams[s].pending_cr = false;
    }
    fn csi(&mut self, s: usize, params: &str, final_byte: u8, kind: OutputKind) {
        let count = params
            .split(';')
            .next()
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 4096);
        match final_byte {
            b'm' => apply_sgr(params, &mut self.streams[s].style),
            b'A' | b'B' => {
                let row = self.streams[s]
                    .open
                    .unwrap_or_else(|| self.end_line().saturating_sub(1));
                let column = self.column(s);
                let row = if final_byte == b'A' {
                    row.saturating_sub(count)
                } else {
                    row.saturating_add(count)
                };
                self.move_row(s, row, column, kind);
            }
            b'H' | b'f' => {
                let mut parts = params.split(';');
                let row = parts
                    .next()
                    .and_then(|p| p.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 4096);
                let column = parts
                    .next()
                    .and_then(|p| p.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 4096);
                self.move_row(s, self.first + row - 1, column - 1, kind);
            }
            b'J' if matches!(params, "2" | "3") => self.clear(),
            b'G' => self.streams[s].cursor = Some(count - 1),
            b'C' => self.streams[s].cursor = Some(self.column(s).saturating_add(count).min(4096)),
            b'D' => self.streams[s].cursor = Some(self.column(s).saturating_sub(count)),
            b'K' => {
                if let Some(li) = self.open_index(s) {
                    let before = self.lines[li].text.len();
                    let column = self.column(s);
                    if params == "2" || self.streams[s].pending_cr {
                        self.lines[li].reset(kind);
                        self.streams[s].cursor = None;
                        self.streams[s].pending_cr = false;
                    } else if params == "1" {
                        for column in 0..=column.min(self.lines[li].chars) {
                            self.lines[li].write_at(column, ' ', self.streams[s].style, kind);
                        }
                    } else {
                        self.lines[li].truncate_at(column);
                    }
                    self.bytes = self.bytes.saturating_sub(before) + self.lines[li].text.len();
                }
            }
            _ => {}
        }
    }

    fn newline(&mut self, s: usize, kind: OutputKind) {
        self.streams[s].pending_cr = false;
        self.streams[s].cursor = None;
        if self.open_index(s).is_none() {
            self.lines.push_back(ConsoleLine::new(kind));
        }
        self.streams[s].open = match self.streams[s].open {
            Some(row) if self.streams[s].vertical_cursor && row + 1 < self.end_line() => {
                self.streams[s].cursor = Some(0);
                Some(row + 1)
            }
            _ => {
                self.streams[s].vertical_cursor = false;
                None
            }
        };
    }

    fn put(&mut self, s: usize, c: char, kind: OutputKind) {
        let li = match self.open_index(s) {
            Some(li) => {
                if self.streams[s].pending_cr {
                    // Carriage return: the new text replaces the line (progress bars).
                    self.bytes -= self.lines[li].text.len();
                    self.lines[li].reset(kind);
                    self.streams[s].cursor = None;
                }
                li
            }
            None => {
                self.lines.push_back(ConsoleLine::new(kind));
                self.streams[s].open = Some(self.end_line() - 1);
                self.lines.len() - 1
            }
        };
        self.streams[s].pending_cr = false;
        let style = if kind == OutputKind::System {
            SgrState::default()
        } else {
            self.streams[s].style
        };
        let before = self.lines[li].text.len();
        if let Some(column) = self.streams[s].cursor {
            self.lines[li].write_at(column, c, style, kind);
            self.streams[s].cursor = Some(column + 1);
        } else {
            self.lines[li].push(c, style, kind);
        }
        self.bytes = self.bytes.saturating_sub(before) + self.lines[li].text.len();
        if self.lines[li].text.len() > self.max_bytes {
            self.trim();
        }
    }

    fn trim(&mut self) {
        while self.lines.len() > self.max_lines || self.bytes > self.max_bytes {
            if let Some(line) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(line.text.len());
            } else {
                break;
            }
            self.first += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OutputKind::{Stderr, Stdout, System};

    fn texts(b: &ConsoleBuffer) -> Vec<&str> {
        b.lines_from(0).map(|(_, l)| l.text.as_str()).collect()
    }

    #[test]
    fn joins_partial_lines_across_chunks() {
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "hel");
        assert_eq!(texts(&b), ["hel"]);
        assert!(b.last_line_open());
        b.push(Stdout, "lo\nwor");
        b.push(Stdout, "ld\n\n");
        assert_eq!(texts(&b), ["hello", "world", ""]);
        assert!(!b.last_line_open());
        assert_eq!(b.text(), "hello\nworld\n\n");
        b.push(Stdout, "a\r\nb\r");
        b.push(Stdout, "\nc");
        assert_eq!(
            texts(&b)[3..],
            ["a", "b", "c"],
            "CRLF, also split across chunks"
        );
        assert_eq!(b.text(), "hello\nworld\n\na\nb\nc");
    }

    #[test]
    fn carriage_return_rewrites_the_line() {
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "start\n");
        b.push(Stderr, "\r  0%|          | 0/10");
        assert_eq!(
            texts(&b),
            ["start", "  0%|          | 0/10"],
            "shown before the line ends"
        );
        b.push(Stderr, "\r 50%|#####     | 5/10");
        b.push(Stderr, "\r100%|##########| 10/10");
        assert_eq!(texts(&b), ["start", "100%|##########| 10/10"]);
        b.push(Stderr, "\n");
        b.push(Stdout, "done 50%\r");
        assert_eq!(texts(&b)[2], "done 50%", "kept until something replaces it");
        b.push(Stdout, "done\n");
        assert_eq!(texts(&b), ["start", "100%|##########| 10/10", "done"]);
        assert_eq!(b.line(1).map(|l| l.kind), Some(Stderr));
        // Erase-in-line and cursor-to-column-1 variants.
        b.push(Stdout, "abc\x1b[2Kxyz\n");
        b.push(Stdout, "old\r\x1b[K\n");
        b.push(Stdout, "old\x1b[1Gnew\n");
        assert_eq!(texts(&b)[3..], ["xyz", "", "new"]);
        // Backspaces step back over characters.
        b.push(Stdout, "ab\x08c\n");
        assert_eq!(texts(&b)[6], "ac");
    }

    #[test]
    fn styles_survive_escape_sequences_split_between_chunks() {
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "\x1b[3");
        assert!(
            b.is_empty(),
            "an incomplete escape sequence waits for the rest"
        );
        b.push(Stdout, "1mred\x1b[0m plain\n");
        let line = b.line(0).unwrap();
        assert_eq!(line.text, "red plain");
        let spans: Vec<_> = line.spans().map(|(r, st, _)| (r, st.fg)).collect();
        assert_eq!(spans, [(0..3, Some(1)), (3..9, None)]);
        // OSC (window title) and other sequences are dropped; tabs expand to stops of 8.
        b.push(Stdout, "\x1b]0;title\x07a\tb\x1b(B\x07\n");
        assert_eq!(b.line(1).unwrap().text, "a       b");
        b.push(Stdout, "é\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ ü\n");
        assert_eq!(b.line(2).unwrap().text, "élink ü");
    }

    #[test]
    fn runs_remember_their_stream() {
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "Name: ");
        b.push(System, "bob\n");
        let line = b.line(0).unwrap();
        assert_eq!(line.text, "Name: bob");
        assert_eq!(line.kind, Stdout);
        let kinds: Vec<_> = line.spans().map(|(r, _, k)| (r, k)).collect();
        assert_eq!(kinds, [(0..6, Stdout), (6..9, System)]);
    }

    #[test]
    fn streams_keep_their_own_lines() {
        // A progress bar on stderr while unbuffered print() writes each result
        // and its line break separately: no result is overwritten.
        let mut b = ConsoleBuffer::default();
        b.push(Stderr, "\r 16.8%|###");
        b.push(Stdout, "result 68");
        b.push(Stderr, "\r 17.0%|###");
        b.push(Stdout, "\n");
        b.push(Stdout, "result 69\n");
        assert_eq!(texts(&b), [" 17.0%|###", "result 68", "result 69"]);
        assert_eq!(b.oldest_open_line(), Some(0), "the bar can still change");
        assert!(!b.last_line_open());
        assert_eq!(b.text(), " 17.0%|###\nresult 68\nresult 69\n");
        b.push(Stderr, "\r100%|##########\n");
        assert_eq!(texts(&b)[0], "100%|##########");
        assert_eq!(b.oldest_open_line(), None);

        // Lines of the two streams never merge, and keep their stream.
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "epoch 81 loss=0.5");
        b.push(Stderr, "WARNING slow batch 81\n");
        b.push(Stdout, "\n");
        b.push(Stdout, "out214");
        b.push(Stderr, "err213");
        b.push(Stderr, "\n");
        b.push(Stdout, "\n");
        assert_eq!(
            texts(&b),
            [
                "epoch 81 loss=0.5",
                "WARNING slow batch 81",
                "out214",
                "err213"
            ]
        );
        let kinds: Vec<_> = b.lines_from(0).map(|(_, l)| l.kind).collect();
        assert_eq!(kinds, [Stdout, Stderr, Stdout, Stderr]);

        // An escape sequence cut off on one stream does not swallow the other's text.
        let mut b = ConsoleBuffer::default();
        b.push(Stdout, "\x1b[3");
        b.push(Stderr, "Traceback\n");
        b.push(Stdout, "1mred\x1b[0m\n");
        assert_eq!(texts(&b), ["Traceback", "red"]);
        assert_eq!(
            b.line(1).unwrap().spans().next().map(|(_, st, _)| st.fg),
            Some(Some(1))
        );
        assert_eq!(
            b.line(0).unwrap().spans().next().map(|(_, st, _)| st.fg),
            Some(None),
            "stdout's color does not leak into stderr"
        );

        // Echoed input continues the prompt's line.
        let mut b = ConsoleBuffer::default();
        b.push(Stderr, "warn\n");
        b.push(Stdout, "Name: ");
        b.push(System, "bob\n");
        assert_eq!(texts(&b), ["warn", "Name: bob"]);
    }

    #[test]
    fn cycle_buffer_drops_the_oldest_lines_and_keeps_numbering() {
        let mut b = ConsoleBuffer::with_max_lines(3);
        for i in 0..5 {
            b.push(Stdout, &format!("l{i}\n"));
        }
        assert_eq!(texts(&b), ["l2", "l3", "l4"]);
        assert_eq!((b.first_line(), b.end_line()), (2, 5));
        assert_eq!(b.line(2).map(|l| l.text.as_str()), Some("l2"));
        assert!(b.line(1).is_none());
        let rev = b.revision();
        b.clear();
        assert!(b.is_empty() && b.revision() > rev);
        assert_eq!(
            (b.first_line(), b.end_line()),
            (5, 5),
            "numbering continues after a clear"
        );
        b.push(Stdout, "next");
        assert_eq!(b.line(5).map(|l| l.text.as_str()), Some("next"));
        b.set_max_lines(1);
        assert_eq!(b.len(), 1);
    }
    #[test]
    fn horizontal_cursor_overwrites_and_erases_with_styles() {
        let mut buffer = ConsoleBuffer::default();
        buffer.push(Stdout, "abcdef\x1b[3D\x1b[31mXY\x1b[K");
        assert_eq!(buffer.text(), "abcXY");
        assert_eq!(
            buffer.line(0).unwrap().runs.last().unwrap().style.fg,
            Some(1)
        );
        buffer.push(Stdout, "\x1b[2G!");
        assert_eq!(buffer.text(), "a!cXY");
    }
    #[test]
    fn byte_cycle_limit_covers_unterminated_output_and_mixed_streams() {
        let mut buffer = ConsoleBuffer::default();
        buffer.set_max_bytes(1024);
        for _ in 0..100 {
            buffer.push(Stdout, &"μ".repeat(100));
            buffer.push(Stderr, "error\n");
            assert!(buffer.retained_bytes() <= 1024);
        }
        let bytes = buffer
            .lines_from(0)
            .map(|(_, line)| line.text.len())
            .sum::<usize>();
        assert_eq!(bytes, buffer.retained_bytes());
    }
}

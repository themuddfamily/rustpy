//! TextMate grammar highlighting for editors whose file type has no built-in
//! highlighter (Settings › Editor › TextMate Bundles).
//!
//! Lines are tokenized from the top with the tokenizer state carried from one
//! line to the next ([`StackState`]). Every line's entry keeps the state it
//! started and ended with, so an edit only re-tokenizes from its first changed
//! line, and only until a re-tokenized line ends in the state the cached line
//! below it started with: from there on the cached lines are still right. The
//! editor view asks for its visible lines only; tokenizing stops there (plus a
//! small look-ahead) or when the frame's time budget runs out, and continues
//! in the next frames.
//!
//! Patterns compile when first needed, which takes long for large grammars
//! (the first lines of a TypeScript file can take a second), so attaching a
//! grammar first tokenizes the document's first lines on a background thread;
//! the view shows plain text until those patterns are compiled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustpy_core::{TextRange, TextSize};
use rustpy_highlight::{HighlightTag, Span};
use rustpy_text::{Document, Edit};
use rustpy_textmate::{Grammar, StackState, Token};

use super::Language;

/// Lines tokenized below the last visible one, so that scrolling a little
/// shows highlighted text at once.
pub const LOOKAHEAD_LINES: usize = 50;

/// Tokenizing time per frame; the rest is left to the following frames.
pub const FRAME_BUDGET: Duration = Duration::from_millis(8);

/// Lines the warm-up thread tokenizes to compile the patterns they need.
const WARM_UP_LINES: usize = 100;

const RAINBOW: [HighlightTag; 6] = [
    HighlightTag::Rainbow1,
    HighlightTag::Rainbow2,
    HighlightTag::Rainbow3,
    HighlightTag::Rainbow4,
    HighlightTag::Rainbow5,
    HighlightTag::Rainbow6,
];

/// The highlight tag for a tag name of `rustpy_textmate::scope_to_tag` /
/// `Token::tag` (every name of `rustpy_textmate::TAG_NAMES`).
pub fn tag_from_name(name: &str) -> Option<HighlightTag> {
    Some(match name {
        "Keyword" => HighlightTag::Keyword,
        "String" => HighlightTag::String,
        "Comment" => HighlightTag::Comment,
        "Number" => HighlightTag::Number,
        "FunctionDefinition" => HighlightTag::FunctionDefinition,
        "ClassDefinition" => HighlightTag::ClassDefinition,
        "Decorator" => HighlightTag::Decorator,
        "Builtin" => HighlightTag::Builtin,
        "Operator" => HighlightTag::Operator,
        "Tag" => HighlightTag::Tag,
        "Key" => HighlightTag::Key,
        "Variable" => HighlightTag::Variable,
        "Link" => HighlightTag::Link,
        "Heading" => HighlightTag::Heading,
        _ => return None,
    })
}

/// Highlight spans of a tokenized line (byte offsets in the line); adjacent
/// tokens with the same tag are merged.
fn spans_of(tokens: &[Token]) -> Vec<(u32, u32, HighlightTag)> {
    let mut out: Vec<(u32, u32, HighlightTag)> = Vec::new();
    for token in tokens {
        let Some(tag) = token.tag().and_then(tag_from_name) else {
            continue;
        };
        let (start, end) = (token.range.start as u32, token.range.end as u32);
        match out.last_mut() {
            Some(last) if last.1 == start && last.2 == tag => last.1 = end,
            _ => out.push((start, end, tag)),
        }
    }
    out
}

/// Whether byte `i` of a line lies in a string or comment span (sorted spans,
/// `k` is a cursor advanced along them).
fn in_string_or_comment(spans: &[(u32, u32, HighlightTag)], k: &mut usize, i: usize) -> bool {
    let i = i as u32;
    while *k < spans.len() && spans[*k].1 <= i {
        *k += 1;
    }
    spans.get(*k).is_some_and(|&(start, _, tag)| {
        start <= i && matches!(tag, HighlightTag::String | HighlightTag::Comment)
    })
}

/// What a line's brackets (outside strings and comments) do to the nesting
/// depth: `depth -> max(depth, dip) + net` (closing brackets never take the
/// depth below zero, as in the editor's rainbow brackets).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Brackets {
    dip: u32,
    net: i32,
}

impl Brackets {
    fn of(text: &str, spans: &[(u32, u32, HighlightTag)]) -> Brackets {
        let (mut sum, mut min, mut k) = (0i32, 0i32, 0usize);
        for (i, b) in text.bytes().enumerate() {
            if !matches!(b, b'(' | b'[' | b'{' | b')' | b']' | b'}')
                || in_string_or_comment(spans, &mut k, i)
            {
                continue;
            }
            if matches!(b, b'(' | b'[' | b'{') {
                sum += 1;
            } else {
                sum -= 1;
                min = min.min(sum);
            }
        }
        Brackets {
            dip: min.unsigned_abs(),
            net: sum,
        }
    }

    fn apply(self, depth: usize) -> usize {
        (depth.max(self.dip as usize) as i64 + self.net as i64).max(0) as usize
    }
}

/// A tokenized line. Its text has not changed since (edited lines lose their
/// entry), so the spans hold as long as the line starts in `start` again.
struct LineEntry {
    start: Arc<StackState>,
    end: Arc<StackState>,
    spans: Vec<(u32, u32, HighlightTag)>,
    brackets: Brackets,
}

/// TextMate highlighting state of one editor.
pub struct TextMateHighlight {
    grammar: Arc<Grammar>,
    /// Language name shown in the status bar.
    name: String,
    /// The editor's language before the grammar was attached.
    base_language: Language,
    /// Document version `entries` belong to.
    version: Option<u64>,
    /// One entry per document line (`None`: not tokenized since it changed).
    entries: Vec<Option<LineEntry>>,
    /// Lines `..valid` are tokenized for the current text.
    valid: usize,
    /// The state line `valid` starts in.
    next: Arc<StackState>,
    initial: Arc<StackState>,
    /// Lines tokenized so far (re-tokenized lines count again).
    tokenized: u64,
    /// Set by the warm-up thread when it is done (`None`: no warm-up).
    warm: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for TextMateHighlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextMateHighlight")
            .field("scope", &self.grammar.scope_name())
            .field("name", &self.name)
            .field("lines", &self.entries.len())
            .field("valid", &self.valid)
            .finish()
    }
}

impl TextMateHighlight {
    pub fn new(grammar: Arc<Grammar>, name: String, base_language: Language) -> Self {
        let initial = Arc::new(StackState::default());
        Self {
            grammar,
            name,
            base_language,
            version: None,
            entries: Vec::new(),
            valid: 0,
            next: initial.clone(),
            initial,
            tokenized: 0,
            warm: None,
        }
    }

    /// Like [`Self::new`], and compiles the patterns the first lines of `doc`
    /// need on a background thread (see [`Self::warming_up`]).
    pub fn with_warm_up(
        grammar: Arc<Grammar>,
        name: String,
        base_language: Language,
        doc: &Document,
    ) -> Self {
        let lines: Vec<String> = (0..doc.line_count().min(WARM_UP_LINES))
            .map(|line| doc.line_text(line))
            .collect();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        let shared = grammar.clone();
        let spawned = std::thread::Builder::new()
            .name("textmate-warm-up".into())
            .spawn(move || {
                // Set the flag even if tokenizing panics, or the editor would wait
                // (and repaint) forever.
                struct Done(Arc<AtomicBool>);
                impl Drop for Done {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _done = Done(flag);
                let mut state = StackState::default();
                for line in &lines {
                    rustpy_textmate::tokenize_line(&shared, line, &mut state);
                }
            });
        let mut tm = Self::new(grammar, name, base_language);
        tm.warm = spawned.is_ok().then_some(done);
        tm
    }

    /// Whether the warm-up thread is still compiling the grammar's patterns.
    pub fn warming_up(&mut self) -> bool {
        match &self.warm {
            Some(done) if !done.load(Ordering::Acquire) => true,
            Some(_) => {
                self.warm = None;
                false
            }
            None => false,
        }
    }

    pub fn grammar(&self) -> &Arc<Grammar> {
        &self.grammar
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub fn base_language(&self) -> Language {
        self.base_language
    }

    /// Lines tokenized so far, re-tokenized ones counted again.
    pub fn tokenized_lines(&self) -> u64 {
        self.tokenized
    }

    /// Number of leading lines that are tokenized for the current text.
    pub fn up_to_date_lines(&self) -> usize {
        self.valid
    }

    /// Tokenizes the lines that are not up to date through `last_line`
    /// (clamped to the document). With a `deadline`, stops once it has passed
    /// (after at least one line). Returns whether every line through
    /// `last_line` is up to date.
    pub fn update(&mut self, doc: &Document, last_line: usize, deadline: Option<Instant>) -> bool {
        self.sync(doc);
        let end = last_line.saturating_add(1).min(self.entries.len());
        let mut line = self.valid;
        let mut state = self.next.clone();
        let mut tokenized_now = 0;
        while line < end {
            if let Some(entry) = &self.entries[line] {
                // An unchanged line starting in the same state as when it was
                // tokenized: its tokens still hold (the states have converged).
                if Arc::ptr_eq(&entry.start, &state) || *entry.start == *state {
                    state = entry.end.clone();
                    line += 1;
                    continue;
                }
            }
            if tokenized_now > 0 && deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            let text = doc.line_text(line);
            let mut after = (*state).clone();
            let tokens = rustpy_textmate::tokenize_line(&self.grammar, &text, &mut after);
            let spans = spans_of(&tokens);
            let brackets = Brackets::of(&text, &spans);
            // Most lines end in the state they started in: share it.
            let end_state = if after == *state {
                state.clone()
            } else {
                Arc::new(after)
            };
            self.entries[line] = Some(LineEntry {
                start: state,
                end: end_state.clone(),
                spans,
                brackets,
            });
            state = end_state;
            line += 1;
            tokenized_now += 1;
        }
        self.valid = line;
        self.next = state;
        self.tokenized += tokenized_now;
        line >= end
    }

    /// Highlight spans of lines `first..=last` that have been tokenized
    /// (lines below the up-to-date ones keep their previous tokens until they
    /// are tokenized again).
    pub fn spans(&self, doc: &Document, first: usize, last: usize) -> Vec<Span> {
        let mut out = Vec::new();
        let end = last.saturating_add(1).min(self.entries.len());
        for line in first.min(end)..end {
            let Some(entry) = &self.entries[line] else {
                continue;
            };
            let range = doc.line_range(line);
            let base = u32::from(range.start());
            let len = u32::from(range.len());
            for &(start, end, tag) in &entry.spans {
                let (start, end) = (start.min(len), end.min(len));
                if start < end {
                    out.push(Span {
                        range: TextRange::new(
                            TextSize::new(base + start),
                            TextSize::new(base + end),
                        ),
                        tag,
                    });
                }
            }
        }
        out
    }

    /// Tokenizes through `last_line` and returns the highlight spans of every
    /// up-to-date line.
    pub fn spans_through(&mut self, doc: &Document, last_line: usize) -> Vec<Span> {
        self.update(doc, last_line, None);
        match self.valid.checked_sub(1) {
            Some(last) => self.spans(doc, 0, last),
            None => Vec::new(),
        }
    }

    /// Tokenizes the whole document and returns its highlight spans.
    pub fn document_spans(&mut self, doc: &Document) -> Vec<Span> {
        self.spans_through(doc, usize::MAX)
    }

    /// Rainbow-bracket spans for lines `first..=last` (brackets outside
    /// strings and comments, colored by nesting depth from the top of the
    /// document).
    pub fn rainbow_overlay(&self, doc: &Document, first: usize, last: usize) -> Vec<Span> {
        let end = last.saturating_add(1).min(self.entries.len());
        let first = first.min(end);
        let mut depth = self.entries[..first]
            .iter()
            .flatten()
            .fold(0, |depth, entry| entry.brackets.apply(depth));
        let mut out = Vec::new();
        for line in first..end {
            let text = doc.line_text(line);
            let spans = self.entries[line]
                .as_ref()
                .map_or(&[][..], |entry| &entry.spans[..]);
            let base = u32::from(doc.line_range(line).start());
            let mut k = 0;
            for (i, b) in text.bytes().enumerate() {
                if !matches!(b, b'(' | b'[' | b'{' | b')' | b']' | b'}')
                    || in_string_or_comment(spans, &mut k, i)
                {
                    continue;
                }
                let tag = if matches!(b, b'(' | b'[' | b'{') {
                    depth += 1;
                    RAINBOW[(depth - 1) % RAINBOW.len()]
                } else {
                    depth = depth.saturating_sub(1);
                    RAINBOW[depth % RAINBOW.len()]
                };
                let at = base + i as u32;
                out.push(Span {
                    range: TextRange::new(TextSize::new(at), TextSize::new(at + 1)),
                    tag,
                });
            }
        }
        out
    }

    /// Brings the line entries to the document's current version: entries of
    /// changed lines are dropped and the others move with their lines.
    fn sync(&mut self, doc: &Document) {
        let version = doc.version();
        let lines = doc.line_count();
        if self.version == Some(version) && self.entries.len() == lines {
            return;
        }
        let damage = match self.version {
            Some(old) if old < version => doc
                .changes_since(old)
                .map(|steps| damaged_range(&steps.iter().map(Vec::as_slice).collect::<Vec<_>>())),
            _ => None,
        };
        let kept = match damage {
            // No text change (e.g. a new line separator).
            Some(None) => self.entries.len() == lines,
            Some(Some((lo, hi))) => self.splice(doc, lo, hi),
            // A new document, or more changes than the change log covers.
            None => false,
        };
        if !kept {
            self.reset(lines);
        }
        self.version = Some(version);
    }

    /// Drops the entries of the lines touched by the changed byte range
    /// `lo..hi` (current coordinates); returns false when the line counts do
    /// not add up (the caller starts over).
    fn splice(&mut self, doc: &Document, lo: usize, hi: usize) -> bool {
        let first = doc.line_of(TextSize::new(lo as u32));
        let last = doc.line_of(TextSize::new(hi as u32)).max(first);
        // Lines after the change moved by the difference in line counts.
        let delta = doc.line_count() as i64 - self.entries.len() as i64;
        let old_end = last as i64 + 1 - delta;
        if old_end <= first as i64 || old_end > self.entries.len() as i64 {
            return false;
        }
        self.entries
            .splice(first..old_end as usize, (first..=last).map(|_| None));
        if self.valid > first {
            self.valid = first;
            self.next = match first.checked_sub(1) {
                Some(prev) => match &self.entries[prev] {
                    Some(entry) => entry.end.clone(),
                    None => return false,
                },
                None => self.initial.clone(),
            };
        }
        self.entries.len() == doc.line_count()
    }

    fn reset(&mut self, lines: usize) {
        self.entries.clear();
        self.entries.resize_with(lines, || None);
        self.valid = 0;
        self.next = self.initial.clone();
    }
}

/// The byte range (in the latest coordinates) covering every change of the
/// edit steps (oldest first, each in the coordinates before it), or `None`
/// when they change nothing.
fn damaged_range(steps: &[&[Edit]]) -> Option<(usize, usize)> {
    let mut range: Option<(usize, usize)> = None;
    for edits in steps {
        if edits.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = range {
            let lo = super::state::map_offset(edits, TextSize::new(lo as u32), false);
            let hi = super::state::map_offset(edits, TextSize::new(hi as u32), true);
            range = Some((u32::from(lo) as usize, u32::from(hi) as usize));
        }
        // The edits are sorted and do not overlap; place each replacement.
        let mut delta: i64 = 0;
        for edit in edits.iter() {
            let start = (u32::from(edit.range.start()) as i64 + delta).max(0) as usize;
            let end = start + edit.text.len();
            delta += edit.text.len() as i64 - u32::from(edit.range.len()) as i64;
            range = Some(match range {
                Some((lo, hi)) => (lo.min(start), hi.max(end)),
                None => (start, end),
            });
        }
    }
    range
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{EditGroup, Selection};

    /// A small C-like language: keywords, numbers, `//` and `/* */` comments
    /// and strings (which may span lines).
    const GRAMMAR: &str = r#"{
        "scopeName": "source.demo",
        "name": "Demo",
        "fileTypes": ["demo"],
        "patterns": [
            { "match": "//.*$", "name": "comment.line.double-slash.demo" },
            { "begin": "/\\*", "end": "\\*/", "name": "comment.block.demo" },
            { "begin": "\"", "end": "\"", "name": "string.quoted.double.demo" },
            { "match": "\\b(fn)\\s+([a-z_]+)", "captures": { "1": { "name": "storage.type.demo" }, "2": { "name": "entity.name.function.demo" } } },
            { "match": "\\b(?:fn|let|if|else|return)\\b", "name": "keyword.control.demo" },
            { "match": "\\b[0-9]+\\b", "name": "constant.numeric.demo" }
        ]
    }"#;

    fn grammar() -> Arc<Grammar> {
        Arc::new(Grammar::from_json(GRAMMAR).unwrap())
    }

    fn highlight() -> TextMateHighlight {
        TextMateHighlight::new(grammar(), "Demo".into(), Language::Other)
    }

    fn ts(n: usize) -> TextSize {
        TextSize::new(n as u32)
    }

    fn edit(doc: &mut Document, start: usize, end: usize, text: &str) {
        doc.edit(
            vec![Edit::replace(TextRange::new(ts(start), ts(end)), text)],
            &[],
            &[Selection::caret(ts(start))],
            EditGroup::Separate,
            None,
        )
        .unwrap();
    }

    /// `(text, tag)` of every span.
    fn tagged(doc: &Document, spans: &[Span]) -> Vec<(String, HighlightTag)> {
        spans.iter().map(|s| (doc.slice(s.range), s.tag)).collect()
    }

    /// The spans a fresh highlighter computes for the whole document.
    fn fresh_spans(doc: &Document) -> Vec<Span> {
        highlight().document_spans(doc)
    }

    /// The view's spans for `first..=last` once highlighting has finished
    /// (each call is one frame with its time budget).
    fn view_frames(state: &mut crate::editor::EditorState, first: usize, last: usize) -> Vec<Span> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            let (spans, pending) = state.view_highlight_spans(first, last);
            if !pending {
                return spans;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("highlighting does not finish");
    }

    #[test]
    fn every_textmate_tag_name_maps_to_the_highlight_tag_of_that_name() {
        for name in rustpy_textmate::TAG_NAMES {
            let tag = tag_from_name(name).unwrap_or_else(|| panic!("{name} has no highlight tag"));
            assert_eq!(format!("{tag:?}"), name);
        }
        assert_eq!(tag_from_name("Punctuation"), None);
        assert_eq!(tag_from_name(""), None);
    }

    #[test]
    fn scopes_map_to_highlight_tags() {
        let doc = Document::new("fn main() { return 42; } // done\nlet s = \"hi\"\n");
        let spans = highlight().document_spans(&doc);
        assert_eq!(
            tagged(&doc, &spans),
            vec![
                ("fn".into(), HighlightTag::Keyword),
                ("main".into(), HighlightTag::FunctionDefinition),
                ("return".into(), HighlightTag::Keyword),
                ("42".into(), HighlightTag::Number),
                ("// done".into(), HighlightTag::Comment),
                ("let".into(), HighlightTag::Keyword),
                // The quotes are punctuation inside the string scope: one span.
                ("\"hi\"".into(), HighlightTag::String),
            ]
        );
    }

    #[test]
    fn state_carries_across_lines() {
        let doc = Document::new("let a /* start\nstill comment if 1\nend */ if 2\n");
        let spans = highlight().document_spans(&doc);
        assert_eq!(
            tagged(&doc, &spans),
            vec![
                ("let".into(), HighlightTag::Keyword),
                ("/* start".into(), HighlightTag::Comment),
                ("still comment if 1".into(), HighlightTag::Comment),
                ("end */".into(), HighlightTag::Comment),
                ("if".into(), HighlightTag::Keyword),
                ("2".into(), HighlightTag::Number),
            ]
        );
    }

    #[test]
    fn tokenizes_lazily_up_to_the_requested_line() {
        let text: String = (0..1000).map(|i| format!("let x{i} = {i}\n")).collect();
        let doc = Document::new(&text);
        let mut tm = highlight();
        assert!(tm.update(&doc, 39, None));
        assert_eq!(tm.up_to_date_lines(), 40);
        assert_eq!(tm.tokenized_lines(), 40);
        // Spans exist for the tokenized lines only.
        assert_eq!(tm.spans(&doc, 0, 999).len(), 80);
        assert!(tm.spans(&doc, 40, 999).is_empty());
        // Asking again costs nothing; asking further continues from line 40.
        assert!(tm.update(&doc, 39, None));
        assert!(tm.update(&doc, 59, None));
        assert_eq!(tm.tokenized_lines(), 60);
    }

    #[test]
    fn deadline_stops_after_one_line_and_work_resumes() {
        let text: String = (0..100).map(|i| format!("let x = {i}\n")).collect();
        let doc = Document::new(&text);
        let mut tm = highlight();
        let past = Instant::now();
        assert!(!tm.update(&doc, 99, Some(past)));
        assert_eq!(tm.up_to_date_lines(), 1);
        assert!(!tm.update(&doc, 99, Some(past)));
        assert_eq!(tm.up_to_date_lines(), 2);
        assert!(tm.update(&doc, 99, None));
        assert_eq!(tm.tokenized_lines(), 100);
    }

    #[test]
    fn typing_in_a_line_retokenizes_only_that_line() {
        let text: String = (0..500)
            .map(|i| format!("let x{i} = {i} // note\n"))
            .collect();
        let mut doc = Document::new(&text);
        let mut tm = highlight();
        tm.update(&doc, 499, None);
        assert_eq!(tm.tokenized_lines(), 500);
        // Type "if " at the start of line 200, one character at a time.
        let at = usize::from(doc.line_range(200).start());
        for (i, c) in "if ".chars().enumerate() {
            edit(&mut doc, at + i, at + i, &c.to_string());
            tm.update(&doc, 499, None);
        }
        assert_eq!(tm.tokenized_lines(), 503);
        assert_eq!(tm.spans(&doc, 0, 499), fresh_spans(&doc));
        assert_eq!(
            tagged(&doc, &tm.spans(&doc, 200, 200))[0],
            ("if".into(), HighlightTag::Keyword)
        );
    }

    #[test]
    fn opening_a_block_comment_retokenizes_below_and_closing_it_converges() {
        let text: String = (0..300).map(|i| format!("let x = {i}\n")).collect();
        let mut doc = Document::new(&text);
        let mut tm = highlight();
        tm.update(&doc, 299, None);
        let at = usize::from(doc.line_range(100).start());
        edit(&mut doc, at, at, "/*");
        tm.update(&doc, 299, None);
        // Every line from 100 on is now inside the comment.
        assert_eq!(tm.tokenized_lines(), 300 + 200);
        assert_eq!(tm.spans(&doc, 0, 299), fresh_spans(&doc));
        assert!(tagged(&doc, &tm.spans(&doc, 250, 250))
            .iter()
            .all(|(_, tag)| *tag == HighlightTag::Comment));
        // Closing it at the end of line 101: the lines below start outside a
        // comment again, unlike their cached entries (tokenized inside the
        // comment), so they are tokenized once more.
        let at = usize::from(doc.line_range(101).end());
        edit(&mut doc, at, at, " */");
        tm.update(&doc, 299, None);
        assert_eq!(tm.tokenized_lines(), 500 + 199);
        assert_eq!(tm.spans(&doc, 0, 299), fresh_spans(&doc));
        assert_eq!(
            tagged(&doc, &tm.spans(&doc, 250, 250))[0],
            ("let".into(), HighlightTag::Keyword)
        );
        let before = tm.tokenized_lines();
        // Now an edit inside the closed comment converges right away.
        let at = usize::from(doc.line_range(100).start()) + 2;
        edit(&mut doc, at, at, " more ");
        tm.update(&doc, 299, None);
        assert_eq!(tm.tokenized_lines(), before + 1);
        assert_eq!(tm.spans(&doc, 0, 299), fresh_spans(&doc));
    }

    #[test]
    fn inserting_and_deleting_lines_moves_cached_lines() {
        let text: String = (0..200)
            .map(|i| {
                if i % 3 == 0 {
                    format!("\"str {i}\" // c\n")
                } else {
                    format!("fn f{i} {i}\n")
                }
            })
            .collect();
        let mut doc = Document::new(&text);
        let mut tm = highlight();
        tm.update(&doc, 199, None);
        // Insert three lines in the middle of line 50.
        let at = usize::from(doc.line_range(50).start()) + 3;
        edit(&mut doc, at, at, "x\nlet y = 1\nz\n");
        tm.update(&doc, 300, None);
        assert_eq!(doc.line_count(), 204);
        assert_eq!(tm.spans(&doc, 0, 300), fresh_spans(&doc));
        assert!(tm.tokenized_lines() <= 200 + 5, "{}", tm.tokenized_lines());
        // Join lines 10..=20 into one.
        let (start, end) = (
            usize::from(doc.line_range(10).end()),
            usize::from(doc.line_range(20).start()),
        );
        edit(&mut doc, start, end, " ");
        let before = tm.tokenized_lines();
        tm.update(&doc, 300, None);
        assert_eq!(tm.spans(&doc, 0, 300), fresh_spans(&doc));
        assert!(
            tm.tokenized_lines() <= before + 2,
            "{}",
            tm.tokenized_lines() - before
        );
    }

    #[test]
    fn edits_below_the_tokenized_lines_keep_them() {
        let text: String = (0..1000).map(|i| format!("let x = {i}\n")).collect();
        let mut doc = Document::new(&text);
        let mut tm = highlight();
        tm.update(&doc, 99, None);
        let at = usize::from(doc.line_range(900).start());
        edit(&mut doc, at, at, "/* open\n");
        tm.update(&doc, 99, None);
        assert_eq!(tm.tokenized_lines(), 100);
        assert_eq!(tm.up_to_date_lines(), 100);
    }

    #[test]
    fn many_steps_undo_redo_and_reload_stay_consistent() {
        let text: String = (0..120)
            .map(|i| format!("fn f{i} {{ return {i} }} /* c{i}\n */\n"))
            .collect();
        let mut doc = Document::new(&text);
        let mut tm = highlight();
        tm.update(&doc, usize::MAX, None);
        // A pseudo-random mix of edits, each followed by an update of part
        // of the document; the result always matches a fresh tokenization.
        let mut seed = 7u64;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n.max(1)
        };
        let pieces = ["\"", "/*", "*/", "\n", "if 1", "x", "\n\n", "// c", ""];
        for round in 0..150 {
            let len = usize::from(doc.len());
            let start = next(len + 1);
            let end = (start + next(12)).min(len);
            let (start, end) = (
                usize::from(doc.clamp(ts(start))),
                usize::from(doc.clamp(ts(end))),
            );
            // Stay on character boundaries (the text is ASCII).
            edit(&mut doc, start, end, pieces[next(pieces.len())]);
            if round % 5 == 0 {
                doc.undo();
            }
            if round % 7 == 0 {
                doc.redo();
            }
            // Sometimes several versions pass between updates.
            if round % 3 != 0 {
                let last = next(doc.line_count());
                tm.update(&doc, last, None);
                assert_eq!(
                    tm.spans(&doc, 0, last),
                    fresh_spans(&doc)
                        .into_iter()
                        .filter(|s| doc.line_of(s.range.start()) <= last)
                        .collect::<Vec<_>>(),
                    "round {round}"
                );
            }
        }
        // More than the change log covers between two updates: starts over.
        tm.update(&doc, usize::MAX, None);
        for i in 0..100 {
            edit(&mut doc, 0, 0, if i % 2 == 0 { "\"" } else { "x" });
        }
        assert!(doc.changes_since(doc.version() - 100).is_none());
        assert_eq!(tm.document_spans(&doc), fresh_spans(&doc));
        // Reloading from disk replaces the text.
        doc.reset("let reloaded = 1\n/* x\n");
        assert_eq!(tm.document_spans(&doc), fresh_spans(&doc));
        assert_eq!(
            tagged(&doc, &tm.document_spans(&doc)),
            vec![
                ("let".into(), HighlightTag::Keyword),
                ("1".into(), HighlightTag::Number),
                ("/* x".into(), HighlightTag::Comment)
            ]
        );
    }

    #[test]
    fn reloading_an_unedited_file_refreshes_its_highlighting() {
        // Caches keyed by the document version (the TextMate lines, the
        // built-in highlighting) must see a reload from disk, also of a file
        // never edited (version 0).
        let mut state =
            crate::editor::EditorState::new(Document::new("x = 1\n"), Some("/p/a.py".into()));
        assert_eq!(
            state
                .highlight_spans()
                .iter()
                .map(|s| s.tag)
                .collect::<Vec<_>>(),
            vec![HighlightTag::Operator, HighlightTag::Number]
        );
        state.doc.reset("def f(): pass\n");
        assert_eq!(state.highlight_spans()[0].tag, HighlightTag::Keyword);
        let mut state =
            crate::editor::EditorState::new(Document::new("let a = 1\n"), Some("/p/a.demo".into()));
        state.set_textmate_grammar(Some((grammar(), "Demo".into())));
        let spans = state.highlight_spans().to_vec();
        assert_eq!(
            tagged(&state.doc, &spans)[0],
            ("let".into(), HighlightTag::Keyword)
        );
        state.doc.reset("// gone\n");
        let spans = state.highlight_spans().to_vec();
        assert_eq!(
            tagged(&state.doc, &spans),
            vec![("// gone".into(), HighlightTag::Comment)]
        );
    }

    #[test]
    fn line_separator_change_keeps_the_tokens() {
        let mut doc = Document::new("let a = 1\nlet b = 2\n");
        // (The change log of a never edited document cannot tell that a new
        // version changed nothing; that only costs a full re-tokenization.)
        edit(&mut doc, 0, 0, "x");
        let mut tm = highlight();
        tm.update(&doc, usize::MAX, None);
        doc.set_line_ending(rustpy_text::LineEnding::CrLf);
        tm.update(&doc, usize::MAX, None);
        assert_eq!(tm.tokenized_lines(), 3);
    }

    #[test]
    fn editor_spans_cover_the_furthest_caret_and_the_view_its_lines() {
        use crate::editor::{Caret, EditorState};
        let text: String = (0..300).map(|i| format!("let x = {i}\n")).collect();
        let mut state = EditorState::new(Document::new(&text), Some("/p/a.demo".into()));
        state.set_textmate_grammar(Some((grammar(), "Demo".into())));
        // With the caret on the first line little is tokenized...
        assert_eq!(state.highlight_spans().len(), 2);
        // ...and a selection down to line 200 (Copy as Rich Text) covers it.
        let end = state.doc.line_range(200).end();
        state.set_carets(vec![Caret::new(TextSize::new(0), end)]);
        assert_eq!(state.highlight_spans().len(), 2 * 201);
        // The view gets the spans of its lines only, once the frames' time
        // budgets have tokenized down to them.
        let spans = view_frames(&mut state, 250, 260);
        assert_eq!(spans.len(), 2 * 11);
        assert_eq!(
            tagged(&state.doc, &spans)[0],
            ("let".into(), HighlightTag::Keyword)
        );
        assert_eq!(state.doc.line_of(spans[0].range.start()), 250);
    }

    #[test]
    fn view_spans_carry_rainbow_brackets_outside_strings() {
        let mut state = crate::editor::EditorState::new(
            Document::new("fn f(a) {\n  \"(\"\n}\n"),
            Some("/p/a.demo".into()),
        );
        state.set_textmate_grammar(Some((grammar(), "Demo".into())));
        state.rainbow_brackets = true;
        let spans = view_frames(&mut state, 1, 3);
        // The `}` closes the `{` of line 0: depth carried from above the view.
        assert_eq!(
            tagged(&state.doc, &spans),
            vec![
                ("\"(\"".into(), HighlightTag::String),
                ("}".into(), HighlightTag::Rainbow1)
            ]
        );
        let spans = view_frames(&mut state, 0, 3);
        assert_eq!(
            tagged(&state.doc, &spans),
            vec![
                ("fn".into(), HighlightTag::Keyword),
                ("f".into(), HighlightTag::FunctionDefinition),
                ("(".into(), HighlightTag::Rainbow1),
                (")".into(), HighlightTag::Rainbow1),
                ("{".into(), HighlightTag::Rainbow1),
                ("\"(\"".into(), HighlightTag::String),
                ("}".into(), HighlightTag::Rainbow1),
            ]
        );
    }

    #[test]
    fn the_view_waits_for_the_warm_up_thread() {
        let text: String = (0..300)
            .map(|i| format!("fn f{i} {{ return {i} }}\n"))
            .collect();
        let doc = Document::new(&text);
        let mut tm =
            TextMateHighlight::with_warm_up(grammar(), "Demo".into(), Language::Other, &doc);
        let start = Instant::now();
        while tm.warming_up() {
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(tm.warm.is_none());
        // The warm-up compiled patterns only; the editor's lines are its own.
        assert_eq!(tm.tokenized_lines(), 0);
        assert_eq!(tm.document_spans(&doc), fresh_spans(&doc));
        // An empty document warms up too.
        let mut empty = TextMateHighlight::with_warm_up(
            grammar(),
            "Demo".into(),
            Language::Other,
            &Document::new(""),
        );
        while empty.warming_up() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn damaged_range_covers_every_step() {
        let r = |a: usize, b: usize| TextRange::new(ts(a), ts(b));
        assert_eq!(damaged_range(&[]), None);
        let step1 = [Edit::insert(ts(10), "abc")];
        assert_eq!(damaged_range(&[&step1]), Some((10, 13)));
        // A later step before the first change shifts it.
        let step2 = [Edit::replace(r(2, 4), "")];
        assert_eq!(damaged_range(&[&step1, &step2]), Some((2, 11)));
        // Several edits in one step, in pre-step coordinates.
        let step = [
            Edit::replace(r(1, 2), "xx"),
            Edit::delete(r(5, 8)),
            Edit::insert(ts(20), "y"),
        ];
        assert_eq!(damaged_range(&[&step]), Some((1, 19)));
        // A deletion is an empty range at its place.
        let del = [Edit::delete(r(3, 9))];
        assert_eq!(damaged_range(&[&del]), Some((3, 3)));
    }

    #[test]
    fn bracket_effects_compose_like_the_editor_rainbow() {
        let none: Vec<(u32, u32, HighlightTag)> = Vec::new();
        assert_eq!(Brackets::of("f(a[1])", &none), Brackets { dip: 0, net: 0 });
        assert_eq!(Brackets::of(")) ((( ", &none), Brackets { dip: 2, net: 1 });
        assert_eq!(Brackets::of(")) ((( ", &none).apply(0), 3);
        assert_eq!(Brackets::of(")) ((( ", &none).apply(5), 6);
        // Brackets in strings and comments do not count.
        let spans = vec![(2, 5, HighlightTag::String), (6, 9, HighlightTag::Comment)];
        assert_eq!(
            Brackets::of("( \")\" /*(*/", &spans),
            Brackets { dip: 0, net: 1 }
        );
    }

    #[test]
    fn rainbow_overlay_continues_the_depth_of_the_lines_above() {
        let doc = Document::new("fn f(a, \"(\") {\n  g([1])\n}\n");
        let mut tm = highlight();
        tm.update(&doc, usize::MAX, None);
        let overlay = tm.rainbow_overlay(&doc, 1, 2);
        let got: Vec<(String, HighlightTag)> = tagged(&doc, &overlay);
        assert_eq!(
            got,
            vec![
                ("(".into(), HighlightTag::Rainbow2),
                ("[".into(), HighlightTag::Rainbow3),
                ("]".into(), HighlightTag::Rainbow3),
                (")".into(), HighlightTag::Rainbow2),
                ("}".into(), HighlightTag::Rainbow1),
            ]
        );
    }
}

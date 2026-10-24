//! Finding typos: [`check_text`] for Python source and [`check_plain_text`]
//! for Markdown documents, commit messages and other prose.

use std::collections::HashMap;
use std::ops::Range;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::SyntaxKind::*;
use serde::{Deserialize, Serialize};

use crate::decl::declared_identifiers;
use crate::dictionary::{recase, Dictionary};
use crate::split::{
    contraction, find_apostrophe, is_acronym, looks_like_hex, split_identifier_ranges,
    touches_digit, Contraction,
};
use crate::vocab;

/// Number of suggestions attached to each [`Typo`].
pub const DEFAULT_MAX_SUGGESTIONS: usize = 5;

/// What [`check_text`] looks at.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckOptions {
    /// Check identifiers where they are declared (`def`/`class` names,
    /// parameters, assignment targets, …), split into words.
    pub check_identifiers: bool,
    /// Check comments.
    pub check_comments: bool,
    /// Check string literals, docstrings and the literal parts of f-strings.
    pub check_strings: bool,
    /// Words (or identifier parts) with fewer characters are never reported.
    pub min_word_len: usize,
}

impl Default for CheckOptions {
    /// Everything checked; words of four or more characters (words of up to
    /// three letters are ignored).
    fn default() -> Self {
        Self {
            check_identifiers: true,
            check_comments: true,
            check_strings: true,
            min_word_len: 4,
        }
    }
}

/// Where a [`Typo`] was found.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypoKind {
    /// A part of a declared identifier: fix it by renaming.
    Identifier,
    Comment,
    /// A string literal (docstrings and f-string text included).
    String,
    /// Text checked with [`check_plain_text`].
    PlainText,
}

/// A misspelled word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typo {
    /// Byte range of `word` in the checked text.
    pub range: TextRange,
    /// The word as written (for identifiers, one part: `recieve` in
    /// `recieve_data`).
    pub word: String,
    /// Up to [`DEFAULT_MAX_SUGGESTIONS`] corrections, best first, cased like
    /// `word`.
    pub suggestions: Vec<String>,
    pub kind: TypoKind,
}

/// Finds the typos in Python source `text`: in comments, string literals
/// (docstrings and f-string text included) and declared identifiers, as
/// selected by `opts`. Identifiers are split into words
/// ([`crate::split_identifier`]) and only reported where they are declared,
/// so names of APIs the file merely uses never are.
///
/// Never reported: words shorter than `opts.min_word_len`; URLs, e-mail
/// addresses and file paths; dotted names (`os.path.join`, `setup.py`),
/// command-line options (`-Wall`, `--dry-run`), mentions and decorators
/// (`@property`); words touching digits (`utf8`, `x86`) and hexadecimal
/// strings (`deadbeef`, `#c0ffee`); ALL-CAPS acronyms (`HTTP`, `URLs`,
/// `HAVE_SSL_H`; only the words of `CONSTANT_CASE` names declared in the file
/// are checked); Python keywords, builtins and dunder names; programming
/// jargon (`kwargs`, `repr`, `json`, …); words with non-ASCII letters; text in
/// backticks, HTML tags and doctest lines; tool directives such as
/// `# type: ignore` or `# noqa`.
pub fn check_text(text: &str, dict: &Dictionary, opts: CheckOptions) -> Vec<Typo> {
    let (tokens, _) = rustpy_syntax::tokenize(text);
    let declared = if opts.check_identifiers {
        declared_identifiers(&tokens, text)
    } else {
        Vec::new()
    };
    let mut checker = Checker::new(dict, opts.min_word_len);
    // Open f-/t-strings and replacement fields, to tell literal text from
    // format specs (`f"{value:>10}"`).
    let mut frames: Vec<Frame> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let start = usize::from(token.range.start());
        let token_text = &text[start..usize::from(token.range.end())];
        match token.kind {
            COMMENT if opts.check_comments => {
                if let Some(content) = comment_prose(text, start, token_text) {
                    checker.check_prose(content, start + 1, Prose::Comment, TypoKind::Comment);
                }
            }
            STRING if opts.check_strings => {
                let (content, raw) = string_content(token_text);
                let mode = if raw { Prose::RawString } else { Prose::String };
                checker.check_prose(
                    &token_text[content.clone()],
                    start + content.start,
                    mode,
                    TypoKind::String,
                );
            }
            IDENT if opts.check_identifiers && declared[index] => {
                checker.check_identifier(token_text, start, TypoKind::Identifier, false);
            }
            FSTRING_START | TSTRING_START => frames.push(Frame::String {
                raw: token_text.bytes().any(|b| matches!(b, b'r' | b'R')),
            }),
            FSTRING_END | TSTRING_END => {
                while let Some(frame) = frames.pop() {
                    if matches!(frame, Frame::String { .. }) {
                        break;
                    }
                }
            }
            FSTRING_MIDDLE | TSTRING_MIDDLE => {
                let in_spec = matches!(frames.last(), Some(Frame::Field { spec: true, .. }));
                if opts.check_strings && !in_spec {
                    let raw = frames.iter().rev().find_map(|f| match f {
                        Frame::String { raw } => Some(*raw),
                        Frame::Field { .. } => None,
                    });
                    let mode = if raw.unwrap_or(false) {
                        Prose::RawString
                    } else {
                        Prose::String
                    };
                    checker.check_prose(token_text, start, mode, TypoKind::String);
                }
            }
            L_BRACE => {
                let opens_field = matches!(
                    frames.last(),
                    Some(Frame::String { .. } | Frame::Field { spec: true, .. })
                );
                if opens_field {
                    frames.push(Frame::Field {
                        depth: 0,
                        spec: false,
                    });
                } else if let Some(Frame::Field { depth, .. }) = frames.last_mut() {
                    *depth += 1;
                }
            }
            R_BRACE => {
                if matches!(frames.last(), Some(Frame::Field { depth: 0, .. })) {
                    frames.pop();
                } else if let Some(Frame::Field { depth, .. }) = frames.last_mut() {
                    *depth -= 1;
                }
            }
            L_PAREN | L_BRACK => {
                if let Some(Frame::Field { depth, .. }) = frames.last_mut() {
                    *depth += 1;
                }
            }
            R_PAREN | R_BRACK => {
                if let Some(Frame::Field { depth, .. }) = frames.last_mut() {
                    *depth = depth.saturating_sub(1);
                }
            }
            COLON => {
                if let Some(Frame::Field { depth: 0, spec }) = frames.last_mut() {
                    *spec = true;
                }
            }
            // F-strings never contain a logical newline; recover from
            // unterminated ones.
            NEWLINE => frames.clear(),
            _ => {}
        }
    }
    checker.finish()
}

/// Finds the typos in prose such as a Markdown document or a commit message.
/// Fenced and indented code blocks, inline code, link destinations, HTML
/// tags and `Signed-off-by:`-style trailers are skipped; the word rules
/// (URLs, paths, dotted names, options, acronyms, jargon, …) are those of
/// [`check_text`] with the default minimum length.
pub fn check_plain_text(text: &str, dict: &Dictionary) -> Vec<Typo> {
    let mut checker = Checker::new(dict, CheckOptions::default().min_word_len);
    checker.check_prose(text, 0, Prose::PlainText, TypoKind::PlainText);
    checker.finish()
}

#[derive(Copy, Clone, Debug)]
enum Frame {
    /// An f-/t-string; `raw` for `rf"..."`.
    String { raw: bool },
    /// A replacement field: bracket depth inside it, and whether its format
    /// spec has started.
    Field { depth: usize, spec: bool },
}

/// The text of a comment to check (after the `#`), or `None` for a shebang,
/// an encoding declaration or a tool directive. A trailing directive
/// (`# text  # noqa: E501`) is cut off.
fn comment_prose<'t>(text: &str, start: usize, comment: &'t str) -> Option<&'t str> {
    let content = comment.strip_prefix('#')?;
    let shebang = start == 0 && content.starts_with('!');
    let encoding = (content.contains("coding:") || content.contains("coding="))
        && text[..start]
            .bytes()
            .filter(|&b| b == b'\n')
            .take(2)
            .count()
            < 2;
    if shebang || encoding || vocab::is_directive(content) {
        return None;
    }
    let end = content
        .match_indices('#')
        .map(|(i, _)| i)
        .find(|&i| vocab::is_directive(&content[i + 1..]));
    Some(&content[..end.unwrap_or(content.len())])
}

/// Byte range of the contents of a string literal token (without prefix and
/// quotes) and whether it is raw.
fn string_content(token: &str) -> (Range<usize>, bool) {
    let b = token.as_bytes();
    let prefix = b.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    let raw = b[..prefix].iter().any(|c| matches!(c, b'r' | b'R'));
    let Some(&quote) = b.get(prefix) else {
        return (prefix..prefix, raw);
    };
    let triple = b.len() >= prefix + 3 && b[prefix + 1] == quote && b[prefix + 2] == quote;
    let quotes = if triple { 3 } else { 1 };
    let start = prefix + quotes;
    let mut end = b.len();
    if end >= start + quotes && b[end - quotes..].iter().all(|&c| c == quote) {
        end -= quotes;
    }
    (start..end.max(start), raw)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Prose {
    Comment,
    String,
    RawString,
    PlainText,
}

struct Checker<'a> {
    dict: &'a Dictionary,
    min_len: usize,
    typos: Vec<Typo>,
    /// Lowercase misspelling → lowercase suggestions.
    suggestions: HashMap<String, Vec<String>>,
}

impl<'a> Checker<'a> {
    fn new(dict: &'a Dictionary, min_len: usize) -> Self {
        Self {
            dict,
            min_len,
            typos: Vec::new(),
            suggestions: HashMap::new(),
        }
    }

    fn finish(mut self) -> Vec<Typo> {
        self.typos.sort_by_key(|t| t.range.start());
        self.typos
    }

    fn known(&self, lower: &str) -> bool {
        vocab::is_known_word(lower) || self.dict.contains_lower(lower)
    }

    fn report(&mut self, start: usize, word: &str, kind: TypoKind) {
        let dict = self.dict;
        let lowercase = self
            .suggestions
            .entry(word.to_lowercase())
            .or_insert_with_key(|lower| dict.suggest_lower(lower, DEFAULT_MAX_SUGGESTIONS));
        let suggestions = lowercase.iter().map(|s| recase(word, s.clone())).collect();
        let range = TextRange::new(
            TextSize::new(start as u32),
            TextSize::new((start + word.len()) as u32),
        );
        self.typos.push(Typo {
            range,
            word: word.to_string(),
            suggestions,
            kind,
        });
    }

    /// Checks the words of an identifier, or of an identifier-like run of
    /// prose, that starts at byte `start`. All-caps parts are acronyms, except
    /// in a declared `CONSTANT_CASE` name, whose words are all checked
    /// (`MAX_RECIEVE_SIZE = 1`). In prose, all-caps runs are skipped: they are
    /// acronyms or references to constants, macros and environment variables
    /// that are declared elsewhere.
    fn check_identifier(&mut self, ident: &str, start: usize, kind: TypoKind, prose: bool) {
        if vocab::is_dunder(ident) || vocab::is_python_name(ident) {
            return;
        }
        let parts = split_identifier_ranges(ident);
        let Some(first) = parts.first() else { return };
        let compound = parts.len() > 1 || first.len() != ident.len();
        if compound && self.known(&ident.to_lowercase()) {
            return;
        }
        let constant = !prose && parts.iter().all(|r| is_acronym(&ident[r.clone()]));
        for range in parts {
            let word = &ident[range.clone()];
            let skip = word.chars().count() < self.min_len
                || (!constant && is_acronym(word))
                || touches_digit(ident, &range)
                || !word.is_ascii()
                || looks_like_hex(word)
                || self.known(&word.to_ascii_lowercase());
            if !skip {
                self.report(start + range.start, word, kind);
            }
        }
    }

    /// Checks one word run of prose (letters, digits, `_` and inner
    /// apostrophes) that starts at byte `start`. Apostrophes are taken one at
    /// a time, left to right (in a loop: `a'a'a'…` must not exhaust the stack).
    fn check_run(&mut self, mut run: &str, mut start: usize, kind: TypoKind) {
        while let Some((at, len)) = find_apostrophe(run) {
            let (head, tail) = (&run[..at], &run[at + len..]);
            match contraction(head, tail) {
                Contraction::Suffix => return self.check_identifier(head, start, kind, true),
                Contraction::Negation => {
                    if !self.dict.negation_known(head) {
                        self.check_identifier(head, start, kind, true);
                    }
                    return;
                }
                Contraction::Other => self.check_identifier(head, start, kind, true),
            }
            run = tail;
            start += at + len;
        }
        self.check_identifier(run, start, kind, true);
    }

    /// Checks prose that starts at byte `base` of the checked text.
    fn check_prose(&mut self, text: &str, base: usize, mode: Prose, kind: TypoKind) {
        let mut bytes = text.as_bytes().to_vec();
        match mode {
            Prose::Comment => {}
            Prose::String => {
                mask_escapes(&mut bytes);
                mask_doctest(&mut bytes);
            }
            Prose::RawString => mask_doctest(&mut bytes),
            Prose::PlainText => mask_markdown(&mut bytes),
        }
        mask_code_spans(&mut bytes);
        mask_markup(&mut bytes);
        // Masking only overwrites whole characters with spaces.
        let masked = String::from_utf8(bytes).expect("masking keeps UTF-8 valid");
        let mut chunk_start = None;
        for (i, c) in masked.char_indices().chain([(masked.len(), ' ')]) {
            if !c.is_whitespace() {
                chunk_start.get_or_insert(i);
            } else if let Some(s) = chunk_start.take() {
                if !skip_chunk(&masked[s..i]) {
                    self.check_chunk(&masked[s..i], base + s, kind);
                }
            }
        }
    }

    /// Checks the word runs of a whitespace-delimited chunk of prose.
    fn check_chunk(&mut self, chunk: &str, start: usize, kind: TypoKind) {
        let chars: Vec<(usize, char)> = chunk.char_indices().collect();
        let mut run: Option<usize> = None;
        for k in 0..=chars.len() {
            let in_run = chars.get(k).is_some_and(|&(_, c)| {
                c.is_alphanumeric()
                    || c == '_'
                    || (matches!(c, '\'' | '’')
                        && run.is_some()
                        && chars[k - 1].1.is_alphabetic()
                        && chars.get(k + 1).is_some_and(|n| n.1.is_alphabetic()))
            });
            if in_run {
                run.get_or_insert(k);
                continue;
            }
            let Some(first) = run.take() else { continue };
            let from = chars[first].0;
            let to = chars.get(k).map_or(chunk.len(), |c| c.0);
            let word = &chunk[from..to];
            let colour = first > 0
                && chars[first - 1].1 == '#'
                && matches!(word.len(), 3 | 4 | 6 | 8)
                && word.bytes().all(|b| b.is_ascii_hexdigit());
            if !colour {
                self.check_run(word, start + from, kind);
            }
        }
    }
}

/// True for chunks that are not prose: URLs, e-mail addresses, file paths,
/// dotted names (`os.path.join`, `setup.py`, `.gitignore`), command-line
/// options (`-Wall`, `--dry-run`), and mentions or decorators (`@user`,
/// `@property`).
fn skip_chunk(chunk: &str) -> bool {
    let t = chunk
        .trim_start_matches(['(', '[', '{', '<', '"', '\'', '`', '*'])
        .trim_end_matches([
            ')', ']', '}', '>', '"', '\'', '`', '*', ',', ';', '!', '?', '.', ':',
        ]);
    if t.contains("://") {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("www.") || lower.starts_with("mailto:") {
        return true;
    }
    if t.find('@')
        .is_some_and(|at| at > 0 && t[at + 1..].contains('.'))
    {
        return true;
    }
    let b = t.as_bytes();
    let drive =
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    if drive
        || t.contains('\\')
        || t.starts_with(['/', '~'])
        || t.starts_with("./")
        || t.starts_with("../")
        || t.matches('/').count() >= 2
    {
        return true;
    }
    let word_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let dotted = b
        .windows(3)
        .any(|w| word_char(w[0]) && w[1] == b'.' && word_char(w[2]));
    let followed_by = |prefix: &str, ok: fn(u8) -> bool| {
        t.strip_prefix(prefix)
            .and_then(|rest| rest.bytes().next())
            .is_some_and(ok)
    };
    let option = followed_by("-", |c| c.is_ascii_alphabetic())
        || followed_by("--", |c| c.is_ascii_alphabetic());
    let mention = followed_by("@", |c| c.is_ascii_alphabetic() || c == b'_');
    let extension = followed_by(".", |c| c.is_ascii_alphanumeric());
    dotted || option || mention || extension
}

/// Overwrites `range` with spaces; offsets stay valid.
fn blank(bytes: &mut [u8], range: Range<usize>) {
    bytes[range].fill(b' ');
}

/// Blanks the escape sequences of a (non-raw) Python string literal, except
/// `\\`, which stays a backslash (so that Windows paths are recognised).
fn mask_escapes(b: &mut [u8]) {
    let hex = |b: &[u8], max: usize| {
        b.iter()
            .take(max)
            .take_while(|c| c.is_ascii_hexdigit())
            .count()
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let Some(&next) = b.get(i + 1) else { break };
        let end = match next {
            b'\\' => {
                i += 2;
                continue;
            }
            b'N' if b.get(i + 2) == Some(&b'{') => {
                let close = b[i + 3..]
                    .iter()
                    .position(|&c| c == b'}' || c == b'\n')
                    .map(|p| i + 3 + p);
                match close {
                    Some(c) if b[c] == b'}' => c + 1,
                    _ => i + 2,
                }
            }
            b'x' => i + 2 + hex(&b[i + 2..], 2),
            b'u' => i + 2 + hex(&b[i + 2..], 4),
            b'U' => i + 2 + hex(&b[i + 2..], 8),
            b'0'..=b'7' => {
                i + 1
                    + b[i + 1..]
                        .iter()
                        .take(3)
                        .take_while(|c| matches!(c, b'0'..=b'7'))
                        .count()
            }
            b'\n' | b'\r' => i + 1,
            c if c.is_ascii() => i + 2,
            // A backslash before a non-ASCII character.
            _ => i + 1,
        };
        blank(b, i..end);
        i = end;
    }
}

/// Byte ranges of the lines of `b` (without their `\n`).
fn lines(b: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start <= b.len() {
        let end = b[start..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| start + p);
        out.push(start..end);
        start = end + 1;
    }
    out
}

fn indentation(line: &[u8]) -> usize {
    line.iter()
        .take_while(|c| matches!(c, b' ' | b'\t'))
        .count()
}

/// Blanks doctest lines (`>>> code`, `... continuation`) in docstrings.
fn mask_doctest(b: &mut [u8]) {
    for line in lines(b) {
        let rest = &b[line.start + indentation(&b[line.clone()])..line.end];
        let prompt = rest.starts_with(b">>>")
            || (rest.starts_with(b"...") && rest.get(3).is_none_or(|c| c.is_ascii_whitespace()));
        if prompt {
            blank(b, line);
        }
    }
}

/// Longest code span or HTML tag that is masked, in bytes.
const MAX_SPAN: usize = 4096;

/// Blanks inline code: text between matching runs of backticks (Markdown
/// `` `code` ``, reST ``` ``code`` ```), not crossing a blank line.
fn mask_code_spans(b: &mut [u8]) {
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let ticks = b[i..].iter().take_while(|&&c| c == b'`').count();
        let limit = (i + ticks + MAX_SPAN).min(b.len());
        let mut j = i + ticks;
        let mut close = None;
        while j < limit {
            if b[j] == b'`' {
                let run = b[j..].iter().take_while(|&&c| c == b'`').count();
                if run == ticks {
                    close = Some(j + run);
                    break;
                }
                j += run;
            } else if b[j] == b'\n'
                && b[j + 1..]
                    .iter()
                    .take_while(|&&c| c != b'\n')
                    .all(u8::is_ascii_whitespace)
            {
                break;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                blank(b, i..end);
                i = end;
            }
            None => i += ticks,
        }
    }
}

/// Blanks HTML/XML tags (`<b>`, `</div>`, `<!-- -->`, autolinks such as
/// `<https://…>`) on a single line, and character references (`&nbsp;`).
fn mask_markup(b: &mut [u8]) {
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' if b
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?')) =>
            {
                let end = b[i + 1..]
                    .iter()
                    .take(MAX_SPAN)
                    .position(|&c| matches!(c, b'>' | b'<' | b'\n'))
                    .map(|p| i + 1 + p);
                match end {
                    Some(e) if b[e] == b'>' => {
                        blank(b, i..e + 1);
                        i = e + 1;
                    }
                    _ => i += 1,
                }
            }
            b'&' => {
                let name = b[i + 1..]
                    .iter()
                    .take(10)
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'#')
                    .count();
                if name >= 2 && b.get(i + 1 + name) == Some(&b';') {
                    blank(b, i..i + 2 + name);
                    i += 2 + name;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Width of the indentation of `line` in columns (tab stops every 4).
fn indent_columns(line: &[u8]) -> usize {
    let mut columns = 0;
    for &c in line {
        match c {
            b' ' => columns += 1,
            b'\t' => columns += 4 - columns % 4,
            _ => break,
        }
    }
    columns
}

/// Blanks the parts of a Markdown document or commit message that are not
/// prose: fenced and indented code blocks (and indented command output),
/// link destinations and trailers.
fn mask_markdown(b: &mut [u8]) {
    // The fence character and length of the open code block.
    let mut fence: Option<(u8, usize)> = None;
    // An indented code block starts after a blank line and goes on over
    // blank lines.
    let (mut after_blank, mut in_indented_code) = (true, false);
    for line in lines(b) {
        let indent = indentation(&b[line.clone()]);
        let rest = &b[line.start + indent..line.end];
        let is_blank = rest.iter().all(u8::is_ascii_whitespace);
        let indented_code = fence.is_none()
            && !is_blank
            && indent_columns(&b[line.clone()]) >= 4
            && (after_blank || in_indented_code);
        if !is_blank {
            in_indented_code = indented_code;
        }
        after_blank = is_blank;
        let marker = rest.first().copied().filter(|c| matches!(c, b'`' | b'~'));
        let marker_len = marker.map_or(0, |m| rest.iter().take_while(|&&c| c == m).count());
        match fence {
            Some((m, n)) => {
                let closes = indent <= 3
                    && marker == Some(m)
                    && marker_len >= n
                    && rest[marker_len..].iter().all(u8::is_ascii_whitespace);
                if closes {
                    fence = None;
                }
                blank(b, line);
            }
            None if indented_code => blank(b, line),
            None if indent <= 3 && marker_len >= 3 => {
                fence = marker.map(|m| (m, marker_len));
                blank(b, line);
            }
            None if is_trailer(&b[line.clone()]) => blank(b, line),
            None => mask_link_destinations(&mut b[line]),
        }
    }
}

/// `Signed-off-by: Name <mail>`, `Co-authored-by: …`, `Change-Id: …`.
fn is_trailer(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&c| c == b':') else {
        return false;
    };
    let key = &line[..colon];
    let well_formed = key.first().is_some_and(u8::is_ascii_alphabetic)
        && key.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-');
    if !well_formed || line.get(colon + 1) != Some(&b' ') {
        return false;
    }
    let key = String::from_utf8_lossy(key).to_ascii_lowercase();
    key.ends_with("-by")
        || matches!(
            key.as_str(),
            "change-id" | "cc" | "bug" | "closes" | "fixes" | "refs" | "see-also" | "link"
        )
}

/// Blanks `(destination)` after `[text]` and the target of a reference
/// definition (`[id]: destination`) on one line.
fn mask_link_destinations(line: &mut [u8]) {
    let find = |hay: &[u8], needle: &[u8], from: usize| {
        hay.get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| from + p)
    };
    let indent = indentation(line);
    if line.get(indent) == Some(&b'[') {
        if let Some(p) = find(line, b"]: ", indent) {
            let len = line.len();
            blank(line, p + 2..len);
            return;
        }
    }
    let mut from = 0;
    while let Some(p) = find(line, b"](", from) {
        let open = p + 1;
        let Some(close) = line[open..].iter().position(|&c| c == b')') else {
            break;
        };
        blank(line, open..open + close + 1);
        from = open + close + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dictionary {
        Dictionary::bundled()
    }

    /// `(word, kind)` of every typo, checking that ranges point at the word.
    fn typos(src: &str, opts: CheckOptions) -> Vec<(String, TypoKind)> {
        let found = check_text(src, &dict(), opts);
        for t in &found {
            assert_eq!(
                &src[usize::from(t.range.start())..usize::from(t.range.end())],
                t.word,
                "{t:?}"
            );
        }
        found.into_iter().map(|t| (t.word, t.kind)).collect()
    }

    fn words(src: &str) -> Vec<String> {
        typos(src, CheckOptions::default())
            .into_iter()
            .map(|(w, _)| w)
            .collect()
    }

    fn plain(src: &str) -> Vec<String> {
        let found = check_plain_text(src, &dict());
        for t in &found {
            assert_eq!(
                &src[usize::from(t.range.start())..usize::from(t.range.end())],
                t.word
            );
            assert_eq!(t.kind, TypoKind::PlainText);
        }
        found.into_iter().map(|t| t.word).collect()
    }

    const SNIPPET: &str = r#"# Recieve the data from the servr.
def recieve_data(buffer_szie, timeout=None):
    """Retrun the mesage that was sent."""
    mesage = f"Hello wrold, {buffer_szie:>10} bytes"
    return mesage
"#;

    #[test]
    fn finds_typos_in_comments_identifiers_and_strings() {
        use TypoKind::*;
        let expected = [
            ("Recieve", Comment),
            ("servr", Comment),
            ("recieve", Identifier),
            ("szie", Identifier),
            ("Retrun", String),
            ("mesage", String),
            ("mesage", Identifier),
            ("wrold", String),
        ];
        let got = typos(SNIPPET, CheckOptions::default());
        let expected: Vec<(std::string::String, TypoKind)> =
            expected.iter().map(|(w, k)| (w.to_string(), *k)).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn typos_carry_ranges_and_suggestions() {
        let found = check_text(SNIPPET, &dict(), CheckOptions::default());
        let first = &found[0];
        assert_eq!(first.word, "Recieve");
        assert_eq!(
            first.range,
            TextRange::new(TextSize::new(2), TextSize::new(9))
        );
        assert_eq!(
            first.suggestions.first().map(std::string::String::as_str),
            Some("Receive")
        );
        assert!(first.suggestions.len() <= DEFAULT_MAX_SUGGESTIONS);
        let ident = found
            .iter()
            .find(|t| t.kind == TypoKind::Identifier)
            .unwrap();
        assert_eq!(ident.word, "recieve");
        let offset = SNIPPET.find("recieve_data").unwrap();
        assert_eq!(usize::from(ident.range.start()), offset);
        assert_eq!(ident.suggestions[0], "receive");
        let retrun = found.iter().find(|t| t.word == "Retrun").unwrap();
        assert_eq!(retrun.suggestions[0], "Return");
    }

    #[test]
    fn options_select_what_is_checked() {
        let only = |identifiers, comments, strings| CheckOptions {
            check_identifiers: identifiers,
            check_comments: comments,
            check_strings: strings,
            min_word_len: 4,
        };
        let kinds = |opts| {
            typos(SNIPPET, opts)
                .into_iter()
                .map(|(_, k)| k)
                .collect::<Vec<_>>()
        };
        assert!(kinds(only(true, false, false))
            .iter()
            .all(|k| *k == TypoKind::Identifier));
        assert!(kinds(only(false, true, false))
            .iter()
            .all(|k| *k == TypoKind::Comment));
        assert!(kinds(only(false, false, true))
            .iter()
            .all(|k| *k == TypoKind::String));
        assert_eq!(kinds(only(false, false, false)), Vec::<TypoKind>::new());
        // `servr` and `szie` are shorter than six letters.
        let long = CheckOptions {
            min_word_len: 6,
            ..CheckOptions::default()
        };
        let got: Vec<std::string::String> =
            typos(SNIPPET, long).into_iter().map(|(w, _)| w).collect();
        assert_eq!(got, ["Recieve", "recieve", "Retrun", "mesage", "mesage"]);
        let short = CheckOptions {
            min_word_len: 3,
            ..CheckOptions::default()
        };
        assert_eq!(words_with("# fix teh bug\n", short), ["teh"]);
        assert_eq!(words("# fix teh bug\n"), Vec::<std::string::String>::new());
    }

    fn words_with(src: &str, opts: CheckOptions) -> Vec<std::string::String> {
        typos(src, opts).into_iter().map(|(w, _)| w).collect()
    }

    #[test]
    fn only_declarations_are_checked() {
        let src = "import numpy as np\nvalues = np.linspace(0, 1)\nresult = frobnicate(values, recieve=True)\nobj.recieve_count = 1\nself.recieve_count = 2\n";
        assert_eq!(words(src), ["recieve"]);
        let found = check_text(src, &dict(), CheckOptions::default());
        assert_eq!(
            usize::from(found[0].range.start()),
            src.find("self.").unwrap() + 5
        );
        let src = "for itme in items:\n    with open(itme) as fyle:\n        pass\nsquares = [nubmer ** 2 for nubmer in range(3)]\n";
        assert_eq!(words(src), ["itme", "fyle", "nubmer"]);
    }

    #[test]
    fn identifier_rules() {
        // Acronyms in mixed-case names, dunders, builtins, jargon, digits.
        let src = "parseHTTPResponse = 1\nclass JSONDecoderMixin: pass\ndef __post_init__(self): pass\nisinstance = 2\nkwargs = args = 3\nutf8_decoder = md5sum = 4\nDEFAULT_TIMEOUT = 5\n";
        assert_eq!(words(src), Vec::<std::string::String>::new());
        // `CONSTANT_CASE` words are checked, case-insensitively.
        assert_eq!(
            words("MAX_RECIEVE_SIZE = 1\nRECIEVE = 2\n"),
            ["RECIEVE", "RECIEVE"]
        );
        let found = check_text("MAX_RECIEVE_SIZE = 1\n", &dict(), CheckOptions::default());
        assert_eq!(found[0].suggestions[0], "RECEIVE");
        // camelCase parts and non-ASCII names.
        assert_eq!(words("def getUserNmae(): pass\nnaïveValue = 1\n"), ["Nmae"]);
    }

    #[test]
    fn prose_skipping_rules() {
        let src = r#"# See https://exmaple.com/recieve and www.exmaple.org, mail recieve@exmaple.com
# Paths: /usr/lib/pythnon3 ./recieve/data ~/recieve C:\Users\recieve a/b/recieve
# Hashes deadbeefcafe 0xdeadbeef 3fa85f64-5717-4562-b3fc-2c963f66afa6 #aabbcc and utf8 recieve2
# Acronyms: HTTP API JSON NASA RECIEVE URLs and the HTTPServer class
# Python: isinstance nonlocal __init__ kwargs dataclass typeshed regex len repr
# Code: `recieve_data` and ``recieve()`` in backticks, <recieve> tags, &nbsp;
# Refs: self.recieve_data(), recieve.py, .recieverc, -Wrecieve --recieve-all @recieve
# Constants: HAVE_RECIEVE_H and MAX_RECIEVE_SIZE are declared elsewhere
# It isn't, don't, can't, won't; the user's data, we're done, o'clock
# naïve café Zürich
"#;
        assert_eq!(words(src), Vec::<std::string::String>::new());
        // Mixed with real typos, only those are reported.
        assert_eq!(
            words("# Use https://exmaple.com to recieve teh mesage\n"),
            ["recieve", "mesage"]
        );
        assert_eq!(
            words("s = 'pass --recieve-all to recieve all.'\n"),
            ["recieve"]
        );
        assert_eq!(words("# isn't recieven't userr's\n"), ["recieven", "userr"]);
    }

    #[test]
    fn identifier_like_words_in_prose() {
        assert_eq!(
            words("# call recieve_data() or getRecievedValue\n"),
            ["recieve", "Recieved"]
        );
        // All-caps names in prose refer to constants, macros or environment
        // variables; only their declaration is checked.
        assert_eq!(
            words("# MAX_RECIEVE_SIZE is too small\ns = 'HAVE_RECIEVE_H'\n"),
            Vec::<std::string::String>::new()
        );
        assert_eq!(
            words("MAX_RECIEVE_SIZE = 1  # see MAX_RECIEVE_SIZE\n"),
            ["RECIEVE"]
        );
    }

    #[test]
    fn comment_directives() {
        let src = "#!/usr/bin/env pythnon\n# -*- coding: utf-8 -*-\nx = 1  # type: ignore[recieve-error]\ny = 2  # noqa: RECIEVE\nz = 3  # pylint: disable=recieve\nw = 4  # trailing commment  # noqa: E501\n";
        assert_eq!(words(src), ["commment"]);
        assert_eq!(words("# coding=utf-8\n"), Vec::<std::string::String>::new());
    }

    #[test]
    fn string_handling() {
        // Escapes do not glue onto words; raw strings keep regexes intact.
        let src = "a = 'Hello\\nrecieve\\tworld\\x41bc'\nb = r'^\\w+recieve\\d$'\nc = b'recieve'\nd = '''\n    >>> recieve(1)\n    recieve\n'''\n";
        let found = check_text(src, &dict(), CheckOptions::default());
        let starts: Vec<usize> = found.iter().map(|t| usize::from(t.range.start())).collect();
        assert_eq!(
            found.iter().map(|t| t.word.as_str()).collect::<Vec<_>>(),
            ["recieve", "recieve", "recieve"]
        );
        assert_eq!(starts[0], src.find("recieve").unwrap());
        assert_eq!(starts[1], src.find("b'recieve'").unwrap() + 2);
        assert_eq!(starts[2], src.rfind("recieve").unwrap());
        // Windows paths with escaped backslashes are paths.
        assert_eq!(
            words("p = 'C:\\\\Users\\\\recieve'\n"),
            Vec::<std::string::String>::new()
        );
        // Unicode escapes and named escapes.
        assert_eq!(
            words("s = '\\u00e9t\\N{BULLET POINT}recieve'\n"),
            ["recieve"]
        );
    }

    #[test]
    fn fstring_text_but_not_format_specs() {
        let src = "s = f'Recieved {count:recievex} itmes {value!r:>{widht}} {d[\"key\"]:%Y-%m}'\n";
        assert_eq!(words(src), ["Recieved", "itmes"]);
        let src = "t = rf'\\brecieve {x} {{literal brace}} wrold'\n";
        assert_eq!(words(src), ["wrold"]);
        let src = "u = f'''\nMultiline {f\"nested {y} txet\"} and mroe\n'''\n";
        assert_eq!(words(src), ["txet", "mroe"]);
    }

    #[test]
    fn plain_text_and_markdown() {
        let md = "# Instalation\n\nRun `recieve --help` or see [the docs](https://exmaple.com/recieve).\n\n```python\nrecieve = 1\n```\n\n~~~\nrecieve\n~~~\n\n[ref]: ./recieve/notes.md\n<details>Some <b>bold</b> text</details>\nThe wrold is big.\n";
        assert_eq!(plain(md), ["Instalation", "wrold"]);
        let commit = "Fix recieving of mesages\n\nSigned-off-by: Jane Doecollins <jane@exmaple.com>\nCo-authored-by: Bob Smithers <bob@exmaple.com>\nChange-Id: I8e3f5a\n";
        assert_eq!(plain(commit), ["recieving", "mesages"]);
        let commit =
            "Rename Parser.recieve() and add --recieve-all\n\nThanks @recieve for the reprot.\n";
        assert_eq!(plain(commit), ["reprot"]);
        // An unclosed fence hides the rest.
        assert_eq!(
            plain("Text\n```\nrecieve\n"),
            Vec::<std::string::String>::new()
        );
        // Indented code blocks start after a blank line (tabs count as four
        // columns) and go on over blank lines; indented list continuations
        // are prose.
        let md = "Intro:\n\n    recieve = 1\n\n\tmore_recieve()\nText wrold\n- item\n    contniued item\n";
        assert_eq!(plain(md), ["wrold", "contniued"]);
        let commit = "Fix the parser\n\nThe error was:\n\n    ValueError: recieved bad input\n";
        assert_eq!(plain(commit), Vec::<std::string::String>::new());
    }

    #[test]
    fn helpers() {
        assert_eq!(string_content("'abc'"), (1..4, false));
        assert_eq!(string_content("rb'''x'''"), (5..6, true));
        assert_eq!(string_content("''"), (1..1, false));
        assert_eq!(string_content("'''unterminated"), (3..15, false));
        assert_eq!(string_content("Rb\"\""), (3..3, true));
        assert!(skip_chunk("(https://exmaple.com)."));
        assert!(skip_chunk("me@exmaple.com,"));
        assert!(skip_chunk("/etc/passwd"));
        assert!(!skip_chunk("and/or"));
        assert!(skip_chunk("os.path.join(dirpath,"));
        assert!(skip_chunk("(setup.py)"));
        assert!(skip_chunk("e.g.,"));
        assert!(skip_chunk(".gitignore"));
        assert!(skip_chunk("-Wall"));
        assert!(skip_chunk("--dry-run"));
        assert!(skip_chunk("@property"));
        assert!(skip_chunk("@_private"));
        for prose in [
            "end.",
            "wait...what",
            "...and",
            "-",
            "--",
            "-1",
            "e-mail",
            "@",
            "x@y",
            "(word)",
            "\"quoted\",",
        ] {
            assert!(!skip_chunk(prose), "{prose}");
        }
        let mut b = b"a <b>x</b> &amp; c".to_vec();
        mask_markup(&mut b);
        assert_eq!(
            b,
            b"a    x     "
                .iter()
                .chain(b"      c")
                .copied()
                .collect::<Vec<u8>>()
        );
        assert!(is_trailer(b"Signed-off-by: A <a@b.c>"));
        assert!(!is_trailer(b"Note: this matters"));
    }

    #[test]
    fn never_panics_and_ranges_are_valid() {
        let inputs = [
            "",
            "#",
            "'",
            "\"\"\"",
            "f'",
            "f'{",
            "f'{x",
            "f'{x:",
            "rf'\\",
            "'\\",
            "x = 'é\\é'",
            "# é`ü` ``",
            "def",
            "class (",
            "a: = b",
            "for in",
            "lambda: :",
            "x[ = 1",
            "\u{feff}# recieve",
            "s = '\\N{'",
            "s = '\\x'",
            "# `\n\n` x",
            "<<<>>>",
            "# &#12; &x;",
            "\r\n# recieve\r\n",
            "日本語 = '漢字テキスト' # コメント",
        ];
        for src in inputs {
            for t in check_text(
                src,
                &dict(),
                CheckOptions {
                    min_word_len: 1,
                    ..CheckOptions::default()
                },
            ) {
                assert!(
                    src.get(usize::from(t.range.start())..usize::from(t.range.end()))
                        == Some(t.word.as_str()),
                    "{src:?}: {t:?}"
                );
            }
            for t in check_plain_text(src, &dict()) {
                assert!(
                    src.get(usize::from(t.range.start())..usize::from(t.range.end()))
                        == Some(t.word.as_str()),
                    "{src:?}: {t:?}"
                );
            }
        }
    }

    /// Inputs that a careless implementation handles in quadratic time or
    /// with unbounded recursion; each must finish promptly.
    #[test]
    fn pathological_inputs_stay_linear() {
        let banner = format!("{} recieve\n", "#".repeat(100_000));
        assert_eq!(words(&banner), ["recieve"]);
        let headers = format!("{}recieve_all = 1\n", "else: ".repeat(50_000));
        assert_eq!(words(&headers), ["recieve"]);
        let keywords = format!("x = [{}]\n", "for lambda as ".repeat(20_000));
        assert_eq!(words(&keywords), Vec::<std::string::String>::new());
        let ticks = format!("# {}recieve\n", "` ``".repeat(20_000));
        assert_eq!(words(&ticks), ["recieve"]);
    }
}

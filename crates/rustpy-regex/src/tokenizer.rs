//! Tokenizer for Python `re` pattern syntax. Never panics; malformed input
//! yields `Error` tokens.

use rustpy_core::{TextRange, TextSize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegexTokenKind {
    /// An ordinary character (one token per character outside classes).
    Literal,
    /// `\d`, `\.`, `\x41`, `\N{...}`, back-references `\1` / `\g<name>`, …
    Escape,
    /// The `[`/`[^` and `]` delimiters of a character class.
    CharClass,
    /// A range `a-z` inside a character class.
    ClassRange,
    /// `^`, `$`, `\b`, `\B`, `\A`, `\Z`.
    Anchor,
    /// `*`, `+`, `?`, `{m,n}` including a lazy `?` or possessive `+` suffix.
    Quantifier,
    /// `(`, `(?:`, `(?P<`, `>`, `(?=`, `(?!`, `(?<=`, `(?<!`, `(?i:`, `(?(`.
    GroupOpen,
    /// `)`.
    GroupClose,
    /// The name inside `(?P<name>`, `(?P=name)`, `\g<name>` or `(?(name)`.
    GroupName,
    /// `|`.
    Alternation,
    /// `.`.
    Dot,
    /// Inline flags `(?imsx)`.
    Flag,
    /// `(?#...)` or, in verbose mode, `# …` to the end of the line.
    Comment,
    /// Unbalanced delimiter, bad escape, trailing backslash, …
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexToken {
    /// Byte range inside the pattern.
    pub range: TextRange,
    pub kind: RegexTokenKind,
}

impl RegexToken {
    fn new(start: usize, end: usize, kind: RegexTokenKind) -> Self {
        Self {
            range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
            kind,
        }
    }
}

/// Tokenizes `pattern`. With `verbose`, whitespace outside character classes
/// is skipped and `#` starts a comment.
pub fn tokenize(pattern: &str, verbose: bool) -> Vec<RegexToken> {
    Lexer {
        s: pattern.as_bytes(),
        src: pattern,
        pos: 0,
        verbose,
        out: Vec::new(),
        open_groups: Vec::new(),
    }
    .run()
}

struct Lexer<'a> {
    s: &'a [u8],
    src: &'a str,
    pos: usize,
    verbose: bool,
    out: Vec<RegexToken>,
    /// Indices into `out` of unclosed `GroupOpen` tokens.
    open_groups: Vec<usize>,
}

impl Lexer<'_> {
    fn run(mut self) -> Vec<RegexToken> {
        while self.pos < self.s.len() {
            self.step();
        }
        // Unclosed groups become errors.
        for idx in self.open_groups.drain(..) {
            if let Some(t) = self.out.get_mut(idx) {
                t.kind = RegexTokenKind::Error;
            }
        }
        self.out
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.s.get(self.pos + off).copied()
    }

    /// Byte length of the UTF-8 character at `pos`.
    fn char_len(&self, pos: usize) -> usize {
        self.src[pos..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1)
    }

    fn push(&mut self, start: usize, end: usize, kind: RegexTokenKind) {
        self.out.push(RegexToken::new(start, end, kind));
    }

    fn step(&mut self) {
        let start = self.pos;
        let c = self.s[start];
        match c {
            b'\\' => self.escape(false),
            b'[' => self.char_class(),
            b'(' => self.group_open(),
            b')' => {
                self.pos += 1;
                if self.open_groups.pop().is_some() {
                    self.push(start, self.pos, RegexTokenKind::GroupClose);
                } else {
                    self.push(start, self.pos, RegexTokenKind::Error);
                }
            }
            b'|' => {
                self.pos += 1;
                self.push(start, self.pos, RegexTokenKind::Alternation);
            }
            b'.' => {
                self.pos += 1;
                self.push(start, self.pos, RegexTokenKind::Dot);
            }
            b'^' | b'$' => {
                self.pos += 1;
                self.push(start, self.pos, RegexTokenKind::Anchor);
            }
            b'*' | b'+' | b'?' => {
                self.pos += 1;
                self.quantifier_suffix();
                self.push(start, self.pos, RegexTokenKind::Quantifier);
            }
            b'{' => {
                if let Some(end) = self.brace_quantifier_end() {
                    self.pos = end;
                    self.quantifier_suffix();
                    self.push(start, self.pos, RegexTokenKind::Quantifier);
                } else {
                    self.pos += 1;
                    self.push(start, self.pos, RegexTokenKind::Literal);
                }
            }
            b'#' if self.verbose => {
                while self.pos < self.s.len() && self.s[self.pos] != b'\n' {
                    self.pos += 1;
                }
                self.push(start, self.pos, RegexTokenKind::Comment);
            }
            b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c if self.verbose => {
                self.pos += 1;
            }
            _ => {
                self.pos += self.char_len(start);
                self.push(start, self.pos, RegexTokenKind::Literal);
            }
        }
    }

    /// Consumes a lazy `?` or possessive `+` suffix after a quantifier.
    fn quantifier_suffix(&mut self) {
        if matches!(self.peek(0), Some(b'?') | Some(b'+')) {
            self.pos += 1;
        }
    }

    /// End (exclusive) of a `{m}`, `{m,}`, `{,n}` or `{m,n}` quantifier at `pos`.
    fn brace_quantifier_end(&self) -> Option<usize> {
        let mut i = self.pos + 1;
        let mut digits_before = 0;
        while i < self.s.len() && self.s[i].is_ascii_digit() {
            i += 1;
            digits_before += 1;
        }
        let mut comma = false;
        let mut digits_after = 0;
        if i < self.s.len() && self.s[i] == b',' {
            comma = true;
            i += 1;
            while i < self.s.len() && self.s[i].is_ascii_digit() {
                i += 1;
                digits_after += 1;
            }
        }
        if i < self.s.len()
            && self.s[i] == b'}'
            && (digits_before > 0 || (comma && digits_after > 0))
        {
            Some(i + 1)
        } else {
            None
        }
    }

    /// `\…` at `pos`. Inside classes, `\b` is a backspace (literal escape).
    fn escape(&mut self, in_class: bool) {
        let start = self.pos;
        let Some(next) = self.peek(1) else {
            self.pos += 1;
            self.push(start, self.pos, RegexTokenKind::Error);
            return;
        };
        self.pos += 1; // backslash
        match next {
            b'A' | b'Z' | b'B' => {
                self.pos += 1;
                self.push(
                    start,
                    self.pos,
                    if in_class {
                        RegexTokenKind::Error
                    } else {
                        RegexTokenKind::Anchor
                    },
                );
            }
            b'b' => {
                self.pos += 1;
                self.push(
                    start,
                    self.pos,
                    if in_class {
                        RegexTokenKind::Escape
                    } else {
                        RegexTokenKind::Anchor
                    },
                );
            }
            b'd' | b'D' | b'w' | b'W' | b's' | b'S' | b'n' | b't' | b'r' | b'f' | b'v' | b'a'
            | b'\\' => {
                self.pos += 1;
                self.push(start, self.pos, RegexTokenKind::Escape);
            }
            b'x' => {
                self.pos += 1;
                let ok = self.take_hex(2);
                self.push(
                    start,
                    self.pos,
                    if ok {
                        RegexTokenKind::Escape
                    } else {
                        RegexTokenKind::Error
                    },
                );
            }
            b'u' => {
                self.pos += 1;
                let ok = self.take_hex(4);
                self.push(
                    start,
                    self.pos,
                    if ok {
                        RegexTokenKind::Escape
                    } else {
                        RegexTokenKind::Error
                    },
                );
            }
            b'U' => {
                self.pos += 1;
                let ok = self.take_hex(8);
                self.push(
                    start,
                    self.pos,
                    if ok {
                        RegexTokenKind::Escape
                    } else {
                        RegexTokenKind::Error
                    },
                );
            }
            b'N' => {
                self.pos += 1;
                if self.peek(0) == Some(b'{') {
                    if let Some(close) = self.src[self.pos..].find('}') {
                        self.pos += close + 1;
                        self.push(start, self.pos, RegexTokenKind::Escape);
                        return;
                    }
                }
                self.push(start, self.pos, RegexTokenKind::Error);
            }
            b'0' => {
                // `\0`, `\0o`, `\0oo` octal.
                self.pos += 1;
                for _ in 0..2 {
                    if self.peek(0).is_some_and(|c| (b'0'..=b'7').contains(&c)) {
                        self.pos += 1;
                    }
                }
                self.push(start, self.pos, RegexTokenKind::Escape);
            }
            b'1'..=b'9' => {
                // Back-reference `\1`..`\99` (or a 3-digit octal escape).
                self.pos += 1;
                if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                    if self.peek(0).is_some_and(|c| (b'0'..=b'7').contains(&c))
                        && (b'0'..=b'7').contains(&next)
                    {
                        self.pos += 1;
                    }
                }
                self.push(
                    start,
                    self.pos,
                    if in_class && !matches!(next, b'0'..=b'7') {
                        RegexTokenKind::Error
                    } else {
                        RegexTokenKind::Escape
                    },
                );
            }
            b'g' if !in_class => {
                self.pos += 1;
                if self.peek(0) == Some(b'<') {
                    let name_start = self.pos + 1;
                    if let Some(close) = self.src[name_start..].find('>') {
                        let name_end = name_start + close;
                        self.push(start, name_start, RegexTokenKind::Escape);
                        self.push(name_start, name_end, RegexTokenKind::GroupName);
                        self.pos = name_end + 1;
                        self.push(name_end, self.pos, RegexTokenKind::Escape);
                        return;
                    }
                }
                self.push(start, self.pos, RegexTokenKind::Error);
            }
            c if c.is_ascii_alphanumeric() => {
                // Unknown letter escapes are errors in Python 3.7+.
                self.pos += 1;
                self.push(start, self.pos, RegexTokenKind::Error);
            }
            _ => {
                // Escaped punctuation / whitespace / non-ASCII: literal escape.
                self.pos += self.char_len(self.pos);
                self.push(start, self.pos, RegexTokenKind::Escape);
            }
        }
    }

    fn take_hex(&mut self, n: usize) -> bool {
        for _ in 0..n {
            if self.peek(0).is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            } else {
                return false;
            }
        }
        true
    }

    /// `[…]` at `pos`.
    fn char_class(&mut self) {
        let start = self.pos;
        self.pos += 1;
        if self.peek(0) == Some(b'^') {
            self.pos += 1;
        }
        let open_idx = self.out.len();
        self.push(start, self.pos, RegexTokenKind::CharClass);
        let mut first = true;
        loop {
            let Some(c) = self.peek(0) else {
                // Unclosed class.
                self.out[open_idx].kind = RegexTokenKind::Error;
                return;
            };
            if c == b']' && !first {
                let s = self.pos;
                self.pos += 1;
                self.push(s, self.pos, RegexTokenKind::CharClass);
                return;
            }
            first = false;
            let item_start = self.pos;
            if c == b'\\' {
                self.escape(true);
            } else {
                self.pos += self.char_len(self.pos);
                self.push(item_start, self.pos, RegexTokenKind::Literal);
            }
            // Range `x-y` (the `-` must not be the last char before `]`).
            if self.peek(0) == Some(b'-') && self.peek(1).is_some_and(|n| n != b']') {
                let lo = self.out.last().cloned();
                let range_ok = lo.as_ref().is_some_and(|t| {
                    matches!(
                        t.kind,
                        RegexTokenKind::Literal | RegexTokenKind::Escape | RegexTokenKind::Error
                    )
                });
                let lo_is_set = lo.as_ref().is_some_and(|t| self.is_set_escape(t));
                self.pos += 1; // '-'
                let hi_start = self.pos;
                if self.peek(0) == Some(b'\\') {
                    self.escape(true);
                } else {
                    self.pos += self.char_len(self.pos);
                    self.push(hi_start, self.pos, RegexTokenKind::Literal);
                }
                if range_ok {
                    // Merge the low token, '-', and the high token into one range token.
                    let hi = self.out.pop().unwrap();
                    let lo = self.out.pop().unwrap();
                    let hi_is_set = self.is_set_escape(&hi);
                    // `[\d-z]` is a bad range in Python 3.7+.
                    let kind = if hi.kind == RegexTokenKind::Error
                        || lo.kind == RegexTokenKind::Error
                        || lo_is_set
                        || hi_is_set
                    {
                        RegexTokenKind::Error
                    } else {
                        RegexTokenKind::ClassRange
                    };
                    self.push(
                        usize::from(lo.range.start()),
                        usize::from(hi.range.end()),
                        kind,
                    );
                } else {
                    // e.g. `[a-z-0]` after a range: the dash is a literal.
                    let hi = self.out.pop().unwrap();
                    self.push(hi_start - 1, hi_start, RegexTokenKind::Literal);
                    self.out.push(hi);
                }
            }
        }
    }

    /// `\d`, `\w`, `\s` and their negations cannot be range endpoints.
    fn is_set_escape(&self, t: &RegexToken) -> bool {
        t.kind == RegexTokenKind::Escape
            && matches!(
                &self.src[usize::from(t.range.start())..usize::from(t.range.end())],
                "\\d" | "\\D" | "\\w" | "\\W" | "\\s" | "\\S"
            )
    }

    /// `(` at `pos` and its extension syntax.
    fn group_open(&mut self) {
        let start = self.pos;
        if self.peek(1) != Some(b'?') {
            self.pos += 1;
            self.open_groups.push(self.out.len());
            self.push(start, self.pos, RegexTokenKind::GroupOpen);
            return;
        }
        match self.peek(2) {
            Some(b'#') => {
                // Comment group.
                match self.src[start..].find(')') {
                    Some(close) => {
                        self.pos = start + close + 1;
                        self.push(start, self.pos, RegexTokenKind::Comment);
                    }
                    None => {
                        self.pos = self.s.len();
                        self.push(start, self.pos, RegexTokenKind::Error);
                    }
                }
            }
            Some(b':') | Some(b'=') | Some(b'!') => {
                self.pos += 3;
                self.open_groups.push(self.out.len());
                self.push(start, self.pos, RegexTokenKind::GroupOpen);
            }
            Some(b'<') => match self.peek(3) {
                Some(b'=') | Some(b'!') => {
                    self.pos += 4;
                    self.open_groups.push(self.out.len());
                    self.push(start, self.pos, RegexTokenKind::GroupOpen);
                }
                _ => {
                    // `(?<name>` (third-party `regex` module / other dialects).
                    self.named_group(start, 3);
                }
            },
            Some(b'P') => match self.peek(3) {
                Some(b'<') => self.named_group(start, 4),
                Some(b'=') => {
                    // `(?P=name)` back-reference.
                    let name_start = start + 4;
                    match self.src[name_start..].find(')') {
                        Some(close) => {
                            let name_end = name_start + close;
                            self.push(start, name_start, RegexTokenKind::Escape);
                            self.push(name_start, name_end, RegexTokenKind::GroupName);
                            self.pos = name_end + 1;
                            self.push(name_end, self.pos, RegexTokenKind::Escape);
                        }
                        None => {
                            self.pos = self.s.len();
                            self.push(start, self.pos, RegexTokenKind::Error);
                        }
                    }
                }
                _ => {
                    self.pos += 3;
                    self.push(start, self.pos, RegexTokenKind::Error);
                }
            },
            Some(b'(') => {
                // Conditional `(?(id/name)yes|no)`: the condition is part of the open token.
                let cond_start = start + 3;
                match self.src[cond_start..].find(')') {
                    Some(close) => {
                        let cond_end = cond_start + close;
                        self.open_groups.push(self.out.len());
                        self.push(start, cond_start, RegexTokenKind::GroupOpen);
                        self.push(cond_start, cond_end, RegexTokenKind::GroupName);
                        self.pos = cond_end + 1;
                        self.push(cond_end, self.pos, RegexTokenKind::GroupOpen);
                    }
                    None => {
                        self.pos = self.s.len();
                        self.push(start, self.pos, RegexTokenKind::Error);
                    }
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'-' => {
                // Inline flags `(?aiLmsux)` or scoped flags `(?i-s:…)`.
                let mut i = start + 2;
                while i < self.s.len() && (self.s[i].is_ascii_alphabetic() || self.s[i] == b'-') {
                    i += 1;
                }
                if let Some(bad) = self.s[start + 2..i]
                    .iter()
                    .position(|c| !b"aiLmsux-".contains(c))
                {
                    // Unknown extension such as `(?Q`.
                    self.pos = start + 3 + bad;
                    self.push(start, self.pos, RegexTokenKind::Error);
                    return;
                }
                match self.s.get(i) {
                    Some(b')') => {
                        self.pos = i + 1;
                        self.push(start, self.pos, RegexTokenKind::Flag);
                    }
                    Some(b':') => {
                        self.pos = i + 1;
                        self.open_groups.push(self.out.len());
                        self.push(start, self.pos, RegexTokenKind::GroupOpen);
                    }
                    _ => {
                        self.pos = i;
                        self.push(start, self.pos, RegexTokenKind::Error);
                    }
                }
            }
            _ => {
                self.pos = (start + 2).min(self.s.len());
                self.push(start, self.pos, RegexTokenKind::Error);
            }
        }
    }

    /// `(?P<name>` / `(?<name>` where `name_off` is the offset of the name from `start`.
    fn named_group(&mut self, start: usize, name_off: usize) {
        let name_start = start + name_off;
        match self.src[name_start.min(self.s.len())..].find('>') {
            Some(close) => {
                let name_end = name_start + close;
                self.open_groups.push(self.out.len());
                self.push(start, name_start, RegexTokenKind::GroupOpen);
                self.push(name_start, name_end, RegexTokenKind::GroupName);
                self.pos = name_end + 1;
                self.push(name_end, self.pos, RegexTokenKind::GroupOpen);
            }
            None => {
                self.pos = self.s.len();
                self.push(start, self.pos, RegexTokenKind::Error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RegexTokenKind::*;

    fn kinds(p: &str) -> Vec<RegexTokenKind> {
        tokenize(p, false).into_iter().map(|t| t.kind).collect()
    }

    fn texts(p: &str, verbose: bool) -> Vec<(String, RegexTokenKind)> {
        tokenize(p, verbose)
            .into_iter()
            .map(|t| {
                (
                    p[usize::from(t.range.start())..usize::from(t.range.end())].to_string(),
                    t.kind,
                )
            })
            .collect()
    }

    #[test]
    fn basic_atoms() {
        assert_eq!(kinds(r"a.b"), vec![Literal, Dot, Literal]);
        assert_eq!(kinds(r"^ab$"), vec![Anchor, Literal, Literal, Anchor]);
        assert_eq!(kinds(r"a|b"), vec![Literal, Alternation, Literal]);
    }

    #[test]
    fn escapes_and_anchors() {
        let t = texts(r"\d\w\s\b\B\A\Z\n\x41\u1234\N{DASH}\1\g<x>\.", false);
        assert_eq!(t[0], ("\\d".into(), Escape));
        assert_eq!(t[3], ("\\b".into(), Anchor));
        assert_eq!(t[4], ("\\B".into(), Anchor));
        assert_eq!(t[5], ("\\A".into(), Anchor));
        assert_eq!(t[6], ("\\Z".into(), Anchor));
        assert_eq!(t[8], ("\\x41".into(), Escape));
        assert_eq!(t[9], ("\\u1234".into(), Escape));
        assert_eq!(t[10], ("\\N{DASH}".into(), Escape));
        assert_eq!(t[11], ("\\1".into(), Escape));
        assert_eq!(t[12], ("\\g<".into(), Escape));
        assert_eq!(t[13], ("x".into(), GroupName));
        assert_eq!(t[14], (">".into(), Escape));
        assert_eq!(t[15], ("\\.".into(), Escape));
    }

    #[test]
    fn bad_escapes() {
        assert_eq!(kinds(r"\q"), vec![Error]);
        assert_eq!(kinds(r"\x4"), vec![Error]);
        assert_eq!(kinds("abc\\"), vec![Literal, Literal, Literal, Error]);
    }

    #[test]
    fn char_classes() {
        let t = texts(r"[^a-z0-9_\d.]", false);
        assert_eq!(t[0], ("[^".into(), CharClass));
        assert_eq!(t[1], ("a-z".into(), ClassRange));
        assert_eq!(t[2], ("0-9".into(), ClassRange));
        assert_eq!(t[3], ("_".into(), Literal));
        assert_eq!(t[4], ("\\d".into(), Escape));
        assert_eq!(t[5], (".".into(), Literal));
        assert_eq!(t[6], ("]".into(), CharClass));
        // `]` first is a literal; trailing `-` is a literal; unclosed class is an error.
        assert_eq!(
            kinds(r"[]a-]"),
            vec![CharClass, Literal, Literal, Literal, CharClass]
        );
        assert_eq!(kinds(r"[abc"), vec![Error, Literal, Literal, Literal]);
        // `\b` inside a class is a backspace escape, not an anchor.
        assert_eq!(kinds(r"[\b]"), vec![CharClass, Escape, CharClass]);
        // A class-set escape cannot be a range endpoint.
        assert_eq!(kinds(r"[\d-z]"), vec![CharClass, Error, CharClass]);
        // A dash after a completed range is a literal.
        assert_eq!(
            kinds(r"[a-z-0]"),
            vec![CharClass, ClassRange, Literal, Literal, CharClass]
        );
        // Unknown extension letters are errors; real flags are not.
        assert_eq!(kinds(r"(?Qa)"), vec![Error, Literal, Error]);
        assert_eq!(kinds(r"(?i-s:a)"), vec![GroupOpen, Literal, GroupClose]);
    }

    #[test]
    fn groups() {
        let t = texts(
            r"(a)(?:b)(?P<name>c)(?P=name)(?=d)(?!e)(?<=f)(?<!g)(?#note)(?i)(?i:h)(?(1)x|y)",
            false,
        );
        let s: Vec<&str> = t.iter().map(|(s, _)| s.as_str()).collect();
        assert!(s.contains(&"(?P<"));
        assert!(s.contains(&"(?P="));
        assert!(s.contains(&"(?<="));
        assert!(s.contains(&"(?<!"));
        assert!(t.iter().any(|(s, k)| s == "(?#note)" && *k == Comment));
        assert!(t.iter().any(|(s, k)| s == "(?i)" && *k == Flag));
        assert!(t.iter().any(|(s, k)| s == "(?i:" && *k == GroupOpen));
        assert!(t.iter().any(|(s, k)| s == "name" && *k == GroupName));
        assert!(t.iter().any(|(s, k)| s == "1" && *k == GroupName));
        assert!(!t.iter().any(|(_, k)| *k == Error), "{t:?}");
        // (a) (?:b) (?P<name>c) (?=d) (?!e) (?<=f) (?<!g) (?i:h) (?(1)x|y) → 9 closes; (?P=name) is a reference.
        assert_eq!(t.iter().filter(|(_, k)| *k == GroupClose).count(), 9);
    }

    #[test]
    fn unbalanced_groups() {
        assert_eq!(kinds("(a"), vec![Error, Literal]);
        assert_eq!(kinds("a)"), vec![Literal, Error]);
        assert_eq!(kinds("(?P<n"), vec![Error]);
        assert_eq!(kinds("(?#x"), vec![Error]);
    }

    #[test]
    fn quantifiers() {
        let t = texts(r"a*b+?c??d{2}e{2,}f{,3}g{1,5}+h{x}", false);
        assert_eq!(t[1], ("*".into(), Quantifier));
        assert_eq!(t[3], ("+?".into(), Quantifier));
        assert_eq!(t[5], ("??".into(), Quantifier));
        assert_eq!(t[7], ("{2}".into(), Quantifier));
        assert_eq!(t[9], ("{2,}".into(), Quantifier));
        assert_eq!(t[11], ("{,3}".into(), Quantifier));
        assert_eq!(t[13], ("{1,5}+".into(), Quantifier));
        // `{x}` is not a quantifier: three literals.
        assert_eq!(t[15], ("{".into(), Literal));
    }

    #[test]
    fn verbose_mode() {
        let t = texts("a b # comment\n  (c)", true);
        assert_eq!(t[0], ("a".into(), Literal));
        assert_eq!(t[1], ("b".into(), Literal));
        assert_eq!(t[2], ("# comment".into(), Comment));
        assert_eq!(t[3], ("(".into(), GroupOpen));
        // Whitespace inside a class is kept even in verbose mode.
        assert_eq!(kinds("[ ]"), vec![CharClass, Literal, CharClass]);
        assert_eq!(texts("[ ]", true).len(), 3);
        // Non-verbose: spaces are literals and `#` too.
        assert_eq!(kinds("a #"), vec![Literal, Literal, Literal]);
    }

    #[test]
    fn unicode_literals_are_single_tokens() {
        let t = texts("héllo", false);
        assert_eq!(t.len(), 5);
        assert_eq!(t[1].0, "é");
    }

    #[test]
    fn never_panics_on_garbage() {
        for p in [
            "(",
            ")",
            "[",
            "]",
            "\\",
            "(?",
            "(?P",
            "(?P<",
            "(?P=",
            "(?(",
            "(?<",
            "{",
            "{1,",
            "a{1,2",
            "[a-",
            "[\\",
            "(?#",
            "\\g<",
            "\\N{",
            "\\x",
            "💥(?P<ü>",
            "((((",
            "[[[",
            "\\g<a",
        ] {
            let _ = tokenize(p, false);
            let _ = tokenize(p, true);
        }
    }
}

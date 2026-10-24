//! Python tokenizer (roadmap §2.1): full 3.x token set, INDENT/DEDENT,
//! implicit line joining, backslash continuation, PEP 701 f-strings and
//! PEP 750 t-strings, with recovery diagnostics instead of failures.

use rustpy_core::{Diagnostic, TextRange, TextSize};

use crate::syntax_kind::SyntaxKind::{self, *};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
}

impl Token {
    pub fn len(&self) -> TextSize {
        self.range.len()
    }
}

/// Tokenizes `src`. Never fails: problems are reported as diagnostics and
/// `ERROR_TOKEN`s so that the output always covers the whole input.
///
/// A bracket that is never closed would swallow the rest of the file, so the
/// lexer can cut such a bracket off at the first following line that looks
/// like the start of a new statement. That heuristic is only applied to
/// brackets known to be unclosed (they are still open at the end of a
/// previous pass), so valid code — whose brackets always balance — is never
/// affected by it.
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut recovery = Recovery {
        suspects: Vec::new(),
        all_from: usize::MAX,
    };
    loop {
        let mut lx = Lexer::new(src, &recovery);
        lx.run();
        match lx.unclosed_at_eof {
            Some(open) if open < recovery.all_from && !recovery.suspects.contains(&open) => {
                if recovery.suspects.len() < MAX_SUSPECTS {
                    recovery.suspects.push(open);
                } else {
                    // Badly broken file: recover every bracket from here on.
                    recovery.all_from = open;
                }
            }
            _ => return (lx.tokens, lx.diags),
        }
    }
}

/// Upper bound on the number of extra lexing passes for unclosed brackets.
const MAX_SUSPECTS: usize = 8;

/// Which top-level brackets may be cut off by the unclosed-bracket heuristic.
struct Recovery {
    /// Start offsets of brackets that a previous pass left open at EOF.
    suspects: Vec<usize>,
    /// Every bracket starting at or after this offset.
    all_from: usize,
}

impl Recovery {
    fn applies_to(&self, open: usize) -> bool {
        open >= self.all_from || self.suspects.contains(&open)
    }
}

/// Python identifiers: `XID_Start`/`_` followed by `XID_Continue` characters.
pub fn is_ident_start(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || c == '_'
    } else {
        unicode_ident::is_xid_start(c)
    }
}

/// See [`is_ident_start`].
pub fn is_ident_continue(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '_'
    } else {
        unicode_ident::is_xid_continue(c)
    }
}

#[derive(Clone, Debug)]
enum Mode {
    /// Regular Python code; `nesting` counts open brackets in this mode.
    Code {
        nesting: usize,
        in_fstring_expr: bool,
    },
    /// Literal part of an f/t-string.
    StringMiddle(StringCtx),
    /// Format spec after `:` inside a replacement field.
    FormatSpec(StringCtx),
}

#[derive(Clone, Copy, Debug)]
struct StringCtx {
    quote: char,
    triple: bool,
    raw: bool,
    template: bool,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
    indents: Vec<(usize, usize)>,
    at_line_start: bool,
    line_has_tokens: bool,
    modes: Vec<Mode>,
    /// `(start offset, indentation column of the opening line)` of the
    /// outermost currently open top-level bracket, when the unclosed-bracket
    /// heuristic applies to it (see [`tokenize`]).
    outer_bracket: Option<(usize, usize)>,
    /// Start offset of the outermost currently open top-level bracket.
    top_open: Option<usize>,
    /// Set at EOF when a top-level bracket was never closed.
    unclosed_at_eof: Option<usize>,
    recovery: &'a Recovery,
    /// Indentation column of the current logical line.
    current_indent: usize,
    /// Memo of [`Lexer::next_code_line`]: scanning from the first offset
    /// found the next code line at the second one.
    next_code_line: (usize, usize),
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, recovery: &'a Recovery) -> Self {
        Self {
            src,
            pos: 0,
            tokens: Vec::with_capacity(src.len() / 4),
            diags: Vec::new(),
            indents: vec![(0, 0)],
            at_line_start: true,
            line_has_tokens: false,
            modes: vec![Mode::Code {
                nesting: 0,
                in_fstring_expr: false,
            }],
            outer_bracket: None,
            top_open: None,
            unclosed_at_eof: None,
            recovery,
            current_indent: 0,
            next_code_line: (usize::MAX, 0),
        }
    }

    // ----- helpers --------------------------------------------------------

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }
    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s)
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    /// Consumes a line break: `\r\n`, `\n` or a lone `\r`.
    fn bump_newline(&mut self) {
        if self.starts_with("\r\n") {
            self.pos += 2;
        } else {
            self.bump();
        }
    }
    fn emit(&mut self, kind: SyntaxKind, start: usize) {
        self.emit_range(kind, start, self.pos);
    }
    fn emit_range(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        if !kind.is_trivia() && !matches!(kind, NEWLINE | INDENT | DEDENT) {
            self.line_has_tokens = true;
        }
        self.tokens.push(Token {
            kind,
            range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
        });
    }
    fn error(&mut self, start: usize, end: usize, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(
            TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
            "syntax",
            msg,
        ));
    }
    fn nesting(&self) -> usize {
        match self.modes.last() {
            Some(Mode::Code { nesting, .. }) => *nesting,
            _ => 1,
        }
    }
    fn nesting_mut(&mut self) -> Option<&mut usize> {
        match self.modes.last_mut() {
            Some(Mode::Code { nesting, .. }) => Some(nesting),
            _ => None,
        }
    }

    // ----- main loop ------------------------------------------------------

    fn run(&mut self) {
        // A UTF-8 byte order mark is not part of the program (CPython strips it).
        if self.src.starts_with('\u{feff}') {
            self.pos = '\u{feff}'.len_utf8();
            self.emit(WHITESPACE, 0);
        }
        while self.pos < self.src.len() {
            match self.modes.last().cloned().expect("mode stack never empty") {
                Mode::Code { .. } => self.lex_code(),
                Mode::StringMiddle(ctx) => self.lex_string_middle(ctx, false),
                Mode::FormatSpec(ctx) => self.lex_string_middle(ctx, true),
            }
        }
        if let Some(Mode::Code { nesting, .. }) = self.modes.first() {
            if *nesting > 0 {
                self.unclosed_at_eof = self.top_open;
            }
        }
        // Close unterminated f-strings.
        while self.modes.len() > 1 {
            let m = self.modes.pop().unwrap();
            if let Mode::StringMiddle(_) | Mode::FormatSpec(_) = m {
                self.error(self.pos, self.pos, "unterminated f-string literal");
            }
        }
        if self.line_has_tokens {
            self.emit_range(NEWLINE, self.pos, self.pos);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.emit_range(DEDENT, self.pos, self.pos);
        }
    }

    fn lex_code(&mut self) {
        if self.at_line_start && self.nesting() == 0 {
            self.at_line_start = false;
            self.lex_indentation();
            return;
        }
        let start = self.pos;
        let c = match self.peek() {
            Some(c) => c,
            None => return,
        };
        match c {
            ' ' | '\t' | '\x0c' => {
                while matches!(self.peek(), Some(' ' | '\t' | '\x0c')) {
                    self.bump();
                }
                self.emit(WHITESPACE, start);
            }
            '\\' => {
                self.bump();
                if matches!(self.peek(), Some('\n' | '\r')) {
                    self.bump_newline();
                    if self.pos == self.src.len() {
                        self.error(
                            start,
                            self.pos,
                            "unexpected end of file after line continuation",
                        );
                    }
                    self.emit(WHITESPACE, start);
                } else if self.peek().is_none() {
                    self.error(
                        start,
                        self.pos,
                        "unexpected end of file after line continuation",
                    );
                    self.emit(WHITESPACE, start);
                } else {
                    self.error(
                        start,
                        self.pos,
                        "unexpected character after line continuation character",
                    );
                    self.emit(ERROR_TOKEN, start);
                }
            }
            '#' => {
                while !matches!(self.peek(), Some('\n' | '\r') | None) {
                    self.bump();
                }
                self.emit(COMMENT, start);
            }
            '\n' | '\r' => {
                self.bump();
                if c == '\r' && self.peek() == Some('\n') {
                    self.bump();
                }
                let in_expr = matches!(
                    self.modes.last(),
                    Some(Mode::Code {
                        in_fstring_expr: true,
                        ..
                    })
                );
                if self.nesting() > 0
                    && !in_expr
                    && self.modes.len() == 1
                    && self.unclosed_bracket_recovery()
                {
                    // Pretend the bracket was closed: behave like a regular logical newline.
                    if let Some(n) = self.nesting_mut() {
                        *n = 0;
                    }
                }
                if self.nesting() == 0 && !in_expr && self.line_has_tokens {
                    self.emit(NEWLINE, start);
                    self.line_has_tokens = false;
                    self.at_line_start = true;
                } else {
                    self.emit(NL, start);
                    if self.nesting() == 0 && !in_expr {
                        self.at_line_start = true;
                    }
                }
            }
            c if c.is_ascii_digit()
                || (c == '.' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                self.lex_number()
            }
            c if is_ident_start(c) => self.lex_ident_or_string(),
            '"' | '\'' => self.lex_string(start, StringFlags::default()),
            _ => self.lex_punct(),
        }
    }

    /// Called at a newline inside brackets: if the next non-blank line starts
    /// at an indentation no deeper than the line that opened the outermost
    /// bracket and begins with a statement keyword, the bracket is reported as
    /// never closed and lexing resumes in statement mode. Only brackets that
    /// a previous pass found unclosed are candidates (see [`tokenize`]).
    fn unclosed_bracket_recovery(&mut self) -> bool {
        let Some((open_pos, open_indent)) = self.outer_bracket else {
            return false;
        };
        let next = self.next_code_line();
        if next.is_empty() {
            return false;
        }
        let indent = next
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .map(|b| if b == b'\t' { 8 } else { 1 })
            .sum::<usize>();
        let word: String = next
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // Keywords that can never continue an expression inside brackets.
        const ALWAYS: &[&str] = &[
            "def", "class", "return", "elif", "while", "with", "try", "except", "finally",
            "import", "pass", "raise", "break", "continue", "global", "nonlocal", "assert", "del",
        ];
        // Keywords that may appear inside expressions; only trust them when the
        // line is not indented deeper than the line that opened the bracket.
        const DEDENTED_ONLY: &[&str] = &[
            "if", "else", "for", "from", "async", "await", "yield", "not", "lambda", "match",
            "case",
        ];
        let is_decorator = next.trim_start().starts_with('@');
        let recover = ALWAYS.contains(&word.as_str())
            || (indent <= open_indent && (DEDENTED_ONLY.contains(&word.as_str()) || is_decorator));
        if !recover {
            return false;
        }
        let ch = self.src.as_bytes()[open_pos] as char;
        self.error(open_pos, open_pos + 1, format!("'{ch}' was never closed"));
        self.outer_bracket = None;
        self.top_open = None;
        true
    }

    /// The next line at or after `self.pos` (a line start) that is neither
    /// blank nor comment-only, with its line break; empty at the end of the
    /// input. Memoized so that a long run of blank or comment lines inside an
    /// unclosed bracket is scanned once instead of once per line.
    fn next_code_line(&mut self) -> &'a str {
        let (from, found) = self.next_code_line;
        let start = if from <= self.pos && self.pos <= found {
            found
        } else {
            let mut line_start = self.pos;
            for line in self.src[self.pos..].split_inclusive(['\n', '\r']) {
                let t = line.trim_start_matches([' ', '\t', '\x0c']);
                if !t.is_empty() && !t.starts_with(['\n', '\r', '#']) {
                    break;
                }
                line_start += line.len();
            }
            self.next_code_line = (self.pos, line_start);
            line_start
        };
        let rest = &self.src[start..];
        &rest[..rest.find(['\n', '\r']).map_or(rest.len(), |i| i + 1)]
    }

    fn lex_indentation(&mut self) {
        let start = self.pos;
        let (mut col, mut altcol) = (0usize, 0usize);
        // Like CPython 3.12+, indentation may be continued over several
        // physical lines with backslashes; the column of the first backslash
        // found at a non-zero column is then the indentation of the line.
        let mut continued_col = 0usize;
        // End of the last backslash continuation inside the indentation.
        let mut continued_end = start;
        loop {
            match self.peek() {
                Some(' ') => {
                    col += 1;
                    altcol += 1;
                }
                Some('\t') => {
                    col = (col / 8 + 1) * 8;
                    altcol += 1;
                }
                Some('\x0c') => {
                    col = 0;
                    altcol = 0;
                }
                Some('\\') if matches!(self.peek_at(1), Some('\n' | '\r')) => {
                    if continued_col == 0 {
                        continued_col = col;
                    }
                    let backslash = self.pos;
                    self.bump();
                    self.bump_newline();
                    continued_end = self.pos;
                    if self.pos == self.src.len() {
                        self.error(
                            backslash,
                            self.pos,
                            "unexpected end of file after line continuation",
                        );
                    }
                    continue;
                }
                _ => break,
            }
            self.bump();
        }
        // Blank or comment-only lines do not affect indentation.
        if matches!(self.peek(), None | Some('\n' | '\r' | '#')) {
            if self.pos > start {
                self.emit(WHITESPACE, start);
            }
            return;
        }
        if continued_col > 0 {
            col = continued_col;
            altcol = continued_col;
        }
        self.current_indent = col;
        let (top, alttop) = *self.indents.last().unwrap();
        if col > top {
            if altcol <= alttop {
                self.error(
                    start,
                    self.pos,
                    "inconsistent use of tabs and spaces in indentation",
                );
            }
            self.indents.push((col, altcol));
            // The continuation lines are trivia; INDENT only covers the
            // whitespace in front of the first token.
            if continued_end > start {
                self.emit_range(WHITESPACE, start, continued_end);
            }
            self.emit_range(INDENT, continued_end, self.pos);
        } else {
            if self.pos > start {
                self.emit(WHITESPACE, start);
            }
            if col < top {
                while self.indents.len() > 1 && col < self.indents.last().unwrap().0 {
                    self.indents.pop();
                    self.emit_range(DEDENT, self.pos, self.pos);
                }
                let (t, at) = *self.indents.last().unwrap();
                if col != t {
                    self.error(
                        start,
                        self.pos,
                        "unindent does not match any outer indentation level",
                    );
                    self.indents.push((col, altcol));
                } else if altcol != at {
                    self.error(
                        start,
                        self.pos,
                        "inconsistent use of tabs and spaces in indentation",
                    );
                }
            } else if altcol != alttop {
                self.error(
                    start,
                    self.pos,
                    "inconsistent use of tabs and spaces in indentation",
                );
            }
        }
    }

    /// Consumes digits matching `pred` with single `_` separators between
    /// them (and, if `leading_underscore`, before the first one: `0x_ff`).
    fn digits(&mut self, pred: fn(char) -> bool, leading_underscore: bool) -> bool {
        let mut any = false;
        loop {
            match self.peek() {
                Some(c) if pred(c) => {
                    self.bump();
                    any = true;
                }
                Some('_') if (any || leading_underscore) && self.peek_at(1).is_some_and(pred) => {
                    self.bump();
                }
                _ => break,
            }
        }
        any
    }

    /// CPython accepts (with a warning) a number that is immediately followed
    /// by a keyword which can follow a number in valid code: `1if x else y`,
    /// `[0x1for x in y]`, `1or 2`.
    fn keyword_follows(&self) -> bool {
        let rest = &self.src[self.pos..];
        ["and", "else", "for", "if", "in", "is", "not", "or"]
            .iter()
            .any(|kw| rest.starts_with(kw))
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        let mut kind = INT_NUMBER;
        let mut name = "decimal";
        if self.peek() == Some('0')
            && matches!(self.peek_at(1), Some('x' | 'X' | 'o' | 'O' | 'b' | 'B'))
        {
            self.bump();
            let radix = self.bump().map_or('x', |c| c.to_ascii_lowercase());
            let any = match radix {
                'x' => {
                    name = "hexadecimal";
                    self.digits(|c| c.is_ascii_hexdigit(), true)
                }
                'o' => {
                    name = "octal";
                    self.digits(|c| matches!(c, '0'..='7'), true)
                }
                _ => {
                    name = "binary";
                    self.digits(|c| matches!(c, '0' | '1'), true)
                }
            };
            if let Some(d) = self.peek().filter(char::is_ascii_digit) {
                // `0o8`, `0b12`: keep the bad digits inside the token.
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                    self.bump();
                }
                self.error(
                    start,
                    self.pos,
                    format!("invalid digit '{d}' in {name} literal"),
                );
                self.emit(INT_NUMBER, start);
                return;
            }
            if !any {
                self.error(start, self.pos, format!("invalid {name} literal"));
            }
        } else {
            self.digits(|c| c.is_ascii_digit(), false);
            let int_end = self.pos;
            // `1.` is a float even when another `.` follows: `1..real`.
            if self.peek() == Some('.') {
                self.bump();
                self.digits(|c| c.is_ascii_digit(), false);
                kind = FLOAT_NUMBER;
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                let save = self.pos;
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                if self.digits(|c| c.is_ascii_digit(), false) {
                    kind = FLOAT_NUMBER;
                } else {
                    self.pos = save;
                }
            }
            if matches!(self.peek(), Some('j' | 'J')) {
                self.bump();
                kind = COMPLEX_NUMBER;
                name = "imaginary";
            } else if kind == INT_NUMBER {
                let int = &self.src[start..int_end];
                if int.starts_with('0') && int.bytes().any(|b| matches!(b, b'1'..=b'9')) {
                    self.error(start, int_end, "leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers");
                }
            }
        }
        // `1abc`, `0x1g`, `1_`: a number directly followed by a name character.
        if self
            .peek()
            .is_some_and(|c| c.is_ascii() && is_ident_continue(c))
            && !self.keyword_follows()
        {
            self.error(start, self.pos, format!("invalid {name} literal"));
        }
        self.emit(kind, start);
    }

    fn lex_ident_or_string(&mut self) {
        let start = self.pos;
        // String prefix?
        let mut flags = StringFlags::default();
        let mut n = 0;
        let mut valid_prefix = true;
        while n < 2 {
            match self.peek_at(n) {
                Some('r' | 'R') if !flags.raw => flags.raw = true,
                Some('b' | 'B')
                    if !flags.bytes && !flags.unicode && !flags.fstring && !flags.template =>
                {
                    flags.bytes = true
                }
                Some('u' | 'U') if n == 0 => flags.unicode = true,
                Some('f' | 'F')
                    if !flags.fstring && !flags.bytes && !flags.unicode && !flags.template =>
                {
                    flags.fstring = true
                }
                Some('t' | 'T')
                    if !flags.template && !flags.bytes && !flags.unicode && !flags.fstring =>
                {
                    flags.template = true
                }
                _ => {
                    valid_prefix = false;
                    break;
                }
            }
            n += 1;
            if matches!(self.peek_at(n), Some('"' | '\'')) {
                break;
            }
            if n == 2 {
                valid_prefix = false;
            }
        }
        if valid_prefix && (flags.unicode && (flags.raw || n != 1)) {
            valid_prefix = false;
        }
        if valid_prefix && n > 0 && matches!(self.peek_at(n), Some('"' | '\'')) {
            self.pos += n;
            self.lex_string(start, flags);
            return;
        }
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let text = &self.src[start..self.pos];
        let kind = SyntaxKind::from_keyword(text).unwrap_or(IDENT);
        self.emit(kind, start);
    }

    fn lex_string(&mut self, start: usize, flags: StringFlags) {
        let quote = self.bump().expect("quote");
        let triple = self.peek() == Some(quote) && self.peek_at(1) == Some(quote);
        if triple {
            self.pos += 2;
        }
        if flags.fstring || flags.template {
            let ctx = StringCtx {
                quote,
                triple,
                raw: flags.raw,
                template: flags.template,
            };
            self.emit(
                if flags.template {
                    TSTRING_START
                } else {
                    FSTRING_START
                },
                start,
            );
            self.modes.push(Mode::StringMiddle(ctx));
            return;
        }
        loop {
            match self.peek() {
                None => {
                    self.error(
                        start,
                        self.pos,
                        if triple {
                            "unterminated triple-quoted string literal"
                        } else {
                            "unterminated string literal"
                        },
                    );
                    break;
                }
                Some('\\') => {
                    self.bump();
                    if self.starts_with("\r\n") {
                        self.pos += 2;
                    } else {
                        self.bump();
                    }
                }
                Some('\n' | '\r') if !triple => {
                    self.error(start, self.pos, "unterminated string literal");
                    break;
                }
                Some(c) if c == quote => {
                    if triple {
                        if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                            self.pos += 3;
                            break;
                        }
                        self.bump();
                    } else {
                        self.bump();
                        break;
                    }
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
        self.emit(STRING, start);
    }

    /// Lexes literal text of an f/t-string (or a format spec) until `{`, the
    /// closing quote, or (for format specs) the closing `}`.
    fn lex_string_middle(&mut self, ctx: StringCtx, format_spec: bool) {
        let start = self.pos;
        let middle = if ctx.template {
            TSTRING_MIDDLE
        } else {
            FSTRING_MIDDLE
        };
        let end_kind = if ctx.template {
            TSTRING_END
        } else {
            FSTRING_END
        };
        loop {
            match self.peek() {
                None => {
                    if self.pos > start {
                        self.emit(middle, start);
                    }
                    self.error(start, self.pos, "unterminated f-string literal");
                    self.modes.pop();
                    return;
                }
                Some('\\') => {
                    // In raw strings too, a backslash keeps the next character
                    // (a quote, a backslash, a newline) from ending the string.
                    self.bump();
                    match self.peek() {
                        // `\{` / `\}`: the brace keeps its meaning (CPython warns
                        // about the invalid escape in non-raw strings).
                        Some('{' | '}') | None => {}
                        Some('N') if !ctx.raw && self.peek_at(1) == Some('{') => {
                            // Named unicode escape `\N{...}`: the braces are literal.
                            self.pos += 2;
                            while !matches!(self.peek(), Some('}' | '\n' | '\r') | None) {
                                self.bump();
                            }
                            if self.peek() == Some('}') {
                                self.bump();
                            }
                        }
                        Some(_) if self.starts_with("\r\n") => self.pos += 2,
                        Some(_) => {
                            self.bump();
                        }
                    }
                }
                Some('{') => {
                    if self.peek_at(1) == Some('{') && !format_spec {
                        self.pos += 2;
                        continue;
                    }
                    if self.pos > start {
                        self.emit(middle, start);
                    }
                    let s = self.pos;
                    self.bump();
                    self.emit(L_BRACE, s);
                    self.modes.push(Mode::Code {
                        nesting: 0,
                        in_fstring_expr: true,
                    });
                    return;
                }
                Some('}') => {
                    if format_spec {
                        if self.pos > start {
                            self.emit(middle, start);
                        }
                        let s = self.pos;
                        self.bump();
                        self.emit(R_BRACE, s);
                        // Leave the format spec and the replacement field.
                        self.modes.pop();
                        return;
                    }
                    if self.peek_at(1) == Some('}') {
                        self.pos += 2;
                        continue;
                    }
                    self.error(
                        self.pos,
                        self.pos + 1,
                        "f-string: single '}' is not allowed",
                    );
                    self.bump();
                }
                Some('\n' | '\r') if !ctx.triple => {
                    if self.pos > start {
                        self.emit(middle, start);
                    }
                    self.error(start, self.pos, "unterminated f-string literal");
                    self.modes.pop();
                    return;
                }
                Some(c) if c == ctx.quote && !format_spec => {
                    let closes = !ctx.triple
                        || (self.peek_at(1) == Some(ctx.quote)
                            && self.peek_at(2) == Some(ctx.quote));
                    if closes {
                        if self.pos > start {
                            self.emit(middle, start);
                        }
                        let s = self.pos;
                        self.pos += if ctx.triple { 3 } else { 1 };
                        self.emit(end_kind, s);
                        self.modes.pop();
                        return;
                    }
                    self.bump();
                }
                Some(c) if c == ctx.quote && format_spec && !ctx.triple => {
                    // Quote inside a format spec terminates the string (error recovery).
                    if self.pos > start {
                        self.emit(middle, start);
                    }
                    self.error(start, self.pos, "f-string: expecting '}'");
                    self.modes.pop();
                    return;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn lex_punct(&mut self) {
        let start = self.pos;
        let in_fstring_expr = matches!(
            self.modes.last(),
            Some(Mode::Code {
                in_fstring_expr: true,
                ..
            })
        );
        const THREE: &[(&str, SyntaxKind)] = &[
            ("...", ELLIPSIS),
            ("//=", SLASH_SLASH_EQ),
            (">>=", SHR_EQ),
            ("<<=", SHL_EQ),
            ("**=", STAR_STAR_EQ),
        ];
        const TWO: &[(&str, SyntaxKind)] = &[
            ("->", ARROW),
            ("+=", PLUS_EQ),
            ("-=", MINUS_EQ),
            ("*=", STAR_EQ),
            ("/=", SLASH_EQ),
            ("%=", PERCENT_EQ),
            ("@=", AT_EQ),
            ("&=", AMP_EQ),
            ("|=", PIPE_EQ),
            ("^=", CARET_EQ),
            ("**", STAR_STAR),
            ("//", SLASH_SLASH),
            ("<<", SHL),
            (">>", SHR),
            (":=", COLON_EQ),
            ("<=", LT_EQ),
            (">=", GT_EQ),
            ("==", EQ_EQ),
            ("!=", NOT_EQ),
            ("<>", NOT_EQ),
        ];
        for (s, k) in THREE {
            if self.starts_with(s) {
                self.pos += 3;
                self.emit(*k, start);
                return;
            }
        }
        for (s, k) in TWO {
            if self.starts_with(s) {
                // In an f-string replacement field, `:=` at depth 0 is a format spec.
                if *k == COLON_EQ && in_fstring_expr && self.nesting() == 0 {
                    break;
                }
                // `<>` is reported by the parser: it is valid after
                // `from __future__ import barry_as_FLUFL`.
                self.pos += 2;
                self.emit(*k, start);
                return;
            }
        }
        let c = self.bump().unwrap();
        let kind = match c {
            '(' | '[' | '{' => {
                let top_level = self.modes.len() == 1;
                let indent = self.current_indent;
                let was_zero = self.nesting() == 0;
                if let Some(n) = self.nesting_mut() {
                    *n += 1;
                }
                if was_zero && top_level {
                    self.top_open = Some(start);
                    if self.recovery.applies_to(start) {
                        self.outer_bracket = Some((start, indent));
                    }
                }
                match c {
                    '(' => L_PAREN,
                    '[' => L_BRACK,
                    _ => L_BRACE,
                }
            }
            ')' | ']' | '}' => {
                let depth = self.nesting();
                if depth == 0 && in_fstring_expr {
                    if c == '}' {
                        // End of replacement field.
                        self.modes.pop();
                        self.emit(R_BRACE, start);
                        return;
                    }
                    self.error(start, self.pos, "f-string: unmatched bracket");
                } else if depth == 0 {
                    self.error(start, self.pos, format!("unmatched '{c}'"));
                } else {
                    let mut closed_all = false;
                    if let Some(n) = self.nesting_mut() {
                        *n -= 1;
                        closed_all = *n == 0;
                    }
                    if closed_all && self.modes.len() == 1 {
                        self.outer_bracket = None;
                        self.top_open = None;
                    }
                }
                match c {
                    ')' => R_PAREN,
                    ']' => R_BRACK,
                    _ => R_BRACE,
                }
            }
            ':' => {
                if in_fstring_expr && self.nesting() == 0 {
                    // Start of a format spec.
                    self.emit(COLON, start);
                    // The enclosing string: a format spec carries the context
                    // of its string, so the search stops right below.
                    let ctx = match self.modes.iter().rev().find_map(|m| match m {
                        Mode::StringMiddle(c) | Mode::FormatSpec(c) => Some(*c),
                        Mode::Code { .. } => None,
                    }) {
                        Some(c) => c,
                        None => return,
                    };
                    // Replace the expression mode by the format-spec mode; the
                    // `}` that ends the spec also ends the replacement field.
                    self.modes.pop();
                    self.modes.push(Mode::FormatSpec(ctx));
                    return;
                }
                COLON
            }
            ',' => COMMA,
            '.' => DOT,
            ';' => SEMICOLON,
            '@' => AT,
            '=' => EQ,
            '+' => PLUS,
            '-' => MINUS,
            '*' => STAR,
            '/' => SLASH,
            '%' => PERCENT,
            '&' => AMP,
            '|' => PIPE,
            '^' => CARET,
            '~' => TILDE,
            '<' => LT,
            '>' => GT,
            '!' => {
                if !in_fstring_expr {
                    self.error(start, self.pos, "invalid syntax");
                }
                BANG
            }
            _ => {
                let msg = match c {
                    '$' | '?' | '`' => format!("invalid character '{c}'"),
                    c if !c.is_ascii() => {
                        format!("invalid non-printable or non-ASCII character '{c}'")
                    }
                    _ => format!("invalid character '{c}'"),
                };
                self.error(start, self.pos, msg);
                ERROR_TOKEN
            }
        };
        self.emit(kind, start);
    }
}

#[derive(Default, Clone, Copy, Debug)]
struct StringFlags {
    raw: bool,
    bytes: bool,
    unicode: bool,
    fstring: bool,
    template: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        tokenize(src)
            .0
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| !matches!(k, WHITESPACE))
            .collect()
    }

    fn texts(src: &str) -> Vec<(SyntaxKind, String)> {
        tokenize(src)
            .0
            .into_iter()
            .map(|t| (t.kind, src[t.range].to_string()))
            .collect()
    }

    #[test]
    fn simple_statement() {
        assert_eq!(kinds("x = 1\n"), vec![IDENT, EQ, INT_NUMBER, NEWLINE]);
        assert_eq!(kinds("x = 1"), vec![IDENT, EQ, INT_NUMBER, NEWLINE]);
    }

    #[test]
    fn indentation() {
        let k = kinds("if x:\n    y\n    z\nw\n");
        assert_eq!(
            k,
            vec![
                IF_KW, IDENT, COLON, NEWLINE, INDENT, IDENT, NEWLINE, IDENT, NEWLINE, DEDENT,
                IDENT, NEWLINE
            ]
        );
        let k = kinds("def f():\n  if a:\n    b\nc");
        assert_eq!(
            k,
            vec![
                DEF_KW, IDENT, L_PAREN, R_PAREN, COLON, NEWLINE, INDENT, IF_KW, IDENT, COLON,
                NEWLINE, INDENT, IDENT, NEWLINE, DEDENT, DEDENT, IDENT, NEWLINE
            ]
        );
    }

    #[test]
    fn blank_and_comment_lines_are_nl() {
        assert_eq!(
            kinds("x\n\n# c\n   \ny\n"),
            vec![IDENT, NEWLINE, NL, COMMENT, NL, NL, IDENT, NEWLINE]
        );
    }

    #[test]
    fn brackets_join_lines() {
        assert_eq!(
            kinds("f(1,\n  2)\n"),
            vec![IDENT, L_PAREN, INT_NUMBER, COMMA, NL, INT_NUMBER, R_PAREN, NEWLINE]
        );
        assert_eq!(
            kinds("x = 1 + \\\n 2\n"),
            vec![IDENT, EQ, INT_NUMBER, PLUS, INT_NUMBER, NEWLINE]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("0x_FF 0o17 0b1_0 1_000 3.14 .5 1e10 2.j 1J 10"),
            vec![
                INT_NUMBER,
                INT_NUMBER,
                INT_NUMBER,
                INT_NUMBER,
                FLOAT_NUMBER,
                FLOAT_NUMBER,
                FLOAT_NUMBER,
                COMPLEX_NUMBER,
                COMPLEX_NUMBER,
                INT_NUMBER,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("x[1:2]"),
            vec![IDENT, L_BRACK, INT_NUMBER, COLON, INT_NUMBER, R_BRACK, NEWLINE]
        );
        assert_eq!(kinds("a...b"), vec![IDENT, ELLIPSIS, IDENT, NEWLINE]);
    }

    #[test]
    fn strings() {
        assert_eq!(
            kinds(r#"'a' "b" '''c''' r"\d" b'x' Rb'y' u'z'"#),
            vec![STRING; 7]
                .into_iter()
                .chain([NEWLINE])
                .collect::<Vec<_>>()
        );
        assert_eq!(kinds("'it\\'s'"), vec![STRING, NEWLINE]);
        let (toks, diags) = tokenize("'abc\nx");
        assert_eq!(
            toks.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![STRING, NEWLINE, IDENT, NEWLINE]
        );
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn fstrings_pep701() {
        assert_eq!(
            texts("f'a{b}c'"),
            vec![
                (FSTRING_START, "f'".into()),
                (FSTRING_MIDDLE, "a".into()),
                (L_BRACE, "{".into()),
                (IDENT, "b".into()),
                (R_BRACE, "}".into()),
                (FSTRING_MIDDLE, "c".into()),
                (FSTRING_END, "'".into()),
                (NEWLINE, "".into())
            ]
        );
        assert_eq!(
            kinds("f'{x!r:>{w}}'"),
            vec![
                FSTRING_START,
                L_BRACE,
                IDENT,
                BANG,
                IDENT,
                COLON,
                FSTRING_MIDDLE,
                L_BRACE,
                IDENT,
                R_BRACE,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("f'{x=}'"),
            vec![
                FSTRING_START,
                L_BRACE,
                IDENT,
                EQ,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("f'{{}}{d[\"k\"]}'"),
            vec![
                FSTRING_START,
                FSTRING_MIDDLE,
                L_BRACE,
                IDENT,
                L_BRACK,
                STRING,
                R_BRACK,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("f'{f\"{1}\"}'"),
            vec![
                FSTRING_START,
                L_BRACE,
                FSTRING_START,
                L_BRACE,
                INT_NUMBER,
                R_BRACE,
                FSTRING_END,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("f'\\N{DASH}{x}'"),
            vec![
                FSTRING_START,
                FSTRING_MIDDLE,
                L_BRACE,
                IDENT,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("f'''\n{a +\n b}\n'''"),
            vec![
                FSTRING_START,
                FSTRING_MIDDLE,
                L_BRACE,
                IDENT,
                PLUS,
                NL,
                IDENT,
                R_BRACE,
                FSTRING_MIDDLE,
                FSTRING_END,
                NEWLINE
            ]
        );
        assert_eq!(
            kinds("t'{x}'"),
            vec![TSTRING_START, L_BRACE, IDENT, R_BRACE, TSTRING_END, NEWLINE]
        );
        assert_eq!(
            kinds("f'{x:{\"a\"}}'"),
            vec![
                FSTRING_START,
                L_BRACE,
                IDENT,
                COLON,
                L_BRACE,
                STRING,
                R_BRACE,
                R_BRACE,
                FSTRING_END,
                NEWLINE
            ]
        );
    }

    #[test]
    fn operators_and_errors() {
        assert_eq!(
            kinds("a **= b // c -> d := e"),
            vec![
                IDENT,
                STAR_STAR_EQ,
                IDENT,
                SLASH_SLASH,
                IDENT,
                ARROW,
                IDENT,
                COLON_EQ,
                IDENT,
                NEWLINE
            ]
        );
        let (toks, diags) = tokenize("a $ b");
        assert_eq!(
            toks.iter()
                .map(|t| t.kind)
                .filter(|k| *k != WHITESPACE)
                .collect::<Vec<_>>(),
            vec![IDENT, ERROR_TOKEN, IDENT, NEWLINE]
        );
        assert_eq!(diags[0].message, "invalid character '$'");
        let (_, diags) = tokenize("if x:\n    a\n  b\n");
        assert!(diags.iter().any(|d| d.message.contains("unindent")));
        let (_, diags) = tokenize(")");
        assert_eq!(diags[0].message, "unmatched ')'");
    }

    #[test]
    fn unclosed_bracket_recovery() {
        let (toks, diags) = tokenize("def f(x:\n    return x\n\ndef g():\n    pass\n");
        assert!(
            diags.iter().any(|d| d.message == "'(' was never closed"),
            "{diags:?}"
        );
        let kinds: Vec<SyntaxKind> = toks
            .iter()
            .map(|t| t.kind)
            .filter(|k| !k.is_trivia())
            .collect();
        // The newline after `x:` becomes a logical NEWLINE followed by INDENT.
        assert_eq!(
            &kinds[..9],
            &[DEF_KW, IDENT, L_PAREN, IDENT, COLON, NEWLINE, INDENT, RETURN_KW, IDENT],
            "{kinds:?}"
        );
        assert_eq!(kinds.iter().filter(|k| **k == DEF_KW).count(), 2);
        // Expression keywords on deeper-indented lines do not trigger recovery.
        let (_, diags) = tokenize("x = (a\n     if b else c)\n");
        assert!(diags.is_empty(), "{diags:?}");
        let (_, diags) = tokenize("y = [i\n     for i in z]\n");
        assert!(diags.is_empty(), "{diags:?}");
        // Continuation lines deeper than the opening line stay inside the bracket.
        let (_, diags) = tokenize("x = foo(\n    a,\n    b)\n");
        assert!(diags.is_empty(), "{diags:?}");
        let (_, diags) = tokenize("items = [\n    1,\n]\nif x:\n    pass\n");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn soft_keywords_are_idents() {
        assert_eq!(
            kinds("match x:\n case _:\n  pass"),
            vec![
                IDENT, IDENT, COLON, NEWLINE, INDENT, IDENT, IDENT, COLON, NEWLINE, INDENT,
                PASS_KW, NEWLINE, DEDENT, DEDENT
            ]
        );
    }

    #[test]
    fn covers_whole_input() {
        for src in [
            "x = 1\nif x:\n  y\n",
            "f'{a}' + 'b'",
            "a\r\nb\r\n",
            "'unterminated",
            "   \n\t\n",
            "def f(\n a,\n):\n pass",
        ] {
            let (toks, _) = tokenize(src);
            let mut pos = 0u32;
            for t in &toks {
                assert_eq!(
                    u32::from(t.range.start()),
                    pos,
                    "gap before {:?} in {src:?}",
                    t
                );
                pos = t.range.end().into();
            }
            assert_eq!(pos as usize, src.len(), "tokens must cover {src:?}");
        }
    }
}

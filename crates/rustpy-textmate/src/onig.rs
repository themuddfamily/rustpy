//! Parser for Oniguruma regular expressions — the dialect TextMate grammars
//! are written in — into a small AST that [`crate::matcher`] translates to
//! `regex` crate syntax.
//!
//! Semantics follow Oniguruma's default syntax as used by vscode-textmate
//! (verified against libonig): `(?m)` means "dot matches newline", isolated
//! options such as `ab(?i)c|d` extend to the end of the enclosing group
//! (`ab(?i:c|d)`), `{n,m}+` is a greedy interval followed by `+` (not
//! possessive), `{n,m}` with `n > m` is a possessive `{m,n}`, `\x{41 42}` is a
//! code point list, `\p` without braces is a literal `p`, and extended mode
//! `(?x)` strips whitespace/comments outside character classes. Possessive
//! quantifiers and atomic groups are approximated by their backtracking forms.
//! Constructs the `regex` crate cannot express (back-references, subexpression
//! calls, conditionals, absent operators) are rejected with an error message.

use std::collections::{HashMap, HashSet};

/// Upper bound accepted for `{n,m}` (Oniguruma's own limit).
const MAX_REPEAT: u32 = 100_000;
/// Maximum nesting of groups / classes (guards the recursion).
const MAX_NESTING: usize = 128;
/// `\h`: a hexadecimal digit.
pub(crate) const HEX_ITEMS: &str = "0-9a-fA-F";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Empty,
    Char(char),
    Dot,
    Class(Class),
    /// A pre-translated `regex` fragment that is safe to quantify.
    Raw(&'static str),
    Assert(Assert),
    Group(Box<Group>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Repeat>),
    Look(Box<Look>),
    /// `\K`: the reported match starts here.
    Keep,
    /// Internal: a consumed trailing look-ahead; the reported match ends at
    /// the start of this group.
    Tail(Box<Node>),
    /// `\g<…>`: a subexpression call. Only exists while parsing: [`parse`]
    /// inlines calls.
    Call(GroupRef),
    /// `\1`, `\k<…>`: a back-reference. Only exists while parsing: [`parse`]
    /// expands back-references to groups with few possible texts.
    BackRef(GroupRef),
}

/// The group a call or back-reference refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GroupRef {
    Number(usize),
    Name(String),
}

/// A character class in `regex` syntax: `items` is valid between brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Class {
    pub negated: bool,
    pub items: String,
}

impl Class {
    /// The class as a bracket expression, usable as an atom or nested item.
    pub(crate) fn to_bracket(&self) -> String {
        format!("[{}{}]", if self.negated { "^" } else { "" }, self.items)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Assert {
    /// `^`
    LineStart,
    /// `$`
    LineEnd,
    /// `\A`
    TextStart,
    /// `\z`
    TextEnd,
    /// `\Z`
    TextEndNewline,
    /// `\b`
    WordBoundary,
    /// `\B`
    NotWordBoundary,
    /// `\G`
    SearchStart,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Group {
    /// Oniguruma capture number (named groups are numbered too).
    pub capture: Option<usize>,
    pub flags: Flags,
    pub node: Node,
}

/// Inline flags that survive translation (`x` is applied by the parser).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    pub case_on: bool,
    pub case_off: bool,
    pub dotall_on: bool,
    pub dotall_off: bool,
}

impl Flags {
    pub(crate) fn is_empty(&self) -> bool {
        !(self.case_on || self.case_off || self.dotall_on || self.dotall_off)
    }

    /// The flag string for a `(?…:…)` group, e.g. `i-s`.
    pub(crate) fn to_regex(self) -> String {
        let mut s = String::new();
        if self.case_on {
            s.push('i');
        }
        if self.dotall_on {
            s.push('s');
        }
        if self.case_off || self.dotall_off {
            s.push('-');
            if self.case_off {
                s.push('i');
            }
            if self.dotall_off {
                s.push('s');
            }
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Repeat {
    pub node: Node,
    pub min: u32,
    pub max: Option<u32>,
    pub greedy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Look {
    pub ahead: bool,
    pub negate: bool,
    pub node: Node,
}

pub(crate) struct Parsed {
    pub node: Node,
    /// Number of capture groups (group 0 excluded).
    pub groups: usize,
}

/// Parses an Oniguruma pattern; subexpression calls and back-references
/// are expanded (see [`expand_references`]).
pub(crate) fn parse(src: &str) -> Result<Parsed, String> {
    let mut p = Parser {
        chars: src.chars().collect(),
        pos: 0,
        groups: 0,
        extended: false,
        depth: 0,
        names: Vec::new(),
    };
    let node = p.parse_alt()?;
    if p.pos < p.chars.len() {
        return Err("unmatched close parenthesis".into());
    }
    let refs = Refs {
        names: &p.names,
        groups: p.groups,
    };
    let node = expand_references(node, &refs)?;
    Ok(Parsed {
        node,
        groups: p.groups,
    })
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    groups: usize,
    extended: bool,
    depth: usize,
    /// Named groups: `(name, group number)`.
    names: Vec<(String, usize)>,
}

enum Item {
    Node(Node),
    /// A code point list such as `\x{41 42}`: a quantifier that follows
    /// applies to the last character only.
    Chars(Vec<char>),
    /// `(?imx-imx)`: applies to the rest of the enclosing group.
    Isolated(Flags, Option<bool>),
    /// A comment group `(?#…)`.
    Nothing,
}

enum ClassAtom {
    Char(char),
    Set(String),
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.chars.get(self.pos + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            Err("pattern nesting too deep".into())
        } else {
            Ok(())
        }
    }

    fn skip_extended(&mut self) {
        if !self.extended {
            return;
        }
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else if c == '#' {
                while let Some(c) = self.bump() {
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn parse_alt(&mut self) -> Result<Node, String> {
        self.enter()?;
        let mut branches = vec![self.parse_concat()?];
        while self.eat('|') {
            branches.push(self.parse_concat()?);
        }
        self.depth -= 1;
        Ok(if branches.len() == 1 {
            branches.swap_remove(0)
        } else {
            Node::Alt(branches)
        })
    }

    fn parse_concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        loop {
            self.skip_extended();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {}
            }
            match self.parse_atom()? {
                Item::Node(atom) => {
                    let atom = self.parse_quantifiers(atom)?;
                    items.push(atom);
                }
                Item::Chars(chars) => {
                    if let Some((&last, init)) = chars.split_last() {
                        items.extend(init.iter().map(|&c| Node::Char(c)));
                        let atom = self.parse_quantifiers(Node::Char(last))?;
                        items.push(atom);
                    }
                }
                Item::Nothing => {}
                Item::Isolated(flags, extended) => {
                    let saved = self.extended;
                    if let Some(e) = extended {
                        self.extended = e;
                    }
                    let rest = self.parse_alt()?;
                    self.extended = saved;
                    items.push(Node::Group(Box::new(Group {
                        capture: None,
                        flags,
                        node: rest,
                    })));
                    break;
                }
            }
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.swap_remove(0),
            _ => Node::Concat(items),
        })
    }

    fn parse_quantifiers(&mut self, mut atom: Node) -> Result<Node, String> {
        loop {
            self.skip_extended();
            let (min, max, interval) = match self.peek() {
                Some('*') => (0, None, false),
                Some('+') => (1, None, false),
                Some('?') => (0, Some(1), false),
                Some('{') => match self.try_interval()? {
                    Some((min, max)) => (min, max, true),
                    None => break,
                },
                _ => break,
            };
            if !interval {
                self.pos += 1;
            }
            if matches!(atom, Node::Assert(_) | Node::Keep) {
                return Err("target of repeat operator is invalid".into());
            }
            let mut greedy = true;
            if interval {
                // `{n,m}?` is lazy; `{n}?` and `{n,m}+` are left for the next
                // iteration (optional / one-or-more of the interval).
                if self.peek() == Some('?') && max != Some(min) {
                    self.pos += 1;
                    greedy = false;
                }
            } else if self.eat('?') {
                greedy = false;
            } else {
                // Possessive `*+`, `++`, `?+`: approximated as greedy.
                self.eat('+');
            }
            atom = Node::Repeat(Box::new(Repeat {
                node: atom,
                min,
                max,
                greedy,
            }));
        }
        Ok(atom)
    }

    fn digits(&mut self) -> Result<Option<u32>, String> {
        let start = self.pos;
        let mut value: u32 = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(d))
                .ok_or("too big number")?;
            self.pos += 1;
        }
        Ok((self.pos > start).then_some(value))
    }

    /// At `{`: parses `{n}`, `{n,}`, `{n,m}` or `{,m}`; `None` (position
    /// unchanged) when the brace is a literal.
    fn try_interval(&mut self) -> Result<Option<(u32, Option<u32>)>, String> {
        let start = self.pos;
        self.pos += 1;
        let restore = |p: &mut Self| {
            p.pos = start;
            Ok(None)
        };
        let a = match self.digits() {
            Ok(a) => a,
            Err(_) => return restore(self),
        };
        let (min, max) = if self.eat(',') {
            let b = match self.digits() {
                Ok(b) => b,
                Err(_) => return restore(self),
            };
            if a.is_none() && b.is_none() {
                return restore(self);
            }
            (a.unwrap_or(0), b)
        } else {
            match a {
                Some(n) => (n, Some(n)),
                None => return restore(self),
            }
        };
        if !self.eat('}') {
            return restore(self);
        }
        // `{n,m}` with `n > m` is Oniguruma's possessive `{m,n}`
        // (approximated as greedy).
        let (min, max) = match max {
            Some(m) if m < min => (m, Some(min)),
            _ => (min, max),
        };
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
            return Err("too big number for repeat range".into());
        }
        Ok(Some((min, max)))
    }

    fn parse_atom(&mut self) -> Result<Item, String> {
        let Some(c) = self.bump() else {
            return Err("unexpected end of pattern".into());
        };
        let node = match c {
            '(' => return self.parse_group(),
            '[' => Node::Class(self.parse_class()?),
            '.' => Node::Dot,
            '^' => Node::Assert(Assert::LineStart),
            '$' => Node::Assert(Assert::LineEnd),
            '\\' => return self.parse_escape(),
            '*' | '+' | '?' => return Err("target of repeat operator is not specified".into()),
            c => Node::Char(c),
        };
        Ok(Item::Node(node))
    }

    fn group_body(&mut self) -> Result<Node, String> {
        let node = self.parse_alt()?;
        if !self.eat(')') {
            return Err("end pattern with unmatched parenthesis".into());
        }
        Ok(node)
    }

    fn capture_group(&mut self) -> Result<Item, String> {
        self.groups += 1;
        let index = self.groups;
        let node = self.group_body()?;
        Ok(Item::Node(Node::Group(Box::new(Group {
            capture: Some(index),
            flags: Flags::default(),
            node,
        }))))
    }

    fn look(&mut self, ahead: bool, negate: bool) -> Result<Item, String> {
        let node = self.group_body()?;
        Ok(Item::Node(Node::Look(Box::new(Look {
            ahead,
            negate,
            node,
        }))))
    }

    fn named_group(&mut self, terminator: char) -> Result<Item, String> {
        let mut name = String::new();
        loop {
            match self.bump() {
                Some(c) if c == terminator => break,
                Some(')') | None => return Err("invalid group name".into()),
                Some(c) => name.push(c),
            }
        }
        if name.is_empty() {
            return Err("group name is empty".into());
        }
        self.names.push((name, self.groups + 1));
        self.capture_group()
    }

    fn parse_group(&mut self) -> Result<Item, String> {
        if !self.eat('?') {
            return self.capture_group();
        }
        match self.bump() {
            Some(':') | Some('>') => {
                let node = self.group_body()?;
                Ok(Item::Node(Node::Group(Box::new(Group {
                    capture: None,
                    flags: Flags::default(),
                    node,
                }))))
            }
            Some('=') => self.look(true, false),
            Some('!') => self.look(true, true),
            Some('<') => match self.peek() {
                Some('=') => {
                    self.pos += 1;
                    self.look(false, false)
                }
                Some('!') => {
                    self.pos += 1;
                    self.look(false, true)
                }
                _ => self.named_group('>'),
            },
            Some('\'') => self.named_group('\''),
            Some('P') if self.eat('<') => self.named_group('>'),
            Some('#') => {
                while let Some(c) = self.bump() {
                    if c == ')' {
                        return Ok(Item::Nothing);
                    }
                }
                Err("end pattern in group".into())
            }
            Some('~') => Err("absent operator is not supported".into()),
            Some('(') => Err("conditional groups are not supported".into()),
            Some(c) if c == '-' || c.is_ascii_alphabetic() => {
                self.pos -= 1;
                self.option_group()
            }
            _ => Err("undefined group option".into()),
        }
    }

    fn option_group(&mut self) -> Result<Item, String> {
        let mut flags = Flags::default();
        let mut extended = None;
        let mut on = true;
        loop {
            match self.bump() {
                Some('-') if on => on = false,
                Some('i') => {
                    if on {
                        flags.case_on = true;
                    } else {
                        flags.case_off = true;
                    }
                }
                Some('m') | Some('s') => {
                    if on {
                        flags.dotall_on = true;
                    } else {
                        flags.dotall_off = true;
                    }
                }
                Some('x') => extended = Some(on),
                // ASCII-range / Unicode / segment options: ignored.
                Some('W' | 'D' | 'S' | 'P' | 'a' | 'u' | 'd' | 'l') => {}
                Some('y') => {
                    if self.eat('{') {
                        while let Some(c) = self.bump() {
                            if c == '}' {
                                break;
                            }
                        }
                    }
                }
                Some(')') => return Ok(Item::Isolated(flags, extended)),
                Some(':') => {
                    let saved = self.extended;
                    if let Some(e) = extended {
                        self.extended = e;
                    }
                    let node = self.group_body();
                    self.extended = saved;
                    return Ok(Item::Node(Node::Group(Box::new(Group {
                        capture: None,
                        flags,
                        node: node?,
                    }))));
                }
                _ => return Err("undefined group option".into()),
            }
        }
    }

    fn parse_escape(&mut self) -> Result<Item, String> {
        let Some(c) = self.bump() else {
            return Err("end pattern at escape".into());
        };
        if matches!(c, 'x' | 'o') && self.eat('{') {
            let list = self.code_point_list(if c == 'x' { 16 } else { 8 }, false)?;
            let mut chars: Vec<char> = list.into_iter().map(|(lo, _)| lo).collect();
            return Ok(if chars.len() == 1 {
                Item::Node(Node::Char(chars.remove(0)))
            } else {
                Item::Chars(chars)
            });
        }
        Ok(Item::Node(match c {
            'w' | 'W' | 'd' | 'D' | 's' | 'S' => Node::Class(Class {
                negated: false,
                items: format!("\\{c}"),
            }),
            'h' => Node::Class(Class {
                negated: false,
                items: HEX_ITEMS.into(),
            }),
            'H' => Node::Class(Class {
                negated: true,
                items: HEX_ITEMS.into(),
            }),
            // Without braces `\p` is a literal `p` in Oniguruma syntax.
            'p' | 'P' if self.peek() == Some('{') => Node::Class(self.parse_property(c == 'P')?),
            'b' => Node::Assert(Assert::WordBoundary),
            'B' => Node::Assert(Assert::NotWordBoundary),
            'A' => Node::Assert(Assert::TextStart),
            'z' => Node::Assert(Assert::TextEnd),
            'Z' => Node::Assert(Assert::TextEndNewline),
            'G' => Node::Assert(Assert::SearchStart),
            'K' => Node::Keep,
            'R' => Node::Raw(r"(?:\r\n|[\n\x0B\x0C\r\x{85}\x{2028}\x{2029}])"),
            'N' => Node::Class(Class {
                negated: true,
                items: r"\n".into(),
            }),
            'O' => Node::Raw(r"(?s:.)"),
            'X' => Node::Raw(r"(?s:\P{M}\p{M}*)"),
            '1'..='9' => {
                let mut n = c.to_digit(10).unwrap_or(0) as usize;
                while let Some(d) = self.peek().and_then(|d| d.to_digit(10)) {
                    n = n
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(d as usize))
                        .ok_or("too big backref number")?;
                    self.pos += 1;
                }
                Node::BackRef(GroupRef::Number(n))
            }
            'k' => Node::BackRef(self.group_ref()?),
            'g' => Node::Call(self.group_ref()?),
            'y' | 'Y' => return Err("text segment boundaries are not supported".into()),
            other => Node::Char(self.escape_char(other)?),
        }))
    }

    /// Escapes that denote a single character (shared by classes).
    fn escape_char(&mut self, c: char) -> Result<char, String> {
        Ok(match c {
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            'f' => '\x0C',
            'v' => '\x0B',
            'a' => '\x07',
            'e' => '\x1B',
            'x' => {
                if self.eat('{') {
                    self.radix_until_brace(16)?
                } else {
                    self.radix_fixed(16, 1, 2)?
                }
            }
            'u' => self.radix_fixed(16, 4, 4)?,
            '0' => self.octal_from(0)?,
            'o' if self.peek() == Some('{') => {
                self.pos += 1;
                self.radix_until_brace(8)?
            }
            'c' => {
                let x = self.bump().ok_or("end pattern at control")?;
                char::from_u32(x as u32 & 0x1F).ok_or("invalid control character")?
            }
            // `\C-x` (control) and `\M-x` (meta).
            'C' if self.peek() == Some('-') => {
                self.pos += 1;
                let x = self.meta_control_target()?;
                char::from_u32(x as u32 & 0x1F).ok_or("invalid control character")?
            }
            'M' if self.peek() == Some('-') => {
                self.pos += 1;
                let x = self.meta_control_target()?;
                char::from_u32((x as u32 & 0xFF) | 0x80).ok_or("invalid meta character")?
            }
            other => other,
        })
    }

    /// After `\k` / `\g`: `<name>`, `'name'`, `<n>`, `<-n>` (the n-th group
    /// to the left) or `<+n>` (to the right).
    fn group_ref(&mut self) -> Result<GroupRef, String> {
        let close = match self.bump() {
            Some('<') => '>',
            Some('\'') => '\'',
            _ => return Err("invalid group name".into()),
        };
        let mut text = String::new();
        loop {
            match self.bump() {
                Some(c) if c == close => break,
                Some(c) => text.push(c),
                None => return Err("invalid group name".into()),
            }
        }
        let (sign, digits) = match text.as_bytes().first() {
            Some(b'-') => (-1, &text[1..]),
            Some(b'+') => (1, &text[1..]),
            _ => (0, text.as_str()),
        };
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = digits.parse().map_err(|_| "too big number")?;
            let number = match sign {
                0 => Some(n),
                -1 => (self.groups + 1)
                    .checked_sub(n)
                    .filter(|&k| n >= 1 && k >= 1),
                _ => self.groups.checked_add(n).filter(|_| n >= 1),
            };
            return number
                .map(GroupRef::Number)
                .ok_or_else(|| "invalid backref number/name".into());
        }
        if text.is_empty() || sign != 0 {
            return Err("invalid group name".into());
        }
        Ok(GroupRef::Name(text))
    }

    fn meta_control_target(&mut self) -> Result<char, String> {
        match self.bump() {
            Some('\\') => {
                let c = self.bump().ok_or("end pattern at meta/control")?;
                self.escape_char(c)
            }
            Some(c) => Ok(c),
            None => Err("end pattern at meta/control".into()),
        }
    }

    /// After `\x{` / `\o{`: a space separated list of code points, ranges
    /// (`41-5A`) allowed in classes. Consumes the closing brace.
    fn code_point_list(&mut self, radix: u32, ranges: bool) -> Result<Vec<(char, char)>, String> {
        let mut list = Vec::new();
        let mut pending_range: Option<char> = None;
        loop {
            while self.eat(' ') {}
            if self.eat('}') {
                break;
            }
            let mut value: u32 = 0;
            let mut n = 0;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) {
                value = value
                    .checked_mul(radix)
                    .and_then(|v| v.checked_add(d))
                    .ok_or("too big code point")?;
                self.pos += 1;
                n += 1;
            }
            if n == 0 {
                return Err("invalid code point value".into());
            }
            let c = char::from_u32(value).ok_or("invalid code point value")?;
            match pending_range.take() {
                Some(lo) if lo > c => return Err("empty range in char class".into()),
                Some(lo) => {
                    list.pop();
                    list.push((lo, c));
                }
                None => list.push((c, c)),
            }
            if ranges && self.peek() == Some('-') {
                self.pos += 1;
                pending_range = Some(c);
            }
        }
        if list.is_empty() || pending_range.is_some() {
            return Err("invalid code point value".into());
        }
        Ok(list)
    }

    /// Up to two more octal digits after an initial value.
    fn octal_from(&mut self, initial: u32) -> Result<char, String> {
        let mut value = initial;
        for _ in 0..2 {
            match self.peek().and_then(|c| c.to_digit(8)) {
                Some(d) => {
                    value = value * 8 + d;
                    self.pos += 1;
                }
                None => break,
            }
        }
        char::from_u32(value).ok_or_else(|| "invalid code point value".into())
    }

    fn radix_fixed(&mut self, radix: u32, min: usize, max: usize) -> Result<char, String> {
        let mut value: u32 = 0;
        let mut n = 0;
        while n < max {
            match self.peek().and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    value = value * radix + d;
                    self.pos += 1;
                    n += 1;
                }
                None => break,
            }
        }
        if n < min {
            return Err("invalid code point value".into());
        }
        char::from_u32(value).ok_or_else(|| "invalid code point value".into())
    }

    fn radix_until_brace(&mut self, radix: u32) -> Result<char, String> {
        let mut value: u32 = 0;
        let mut n = 0;
        loop {
            match self.bump() {
                Some('}') if n > 0 => break,
                Some(c) => {
                    let d = c.to_digit(radix).ok_or("invalid code point value")?;
                    value = value
                        .checked_mul(radix)
                        .and_then(|v| v.checked_add(d))
                        .ok_or("too big code point")?;
                    n += 1;
                }
                None => return Err("invalid code point value".into()),
            }
        }
        char::from_u32(value).ok_or_else(|| "invalid code point value".into())
    }

    fn parse_property(&mut self, upper: bool) -> Result<Class, String> {
        let name: String = if self.eat('{') {
            let mut name = String::new();
            loop {
                match self.bump() {
                    Some('}') => break,
                    Some(c) => name.push(c),
                    None => return Err("invalid character property name".into()),
                }
            }
            name
        } else {
            self.bump()
                .ok_or("invalid character property name")?
                .to_string()
        };
        let (caret, name) = match name.strip_prefix('^') {
            Some(rest) => (true, rest.to_string()),
            None => (false, name),
        };
        Ok(Class {
            negated: upper ^ caret,
            items: property_items(&name)?,
        })
    }

    fn parse_class(&mut self) -> Result<Class, String> {
        self.enter()?;
        let negated = self.eat('^');
        let mut items = String::new();
        let mut first = true;
        loop {
            let Some(c) = self.bump() else {
                return Err("premature end of char-class".into());
            };
            let atom = match c {
                ']' if !first => break,
                '[' => match self.try_posix()? {
                    Some(set) => ClassAtom::Set(set),
                    None => ClassAtom::Set(self.parse_class()?.to_bracket()),
                },
                '&' if self.peek() == Some('&') => {
                    self.pos += 1;
                    items.push_str("&&");
                    first = false;
                    continue;
                }
                '\\' => self.class_escape()?,
                c => ClassAtom::Char(c),
            };
            first = false;
            match atom {
                ClassAtom::Set(set) => items.push_str(&set),
                ClassAtom::Char(lo) => {
                    let is_range = self.peek() == Some('-')
                        && !matches!(self.peek_at(1), None | Some(']') | Some('['));
                    if !is_range {
                        push_class_char(&mut items, lo);
                        continue;
                    }
                    self.pos += 1;
                    let hi = match self.bump() {
                        Some('\\') => self.class_escape()?,
                        Some(c) => ClassAtom::Char(c),
                        None => return Err("premature end of char-class".into()),
                    };
                    match hi {
                        ClassAtom::Char(hi) => {
                            if hi < lo {
                                return Err("empty range in char class".into());
                            }
                            push_class_char(&mut items, lo);
                            items.push('-');
                            push_class_char(&mut items, hi);
                        }
                        ClassAtom::Set(set) => {
                            push_class_char(&mut items, lo);
                            items.push_str("\\-");
                            items.push_str(&set);
                        }
                    }
                }
            }
        }
        self.depth -= 1;
        if items.is_empty() || items.ends_with("&&") || items.starts_with("&&") {
            return Err("empty char-class".into());
        }
        Ok(Class { negated, items })
    }

    /// After `[` inside a class: a POSIX bracket `[:name:]` / `[:^name:]`.
    fn try_posix(&mut self) -> Result<Option<String>, String> {
        if self.peek() != Some(':') {
            return Ok(None);
        }
        let save = self.pos;
        self.pos += 1;
        let negated = self.eat('^');
        let mut name = String::new();
        while let Some(c) = self.peek().filter(|c| c.is_ascii_alphabetic()) {
            name.push(c);
            self.pos += 1;
        }
        if name.is_empty() || !self.eat(':') || !self.eat(']') {
            self.pos = save;
            return Ok(None);
        }
        let items = posix_items(&name).ok_or("invalid POSIX bracket type")?;
        Ok(Some(if negated {
            format!("[^{items}]")
        } else {
            items.to_string()
        }))
    }

    fn class_escape(&mut self) -> Result<ClassAtom, String> {
        let c = self.bump().ok_or("premature end of char-class")?;
        if matches!(c, 'x' | 'o') && self.eat('{') {
            let list = self.code_point_list(if c == 'x' { 16 } else { 8 }, true)?;
            if let [(lo, hi)] = list[..] {
                if lo == hi {
                    return Ok(ClassAtom::Char(lo));
                }
            }
            let mut items = String::new();
            for (lo, hi) in list {
                push_class_char(&mut items, lo);
                if hi != lo {
                    items.push('-');
                    push_class_char(&mut items, hi);
                }
            }
            return Ok(ClassAtom::Set(items));
        }
        Ok(match c {
            'w' | 'W' | 'd' | 'D' | 's' | 'S' => ClassAtom::Set(format!("\\{c}")),
            'h' => ClassAtom::Set(HEX_ITEMS.into()),
            'H' => ClassAtom::Set(format!("[^{HEX_ITEMS}]")),
            'p' | 'P' if self.peek() == Some('{') => {
                let class = self.parse_property(c == 'P')?;
                ClassAtom::Set(if class.negated {
                    class.to_bracket()
                } else {
                    class.items
                })
            }
            'b' => ClassAtom::Char('\x08'),
            '1'..='7' => ClassAtom::Char(self.octal_from(c.to_digit(8).unwrap_or(0))?),
            other => ClassAtom::Char(self.escape_char(other)?),
        })
    }
}

/// Appends a literal character to class items, escaped as needed.
pub(crate) fn push_class_char(out: &mut String, c: char) {
    match c {
        '\\' | '[' | ']' | '^' | '-' | '&' | '~' => {
            out.push('\\');
            out.push(c);
        }
        c if c.is_control() || c.is_whitespace() => {
            out.push_str(&format!("\\x{{{:X}}}", c as u32));
        }
        c => out.push(c),
    }
}

fn posix_items(name: &str) -> Option<&'static str> {
    Some(match name {
        "alnum" => r"\p{Alphabetic}\p{Nd}",
        "alpha" => r"\p{Alphabetic}",
        "ascii" => r"\x00-\x7F",
        "blank" => r"\p{Zs}\t",
        "cntrl" => r"\p{Cc}",
        "digit" => r"\p{Nd}",
        "graph" => r"[^\s\p{Cc}\p{Cn}]",
        "lower" => r"\p{Lowercase}",
        "print" => r"[^\p{Cc}\p{Cn}\p{Zl}\p{Zp}]",
        "punct" => r"\p{P}\$\+<=>\^`\|\~",
        "space" => r"\s",
        "upper" => r"\p{Uppercase}",
        "xdigit" => r"0-9A-Fa-f",
        "word" => r"\w",
        _ => return None,
    })
}

/// Class items for a `\p{…}` property name (Oniguruma names are matched
/// loosely; unknown names pass through to the `regex` crate).
fn property_items(name: &str) -> Result<String, String> {
    let key: String = name
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(items) = posix_items(&key) {
        return Ok(items.to_string());
    }
    match key.as_str() {
        "any" => return Ok(r"\x{0}-\x{10FFFF}".into()),
        // Surrogates never occur in a `&str`: an empty set (as a nested
        // class, so it also works inside other classes).
        "cs" | "surrogate" => return Ok(r"[^\x{0}-\x{10FFFF}]".into()),
        _ => {}
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '-' | '=' | '.' | ':'))
    {
        return Err("invalid character property name".into());
    }
    Ok(format!(r"\p{{{name}}}"))
}

/// Subexpression calls nest at most this deep; a deeper call never matches.
const MAX_CALL_DEPTH: usize = 4;
/// Node budget for inlining calls.
const MAX_EXPANDED_NODES: usize = 20_000;
/// Texts a back-referenced group may have (and variants a pattern may
/// expand to).
const MAX_TEXTS: usize = 24;
/// Longest text of a back-referenced group.
const MAX_TEXT_LEN: usize = 64;
/// Unbounded repetitions in a back-referenced group are enumerated up to
/// this many extra iterations.
const REPEAT_CAP: u32 = 6;

/// Never matches (an empty class).
fn never() -> Node {
    Node::Class(Class {
        negated: true,
        items: r"\x{0}-\x{10FFFF}".into(),
    })
}

/// Group names and count of a parsed pattern.
struct Refs<'a> {
    names: &'a [(String, usize)],
    groups: usize,
}

impl Refs<'_> {
    /// The group number of a reference (0, the whole pattern, only for
    /// calls).
    fn number(&self, reference: &GroupRef, call: bool) -> Result<usize, String> {
        let n = match reference {
            GroupRef::Number(n) => *n,
            GroupRef::Name(name) => {
                let mut found = self
                    .names
                    .iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, k)| *k);
                let first = found
                    .next()
                    .ok_or_else(|| format!("undefined name <{name}> reference"))?;
                if found.next().is_some() {
                    return Err("references to a name defined twice are not supported".into());
                }
                first
            }
        };
        if n > self.groups || (n == 0 && !call) {
            return Err("invalid backref number/name".into());
        }
        Ok(n)
    }
}

/// Inlines subexpression calls (`\g<name>`, recursion bounded by
/// [`MAX_CALL_DEPTH`]) and expands back-references: when every referenced
/// group sits outside quantifiers, alternations and look-arounds, closes
/// before its references, and can only match a few texts (unbounded
/// repetitions enumerated up to [`REPEAT_CAP`] extra iterations), the
/// pattern becomes an alternation of variants — one per combination of
/// texts, in priority order — with the group and its references replaced by
/// that text. Other back-references are unsupported.
fn expand_references(node: Node, refs: &Refs<'_>) -> Result<Node, String> {
    // A back-reference to a called group would see the call's capture.
    let mut called = HashSet::new();
    collect_calls(&node, refs, &mut called)?;
    let node = if !called.is_empty() {
        let mut defs = HashMap::new();
        collect_groups(&node, &mut defs);
        let mut expander = CallExpander {
            refs,
            defs,
            root: node.clone(),
            budget: MAX_EXPANDED_NODES,
        };
        expander.expand(&node, 0)?
    } else {
        node
    };
    if !contains(&node, &|n| matches!(n, Node::BackRef(_))) {
        return Ok(node);
    }
    let mut closed = HashSet::new();
    let mut targets = Vec::new();
    check_backrefs(&node, refs, &mut closed, &mut targets)?;
    let mut choices: Vec<(usize, Vec<String>)> = Vec::new();
    for k in targets {
        let unsupported = || "back-references to this group are not supported".to_string();
        if called.contains(&k) {
            return Err(unsupported());
        }
        let group = spine_group(&node, k).ok_or_else(unsupported)?;
        choices.push((k, texts(group).ok_or_else(unsupported)?));
    }
    let mut combos: Vec<Vec<(usize, &str)>> = vec![Vec::new()];
    for (k, texts) in &choices {
        let mut next = Vec::new();
        for combo in &combos {
            for text in texts {
                let mut combo = combo.clone();
                combo.push((*k, text.as_str()));
                next.push(combo);
            }
        }
        if next.len() > MAX_TEXTS {
            return Err("back-reference expands to too many variants".into());
        }
        combos = next;
    }
    let mut variants = combos
        .iter()
        .map(|combo| substitute(&node, combo, refs))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(if variants.len() == 1 {
        variants.remove(0)
    } else {
        Node::Alt(variants)
    })
}

/// Group numbers of every subexpression call.
fn collect_calls(node: &Node, refs: &Refs<'_>, out: &mut HashSet<usize>) -> Result<(), String> {
    match node {
        Node::Call(reference) => {
            out.insert(refs.number(reference, true)?);
        }
        Node::Group(g) => collect_calls(&g.node, refs, out)?,
        Node::Concat(items) | Node::Alt(items) => {
            for n in items {
                collect_calls(n, refs, out)?;
            }
        }
        Node::Repeat(r) => collect_calls(&r.node, refs, out)?,
        Node::Look(l) => collect_calls(&l.node, refs, out)?,
        Node::Tail(t) => collect_calls(t, refs, out)?,
        _ => {}
    }
    Ok(())
}

fn contains(node: &Node, pred: &dyn Fn(&Node) -> bool) -> bool {
    pred(node)
        || match node {
            Node::Group(g) => contains(&g.node, pred),
            Node::Concat(items) | Node::Alt(items) => items.iter().any(|n| contains(n, pred)),
            Node::Repeat(r) => contains(&r.node, pred),
            Node::Look(l) => contains(&l.node, pred),
            Node::Tail(t) => contains(t, pred),
            _ => false,
        }
}

/// Group number → group body.
fn collect_groups(node: &Node, out: &mut HashMap<usize, Node>) {
    match node {
        Node::Group(g) => {
            if let Some(k) = g.capture {
                out.insert(k, g.node.clone());
            }
            collect_groups(&g.node, out);
        }
        Node::Concat(items) | Node::Alt(items) => items.iter().for_each(|n| collect_groups(n, out)),
        Node::Repeat(r) => collect_groups(&r.node, out),
        Node::Look(l) => collect_groups(&l.node, out),
        Node::Tail(t) => collect_groups(t, out),
        _ => {}
    }
}

struct CallExpander<'a> {
    refs: &'a Refs<'a>,
    defs: HashMap<usize, Node>,
    root: Node,
    budget: usize,
}

impl CallExpander<'_> {
    fn expand(&mut self, node: &Node, depth: usize) -> Result<Node, String> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or("subexpression calls expand too much")?;
        Ok(match node {
            Node::Call(reference) => {
                let k = self.refs.number(reference, true)?;
                if depth >= MAX_CALL_DEPTH {
                    return Ok(never());
                }
                let target = if k == 0 {
                    self.root.clone()
                } else {
                    self.defs.get(&k).cloned().ok_or("undefined group")?
                };
                // The copy keeps the group's capture: like Oniguruma, the
                // capture that completes last is reported.
                let body = self.expand(&target, depth + 1)?;
                let capture = (k > 0).then_some(k);
                Node::Group(Box::new(Group {
                    capture,
                    flags: Flags::default(),
                    node: body,
                }))
            }
            Node::Group(g) => Node::Group(Box::new(Group {
                capture: g.capture,
                flags: g.flags,
                node: self.expand(&g.node, depth)?,
            })),
            Node::Concat(items) => Node::Concat(
                items
                    .iter()
                    .map(|n| self.expand(n, depth))
                    .collect::<Result<_, _>>()?,
            ),
            Node::Alt(items) => Node::Alt(
                items
                    .iter()
                    .map(|n| self.expand(n, depth))
                    .collect::<Result<_, _>>()?,
            ),
            Node::Repeat(r) => Node::Repeat(Box::new(Repeat {
                node: self.expand(&r.node, depth)?,
                min: r.min,
                max: r.max,
                greedy: r.greedy,
            })),
            Node::Look(l) => Node::Look(Box::new(Look {
                ahead: l.ahead,
                negate: l.negate,
                node: self.expand(&l.node, depth)?,
            })),
            other => other.clone(),
        })
    }
}

/// Validates back-references in pattern order: each must follow the end of
/// its group. Collects the referenced groups (in order of first reference).
fn check_backrefs(
    node: &Node,
    refs: &Refs<'_>,
    closed: &mut HashSet<usize>,
    targets: &mut Vec<usize>,
) -> Result<(), String> {
    match node {
        Node::BackRef(reference) => {
            let k = refs.number(reference, false)?;
            if !closed.contains(&k) {
                return Err("back-reference before the end of its group".into());
            }
            if !targets.contains(&k) {
                targets.push(k);
            }
        }
        Node::Group(g) => {
            check_backrefs(&g.node, refs, closed, targets)?;
            if let Some(k) = g.capture {
                closed.insert(k);
            }
        }
        Node::Concat(items) | Node::Alt(items) => {
            for n in items {
                check_backrefs(n, refs, closed, targets)?;
            }
        }
        Node::Repeat(r) => check_backrefs(&r.node, refs, closed, targets)?,
        Node::Look(l) => check_backrefs(&l.node, refs, closed, targets)?,
        Node::Tail(t) => check_backrefs(t, refs, closed, targets)?,
        _ => {}
    }
    Ok(())
}

/// The body of capture group `k` when it is reached only through sequences
/// and groups (it matches exactly once in every match).
fn spine_group(node: &Node, k: usize) -> Option<&Node> {
    match node {
        Node::Group(g) if g.capture == Some(k) => Some(&g.node),
        Node::Group(g) => spine_group(&g.node, k),
        Node::Concat(items) => items.iter().find_map(|n| spine_group(n, k)),
        _ => None,
    }
}

/// The texts a node can match, in priority order, when they are few.
fn texts(node: &Node) -> Option<Vec<String>> {
    let out = match node {
        Node::Empty => vec![String::new()],
        Node::Char(c) => vec![c.to_string()],
        Node::Class(c) => class_chars(c)?.into_iter().map(String::from).collect(),
        Node::Group(g) if g.capture.is_none() => texts(&g.node)?,
        Node::Concat(items) => items
            .iter()
            .try_fold(vec![String::new()], |acc, n| product(&acc, &texts(n)?))?,
        Node::Alt(branches) => {
            let mut out: Vec<String> = Vec::new();
            for b in branches {
                for text in texts(b)? {
                    if !out.contains(&text) {
                        out.push(text);
                    }
                }
            }
            out
        }
        Node::Repeat(r) => {
            let unit = texts(&r.node)?;
            if unit.iter().any(String::is_empty) {
                return None;
            }
            let max = r
                .max
                .unwrap_or(u32::MAX)
                .min(r.min.saturating_add(REPEAT_CAP));
            let counts: Vec<u32> = if r.greedy {
                (r.min..=max).rev().collect()
            } else {
                (r.min..=max).collect()
            };
            let mut out: Vec<String> = Vec::new();
            for count in counts {
                let mut acc = vec![String::new()];
                for _ in 0..count {
                    acc = product(&acc, &unit)?;
                }
                for text in acc {
                    if !out.contains(&text) {
                        out.push(text);
                    }
                }
            }
            out
        }
        _ => return None,
    };
    (out.len() <= MAX_TEXTS).then_some(out)
}

fn product(left: &[String], right: &[String]) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(left.len() * right.len());
    for a in left {
        for b in right {
            if a.len() + b.len() > MAX_TEXT_LEN || out.len() >= MAX_TEXTS {
                return None;
            }
            out.push(format!("{a}{b}"));
        }
    }
    Some(out)
}

/// The characters of a small, simple class (as built by this parser).
fn class_chars(class: &Class) -> Option<Vec<char>> {
    if class.negated {
        return None;
    }
    let chars: Vec<char> = class.items.chars().collect();
    let atom = |i: &mut usize| -> Option<char> {
        let c = *chars.get(*i)?;
        *i += 1;
        if c != '\\' {
            return (!matches!(c, '[' | ']' | '&' | '~')).then_some(c);
        }
        let e = *chars.get(*i)?;
        *i += 1;
        match e {
            'x' if chars.get(*i) == Some(&'{') => {
                let end = *i + chars[*i..].iter().position(|&c| c == '}')?;
                let hex: String = chars[*i + 1..end].iter().collect();
                *i = end + 1;
                char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
            }
            e if e.is_ascii_alphanumeric() => None,
            e => Some(e),
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let lo = atom(&mut i)?;
        if chars.get(i) == Some(&'-') && i + 1 < chars.len() {
            i += 1;
            let hi = atom(&mut i)?;
            if hi < lo || (hi as u32 - lo as u32) as usize > MAX_TEXTS {
                return None;
            }
            out.extend(lo..=hi);
        } else {
            out.push(lo);
        }
        if out.len() > MAX_TEXTS {
            return None;
        }
    }
    Some(out)
}

/// The node for a fixed text.
fn literal(text: &str) -> Node {
    let mut chars: Vec<Node> = text.chars().map(Node::Char).collect();
    match chars.len() {
        0 => Node::Empty,
        1 => chars.remove(0),
        _ => Node::Group(Box::new(Group {
            capture: None,
            flags: Flags::default(),
            node: Node::Concat(chars),
        })),
    }
}

/// `node` with the groups of `combo` (and their back-references) replaced by
/// their texts.
fn substitute(node: &Node, combo: &[(usize, &str)], refs: &Refs<'_>) -> Result<Node, String> {
    let text_of = |k: usize| combo.iter().find(|(g, _)| *g == k).map(|(_, t)| *t);
    Ok(match node {
        Node::BackRef(reference) => {
            let k = refs.number(reference, false)?;
            literal(text_of(k).ok_or("back-references are not supported")?)
        }
        Node::Group(g) => match g.capture.and_then(text_of) {
            Some(text) => Node::Group(Box::new(Group {
                capture: g.capture,
                flags: g.flags,
                node: literal(text),
            })),
            None => Node::Group(Box::new(Group {
                capture: g.capture,
                flags: g.flags,
                node: substitute(&g.node, combo, refs)?,
            })),
        },
        Node::Concat(items) => Node::Concat(
            items
                .iter()
                .map(|n| substitute(n, combo, refs))
                .collect::<Result<_, _>>()?,
        ),
        Node::Alt(items) => Node::Alt(
            items
                .iter()
                .map(|n| substitute(n, combo, refs))
                .collect::<Result<_, _>>()?,
        ),
        Node::Repeat(r) => Node::Repeat(Box::new(Repeat {
            node: substitute(&r.node, combo, refs)?,
            min: r.min,
            max: r.max,
            greedy: r.greedy,
        })),
        Node::Look(l) => Node::Look(Box::new(Look {
            ahead: l.ahead,
            negate: l.negate,
            node: substitute(&l.node, combo, refs)?,
        })),
        Node::Tail(t) => Node::Tail(Box::new(substitute(t, combo, refs)?)),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Node {
        parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}")).node
    }

    #[test]
    fn counts_groups_including_named() {
        assert_eq!(parse(r"(a)(?<n>b)(?:c)(?'m'd)((e))").unwrap().groups, 5);
    }

    #[test]
    fn isolated_option_extends_to_group_end() {
        let node = p("ab(?i)c|d");
        let Node::Concat(items) = node else {
            panic!("expected concat")
        };
        assert_eq!(items.len(), 3);
        let Node::Group(g) = &items[2] else {
            panic!("expected group")
        };
        assert!(g.flags.case_on);
        assert!(matches!(g.node, Node::Alt(ref b) if b.len() == 2));
    }

    #[test]
    fn extended_mode_strips_whitespace_and_comments_but_not_in_classes() {
        let node = p("(?x) a  b # comment\n [ ]");
        let Node::Group(g) = node else {
            panic!("expected group")
        };
        let Node::Concat(items) = &g.node else {
            panic!("expected concat")
        };
        assert_eq!(items[0], Node::Char('a'));
        assert_eq!(items[1], Node::Char('b'));
        assert_eq!(
            items[2],
            Node::Class(Class {
                negated: false,
                items: r"\x{20}".into()
            })
        );
    }

    #[test]
    fn quantifier_forms() {
        let Node::Repeat(r) = p("a{,3}") else {
            panic!()
        };
        assert_eq!((r.min, r.max, r.greedy), (0, Some(3), true));
        let Node::Repeat(r) = p("a*+") else { panic!() };
        assert!(r.greedy);
        let Node::Repeat(r) = p("a{2,3}?") else {
            panic!()
        };
        assert!(!r.greedy);
        // `{n}+` is a one-or-more of the interval in Oniguruma syntax.
        let Node::Repeat(outer) = p("a{2}+") else {
            panic!()
        };
        assert_eq!((outer.min, outer.max), (1, None));
        assert!(matches!(outer.node, Node::Repeat(_)));
        // A brace that is not an interval is literal.
        assert_eq!(
            p("a{x"),
            Node::Concat(vec![Node::Char('a'), Node::Char('{'), Node::Char('x')])
        );
    }

    #[test]
    fn escapes_and_classes() {
        // `\u` + 4 hex digits, built with concat! to keep the source readable.
        let src = concat!(r"\x41\", "u0042", r"\x{43}\012\e\/");
        assert_eq!(
            p(src),
            Node::Concat(vec![
                Node::Char('A'),
                Node::Char('B'),
                Node::Char('C'),
                Node::Char('\n'),
                Node::Char('\x1B'),
                Node::Char('/'),
            ])
        );
        assert_eq!(
            p(r"[\101]"),
            Node::Class(Class {
                negated: false,
                items: "A".into()
            })
        );
        assert_eq!(
            p(r"[\h_-]"),
            Node::Class(Class {
                negated: false,
                items: r"0-9a-fA-F_\-".into()
            })
        );
        assert_eq!(
            p(r"[^a-c[:digit:]]"),
            Node::Class(Class {
                negated: true,
                items: r"a-c\p{Nd}".into()
            })
        );
        assert_eq!(
            p(r"[]a]"),
            Node::Class(Class {
                negated: false,
                items: r"\]a".into()
            })
        );
        assert_eq!(
            p(r"\p{^Alpha}"),
            Node::Class(Class {
                negated: true,
                items: r"\p{Alphabetic}".into()
            })
        );
    }

    #[test]
    fn oniguruma_specific_syntax() {
        // `{n,m}` with n > m: possessive `{m,n}`.
        let Node::Repeat(r) = p("a{3,1}") else {
            panic!()
        };
        assert_eq!((r.min, r.max), (1, Some(3)));
        // Code point lists: a sequence outside classes (quantifier binds to
        // the last), a set (with ranges) inside.
        assert_eq!(
            p(r"\x{41 42}+"),
            Node::Concat(vec![
                Node::Char('A'),
                Node::Repeat(Box::new(Repeat {
                    node: Node::Char('B'),
                    min: 1,
                    max: None,
                    greedy: true
                }))
            ])
        );
        assert_eq!(
            p(r"[\x{41 61-63}]"),
            Node::Class(Class {
                negated: false,
                items: "Aa-c".into()
            })
        );
        assert_eq!(p(r"\o{101}"), Node::Char('A'));
        // `\p` without braces is a literal.
        assert_eq!(
            p(r"\pL"),
            Node::Concat(vec![Node::Char('p'), Node::Char('L')])
        );
        assert_eq!(
            p(r"[\p]"),
            Node::Class(Class {
                negated: false,
                items: "p".into()
            })
        );
        // Control / meta escapes.
        assert_eq!(
            p(r"\C-a\M-a\cA"),
            Node::Concat(vec![
                Node::Char('\x01'),
                Node::Char('\u{e1}'),
                Node::Char('\x01')
            ])
        );
        // Group names are not restricted to word characters.
        assert_eq!(parse(r"(?<type-name>x)").unwrap().groups, 1);
    }

    #[test]
    fn unsupported_constructs_are_errors() {
        for src in [
            r"(.)\1",
            r"(\w+)\s+\1",
            r"(?:(x)|y)\1",
            r"(a)?\1",
            r"\1(a)",
            r"(?<x>a|b)\g<x>\k<x>",
            r"(?<n>a)(?<n>b)\k<n>",
            r"\k<x>",
            r"\g<1>",
            r"\g<-1>",
            r"(?(1)a|b)",
            r"(?~ab)",
            "(a",
            "a)",
            "[a",
            "*a",
            r"\",
            r"\x{}",
            r"\x{110000}",
            r"[\x{43-41}]",
            "(?<>a)",
            "(?<a",
        ] {
            assert!(parse(src).is_err(), "{src:?} should be rejected");
        }
    }
}

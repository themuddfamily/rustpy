//! Parser for markup abbreviations (`ul>li.item$*3`, `a[href=#]{Click}`,
//! `(dt+dd)*2`, ...) into a token tree. Repeats and `$` numbering are kept
//! symbolic here and resolved by `tree::convert`.

use super::snippets;

/// A fragment of a name, attribute value or text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    /// `$`, `$$`, `$@-`, `$@3`, `$@^`...: the counter of a repeater.
    Number {
        /// Zero-pad to this many digits (the number of `$` signs).
        size: usize,
        /// `@-`: count down instead of up.
        reverse: bool,
        /// `@N`: first value (default 1).
        base: usize,
        /// `@^`: how many repeaters to skip outwards (0 = innermost).
        parent: usize,
    },
    /// `${N}` / `${N:placeholder}`: an edit point (used by snippets).
    Field(String),
}

pub(crate) type Pieces = Vec<Piece>;

#[derive(Clone, Debug, Default)]
pub(crate) struct AttrSpec {
    /// Empty for a value-only attribute (`["value"]`).
    pub name: Pieces,
    pub value: Option<Pieces>,
    /// Written `[name.]`: always output without a value.
    pub boolean: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Repeat {
    pub count: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ElementSpec {
    pub name: Option<Pieces>,
    pub attrs: Vec<AttrSpec>,
    pub text: Option<Pieces>,
    pub repeat: Option<Repeat>,
    pub self_close: bool,
    pub children: Vec<Item>,
}

#[derive(Clone, Debug)]
pub(crate) enum Item {
    Element(ElementSpec),
    Group {
        items: Vec<Item>,
        repeat: Option<Repeat>,
    },
}

impl Item {
    fn children_mut(&mut self) -> &mut Vec<Item> {
        match self {
            Item::Element(e) => &mut e.children,
            Item::Group { items, .. } => items,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ParseError;

type PResult<T> = Result<T, ParseError>;

/// Deepest nesting (groups plus `>` levels) accepted; keeps the recursive
/// passes far from the stack limit.
const MAX_DEPTH: usize = 64;
/// Largest `*N` accepted (the node budget in `tree` caps the total).
pub(crate) const MAX_REPEAT: usize = 10_000;
/// Longest `$$$...` accepted.
const MAX_PAD: usize = 32;

/// Parses a whole abbreviation; trailing garbage is an error.
pub(crate) fn parse(src: &str) -> PResult<Vec<Item>> {
    let mut p = Parser {
        s: src.chars().collect(),
        pos: 0,
        depth: 0,
    };
    let items = p.statements()?;
    if p.pos < p.s.len() || items.is_empty() {
        return Err(ParseError);
    }
    Ok(items)
}

/// Characters of element names and unquoted id/class values.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '!')
}

#[derive(Default)]
struct PiecesBuilder {
    pieces: Pieces,
    buf: String,
}

impl PiecesBuilder {
    fn push_char(&mut self, c: char) {
        self.buf.push(c);
    }

    fn push(&mut self, piece: Piece) {
        self.flush();
        self.pieces.push(piece);
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            self.pieces.push(Piece::Text(std::mem::take(&mut self.buf)));
        }
    }

    fn is_empty(&self) -> bool {
        self.pieces.is_empty() && self.buf.is_empty()
    }

    /// The text so far when it has no numbering/field pieces.
    fn plain(&self) -> Option<&str> {
        self.pieces.is_empty().then_some(self.buf.as_str())
    }

    fn finish(mut self) -> Pieces {
        self.flush();
        self.pieces
    }
}

struct Parser {
    s: Vec<char>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.pos + n).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn digits(&mut self) -> String {
        let mut d = String::new();
        while let Some(c) = self.peek().filter(char::is_ascii_digit) {
            d.push(c);
            self.pos += 1;
        }
        d
    }

    /// `item (op item)*` where op is `>` (child), `+` (sibling) or `^`
    /// (climb up one level per caret).
    fn statements(&mut self) -> PResult<Vec<Item>> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError);
        }
        // stack[0] is the result list; each deeper entry collects the children
        // of the last item of the entry below it.
        let mut stack: Vec<Vec<Item>> = vec![Vec::new()];
        while let Some(item) = self.item()? {
            stack.last_mut().expect("stack is never empty").push(item);
            match self.peek() {
                Some('>') => {
                    self.pos += 1;
                    stack.push(Vec::new());
                    if self.depth + stack.len() > MAX_DEPTH {
                        return Err(ParseError);
                    }
                }
                Some('+') => self.pos += 1,
                Some('^') => {
                    while self.eat('^') {
                        if stack.len() > 1 {
                            let children = stack.pop().expect("checked length");
                            attach(&mut stack, children);
                        }
                    }
                }
                // Emmet quirk: a text block or group right after an item is
                // its sibling (`p{a}{b}`, `div(p)`).
                Some('{' | '(') => {}
                _ => break,
            }
        }
        while stack.len() > 1 {
            let children = stack.pop().expect("checked length");
            attach(&mut stack, children);
        }
        self.depth -= 1;
        Ok(stack.pop().expect("stack is never empty"))
    }

    fn item(&mut self) -> PResult<Option<Item>> {
        if self.eat('(') {
            let items = self.statements()?;
            // An empty group would produce nothing, however often repeated.
            if items.is_empty() || !self.eat(')') {
                return Err(ParseError);
            }
            let repeat = self.repeat()?;
            return Ok(Some(Item::Group { items, repeat }));
        }
        Ok(self.element()?.map(Item::Element))
    }

    /// `*N` (or a bare `*`, meaning once).
    fn repeat(&mut self) -> PResult<Option<Repeat>> {
        if !self.eat('*') {
            return Ok(None);
        }
        let digits = self.digits();
        let count = if digits.is_empty() {
            1
        } else {
            digits.parse::<usize>().map_err(|_| ParseError)?.max(1)
        };
        if count > MAX_REPEAT {
            return Err(ParseError);
        }
        Ok(Some(Repeat { count }))
    }

    /// `name? (#id | .class | [attrs] | {text} | *N | /)*`, at least one part.
    fn element(&mut self) -> PResult<Option<ElementSpec>> {
        let mut el = ElementSpec::default();
        let mut any = false;
        if let Some(name) = self.name()? {
            el.name = Some(name);
            any = true;
        }
        loop {
            match self.peek() {
                Some('#') => {
                    self.pos += 1;
                    let value = self.short_value()?;
                    el.attrs.push(AttrSpec {
                        name: text("id"),
                        value: Some(value),
                        boolean: false,
                    });
                }
                Some('.') => {
                    self.pos += 1;
                    let value = self.short_value()?;
                    el.attrs.push(AttrSpec {
                        name: text("class"),
                        value: Some(value),
                        boolean: false,
                    });
                }
                Some('[') => {
                    self.pos += 1;
                    self.attr_set(&mut el.attrs)?;
                }
                Some('{') if el.text.is_none() => {
                    self.pos += 1;
                    el.text = Some(self.text_body()?);
                }
                Some('*') if any && el.repeat.is_none() => el.repeat = self.repeat()?,
                Some('/') if any && !el.self_close => {
                    self.pos += 1;
                    el.self_close = true;
                }
                _ => break,
            }
            any = true;
        }
        Ok(any.then_some(el))
    }

    fn name(&mut self) -> PResult<Option<Pieces>> {
        let mut b = PiecesBuilder::default();
        loop {
            match self.peek() {
                Some(c) if is_name_char(c) => {
                    b.push_char(c);
                    self.pos += 1;
                }
                Some('$') => match self.dollar()? {
                    Piece::Field(_) => return Err(ParseError),
                    piece => b.push(piece),
                },
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                // `ul+`, `table+`...: a trailing `+` that belongs to a snippet name.
                Some('+')
                    if matches!(self.peek_at(1), None | Some(')' | '^' | '+'))
                        && b.plain().is_some_and(|n| {
                            !n.is_empty() && snippets::lookup(&format!("{n}+")).is_some()
                        }) =>
                {
                    b.push_char('+');
                    self.pos += 1;
                }
                _ => break,
            }
        }
        Ok((!b.is_empty()).then(|| b.finish()))
    }

    /// The value after `#` or `.`.
    fn short_value(&mut self) -> PResult<Pieces> {
        let mut b = PiecesBuilder::default();
        loop {
            match self.peek() {
                Some(c) if is_name_char(c) => {
                    b.push_char(c);
                    self.pos += 1;
                }
                // `.w-1/2`, `.bg-black/50` (Tailwind): a slash between two
                // alphanumerics is part of the name, not a self-closing mark.
                Some('/')
                    if self.pos > 0
                        && self.s[self.pos - 1].is_alphanumeric()
                        && self.peek_at(1).is_some_and(char::is_alphanumeric) =>
                {
                    b.push_char('/');
                    self.pos += 1;
                }
                Some('$') => {
                    let piece = self.dollar()?;
                    b.push(piece);
                }
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                _ => break,
            }
        }
        if b.is_empty() {
            return Err(ParseError);
        }
        Ok(b.finish())
    }

    /// After `\`: the escaped character, taken literally.
    fn escaped(&mut self) -> PResult<char> {
        self.pos += 1;
        let c = self.peek().ok_or(ParseError)?;
        self.pos += 1;
        Ok(c)
    }

    /// At `$`: a field (`${1:text}`), a template-literal variable kept as
    /// typed (`${total}`) or a numbering token (`$$@-3`).
    fn dollar(&mut self) -> PResult<Piece> {
        if self.peek_at(1) == Some('{')
            && self
                .peek_at(2)
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            let mut literal = String::from("${");
            self.pos += 2;
            while let Some(c) = self.peek() {
                self.pos += 1;
                literal.push(c);
                if c == '}' {
                    return Ok(Piece::Text(literal));
                }
            }
            return Err(ParseError);
        }
        if self.peek_at(1) == Some('{') {
            self.pos += 2;
            self.digits();
            let mut placeholder = String::new();
            if self.eat(':') {
                let mut depth = 0usize;
                loop {
                    match self.peek() {
                        None => return Err(ParseError),
                        Some('}') if depth == 0 => break,
                        Some(c) => {
                            if c == '{' {
                                depth += 1;
                            } else if c == '}' {
                                depth -= 1;
                            }
                            placeholder.push(c);
                            self.pos += 1;
                        }
                    }
                }
            }
            if !self.eat('}') {
                return Err(ParseError);
            }
            return Ok(Piece::Field(placeholder));
        }
        let mut size = 0;
        while self.eat('$') {
            size += 1;
        }
        if size > MAX_PAD {
            return Err(ParseError);
        }
        let (mut reverse, mut base, mut parent) = (false, 1, 0);
        if self.eat('@') {
            while self.eat('^') {
                parent += 1;
            }
            reverse = self.eat('-');
            let digits = self.digits();
            if !digits.is_empty() {
                base = digits.parse().map_err(|_| ParseError)?;
            }
        }
        Ok(Piece::Number {
            size,
            reverse,
            base,
            parent,
        })
    }

    /// After `[`: attributes up to the closing `]`.
    fn attr_set(&mut self, out: &mut Vec<AttrSpec>) -> PResult<()> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.pos += 1;
            }
            match self.peek() {
                None => return Err(ParseError),
                Some(']') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('"' | '\'') => {
                    let value = self.quoted()?;
                    out.push(AttrSpec {
                        name: Vec::new(),
                        value: Some(value),
                        boolean: false,
                    });
                }
                Some(_) => {
                    let attr = self.attribute()?;
                    out.push(attr);
                }
            }
        }
    }

    /// `name`, `name=value`, `name="quoted value"`, `name.` (boolean) or
    /// `!name` (implied; treated like `name`).
    fn attribute(&mut self) -> PResult<AttrSpec> {
        let mut b = PiecesBuilder::default();
        loop {
            match self.peek() {
                None => return Err(ParseError),
                Some(c) if c.is_whitespace() || c == '=' || c == ']' => break,
                Some('"' | '\'' | '[') => return Err(ParseError),
                Some('$') => {
                    let piece = self.dollar()?;
                    if matches!(piece, Piece::Field(_)) {
                        return Err(ParseError);
                    }
                    b.push(piece);
                }
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                Some(c) => {
                    b.push_char(c);
                    self.pos += 1;
                }
            }
        }
        let mut name = b.finish();
        if let Some(Piece::Text(first)) = name.first_mut() {
            if let Some(rest) = first.strip_prefix('!') {
                *first = rest.to_string();
            }
        }
        let mut boolean = false;
        if let Some(Piece::Text(last)) = name.last_mut() {
            if let Some(rest) = last.strip_suffix('.') {
                *last = rest.to_string();
                boolean = true;
            }
        }
        name.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
        if name.is_empty() {
            return Err(ParseError);
        }
        let value = if self.eat('=') {
            Some(match self.peek() {
                Some('"' | '\'') => self.quoted()?,
                _ => self.unquoted()?,
            })
        } else {
            None
        };
        Ok(AttrSpec {
            name,
            value,
            boolean,
        })
    }

    fn unquoted(&mut self) -> PResult<Pieces> {
        let mut b = PiecesBuilder::default();
        loop {
            match self.peek() {
                None => return Err(ParseError),
                Some(c) if c.is_whitespace() || c == ']' => break,
                Some('$') => {
                    let piece = self.dollar()?;
                    b.push(piece);
                }
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                Some(c) => {
                    b.push_char(c);
                    self.pos += 1;
                }
            }
        }
        Ok(b.finish())
    }

    /// At a quote: everything up to the matching quote.
    fn quoted(&mut self) -> PResult<Pieces> {
        let quote = self.peek().ok_or(ParseError)?;
        self.pos += 1;
        let mut b = PiecesBuilder::default();
        loop {
            match self.peek() {
                None => return Err(ParseError),
                Some(c) if c == quote => {
                    self.pos += 1;
                    break;
                }
                Some('$') => {
                    let piece = self.dollar()?;
                    b.push(piece);
                }
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                Some(c) => {
                    b.push_char(c);
                    self.pos += 1;
                }
            }
        }
        Ok(b.finish())
    }

    /// After `{`: text up to the matching `}` (nested braces are kept).
    fn text_body(&mut self) -> PResult<Pieces> {
        let mut b = PiecesBuilder::default();
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return Err(ParseError),
                Some('}') if depth == 0 => {
                    self.pos += 1;
                    break;
                }
                Some('$') => {
                    let piece = self.dollar()?;
                    b.push(piece);
                }
                Some('\\') => {
                    let c = self.escaped()?;
                    b.push_char(c);
                }
                Some(c) => {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                    }
                    b.push_char(c);
                    self.pos += 1;
                }
            }
        }
        Ok(b.finish())
    }
}

fn text(s: &str) -> Pieces {
    vec![Piece::Text(s.to_string())]
}

/// Hands `children` to the last item of the list now on top of `stack`.
fn attach(stack: &mut [Vec<Item>], children: Vec<Item>) {
    if let Some(parent) = stack.last_mut().and_then(|list| list.last_mut()) {
        parent.children_mut().extend(children);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(item: &Item) -> String {
        match item {
            Item::Element(e) => match e.name.as_deref() {
                Some([Piece::Text(t)]) => t.clone(),
                _ => String::new(),
            },
            Item::Group { .. } => "()".into(),
        }
    }

    fn shape(items: &[Item]) -> String {
        items
            .iter()
            .map(|i| {
                let kids = match i {
                    Item::Element(e) => &e.children,
                    Item::Group { items, .. } => items,
                };
                if kids.is_empty() {
                    name_of(i)
                } else {
                    format!("{}[{}]", name_of(i), shape(kids))
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn operators_build_the_tree() {
        assert_eq!(shape(&parse("a>b+c").unwrap()), "a[b,c]");
        assert_eq!(shape(&parse("a>b>c^d").unwrap()), "a[b[c],d]");
        assert_eq!(shape(&parse("a>b>c^^d").unwrap()), "a[b[c]],d");
        assert_eq!(shape(&parse("a>(b>c)+d").unwrap()), "a[()[b[c]],d]");
        assert_eq!(shape(&parse("a^^b").unwrap()), "a,b");
        assert_eq!(shape(&parse("div>").unwrap()), "div");
    }

    #[test]
    fn element_parts() {
        let items = parse("a#x.y.z[href='#' title=t data-n]{Hi}*3/").unwrap();
        let Item::Element(e) = &items[0] else {
            panic!()
        };
        assert_eq!(e.attrs.len(), 6);
        assert_eq!(e.text, Some(vec![Piece::Text("Hi".into())]));
        assert_eq!(e.repeat.map(|r| r.count), Some(3));
        assert!(e.self_close);
        assert_eq!(e.attrs[5].value, None);
    }

    #[test]
    fn numbering_tokens() {
        let items = parse("li.x$$@-3").unwrap();
        let Item::Element(e) = &items[0] else {
            panic!()
        };
        assert_eq!(
            e.attrs[0].value.as_ref().unwrap()[1],
            Piece::Number {
                size: 2,
                reverse: true,
                base: 3,
                parent: 0
            }
        );
    }

    #[test]
    fn dollar_forms() {
        let text_of = |abbr: &str| match &parse(abbr).unwrap()[0] {
            Item::Element(e) => e.text.clone().unwrap(),
            Item::Group { .. } => panic!("group"),
        };
        assert_eq!(text_of("p{${1:hi}}"), [Piece::Field("hi".into())]);
        assert_eq!(text_of("p{${}}"), [Piece::Field(String::new())]);
        // A template-literal variable is plain text.
        assert_eq!(
            text_of("p{Sum: ${total}}"),
            [Piece::Text("Sum: ".into()), Piece::Text("${total}".into())]
        );
        assert!(parse("p{${total").is_err());
        assert!(parse("p{${1:x").is_err());
    }

    #[test]
    fn slashes_in_class_names() {
        let class_of = |abbr: &str| match &parse(abbr).unwrap()[0] {
            Item::Element(e) => (e.attrs[0].value.clone().unwrap(), e.self_close),
            Item::Group { .. } => panic!("group"),
        };
        assert_eq!(
            class_of(".w-1/2"),
            (vec![Piece::Text("w-1/2".into())], false)
        );
        assert_eq!(
            class_of(".bg-black/50"),
            (vec![Piece::Text("bg-black/50".into())], false)
        );
        assert_eq!(class_of("br.x/"), (vec![Piece::Text("x".into())], true));
    }

    #[test]
    fn plus_snippet_names() {
        assert_eq!(shape(&parse("ul+").unwrap()), "ul+");
        assert_eq!(shape(&parse("div+ul+").unwrap()), "div,ul+");
        assert_eq!(shape(&parse("ul+p").unwrap()), "ul,p");
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "div[",
            "p{",
            "(div",
            "div)",
            ">p",
            "a b",
            "div.",
            "a[=x]",
            "li**2",
            "a]",
            "${1}",
            "()",
            "(()*9)*9",
            "div*10001",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
        // Nesting is bounded so later recursive passes cannot overflow the stack.
        assert!(parse(&["a"; 100].join(">")).is_err());
        assert!(parse(&format!("{}a{}", "(".repeat(100), ")".repeat(100))).is_err());
        assert!(parse(&["a"; 40].join(">")).is_ok());
    }
}

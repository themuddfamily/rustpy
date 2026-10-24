//! Default Django/Jinja expression syntax. Arena AST borrows source ranges;
//! no evaluation or extension registry. Recursion is bounded independently of input.
use crate::template::Dialect;
use std::ops::Range;
pub type NodeId = usize;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Output,
    Condition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnexpectedToken,
    ExpectedExpression,
    ExpectedName,
    ExpectedDelimiter,
    UnterminatedString,
    InvalidNumber,
    InvalidCharacter,
    InvalidArgumentOrder,
    DuplicateKeyword,
    UnsupportedDialectSyntax,
    TooDeep,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub error: Error,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Argument {
    Positional(NodeId),
    Keyword(Range<usize>, NodeId),
    Star(NodeId),
    DoubleStar(NodeId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Name,
    Number,
    String,
    Boolean,
    None,
    Translation(NodeId),
    Unary {
        operator: Range<usize>,
        value: NodeId,
    },
    Binary {
        left: NodeId,
        operator: Range<usize>,
        right: NodeId,
    },
    Compare {
        first: NodeId,
        rest: Vec<(Range<usize>, NodeId)>,
    },
    Conditional {
        value: NodeId,
        condition: NodeId,
        otherwise: Option<NodeId>,
    },
    Attribute {
        value: NodeId,
        name: Range<usize>,
    },
    Index {
        value: NodeId,
        index: NodeId,
    },
    Slice {
        start: Option<NodeId>,
        stop: Option<NodeId>,
        step: Option<NodeId>,
    },
    Call {
        value: NodeId,
        arguments: Vec<Argument>,
    },
    Filter {
        value: NodeId,
        name: Range<usize>,
        arguments: Vec<Argument>,
    },
    Test {
        value: NodeId,
        name: Range<usize>,
        negated: bool,
        arguments: Vec<Argument>,
    },
    List(Vec<NodeId>),
    Tuple(Vec<NodeId>),
    Dict(Vec<(NodeId, NodeId)>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub range: Range<usize>,
    pub kind: Kind,
}
#[derive(Debug)]
pub struct Expression<'a> {
    pub source: &'a str,
    pub nodes: Vec<Node>,
    pub root: Option<NodeId>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug)]
struct Token {
    range: Range<usize>,
    text: String,
    category: Category,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Name,
    Number,
    String,
    Symbol,
    End,
}
fn lex(src: &str, dialect: Dialect) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let c = src[i..].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let category;
        if c == '\'' || c == '"' {
            category = Category::String;
            i += 1;
            let mut closed = false;
            while i < src.len() {
                let ch = src[i..].chars().next().unwrap();
                i += ch.len_utf8();
                if ch == '\\' {
                    if let Some(next) = src[i..].chars().next() {
                        i += next.len_utf8();
                    }
                } else if ch == c {
                    closed = true;
                    break;
                }
            }
            if !closed {
                errors.push(Diagnostic {
                    range: start..i,
                    error: Error::UnterminatedString,
                });
            }
        } else if c == '_' || unicode_ident::is_xid_start(c) {
            category = Category::Name;
            i += c.len_utf8();
            while let Some(ch) = src[i..].chars().next() {
                if ch == '_' || unicode_ident::is_xid_continue(ch) {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
        } else if c.is_ascii_digit() {
            category = Category::Number;
            i += 1;
            if dialect == Dialect::Jinja
                && c == '0'
                && src
                    .as_bytes()
                    .get(i)
                    .is_some_and(|b| matches!(b, b'x' | b'X' | b'o' | b'O' | b'b' | b'B'))
            {
                let base = src.as_bytes()[i].to_ascii_lowercase();
                i += 1;
                let digits = i;
                while src
                    .as_bytes()
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    i += 1;
                }
                let value = &src[digits..i];
                let radix = match base {
                    b'x' => 16,
                    b'o' => 8,
                    _ => 2,
                };
                if value.is_empty()
                    || value.starts_with('_')
                    || value.ends_with('_')
                    || value.contains("__")
                    || !value
                        .chars()
                        .filter(|c| *c != '_')
                        .all(|c| c.is_digit(radix))
                {
                    errors.push(Diagnostic {
                        range: start..i,
                        error: Error::InvalidNumber,
                    });
                }
            } else {
                while src
                    .as_bytes()
                    .get(i)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                {
                    i += 1;
                }
                if src.as_bytes().get(i) == Some(&b'.')
                    && src.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit)
                {
                    i += 1;
                    while src
                        .as_bytes()
                        .get(i)
                        .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                    {
                        i += 1;
                    }
                }
                if src
                    .as_bytes()
                    .get(i)
                    .is_some_and(|b| matches!(b, b'e' | b'E'))
                {
                    i += 1;
                    if src
                        .as_bytes()
                        .get(i)
                        .is_some_and(|b| matches!(b, b'+' | b'-'))
                    {
                        i += 1;
                    }
                    let exponent = i;
                    while src.as_bytes().get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if i == exponent {
                        errors.push(Diagnostic {
                            range: start..i,
                            error: Error::InvalidNumber,
                        });
                    }
                }
                let value = &src[start..i];
                if value.ends_with('_')
                    || value.contains("__")
                    || value.contains("_.")
                    || value.contains("._")
                    || dialect == Dialect::Django && value.contains('_')
                {
                    errors.push(Diagnostic {
                        range: start..i,
                        error: Error::InvalidNumber,
                    });
                }
            }
        } else {
            category = Category::Symbol;
            i += c.len_utf8();
            if ["==", "!=", ">=", "<=", "//", "**"]
                .iter()
                .any(|op| src[start..].starts_with(op))
            {
                i = start + 2;
            } else if !"+-*/%~<>=!.,:()[]{}|".contains(c) {
                errors.push(Diagnostic {
                    range: start..i,
                    error: Error::InvalidCharacter,
                });
            }
        }
        tokens.push(Token {
            range: start..i,
            text: src[start..i].to_owned(),
            category,
        });
    }
    tokens.push(Token {
        range: src.len()..src.len(),
        text: String::new(),
        category: Category::End,
    });
    (tokens, errors)
}
struct Parser<'a> {
    result: Expression<'a>,
    tokens: Vec<Token>,
    at: usize,
    dialect: Dialect,
    context: Context,
    depth: usize,
}
impl Parser<'_> {
    fn token(&self) -> &Token {
        &self.tokens[self.at]
    }
    fn is(&self, text: &str) -> bool {
        self.token().text == text
    }
    fn take(&mut self) -> Token {
        let token = self.token().clone();
        if token.category != Category::End {
            self.at += 1;
        }
        token
    }
    fn eat(&mut self, text: &str) -> bool {
        if self.is(text) {
            self.take();
            true
        } else {
            false
        }
    }
    fn error(&mut self, error: Error) {
        self.result.diagnostics.push(Diagnostic {
            range: self.token().range.clone(),
            error,
        });
    }
    fn require(&mut self, text: &str) -> bool {
        if self.eat(text) {
            true
        } else {
            self.error(Error::ExpectedDelimiter);
            false
        }
    }
    fn node(&mut self, start: usize, kind: Kind) -> NodeId {
        let end = self.tokens[self.at.saturating_sub(1)].range.end;
        let id = self.result.nodes.len();
        self.result.nodes.push(Node {
            range: start..end,
            kind,
        });
        id
    }
    fn name(&mut self) -> Option<Range<usize>> {
        if self.token().category == Category::Name {
            let token = self.take();
            if self.dialect == Dialect::Django && token.text.starts_with('_') {
                self.result.diagnostics.push(Diagnostic {
                    range: token.range,
                    error: Error::UnsupportedDialectSyntax,
                });
                None
            } else {
                Some(token.range)
            }
        } else {
            self.error(Error::ExpectedName);
            None
        }
    }
    fn qualified_name(&mut self) -> Option<Range<usize>> {
        let mut name = self.name()?;
        if self.dialect == Dialect::Jinja {
            while self.eat(".") {
                name.end = self.name()?.end;
            }
        }
        Some(name)
    }
    fn expression(&mut self, min: u8) -> Option<NodeId> {
        if self.depth >= 128 {
            self.error(Error::TooDeep);
            return None;
        }
        self.depth += 1;
        let result = self.expression_inner(min);
        self.depth -= 1;
        result
    }
    fn expression_inner(&mut self, min: u8) -> Option<NodeId> {
        let start = self.token().range.start;
        let mut left = if self.is("not") {
            if self.dialect == Dialect::Django && self.context == Context::Output {
                self.error(Error::UnsupportedDialectSyntax);
                return None;
            }
            let op = self.take().range;
            let value = self.expression(25)?;
            self.node(
                start,
                Kind::Unary {
                    operator: op,
                    value,
                },
            )
        } else if self.is("+") || self.is("-") {
            if self.dialect == Dialect::Django {
                // Django permits signed numeric literals only.
                let op = self.take().range;
                if self.token().category != Category::Number {
                    self.error(Error::UnsupportedDialectSyntax);
                    return None;
                }
                let value = self.atom()?;
                self.node(
                    start,
                    Kind::Unary {
                        operator: op,
                        value,
                    },
                )
            } else {
                let op = self.take().range;
                let value = self.expression(90)?;
                self.node(
                    start,
                    Kind::Unary {
                        operator: op,
                        value,
                    },
                )
            }
        } else {
            self.atom()?
        };
        loop {
            // Lookup/call suffixes and filters bind more tightly than arithmetic.
            if 90 >= min && self.is(".") {
                self.take();
                let name = if self.token().category == Category::Number {
                    self.take().range
                } else {
                    self.name()?
                };
                left = self.node(start, Kind::Attribute { value: left, name });
                continue;
            }
            if self.dialect == Dialect::Jinja && 90 >= min && self.is("[") {
                self.take();
                let index = self.subscript()?;
                self.require("]");
                left = self.node(start, Kind::Index { value: left, index });
                continue;
            }
            if self.dialect == Dialect::Jinja && 90 >= min && self.is("(") {
                let arguments = self.arguments()?;
                left = self.node(
                    start,
                    Kind::Call {
                        value: left,
                        arguments,
                    },
                );
                continue;
            }
            if 85 >= min && self.is("|") {
                let pipe = self.take().range;
                if self.dialect == Dialect::Django
                    && (self.result.nodes[left].range.end != pipe.start
                        || self.token().range.start != pipe.end)
                {
                    self.error(Error::UnsupportedDialectSyntax);
                    return None;
                }
                let name = self.qualified_name()?;
                let arguments = if self.dialect == Dialect::Jinja && self.is("(") {
                    self.arguments()?
                } else if self.dialect == Dialect::Django && self.is(":") {
                    let colon = self.take().range;
                    if name.end != colon.start || self.token().range.start != colon.end {
                        self.error(Error::UnsupportedDialectSyntax);
                        return None;
                    }
                    vec![Argument::Positional(self.django_value()?)]
                } else {
                    Vec::new()
                };
                left = self.node(
                    start,
                    Kind::Filter {
                        value: left,
                        name,
                        arguments,
                    },
                );
                continue;
            }
            if self.dialect == Dialect::Jinja && 85 >= min && self.eat("is") {
                let negated = self.eat("not");
                let name = self.qualified_name()?;
                let arguments = if self.is("(") {
                    self.arguments()?
                } else if self.starts_atom()
                    && !matches!(
                        self.token().text.as_str(),
                        "if" | "else" | "and" | "or" | "not" | "in" | "is"
                    )
                {
                    vec![Argument::Positional(self.expression(90)?)]
                } else {
                    Vec::new()
                };
                if self.is("is") {
                    self.error(Error::UnexpectedToken);
                    return None;
                }
                left = self.node(
                    start,
                    Kind::Test {
                        value: left,
                        name,
                        negated,
                        arguments,
                    },
                );
                continue;
            }
            if min == 0 && self.dialect == Dialect::Jinja && self.eat("if") {
                let condition = self.expression(1)?;
                let otherwise = if self.eat("else") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                left = self.node(
                    start,
                    Kind::Conditional {
                        value: left,
                        condition,
                        otherwise,
                    },
                );
                continue;
            }
            let op = self.token().text.as_str();
            let comparison = matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=" | "in")
                || self.dialect == Dialect::Django && op == "is"
                || op == "not" && self.tokens.get(self.at + 1).is_some_and(|t| t.text == "in");
            let bp = if comparison {
                30
            } else {
                match op {
                    "or" => 10,
                    "and" => 20,
                    "~" => 55,
                    "+" | "-" => 50,
                    "*" | "/" | "//" | "%" => 60,
                    "**" => 70,
                    _ => break,
                }
            };
            if bp < min {
                break;
            }
            if self.dialect == Dialect::Django
                && (self.context == Context::Output || !comparison && !matches!(op, "and" | "or"))
            {
                self.error(Error::UnsupportedDialectSyntax);
                return None;
            }
            let mut operator = self.take().range;
            if &self.result.source[operator.clone()] == "not"
                || self.dialect == Dialect::Django
                    && &self.result.source[operator.clone()] == "is"
                    && self.is("not")
            {
                operator.end = self.take().range.end;
            }
            let right = self.expression(bp + 1)?;
            if comparison && self.dialect == Dialect::Jinja {
                if let Kind::Compare { rest, .. } = &mut self.result.nodes[left].kind {
                    rest.push((operator, right));
                    self.result.nodes[left].range.end =
                        self.tokens[self.at.saturating_sub(1)].range.end;
                } else {
                    left = self.node(
                        start,
                        Kind::Compare {
                            first: left,
                            rest: vec![(operator, right)],
                        },
                    );
                }
            } else {
                left = self.node(
                    start,
                    Kind::Binary {
                        left,
                        operator,
                        right,
                    },
                );
            }
        }
        Some(left)
    }
    fn starts_atom(&self) -> bool {
        matches!(
            self.token().category,
            Category::Name | Category::Number | Category::String
        ) || matches!(self.token().text.as_str(), "(" | "[" | "{")
    }
    fn django_value(&mut self) -> Option<NodeId> {
        let start = self.token().range.start;
        let mut value = if self.is("+") || self.is("-") {
            let operator = self.take().range;
            if self.token().category != Category::Number {
                self.error(Error::ExpectedExpression);
                return None;
            }
            let value = self.atom()?;
            self.node(start, Kind::Unary { operator, value })
        } else {
            self.atom()?
        };
        while self.eat(".") {
            let name = if self.token().category == Category::Number {
                self.take().range
            } else {
                self.name()?
            };
            value = self.node(start, Kind::Attribute { value, name });
        }
        Some(value)
    }
    fn atom(&mut self) -> Option<NodeId> {
        let token = self.take();
        let start = token.range.start;
        let kind = match token.category {
            Category::Name => {
                if self.dialect == Dialect::Django && token.text == "_" && self.eat("(") {
                    if self.token().category != Category::String {
                        self.error(Error::ExpectedExpression);
                        return None;
                    }
                    let value = self.atom()?;
                    self.require(")");
                    return Some(self.node(start, Kind::Translation(value)));
                }
                if self.dialect == Dialect::Django && token.text.starts_with('_') {
                    self.result.diagnostics.push(Diagnostic {
                        range: token.range,
                        error: Error::UnsupportedDialectSyntax,
                    });
                    return None;
                }
                match token.text.as_str() {
                    "True" | "False" => Kind::Boolean,
                    "true" | "false" if self.dialect == Dialect::Jinja => Kind::Boolean,
                    "None" => Kind::None,
                    "none" if self.dialect == Dialect::Jinja => Kind::None,
                    "and" | "or" | "not" | "in" | "is" | "if" | "else" => {
                        self.result.diagnostics.push(Diagnostic {
                            range: token.range,
                            error: Error::ExpectedExpression,
                        });
                        return None;
                    }
                    _ => Kind::Name,
                }
            }
            Category::Number => Kind::Number,
            Category::String => {
                if self.dialect == Dialect::Jinja {
                    while self.token().category == Category::String {
                        self.take();
                    }
                }
                Kind::String
            }
            _ => {
                if self.dialect == Dialect::Django {
                    self.result.diagnostics.push(Diagnostic {
                        range: token.range,
                        error: Error::UnsupportedDialectSyntax,
                    });
                    return None;
                }
                match token.text.as_str() {
                    "(" => {
                        if self.eat(")") {
                            Kind::Tuple(Vec::new())
                        } else {
                            let first = self.expression(0)?;
                            if self.eat(",") {
                                let mut values = vec![first];
                                while !self.is(")") && self.token().category != Category::End {
                                    values.push(self.expression(0)?);
                                    if !self.eat(",") {
                                        break;
                                    }
                                }
                                self.require(")");
                                Kind::Tuple(values)
                            } else {
                                self.require(")");
                                return Some(first);
                            }
                        }
                    }
                    "[" => {
                        let mut values = Vec::new();
                        while !self.is("]") && self.token().category != Category::End {
                            values.push(self.expression(0)?);
                            if !self.eat(",") {
                                break;
                            }
                        }
                        self.require("]");
                        Kind::List(values)
                    }
                    "{" => {
                        let mut values = Vec::new();
                        while !self.is("}") && self.token().category != Category::End {
                            let key = self.expression(0)?;
                            self.require(":");
                            let value = self.expression(0)?;
                            values.push((key, value));
                            if !self.eat(",") {
                                break;
                            }
                        }
                        self.require("}");
                        Kind::Dict(values)
                    }
                    _ => {
                        self.result.diagnostics.push(Diagnostic {
                            range: token.range,
                            error: Error::ExpectedExpression,
                        });
                        return None;
                    }
                }
            }
        };
        Some(self.node(start, kind))
    }
    fn subscript_item(&mut self) -> Option<NodeId> {
        let start = self.token().range.start;
        let first = if self.is(":") {
            None
        } else {
            Some(self.expression(0)?)
        };
        if self.eat(":") {
            let stop = if self.is(":") || self.is("]") || self.is(",") {
                None
            } else {
                Some(self.expression(0)?)
            };
            let step = if self.eat(":") && !self.is("]") && !self.is(",") {
                Some(self.expression(0)?)
            } else {
                None
            };
            Some(self.node(
                start,
                Kind::Slice {
                    start: first,
                    stop,
                    step,
                },
            ))
        } else {
            first
        }
    }
    fn subscript(&mut self) -> Option<NodeId> {
        let start = self.token().range.start;
        let one = self.subscript_item()?;
        if self.eat(",") {
            let mut values = vec![one];
            while !self.is("]") && self.token().category != Category::End {
                values.push(self.subscript_item()?);
                if !self.eat(",") {
                    break;
                }
            }
            Some(self.node(start, Kind::Tuple(values)))
        } else {
            Some(one)
        }
    }
    fn arguments(&mut self) -> Option<Vec<Argument>> {
        self.require("(");
        let source = self.result.source;
        let mut args = Vec::new();
        let mut keywords = std::collections::HashSet::new();
        let mut keyword = false;
        let mut star = false;
        let mut double = false;
        while !self.is(")") && self.token().category != Category::End {
            let arg = if self.eat("**") {
                if double {
                    self.error(Error::InvalidArgumentOrder);
                }
                double = true;
                Argument::DoubleStar(self.expression(0)?)
            } else if self.eat("*") {
                if star || double {
                    self.error(Error::InvalidArgumentOrder);
                }
                star = true;
                Argument::Star(self.expression(0)?)
            } else if self.token().category == Category::Name
                && self.tokens.get(self.at + 1).is_some_and(|t| t.text == "=")
            {
                if double {
                    self.error(Error::InvalidArgumentOrder);
                }
                keyword = true;
                let name = self.take().range;
                self.take();
                let text = &source[name.clone()];
                if !keywords.insert(text) {
                    self.result.diagnostics.push(Diagnostic {
                        range: name.clone(),
                        error: Error::DuplicateKeyword,
                    });
                }
                Argument::Keyword(name, self.expression(0)?)
            } else {
                if keyword || star || double {
                    self.error(Error::InvalidArgumentOrder);
                }
                Argument::Positional(self.expression(0)?)
            };
            args.push(arg);
            if !self.eat(",") {
                break;
            }
        }
        self.require(")");
        Some(args)
    }
}
pub fn parse(source: &str, dialect: Dialect, context: Context) -> Expression<'_> {
    let (tokens, diagnostics) = lex(source, dialect);
    let mut parser = Parser {
        result: Expression {
            source,
            nodes: Vec::new(),
            root: None,
            diagnostics,
        },
        tokens,
        at: 0,
        dialect,
        context,
        depth: 0,
    };
    let minimum = if context == Context::Condition { 1 } else { 0 };
    let mut root = parser.expression(minimum);
    if parser.dialect == Dialect::Jinja && parser.eat(",") {
        if let Some(first) = root {
            let start = parser.result.nodes[first].range.start;
            let mut values = vec![first];
            while parser.token().category != Category::End {
                let Some(value) = parser.expression(minimum) else {
                    break;
                };
                values.push(value);
                if !parser.eat(",") {
                    break;
                }
            }
            root = Some(parser.node(start, Kind::Tuple(values)));
        }
    }
    parser.result.root = root;
    if parser.token().category != Category::End {
        parser.error(Error::UnexpectedToken);
    }
    parser.result
}

//! Source-preserving default-delimiter Django/Jinja structure, without evaluation.
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Django,
    Jinja,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Expression,
    Comment,
    Statement,
    RawText,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Region {
    pub range: Range<usize>,
    pub content: Range<usize>,
    pub kind: Kind,
    pub parent: Option<usize>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Block {
    pub opener: usize,
    pub end: Option<usize>,
    pub branches: Vec<usize>,
    pub parent: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnclosedDelimiter,
    EmptyConstruct,
    InvalidExpression,
    InvalidStatement,
    InvalidBuiltin,
    UnexpectedEnd,
    MismatchedEnd,
    InvalidBranch,
    UnclosedBlock,
    NamedEndMismatch,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub error: Error,
}
#[derive(Debug)]
pub struct Document<'a> {
    pub source: &'a str,
    pub regions: Vec<Region>,
    pub blocks: Vec<Block>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Compatibility lexical ranges used by the existing highlighter. Deliberately
/// retains its first-delimiter semantics; structural consumers should use parse.
pub fn highlight_constructs(src: &str) -> Vec<(usize, usize, u8)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && matches!(b[i + 1], b'{' | b'%' | b'#') {
            let kind = b[i + 1];
            let close: &[u8] = match kind {
                b'{' => b"}}",
                b'%' => b"%}",
                _ => b"#}",
            };
            let end = b[i + 2..]
                .windows(2)
                .position(|w| w == close)
                .map(|p| i + 2 + p + 2)
                .unwrap_or(b.len());
            out.push((i, end, kind));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}
fn trim(src: &str, range: Range<usize>) -> Range<usize> {
    let s = &src[range.clone()];
    let left = s.len() - s.trim_start().len();
    let right = s.trim_end().len();
    range.start + left..range.start + right.max(left)
}
fn closing(src: &str, start: usize, kind: u8) -> Option<usize> {
    let b = src.as_bytes();
    let mut i = start;
    let mut quote = 0;
    let mut depth = 0usize;
    while i + 1 < b.len() {
        if quote != 0 {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == quote {
                quote = 0;
            }
        } else if kind != b'#' && matches!(b[i], b'\'' | b'"') {
            quote = b[i];
        } else {
            let end = match kind {
                b'{' => b[i] == b'}' && b[i + 1] == b'}' && depth == 0,
                b'%' => b[i] == b'%' && b[i + 1] == b'}',
                _ => b[i] == b'#' && b[i + 1] == b'}',
            };
            if end {
                return Some(i);
            }
            if kind == b'{' {
                if matches!(b[i], b'{' | b'[' | b'(') {
                    depth += 1;
                } else if matches!(b[i], b'}' | b']' | b')') {
                    depth = depth.saturating_sub(1);
                }
            }
        }
        i += 1;
    }
    None
}
/// Default tag-token boundary (Jinja allows call(...), unlike Django).
pub(crate) fn statement_tag(source: &str, dialect: Dialect) -> &str {
    if dialect == Dialect::Django {
        return source.split_whitespace().next().unwrap_or("");
    }
    let length = source
        .chars()
        .take_while(|c| *c == '_' || unicode_ident::is_xid_continue(*c))
        .map(char::len_utf8)
        .sum::<usize>();
    &source[..length]
}
fn unquoted_equals(body: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let bytes = body.as_bytes();
    for (offset, c) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote.is_some() && c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if matches!(c, '(' | '[' | '{') {
            depth += 1;
        } else if matches!(c, ')' | ']' | '}') {
            depth = depth.saturating_sub(1);
        } else if c == '='
            && depth == 0
            && !bytes.get(offset + 1).is_some_and(|b| *b == b'=')
            && !offset
                .checked_sub(1)
                .and_then(|i| bytes.get(i))
                .is_some_and(|b| matches!(b, b'=' | b'!' | b'<' | b'>'))
        {
            return true;
        }
    }
    false
}
fn opener(tag: &str, body: &str, dialect: Dialect) -> bool {
    match dialect {
        Dialect::Jinja => {
            matches!(
                tag,
                "if" | "for"
                    | "block"
                    | "macro"
                    | "call"
                    | "filter"
                    | "with"
                    | "autoescape"
                    | "trans"
                    | "raw"
            ) || tag == "set" && !unquoted_equals(body)
        }
        Dialect::Django => matches!(
            tag,
            "if" | "for"
                | "block"
                | "with"
                | "autoescape"
                | "filter"
                | "spaceless"
                | "comment"
                | "verbatim"
                | "language"
                | "localtime"
                | "timezone"
                | "ifchanged"
                | "ifequal"
                | "ifnotequal"
                | "blocktranslate"
                | "blocktrans"
        ),
    }
}
fn branch(tag: &str, open: &str, dialect: Dialect) -> bool {
    match tag {
        "elif" => open == "if",
        "else" => {
            matches!(open, "if" | "ifchanged" | "ifequal" | "ifnotequal")
                || open == "for" && dialect == Dialect::Jinja
        }
        "empty" => open == "for" && dialect == Dialect::Django,
        "pluralize" => dialect == Dialect::Jinja && open == "trans",
        "plural" => dialect == Dialect::Django && matches!(open, "blocktrans" | "blocktranslate"),
        _ => false,
    }
}
/// Regions partition the entire UTF-8 source; all offsets are byte boundaries.
/// Unknown extension tags are statements, not guessed blocks. Syntax expressions
/// and custom configured delimiters are intentionally outside this API.
pub fn parse(src: &str, dialect: Dialect) -> Document<'_> {
    let mut doc = Document {
        source: src,
        regions: Vec::new(),
        blocks: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut stack: Vec<(usize, &str, Option<&str>, bool)> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let parent = stack.last().map(|x| x.0);
        let raw = stack
            .last()
            .filter(|x| matches!(x.1, "raw" | "verbatim" | "comment"));
        let next = if let Some((_, tag, name, _)) = raw {
            let expected = format!("end{tag}");
            let mut cursor = i;
            let mut found = None;
            while let Some(p) = src[cursor..].find("{%") {
                let at = cursor + p;
                // Django's lexer compares the entire trimmed closing contents with
                // "end" + opening contents, including opaque suffix spacing.
                if dialect == Dialect::Django && *tag == "verbatim" {
                    if let Some(close) = src[at + 2..].find("%}") {
                        let contents = src[at + 2..at + 2 + close].trim();
                        let suffix = name.unwrap_or("");
                        if contents == format!("endverbatim{suffix}") {
                            found = Some(at);
                            break;
                        }
                    }
                    cursor = at + 2;
                    continue;
                }
                let mut j = at + 2;
                if dialect == Dialect::Jinja
                    && src
                        .as_bytes()
                        .get(j)
                        .is_some_and(|b| matches!(b, b'-' | b'+'))
                {
                    j += 1;
                }
                while src.as_bytes().get(j).is_some_and(u8::is_ascii_whitespace) {
                    j += 1;
                }
                if src[j..].starts_with(&expected)
                    && src
                        .as_bytes()
                        .get(j + expected.len())
                        .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'%' | b'-' | b'+'))
                {
                    let mut tail = j + expected.len();
                    while src
                        .as_bytes()
                        .get(tail)
                        .is_some_and(u8::is_ascii_whitespace)
                    {
                        tail += 1;
                    }
                    let named = if let Some(name) = name {
                        if src[tail..].starts_with(name) {
                            tail += name.len();
                            true
                        } else {
                            false
                        }
                    } else {
                        true
                    };
                    while src
                        .as_bytes()
                        .get(tail)
                        .is_some_and(u8::is_ascii_whitespace)
                    {
                        tail += 1;
                    }
                    if dialect == Dialect::Jinja
                        && src
                            .as_bytes()
                            .get(tail)
                            .is_some_and(|b| matches!(b, b'-' | b'+'))
                    {
                        tail += 1;
                    }
                    if named && src[tail..].starts_with("%}") {
                        found = Some(at);
                        break;
                    }
                }
                cursor = at + 2;
            }
            found
        } else {
            src.as_bytes()[i..]
                .windows(2)
                .position(|w| w[0] == b'{' && matches!(w[1], b'{' | b'%' | b'#'))
                .map(|p| i + p)
        };
        let Some(start) = next else {
            doc.regions.push(Region {
                range: i..src.len(),
                content: i..src.len(),
                kind: if raw.is_some() {
                    Kind::RawText
                } else {
                    Kind::Text
                },
                parent,
            });
            break;
        };
        if start > i {
            doc.regions.push(Region {
                range: i..start,
                content: i..start,
                kind: if raw.is_some() {
                    Kind::RawText
                } else {
                    Kind::Text
                },
                parent,
            });
        }
        let kind = src.as_bytes()[start + 1];
        let finish = closing(src, start + 2, kind);
        let end = finish.map(|e| e + 2).unwrap_or(src.len());
        let mut content = start + 2..finish.unwrap_or(src.len());
        if dialect == Dialect::Jinja {
            if src
                .as_bytes()
                .get(content.start)
                .is_some_and(|b| matches!(b, b'-' | b'+'))
            {
                content.start += 1;
            }
            if content.end > content.start && matches!(src.as_bytes()[content.end - 1], b'-' | b'+')
            {
                content.end -= 1;
            }
        }
        content = trim(src, content);
        let index = doc.regions.len();
        doc.regions.push(Region {
            range: start..end,
            content: content.clone(),
            kind: match kind {
                b'{' => Kind::Expression,
                b'#' => Kind::Comment,
                _ => Kind::Statement,
            },
            parent,
        });
        if finish.is_none() {
            doc.diagnostics.push(Diagnostic {
                range: start..end,
                error: Error::UnclosedDelimiter,
            });
        }
        if content.is_empty() && kind != b'#' {
            doc.diagnostics.push(Diagnostic {
                range: start..end,
                error: Error::EmptyConstruct,
            });
        }
        if finish.is_some() && !content.is_empty() {
            let body = &src[content.clone()];
            let expression = if kind == b'{' {
                Some((
                    content.start,
                    body,
                    crate::template_expression::Context::Output,
                ))
            } else if kind == b'%' {
                let tag = statement_tag(body, dialect);
                if matches!(tag, "if" | "elif") {
                    let tail = &body[tag.len()..];
                    let expression = tail.trim_start();
                    Some((
                        content.start + tag.len() + tail.len() - expression.len(),
                        expression,
                        crate::template_expression::Context::Condition,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((offset, expression, context)) = expression {
                let parsed = crate::template_expression::parse(expression, dialect, context);
                for diagnostic in parsed.diagnostics {
                    doc.diagnostics.push(Diagnostic {
                        range: offset + diagnostic.range.start..offset + diagnostic.range.end,
                        error: Error::InvalidExpression,
                    });
                }
            }
        }
        if kind == b'%' && finish.is_some() {
            let final_tags = crate::template_final_tags::validate(&src[content.clone()], dialect);
            for diagnostic in final_tags.diagnostics {
                doc.diagnostics.push(Diagnostic {
                    range: content.start + diagnostic.range.start
                        ..content.start + diagnostic.range.end,
                    error: Error::InvalidBuiltin,
                });
            }
            for diagnostic in
                crate::template_builtin::validate(&src[content.clone()], dialect).diagnostics
            {
                doc.diagnostics.push(Diagnostic {
                    range: content.start + diagnostic.range.start
                        ..content.start + diagnostic.range.end,
                    error: Error::InvalidBuiltin,
                });
            }
        }
        if kind == b'%' && finish.is_some() {
            let statement = crate::template_statement::validate(&src[content.clone()], dialect);
            for diagnostic in statement.diagnostics {
                doc.diagnostics.push(Diagnostic {
                    range: content.start + diagnostic.range.start
                        ..content.start + diagnostic.range.end,
                    error: Error::InvalidStatement,
                });
            }
        }
        if kind == b'%' && finish.is_some() {
            let body = &src[content.clone()];
            let tag = statement_tag(body, dialect);
            let name = body[tag.len()..].split_whitespace().next();
            if let Some(wanted) = tag
                .strip_prefix("end")
                .filter(|wanted| opener(wanted, "", dialect))
            {
                if let Some(pos) = stack.iter().rposition(|x| x.1 == wanted) {
                    if pos + 1 != stack.len() {
                        doc.diagnostics.push(Diagnostic {
                            range: start..end,
                            error: Error::MismatchedEnd,
                        });
                    }
                    while stack.len() > pos + 1 {
                        let (b, _, _, _) = stack.pop().unwrap();
                        doc.diagnostics.push(Diagnostic {
                            range: doc.regions[doc.blocks[b].opener].range.clone(),
                            error: Error::UnclosedBlock,
                        });
                    }
                    let (b, open, oldname, _) = stack.pop().unwrap();
                    let invalid_block_end = open == "block"
                        && if dialect == Dialect::Django {
                            body != "endblock"
                                && oldname.is_none_or(|name| body != format!("endblock {name}"))
                        } else {
                            name.is_some() && name != oldname
                        };
                    if invalid_block_end {
                        doc.diagnostics.push(Diagnostic {
                            range: start..end,
                            error: Error::NamedEndMismatch,
                        });
                    }
                    doc.blocks[b].end = Some(index);
                } else {
                    doc.diagnostics.push(Diagnostic {
                        range: start..end,
                        error: Error::UnexpectedEnd,
                    });
                }
            } else if matches!(tag, "elif" | "else" | "empty" | "pluralize" | "plural") {
                if let Some((b, open, _, final_branch)) = stack.last_mut() {
                    if branch(tag, open, dialect) && !*final_branch {
                        doc.blocks[*b].branches.push(index);
                        if tag != "elif" {
                            *final_branch = true;
                        }
                    } else {
                        doc.diagnostics.push(Diagnostic {
                            range: start..end,
                            error: Error::InvalidBranch,
                        });
                    }
                } else {
                    doc.diagnostics.push(Diagnostic {
                        range: start..end,
                        error: Error::InvalidBranch,
                    });
                }
            } else if opener(tag, body, dialect) {
                let b = doc.blocks.len();
                doc.blocks.push(Block {
                    opener: index,
                    end: None,
                    branches: Vec::new(),
                    parent,
                });
                stack.push((
                    b,
                    tag,
                    if tag == "verbatim" {
                        Some(&body[tag.len()..])
                    } else if tag == "block" {
                        name
                    } else {
                        None
                    },
                    false,
                ));
            }
        }
        i = end;
    }
    for (b, _, _, _) in stack {
        doc.diagnostics.push(Diagnostic {
            range: doc.regions[doc.blocks[b].opener].range.clone(),
            error: Error::UnclosedBlock,
        });
    }
    for diagnostic in
        crate::template_builtin::validate_document(src, &doc.regions, &doc.blocks, dialect)
    {
        doc.diagnostics.push(Diagnostic {
            range: diagnostic.range,
            error: Error::InvalidBuiltin,
        });
    }
    doc
}

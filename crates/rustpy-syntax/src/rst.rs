//! Lightweight source-preserving reStructuredText block inventory, not Docutils execution.
use std::ops::Range;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Heading {
        title: Range<usize>,
        adornment: char,
        overline: bool,
    },
    Paragraph,
    Directive {
        name: Range<usize>,
        arguments: Range<usize>,
    },
    Literal,
    ListItem {
        marker: Range<usize>,
        content: Range<usize>,
    },
    Field {
        name: Range<usize>,
        content: Range<usize>,
    },
    ExplicitMarkup,
    Indented,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub range: Range<usize>,
    pub kind: Kind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    ShortAdornment,
    MissingLiteral,
    MalformedDirective,
    EmptyField,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub kind: ErrorKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document<'a> {
    pub source: &'a str,
    pub blocks: Vec<Block>,
    pub diagnostics: Vec<Diagnostic>,
}
struct Line<'a> {
    text: &'a str,
    range: Range<usize>,
    indent: usize,
    trimmed: Range<usize>,
}
fn adornment(s: &str) -> Option<char> {
    let c = s.chars().next()?;
    (c.is_ascii_punctuation() && s.chars().all(|x| x == c)).then_some(c)
}
fn list_marker(s: &str) -> Option<usize> {
    let first = s.chars().next()?;
    if matches!(first, '*' | '+' | '-' | '•' | '‣' | '⁃') {
        let n = first.len_utf8();
        return s[n..].starts_with(char::is_whitespace).then_some(n);
    }
    let end = s.find(['.', ')'])?;
    let token = s[..end].trim_start_matches('(');
    let valid = !token.is_empty()
        && (token == "#"
            || token.chars().all(|c| c.is_ascii_digit())
            || (token.len() == 1 && token.chars().all(|c| c.is_ascii_alphabetic())));
    (valid && s[end + 1..].starts_with(char::is_whitespace)).then_some(end + 1)
}
fn body_end(lines: &[Line<'_>], start: usize) -> usize {
    let mut end = start + 1;
    let mut probe = end;
    while probe < lines.len() {
        if lines[probe].text.trim().is_empty() {
            probe += 1;
            continue;
        }
        if lines[probe].indent <= lines[start].indent {
            break;
        }
        end = probe + 1;
        probe += 1;
    }
    end
}
/// Inventory top-level blocks and list/field entries; nested bodies remain exact
/// source ranges, without pretending to resolve roles, references or directives.
pub fn parse(source: &str) -> Document<'_> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for physical in source.split_inclusive('\n') {
        let text = physical.strip_suffix('\n').unwrap_or(physical);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let left = text.len() - text.trim_start().len();
        let trimmed = offset + left..offset + left + text.trim().len();
        lines.push(Line {
            text,
            range: offset..offset + text.len(),
            indent: text.chars().take_while(|c| c.is_whitespace()).count(),
            trimmed,
        });
        offset += physical.len();
    }
    let mut out = Document {
        source,
        blocks: vec![],
        diagnostics: vec![],
    };
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let text = &source[l.trimmed.clone()];
        if text.is_empty() {
            i += 1;
            continue;
        }
        let mut heading = None;
        if l.indent == 0 {
            if let Some(c) = adornment(text) {
                if i + 2 < lines.len()
                    && lines[i + 1].indent == 0
                    && !lines[i + 1].text.trim().is_empty()
                    && adornment(lines[i + 2].text) == Some(c)
                    && lines[i + 2].text == text
                {
                    heading = Some((i + 1, i + 3, c, true));
                }
            }
            if heading.is_none()
                && i + 1 < lines.len()
                && lines[i + 1].indent == 0
                && adornment(text).is_none()
            {
                if let Some(c) = adornment(lines[i + 1].text) {
                    heading = Some((i, i + 2, c, false));
                }
            }
        }
        if let Some((title, end, c, overline)) = heading {
            if lines[end - 1].text.chars().count() < lines[title].text.chars().count() {
                out.diagnostics.push(Diagnostic {
                    range: lines[end - 1].range.clone(),
                    kind: ErrorKind::ShortAdornment,
                });
            }
            out.blocks.push(Block {
                range: l.range.start..lines[end - 1].range.end,
                kind: Kind::Heading {
                    title: lines[title].range.clone(),
                    adornment: c,
                    overline,
                },
            });
            i = end;
            continue;
        }
        if let Some(explicit) = text.strip_prefix(".. ") {
            let end = body_end(&lines, i);
            // Comments/substitutions may contain :: anywhere. Only a leading
            // isolated reference-name followed immediately by :: is a directive.
            let kind = if let Some(colon) = explicit.find("::").map(|n| n + 3) {
                let name = &text[3..colon];
                let mut previous_punctuation = true;
                let valid = name.chars().all(|c| {
                    if c.is_alphanumeric() {
                        previous_punctuation = false;
                        true
                    } else if matches!(c, '-' | '_' | '.' | '+' | ':') && !previous_punctuation {
                        previous_punctuation = true;
                        true
                    } else {
                        false
                    }
                }) && !previous_punctuation;
                if valid {
                    let rest = &text[colon + 2..];
                    let lead = rest.len() - rest.trim_start().len();
                    Kind::Directive {
                        name: l.trimmed.start + 3..l.trimmed.start + colon,
                        arguments: l.trimmed.start + colon + 2 + lead..l.trimmed.end,
                    }
                } else {
                    Kind::ExplicitMarkup
                }
            } else {
                Kind::ExplicitMarkup
            };
            out.blocks.push(Block {
                range: l.range.start..lines[end - 1].range.end,
                kind,
            });
            i = end;
            continue;
        }
        let field = text
            .strip_prefix(':')
            .and_then(|s| s.find(':').map(|n| n + 1))
            .filter(|&n| {
                text != "::"
                    && (text[n + 1..].is_empty() || text[n + 1..].starts_with(char::is_whitespace))
            });
        let structured = if let Some(n) = field {
            if n == 1 {
                out.diagnostics.push(Diagnostic {
                    range: l.trimmed.clone(),
                    kind: ErrorKind::EmptyField,
                });
            }
            let rest = &text[n + 1..];
            let skip = rest.len() - rest.trim_start().len();
            Some(Kind::Field {
                name: l.trimmed.start + 1..l.trimmed.start + n,
                content: l.trimmed.start + n + 1 + skip..l.trimmed.end,
            })
        } else {
            list_marker(text).map(|n| {
                let rest = &text[n..];
                let skip = rest.len() - rest.trim_start().len();
                Kind::ListItem {
                    marker: l.trimmed.start..l.trimmed.start + n,
                    content: l.trimmed.start + n + skip..l.trimmed.end,
                }
            })
        };
        if let Some(kind) = structured {
            let end = body_end(&lines, i);
            out.blocks.push(Block {
                range: l.range.start..lines[end - 1].range.end,
                kind,
            });
            i = end;
            continue;
        }
        if l.indent > 0 {
            let end = body_end(&lines, i);
            out.blocks.push(Block {
                range: l.range.start..lines[end - 1].range.end,
                kind: Kind::Indented,
            });
            i = end;
            continue;
        }
        let mut end = i + 1;
        while end < lines.len()
            && !lines[end].text.trim().is_empty()
            && lines[end].indent == 0
            && !lines[end].text.starts_with(".. ")
            && !lines[end].text.starts_with(':')
            && list_marker(lines[end].text).is_none()
        {
            if end + 1 < lines.len() && adornment(lines[end + 1].text).is_some() {
                break;
            }
            end += 1;
        }
        out.blocks.push(Block {
            range: l.range.start..lines[end - 1].range.end,
            kind: Kind::Paragraph,
        });
        let paragraph_end = lines[end - 1].text.trim_end();
        let literal = paragraph_end.strip_suffix("::").is_some_and(|prefix| {
            prefix
                .chars()
                .rev()
                .take_while(|&c| c == '\\')
                .count()
                .is_multiple_of(2)
        });
        i = end;
        if literal {
            let mut start = i;
            while start < lines.len() && lines[start].text.trim().is_empty() {
                start += 1;
            }
            if start < lines.len() && lines[start].indent > l.indent {
                let mut finish = start + 1;
                let mut probe = finish;
                while probe < lines.len() {
                    if lines[probe].text.trim().is_empty() {
                        probe += 1;
                        continue;
                    }
                    if lines[probe].indent <= l.indent {
                        break;
                    }
                    finish = probe + 1;
                    probe += 1;
                }
                out.blocks.push(Block {
                    range: lines[start].range.start..lines[finish - 1].range.end,
                    kind: Kind::Literal,
                });
                i = finish;
            } else if start < lines.len()
                && lines[start]
                    .text
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_punctuation())
            {
                let quote = lines[start].text.chars().next().unwrap();
                let mut finish = start + 1;
                while finish < lines.len()
                    && lines[finish].text.starts_with(quote)
                    && !lines[finish].text.trim().is_empty()
                {
                    finish += 1;
                }
                out.blocks.push(Block {
                    range: lines[start].range.start..lines[finish - 1].range.end,
                    kind: Kind::Literal,
                });
                i = finish;
            } else {
                out.diagnostics.push(Diagnostic {
                    range: lines[end - 1].trimmed.clone(),
                    kind: ErrorKind::MissingLiteral,
                });
            }
        }
    }
    out
}

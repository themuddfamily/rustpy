//! Source-preserving INI syntax for setup.cfg and EditorConfig consumers.
//!
//! This parses structure, not interpolation or application-specific values.
//! Inline `#`/`;` remain literal in values, matching ConfigParser's default.
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// setup.cfg: section required, `=`/`:` and indented continuations.
    ConfigParser,
    /// .editorconfig: global properties allowed, `=` only, no continuation.
    EditorConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub range: Range<usize>,
    pub name: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub range: Range<usize>,
    pub section: Option<usize>,
    pub key: Range<usize>,
    /// Each physical value part, excluding indentation and line endings.
    pub values: Vec<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    MalformedSection,
    TrailingSectionText,
    MissingSection,
    MissingDelimiter,
    EmptyKey,
    DuplicateSection,
    DuplicateKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub kind: ErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document<'a> {
    pub source: &'a str,
    pub sections: Vec<Section>,
    pub properties: Vec<Property>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Document<'_> {
    /// Raw logical value, with continuation parts joined by newline.
    /// No interpolation, unquoting, case folding or value conversion occurs.
    pub fn value(&self, property: &Property) -> String {
        property
            .values
            .iter()
            .map(|part| &self.source[part.clone()])
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn trimmed_range(text: &str, start: usize) -> Range<usize> {
    let left = text.len() - text.trim_start().len();
    start + left..start + left + text.trim().len()
}

/// Recoverable line parser. Every returned byte range indexes `source` at
/// UTF-8 boundaries; unknown or malformed lines produce diagnostics rather
/// than swallowing subsequent valid sections/properties. Duplicate records
/// remain present so consumers choose their own strictness/precedence policy.
pub fn parse(source: &str, dialect: Dialect) -> Document<'_> {
    let mut out = Document {
        source,
        sections: vec![],
        properties: vec![],
        diagnostics: vec![],
    };
    let mut section = None;
    let mut pending: Option<(usize, usize)> = None;
    let mut section_names = HashMap::new();
    let mut canonical_section = None;
    let mut keys = HashSet::new();
    let mut offset = 0;
    for physical in source.split_inclusive('\n') {
        let line = physical.trim_end_matches('\n').trim_end_matches('\r');
        let (line, start) = if offset == 0 {
            match line.strip_prefix('\u{feff}') {
                Some(rest) => (rest, 3),
                None => (line, 0),
            }
        } else {
            (line, offset)
        };
        let range = trimmed_range(line, start);
        let text = &source[range.clone()];
        let indentation = line.chars().take_while(|ch| ch.is_whitespace()).count();
        let end = start + line.len();
        offset += physical.len();
        if text.starts_with(['#', ';']) {
            continue;
        }
        if text.is_empty() {
            if dialect == Dialect::ConfigParser {
                if let Some((index, _)) = pending {
                    out.properties[index].values.push(end..end);
                    out.properties[index].range.end = end;
                }
            }
            continue;
        }
        if dialect == Dialect::ConfigParser {
            if let Some((index, previous_indent)) = pending {
                if indentation > previous_indent {
                    out.properties[index].values.push(range);
                    out.properties[index].range.end = end;
                    continue;
                }
            }
        }
        pending = None;
        if text.starts_with('[') {
            // Last bracket permits EditorConfig glob character classes.
            let Some(close) = text.rfind(']') else {
                out.diagnostics.push(Diagnostic {
                    range,
                    kind: ErrorKind::MalformedSection,
                });
                section = None;
                canonical_section = None;
                continue;
            };
            let name = range.start + 1..range.start + close;
            if name.is_empty() {
                out.diagnostics.push(Diagnostic {
                    range,
                    kind: ErrorKind::MalformedSection,
                });
                section = None;
                canonical_section = None;
                continue;
            }
            let tail = text[close + 1..].trim();
            // ConfigParser's section match permits arbitrary trailing text.
            // EditorConfig headers must end at the closing bracket.
            if !tail.is_empty() && dialect == Dialect::EditorConfig {
                out.diagnostics.push(Diagnostic {
                    range: range.start + close + 1..range.end,
                    kind: ErrorKind::TrailingSectionText,
                });
            }
            let name_text = &source[name.clone()];
            canonical_section = Some(if let Some(&first) = section_names.get(name_text) {
                if dialect == Dialect::ConfigParser && name_text != "DEFAULT" {
                    out.diagnostics.push(Diagnostic {
                        range: name.clone(),
                        kind: ErrorKind::DuplicateSection,
                    });
                }
                first
            } else {
                let index = out.sections.len();
                section_names.insert(name_text, index);
                index
            });
            section = Some(out.sections.len());
            out.sections.push(Section { range, name });
            continue;
        }
        let separator =
            text.find(|ch| ch == '=' || (dialect == Dialect::ConfigParser && ch == ':'));
        let Some(separator) = separator else {
            out.diagnostics.push(Diagnostic {
                range,
                kind: ErrorKind::MissingDelimiter,
            });
            continue;
        };
        let key = trimmed_range(&text[..separator], range.start);
        if key.is_empty() {
            out.diagnostics.push(Diagnostic {
                range,
                kind: ErrorKind::EmptyKey,
            });
            continue;
        }
        if section.is_none() && dialect == Dialect::ConfigParser {
            out.diagnostics.push(Diagnostic {
                range: key.clone(),
                kind: ErrorKind::MissingSection,
            });
        }
        if !keys.insert((canonical_section, source[key.clone()].to_lowercase()))
            && dialect == Dialect::ConfigParser
        {
            out.diagnostics.push(Diagnostic {
                range: key.clone(),
                kind: ErrorKind::DuplicateKey,
            });
        }
        let value = trimmed_range(&text[separator + 1..], range.start + separator + 1);
        let index = out.properties.len();
        out.properties.push(Property {
            range: start..end,
            section,
            key,
            values: vec![value],
        });
        pending = Some((index, indentation));
    }
    out
}

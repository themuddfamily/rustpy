//! Structured YAML syntax; aliases are graph edges, never recursively expanded.
use std::collections::HashMap;
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 128;
const MAX_DOCUMENTS: usize = 128;
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Scalar { value: String, style: TScalarStyle },
    Sequence(Vec<usize>),
    Mapping(Vec<(usize, usize)>),
    Alias(usize),
}
#[derive(Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: Kind,
    pub tag: Option<Tag>,
    pub line: usize,
    pub column: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Document {
    pub root: usize,
    pub nodes: Vec<Node>,
}
struct Frame {
    id: usize,
    mapping: bool,
    children: Vec<usize>,
}
/// All documents or an error; callers must not treat a partial inventory as valid.
pub fn parse(source: &str) -> Result<Vec<Document>, Diagnostic> {
    let fail = |line, column, message: &str| Diagnostic {
        line,
        column,
        message: message.into(),
    };
    if source.len() > MAX_SOURCE_BYTES {
        return Err(fail(1, 1, "YAML source exceeds 2 MiB"));
    }
    let mut parser = Parser::new_from_str(source);
    let mut documents = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors = HashMap::new();
    let mut root = None;
    let mut total_nodes = 0;
    loop {
        let (event, mark) = parser.next_token().map_err(|error| Diagnostic {
            line: error.marker().line(),
            column: error.marker().col() + 1,
            message: error.to_string(),
        })?;
        let line = mark.line();
        let column = mark.col() + 1;
        match event {
            Event::StreamStart => {}
            Event::StreamEnd => return Ok(documents),
            Event::DocumentStart => {
                anchors.clear();
                root = None;
            }
            Event::DocumentEnd => {
                if !stack.is_empty() {
                    return Err(fail(line, column, "Unclosed YAML collection"));
                }
                let root = root
                    .take()
                    .ok_or_else(|| fail(line, column, "Missing YAML document root"))?;
                if documents.len() >= MAX_DOCUMENTS {
                    return Err(fail(line, column, "YAML document limit exceeded"));
                }
                documents.push(Document {
                    root,
                    nodes: std::mem::take(&mut nodes),
                });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let frame = stack
                    .pop()
                    .ok_or_else(|| fail(line, column, "Unexpected YAML collection end"))?;
                if frame.mapping {
                    if !frame.children.len().is_multiple_of(2) {
                        return Err(fail(line, column, "Unpaired YAML mapping key"));
                    }
                    nodes[frame.id].kind = Kind::Mapping(
                        frame
                            .children
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| (pair[0], pair[1]))
                            .collect(),
                    );
                } else {
                    nodes[frame.id].kind = Kind::Sequence(frame.children);
                }
            }
            event => {
                let (kind, tag, anchor, container) = match event {
                    Event::Scalar(value, style, anchor, tag) => {
                        (Kind::Scalar { value, style }, tag, anchor, None)
                    }
                    Event::SequenceStart(anchor, tag) => {
                        (Kind::Sequence(Vec::new()), tag, anchor, Some(false))
                    }
                    Event::MappingStart(anchor, tag) => {
                        (Kind::Mapping(Vec::new()), tag, anchor, Some(true))
                    }
                    Event::Alias(anchor) => {
                        let target = *anchors.get(&anchor).ok_or_else(|| {
                            fail(line, column, "Alias has no anchor in this YAML document")
                        })?;
                        (Kind::Alias(target), None, 0, None)
                    }
                    _ => return Err(fail(line, column, "Unexpected YAML parser event")),
                };
                if total_nodes >= MAX_NODES {
                    return Err(fail(line, column, "YAML node limit exceeded"));
                }
                total_nodes += 1;
                let id = nodes.len();
                nodes.push(Node {
                    kind,
                    tag,
                    line,
                    column,
                });
                if anchor != 0 {
                    anchors.insert(anchor, id);
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(id);
                } else if root.replace(id).is_some() {
                    return Err(fail(line, column, "Multiple roots in YAML document"));
                }
                if let Some(mapping) = container {
                    if stack.len() >= MAX_DEPTH {
                        return Err(fail(line, column, "YAML nesting limit exceeded"));
                    }
                    stack.push(Frame {
                        id,
                        mapping,
                        children: Vec::new(),
                    });
                }
            }
        }
    }
}
/// Generic data validation consumes the same structured parser, not a lexer heuristic.
pub fn validate(source: &str) -> Vec<Diagnostic> {
    parse(source).err().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_unicode_multidoc_and_alias_identity() {
        let docs =
            parse("---\nbase: &base [π, 🙂]\ncopy: *base\n---\nvalue: |\n  α\n  β\n").unwrap();
        assert_eq!(docs.len(), 2);
        let Kind::Mapping(pairs) = &docs[0].nodes[docs[0].root].kind else {
            panic!("mapping root")
        };
        assert_eq!(pairs.len(), 2);
        assert_eq!(docs[0].nodes[pairs[1].1].kind, Kind::Alias(pairs[0].1));
        let Kind::Sequence(items) = &docs[0].nodes[pairs[0].1].kind else {
            panic!("sequence")
        };
        assert!(matches!(&docs[0].nodes[items[0]].kind,Kind::Scalar{value,..} if value=="π"));
        assert!(docs[1]
            .nodes
            .iter()
            .any(|node| matches!(&node.kind,Kind::Scalar{value,..} if value=="α\nβ\n")));
    }
    #[test]
    fn tags_complex_keys_duplicate_keys_and_cycles_are_preserved() {
        let docs =
            parse("tagged: !custom π\n? [a, b]\n: value\na: 1\na: 2\ncycle: &cycle [*cycle]\n")
                .unwrap();
        assert!(docs[0]
            .nodes
            .iter()
            .any(|n| n.tag.as_ref().is_some_and(|tag| tag.suffix == "custom")));
        let Kind::Mapping(pairs) = &docs[0].nodes[0].kind else {
            panic!("mapping")
        };
        assert_eq!(pairs.len(), 5);
        let sequence = pairs[4].1;
        let Kind::Sequence(items) = &docs[0].nodes[sequence].kind else {
            panic!("sequence")
        };
        assert_eq!(docs[0].nodes[items[0]].kind, Kind::Alias(sequence));
    }
    #[test]
    fn malformed_or_later_invalid_document_never_publishes_partial_success() {
        assert!(!validate("x: [unterminated\n").is_empty());
        assert!(parse("---\na: 1\n---\nb: [\n").is_err());
        assert!(parse("---\na: &first 1\n---\nb: *first\n").is_err());
    }
    #[test]
    fn bounded_input_depth_and_documents() {
        assert!(parse(&"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert!(parse(&format!("{}0{}", "[".repeat(129), "]".repeat(129))).is_err());
        assert!(parse(&"---\na\n".repeat(129)).is_err());
    }

    #[test]
    fn scalar_styles_and_unicode_crlf_marks_are_character_positions() {
        let source = "π: 'λ'\r\n雪: |-\r\n  α\r\n  β\r\n";
        let documents = parse(source).unwrap();
        let document = &documents[0];
        let Kind::Mapping(pairs) = &document.nodes[document.root].kind else {
            panic!("mapping root")
        };
        let quoted = &document.nodes[pairs[0].1];
        assert_eq!((quoted.line, quoted.column), (1, 4));
        assert_eq!(
            quoted.kind,
            Kind::Scalar {
                value: "λ".into(),
                style: TScalarStyle::SingleQuoted,
            }
        );
        let literal = &document.nodes[pairs[1].1];
        // Upstream block-scalar marks point at the first content character,
        // after scanning the header and its indentation.
        assert_eq!((literal.line, literal.column), (3, 3));
        assert_eq!(
            literal.kind,
            Kind::Scalar {
                value: "α\nβ".into(),
                style: TScalarStyle::Literal,
            }
        );
    }

    #[test]
    fn node_budget_accepts_boundary_and_is_shared_across_documents() {
        let at_limit = format!("[{}]", "v,".repeat(MAX_NODES - 1));
        assert_eq!(parse(&at_limit).unwrap()[0].nodes.len(), MAX_NODES);
        let half = "v,".repeat(MAX_NODES / 2);
        let two_documents = format!("---\n[{half}]\n---\n[{half}]\n");
        let error = parse(&two_documents).unwrap_err();
        assert_eq!(error.message, "YAML node limit exceeded");
        assert!(error.line >= 3, "the first document fits the budget");
    }
}

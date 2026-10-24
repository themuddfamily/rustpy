//! Non-Python outlines and XSD skeleton generation, without executing loaders.
use super::syntax_support::{StructureItem, StructureKind};
use rustpy_core::{TextRange, TextSize};
use std::path::Path;
fn row(source: &str, name: String, start: usize, end: usize, depth: usize) -> StructureItem {
    StructureItem {
        name,
        detail: String::new(),
        kind: StructureKind::Variable,
        range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
        name_range: TextRange::new(
            TextSize::new(start as u32),
            TextSize::new(end.min(start + 1) as u32),
        ),
        depth,
        line: source[..start.min(source.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count(),
    }
}
pub fn outline(source: &str, path: &Path) -> Vec<StructureItem> {
    if source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if matches!(extension, "json" | "json5" | "jsonc" | "jsonl" | "ndjson") {
        return rustpy_project::data_formats::parse_json(
            source,
            rustpy_project::data_formats::mode(path),
        )
        .map(|parsed| {
            parsed
                .nodes
                .into_iter()
                .filter(|node| node.depth > 0)
                .map(|node| {
                    row(
                        source,
                        if node.label.is_empty() {
                            "$".into()
                        } else {
                            node.label
                        },
                        node.range.start,
                        node.range.end,
                        node.depth.saturating_sub(1),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    }
    if matches!(extension, "xml" | "xsd" | "html" | "htm") {
        if let Ok(document) = roxmltree::Document::parse(source) {
            return document
                .descendants()
                .filter(|node| node.is_element())
                .take(20000)
                .map(|node| {
                    row(
                        source,
                        node.tag_name().name().into(),
                        node.range().start,
                        node.range().end,
                        node.ancestors()
                            .filter(|node| node.is_element())
                            .count()
                            .saturating_sub(1),
                    )
                })
                .collect();
        }
    }
    let mut out = Vec::new();
    let mut at = 0;
    let mut stack = Vec::new();
    let mut triple = None;
    for line in source.split_inclusive('\n').take(20000) {
        let trimmed = line.trim();
        if let Some(delimiter) = triple {
            if line.contains(delimiter) {
                triple = None;
            }
            at += line.len();
            continue;
        }
        if (line.matches("\"\"\"").count() == 1) || (line.matches("'''").count() == 1) {
            triple = Some(if line.contains("\"\"\"") {
                "\"\"\""
            } else {
                "'''"
            });
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            out.push(row(source, trimmed.into(), at, at + line.len(), 0));
        } else if !trimmed.starts_with(['#', ';']) {
            if let Some((key, _)) = trimmed.split_once(if matches!(extension, "yaml" | "yml") {
                ':'
            } else {
                '='
            }) {
                let indent = line.len() - line.trim_start().len();
                while stack.last().is_some_and(|old| *old >= indent) {
                    stack.pop();
                }
                let depth = if matches!(extension, "yaml" | "yml") {
                    stack.len()
                } else {
                    1
                };
                out.push(row(
                    source,
                    key.trim_start_matches('-').trim().into(),
                    at,
                    at + line.len(),
                    depth,
                ));
                if matches!(extension, "yaml" | "yml") {
                    stack.push(indent);
                }
            }
        }
        at += line.len();
    }
    out
}
fn escape(source: &str) -> String {
    source
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}
pub fn xml_from_schema(source: &str) -> Result<String, String> {
    if source.len() > 2 * 1024 * 1024 {
        return Err("XSD exceeds 2 MiB".into());
    }
    let document = roxmltree::Document::parse(source).map_err(|error| error.to_string())?;
    let schema = document.root_element();
    if schema.tag_name().name() != "schema" {
        return Err("Select an XML Schema document".into());
    }
    let root = schema
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "element")
        .ok_or("Schema has no global root element")?;
    fn element(
        node: roxmltree::Node<'_, '_>,
        schema: roxmltree::Node<'_, '_>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<String, String> {
        if depth > 16 || *budget >= 4096 {
            return Err("XSD generation exceeds the bounded recursion budget".into());
        }
        *budget += 1;
        let name = node
            .attribute("name")
            .or_else(|| node.attribute("ref"))
            .ok_or("XSD element needs name or ref")?;
        let mut out = format!("{}<{name}", "  ".repeat(depth));
        if depth == 0 {
            if let Some(namespace) = schema.attribute("targetNamespace") {
                out.push_str(&format!(" xmlns=\"{}\"", escape(namespace)));
            }
        }
        let kind = node.attribute("type").and_then(|name| {
            schema.children().find(|item| {
                item.attribute("name") == Some(name.rsplit(':').next().unwrap_or(name))
            })
        });
        let complex = node
            .children()
            .find(|item| item.tag_name().name() == "complexType")
            .or(kind);
        if let Some(complex) = complex {
            for attr in complex
                .descendants()
                .filter(|item| {
                    item.is_element()
                        && item.tag_name().name() == "attribute"
                        && item.attribute("use") == Some("required")
                })
                .take(64)
            {
                if let Some(name) = attr.attribute("name") {
                    out.push_str(&format!(
                        " {name}=\"{}\"",
                        escape(
                            attr.attribute("fixed")
                                .or_else(|| attr.attribute("default"))
                                .unwrap_or("")
                        )
                    ));
                }
            }
            out.push('>');
            let children = complex
                .descendants()
                .filter(|item| {
                    item.is_element()
                        && item.tag_name().name() == "element"
                        && item
                            .ancestors()
                            .skip(1)
                            .take_while(|item| *item != complex)
                            .all(|item| item.tag_name().name() != "element")
                })
                .collect::<Vec<_>>();
            if !children.is_empty() {
                out.push('\n');
                for child in children {
                    out.push_str(&element(child, schema, depth + 1, budget)?);
                }
                out.push_str(&"  ".repeat(depth));
            }
            out.push_str(&format!("</{name}>\n"));
        } else {
            out.push('>');
            let value = node
                .attribute("fixed")
                .or_else(|| node.attribute("default"))
                .unwrap_or_else(|| {
                    match node
                        .attribute("type")
                        .unwrap_or("")
                        .rsplit(':')
                        .next()
                        .unwrap_or("")
                    {
                        "integer" | "int" | "decimal" | "double" => "0",
                        "boolean" => "false",
                        _ => "",
                    }
                });
            out.push_str(&escape(value));
            out.push_str(&format!("</{name}>\n"));
        }
        Ok(out)
    }
    let output = element(root, schema, 0, &mut 0)?;
    roxmltree::Document::parse(&output)
        .map_err(|error| format!("Generated XML needs namespace/import configuration: {error}"))?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{output}"
    ))
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn xsd_generator_handles_required_attributes_and_scalar_children() {
        let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"><xs:complexType><xs:sequence><xs:element name="count" type="xs:int"/></xs:sequence><xs:attribute name="id" use="required"/></xs:complexType></xs:element></xs:schema>"#;
        let output = xml_from_schema(xsd).unwrap();
        assert!(output.contains("id=\"\""));
        assert!(output.contains("<count>0</count>"));
    }
    #[test]
    fn outlines_point_at_original_unicode_document() {
        let source = "{\"π\":{\"items\":[1,2]}}";
        let rows = outline(source, Path::new("data.json"));
        assert!(rows.iter().any(|row| row.name == "π"));
        assert!(rows
            .iter()
            .all(|row| source.is_char_boundary(usize::from(row.range.start()))));
    }
}

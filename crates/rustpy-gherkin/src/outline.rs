//! Scenario outline expansion (`<placeholder>` substitution per Examples row).

use serde::{Deserialize, Serialize};

use crate::model::{Scenario, Step};

/// One Examples row of a scenario outline, expanded into a concrete scenario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineExample {
    /// The name behave gives the generated scenario with its default
    /// `scenario_outline_annotation_schema` (`"{name} -- @{row.id} {examples.name}"`),
    /// e.g. `"Add 1 and 2 -- @1.1 Small numbers"` (note: a trailing space
    /// remains when the Examples section has no name, as in behave).
    pub name: String,
    /// 1-based index of the Examples section within the outline.
    pub examples_index: usize,
    /// 1-based index of the data row within its Examples table.
    pub row_index: usize,
    /// Examples name with placeholders substituted.
    pub examples_name: String,
    /// Line of the Examples data row (behave reports this as the scenario location).
    pub line: usize,
    /// Scenario tags followed by the Examples tags.
    pub tags: Vec<String>,
    /// `(column, value)` pairs of the row.
    pub values: Vec<(String, String)>,
    /// The outline's steps with placeholders substituted in their text, doc
    /// string and table cells; lines and ranges still point at the outline steps.
    pub steps: Vec<Step>,
}

/// Replaces `<name>` placeholders whose name is a column in `values`; unknown
/// placeholders are kept verbatim.
pub fn render_placeholders(text: &str, values: &[(String, String)]) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after
            .find(['>', '<'])
            .filter(|&c| after.as_bytes()[c] == b'>');
        match close {
            Some(c) => {
                let name = &after[..c];
                match values.iter().find(|(k, _)| k == name) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push('<');
                        out.push_str(name);
                        out.push('>');
                    }
                }
                rest = &after[c + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// behave's generated scenario name for an outline row (default annotation
/// schema). `values` are the row's `(column, value)` pairs; behave's special
/// placeholders `<examples.name>`, `<examples.index>`, `<row.index>` and
/// `<row.id>` are supported too.
pub fn behave_outline_name(
    outline_name: &str,
    examples_name: &str,
    examples_index: usize,
    row_index: usize,
    values: &[(String, String)],
) -> String {
    let row_id = format!("{examples_index}.{row_index}");
    let mut params: Vec<(String, String)> = values.to_vec();
    params.push(("examples.name".into(), examples_name.to_string()));
    params.push(("examples.index".into(), examples_index.to_string()));
    params.push(("row.index".into(), row_index.to_string()));
    params.push(("row.id".into(), row_id.clone()));
    let rendered_examples = render_placeholders(examples_name, &params);
    if let Some(p) = params.iter_mut().find(|(k, _)| k == "examples.name") {
        p.1 = rendered_examples.clone();
    }
    let name = render_placeholders(outline_name, &params);
    format!("{name} -- @{row_id} {rendered_examples}")
}

fn expand_step(step: &Step, values: &[(String, String)]) -> Step {
    let mut s = step.clone();
    s.text = render_placeholders(&step.text, values);
    if let Some(d) = &mut s.doc_string {
        d.content = render_placeholders(&d.content, values);
    }
    for row in &mut s.table {
        for c in &mut row.cells {
            *c = render_placeholders(c, values);
        }
    }
    s
}

/// Expands a scenario outline into one [`OutlineExample`] per Examples data
/// row, in document order. Plain scenarios (no outline keyword and no
/// Examples) and outlines without data rows yield an empty list.
pub fn outline_steps_expanded(scenario: &Scenario) -> Vec<OutlineExample> {
    let mut out = Vec::new();
    if !scenario.is_outline() {
        return out;
    }
    for (ei, ex) in scenario.examples.iter().enumerate() {
        let Some(header) = &ex.header else { continue };
        for (ri, row) in ex.rows.iter().enumerate() {
            let values: Vec<(String, String)> = header
                .cells
                .iter()
                .cloned()
                .zip(
                    row.cells
                        .iter()
                        .cloned()
                        .chain(std::iter::repeat(String::new())),
                )
                .collect();
            let (examples_index, row_index) = (ei + 1, ri + 1);
            let name =
                behave_outline_name(&scenario.name, &ex.name, examples_index, row_index, &values);
            let examples_name = render_placeholders(&ex.name, &values);
            let mut tags = scenario.tags.clone();
            tags.extend(ex.tags.iter().cloned());
            let steps = scenario
                .steps
                .iter()
                .map(|s| expand_step(s, &values))
                .collect();
            out.push(OutlineExample {
                name,
                examples_index,
                row_index,
                examples_name,
                line: row.line,
                tags,
                values,
                steps,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn renders_known_placeholders_only() {
        let v = vec![
            ("a".to_string(), "1".to_string()),
            ("first name".to_string(), "Ann".to_string()),
        ];
        assert_eq!(render_placeholders("add <a> to <a>", &v), "add 1 to 1");
        assert_eq!(
            render_placeholders("hi <first name> <b> <<a>> a<b", &v),
            "hi Ann <b> <1> a<b"
        );
        assert_eq!(
            render_placeholders("no placeholders", &v),
            "no placeholders"
        );
        assert_eq!(render_placeholders("<a", &v), "<a");
    }

    #[test]
    fn expands_rows_like_behave() {
        let f = parse("Feature: F\n  @o\n  Scenario Outline: Add <a> and <b>\n    When I add <a> and <b>\n      | x   |\n      | <a> |\n    Then the result is <sum>\n      \"\"\"\n      sum=<sum>\n      \"\"\"\n\n    Examples: Small numbers\n      | a | b | sum |\n      | 1 | 2 | 3   |\n      | 2 | 2 | 4   |\n\n    @big\n    Examples:\n      | a  | b  | sum |\n      | 10 | 20 | 30  |\n");
        let o = &f.scenarios[0];
        let ex = outline_steps_expanded(o);
        let names: Vec<&str> = ex.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Add 1 and 2 -- @1.1 Small numbers",
                "Add 2 and 2 -- @1.2 Small numbers",
                "Add 10 and 20 -- @2.1 "
            ]
        );
        assert_eq!(ex[0].line, 14);
        assert_eq!(ex[2].line, 20);
        assert_eq!((ex[2].examples_index, ex[2].row_index), (2, 1));
        assert_eq!(ex[2].tags, vec!["@o", "@big"]);
        assert_eq!(
            ex[1].values,
            vec![
                ("a".to_string(), "2".to_string()),
                ("b".to_string(), "2".to_string()),
                ("sum".to_string(), "4".to_string())
            ]
        );
        let steps: Vec<&str> = ex[0].steps.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(steps, vec!["I add 1 and 2", "the result is 3"]);
        assert_eq!(ex[0].steps[0].table[1].cells, vec!["1"]);
        assert_eq!(ex[0].steps[1].doc_string.as_ref().unwrap().content, "sum=3");
        assert_eq!(ex[0].steps[0].line, 4);
        // The outline itself keeps its placeholders.
        assert_eq!(o.steps[0].text, "I add <a> and <b>");
    }

    #[test]
    fn special_placeholders_in_names() {
        assert_eq!(
            behave_outline_name(
                "Row <row.id> of <examples.name>",
                "Set <x>",
                1,
                3,
                &[("x".into(), "A".into())]
            ),
            "Row 1.3 of Set A -- @1.3 Set A"
        );
    }

    #[test]
    fn plain_scenarios_do_not_expand() {
        let f = parse("Feature: F\n  Scenario: s\n    Given x\n  Scenario Outline: no rows\n    Given <x>\n    Examples:\n      | x |\n");
        assert!(outline_steps_expanded(&f.scenarios[0]).is_empty());
        assert!(outline_steps_expanded(&f.scenarios[1]).is_empty());
    }
}

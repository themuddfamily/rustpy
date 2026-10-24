//! Parses a `.feature` file into a [`Feature`] outline. Never fails: problems
//! become [`Diagnostic`]s and parsing continues.

use std::ops::Range;

use crate::lexer::{scan, LineTok, Tok};
use crate::model::{
    Background, BlockKind, Diagnostic, DocString, Examples, Feature, Rule, Scenario, Step,
    StepKeyword, StepKind, TableRow,
};

/// Where the current block's steps / description go.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Cur {
    None,
    Feature,
    Rule(usize),
    FeatureBackground,
    RuleBackground(usize),
    Scenario(usize),
    Examples(usize, usize),
}

/// The step that a following doc string / table belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum StepRef {
    FeatureBackground(usize),
    RuleBackground(usize, usize),
    Scenario(usize, usize),
}

struct OpenDoc {
    step: Option<StepRef>,
    fence: &'static str,
    media: String,
    line: usize,
    start: usize,
    /// Indentation (in chars) of the opening delimiter.
    indent: usize,
    lines: Vec<String>,
}

struct P<'a> {
    src: &'a str,
    f: Feature,
    seen_feature: bool,
    pending_tags: Vec<String>,
    cur: Cur,
    rule: Option<usize>,
    desc: Vec<String>,
    last_kind: Option<StepKind>,
    /// Effective kind of the last step of the most recent background.
    background_kind: Option<StepKind>,
    /// Step that may still receive a table / doc string argument.
    arg_step: Option<StepRef>,
    doc: Option<OpenDoc>,
}

/// Parses `src` into a feature outline.
pub fn parse(src: &str) -> Feature {
    let scanned = scan(src);
    let mut p = P {
        src,
        f: Feature::default(),
        seen_feature: false,
        pending_tags: Vec::new(),
        cur: Cur::None,
        rule: None,
        desc: Vec::new(),
        last_kind: None,
        background_kind: None,
        arg_step: None,
        doc: None,
    };
    p.f.language = scanned.dialect.code.to_string();
    if let Some((code, false)) = &scanned.language {
        if let Some(l) = scanned
            .lines
            .iter()
            .find(|l| matches!(l.tok, Tok::Language { .. }))
        {
            p.diag(
                l,
                l.indent..l.trim_end,
                format!("Unknown language '{code}'; using English keywords"),
            );
        }
    }
    for l in &scanned.lines {
        p.line(l);
    }
    p.finish()
}

fn unescape_cell(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('|') => {
                    out.push('|');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some('n') => {
                    out.push('\n');
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Removes up to `indent` leading whitespace characters (the doc string
/// delimiter's indentation) from `line`.
fn dedent(line: &str, indent: usize) -> &str {
    for (removed, (i, c)) in line.char_indices().enumerate() {
        if removed >= indent || !c.is_whitespace() {
            return &line[i..];
        }
    }
    ""
}

fn join_description(lines: &mut Vec<String>) -> String {
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let s = lines.join("\n");
    lines.clear();
    s
}

impl P<'_> {
    fn diag(&mut self, l: &LineTok, range: Range<usize>, message: impl Into<String>) {
        self.f.diagnostics.push(Diagnostic {
            line: l.line + 1,
            range,
            message: message.into(),
        });
    }

    fn text(&self, r: &Range<usize>) -> String {
        self.src[r.clone()].to_string()
    }

    fn take_tags(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_tags)
    }

    /// Stores the collected description in the current block.
    fn flush_description(&mut self) {
        if self.desc.is_empty() {
            return;
        }
        let d = join_description(&mut self.desc);
        match self.cur {
            Cur::None => {}
            Cur::Feature => self.f.description = d,
            Cur::Rule(r) => self.f.rules[r].description = d,
            Cur::FeatureBackground => {
                if let Some(b) = &mut self.f.background {
                    b.description = d;
                }
            }
            Cur::RuleBackground(r) => {
                if let Some(b) = &mut self.f.rules[r].background {
                    b.description = d;
                }
            }
            Cur::Scenario(s) => self.f.scenarios[s].description = d,
            Cur::Examples(s, e) => self.f.scenarios[s].examples[e].description = d,
        }
    }

    fn current_scenario(&self) -> Option<usize> {
        match self.cur {
            Cur::Scenario(s) | Cur::Examples(s, _) => Some(s),
            _ => None,
        }
    }

    fn extend_scenario(&mut self, line: usize) {
        if let Some(s) = self.current_scenario() {
            let sc = &mut self.f.scenarios[s];
            sc.end_line = sc.end_line.max(line);
        }
    }

    fn step_mut(&mut self, r: StepRef) -> Option<&mut Step> {
        match r {
            StepRef::FeatureBackground(i) => self.f.background.as_mut()?.steps.get_mut(i),
            StepRef::RuleBackground(ru, i) => self
                .f
                .rules
                .get_mut(ru)?
                .background
                .as_mut()?
                .steps
                .get_mut(i),
            StepRef::Scenario(s, i) => self.f.scenarios.get_mut(s)?.steps.get_mut(i),
        }
    }

    fn line(&mut self, l: &LineTok) {
        if let Some(doc) = &mut self.doc {
            match &l.tok {
                Tok::DocClose { delimiter } => {
                    let end = delimiter.end;
                    self.close_doc(end);
                    self.extend_scenario(l.line + 1);
                }
                _ => {
                    doc.lines.push(self.src[l.start..l.end].to_string());
                    self.extend_scenario(l.line + 1);
                }
            }
            return;
        }
        match &l.tok {
            Tok::Empty => {
                if !self.desc.is_empty() {
                    self.desc.push(String::new());
                }
            }
            Tok::Comment | Tok::Language { .. } => {}
            Tok::Tags { tags, .. } => {
                self.flush_description();
                self.arg_step = None;
                let texts: Vec<String> = tags.iter().map(|t| self.text(t)).collect();
                self.pending_tags.extend(texts);
            }
            Tok::Description => {
                let t = self.src[l.indent..l.trim_end].to_string();
                self.desc.push(t);
            }
            Tok::Other => {
                self.flush_description();
                self.arg_step = None;
                let msg = if !self.seen_feature {
                    "Expected 'Feature:' before this line"
                } else {
                    "Unexpected text (expected a step, a table, a doc string or a new section)"
                };
                self.diag(l, l.indent..l.trim_end, msg);
            }
            Tok::Block {
                kind,
                keyword,
                name,
            } => {
                self.flush_description();
                self.arg_step = None;
                self.block(l, *kind, keyword.clone(), name.clone());
            }
            Tok::Step {
                keyword_type,
                keyword,
                text,
            } => {
                self.flush_description();
                self.step(l, *keyword_type, keyword.clone(), text.clone());
            }
            Tok::Row {
                cells, examples, ..
            } => {
                self.flush_description();
                self.row(l, cells, *examples);
            }
            Tok::DocOpen { fence, media, .. } => {
                self.flush_description();
                let step = self.arg_step.take();
                if step.is_none() {
                    self.diag(l, l.indent..l.trim_end, "A doc string must follow a step");
                }
                let indent = self.src[l.start..l.indent].chars().count();
                self.doc = Some(OpenDoc {
                    step,
                    fence,
                    media: self.text(media),
                    line: l.line + 1,
                    start: l.indent,
                    indent,
                    lines: Vec::new(),
                });
                self.extend_scenario(l.line + 1);
            }
            Tok::DocLine | Tok::DocClose { .. } => {}
        }
    }

    fn close_doc(&mut self, end: usize) {
        let Some(doc) = self.doc.take() else { return };
        let fence = doc.fence;
        let escaped: String = fence.chars().flat_map(|c| ['\\', c]).collect();
        let content: Vec<String> = doc
            .lines
            .iter()
            .map(|line| dedent(line, doc.indent).replace(&escaped, fence))
            .collect();
        let ds = DocString {
            delimiter: fence.to_string(),
            media_type: doc.media,
            content: content.join("\n"),
            line: doc.line,
            range: doc.start..end,
        };
        if let Some(r) = doc.step {
            if let Some(step) = self.step_mut(r) {
                step.doc_string = Some(ds);
            }
        }
    }

    fn block(&mut self, l: &LineTok, kind: BlockKind, keyword: Range<usize>, name: Range<usize>) {
        let kw =
            self.src[keyword.start..keyword.end.saturating_sub(1).max(keyword.start)].to_string();
        let name_text = self.text(&name);
        let line = l.line + 1;
        match kind {
            BlockKind::Feature => {
                if self.seen_feature {
                    self.diag(l, keyword, "Only one 'Feature:' is allowed per file");
                    return;
                }
                self.seen_feature = true;
                self.f.keyword = kw;
                self.f.name = name_text;
                self.f.line = line;
                self.f.tags = self.take_tags();
                self.cur = Cur::Feature;
                self.rule = None;
            }
            BlockKind::Rule => {
                if !self.seen_feature {
                    self.diag(l, keyword.clone(), "'Rule:' must be inside a 'Feature:'");
                }
                let tags = self.take_tags();
                self.f.rules.push(Rule {
                    keyword: kw,
                    name: name_text,
                    description: String::new(),
                    line,
                    tags,
                    background: None,
                });
                let r = self.f.rules.len() - 1;
                self.rule = Some(r);
                self.cur = Cur::Rule(r);
                self.background_kind = None;
            }
            BlockKind::Background => {
                if !self.pending_tags.is_empty() {
                    self.pending_tags.clear();
                    self.diag(l, keyword.clone(), "Tags are not allowed on 'Background:'");
                }
                if !self.seen_feature {
                    self.diag(
                        l,
                        keyword.clone(),
                        "'Background:' must be inside a 'Feature:'",
                    );
                }
                let bg = Background {
                    keyword: kw,
                    name: name_text,
                    description: String::new(),
                    line,
                    steps: Vec::new(),
                };
                let in_rule = self.rule;
                let scenarios_before = self.f.scenarios.iter().any(|s| s.rule == in_rule);
                if scenarios_before {
                    self.diag(
                        l,
                        keyword.clone(),
                        "'Background:' must come before the first scenario",
                    );
                }
                match in_rule {
                    Some(r) => {
                        if self.f.rules[r].background.is_some() {
                            self.diag(
                                l,
                                keyword.clone(),
                                "Only one 'Background:' is allowed per rule",
                            );
                        }
                        self.f.rules[r].background = Some(bg);
                        self.cur = Cur::RuleBackground(r);
                    }
                    None => {
                        if self.f.background.is_some() {
                            self.diag(
                                l,
                                keyword.clone(),
                                "Only one 'Background:' is allowed per feature",
                            );
                        }
                        self.f.background = Some(bg);
                        self.cur = Cur::FeatureBackground;
                    }
                }
                self.last_kind = None;
                self.background_kind = None;
            }
            BlockKind::Scenario | BlockKind::ScenarioOutline => {
                if !self.seen_feature {
                    self.diag(l, keyword.clone(), "Scenarios must be inside a 'Feature:'");
                }
                let tags = self.take_tags();
                let range = l.indent..l.trim_end;
                self.f.scenarios.push(Scenario {
                    keyword: kw,
                    name: name_text,
                    description: String::new(),
                    line,
                    tags,
                    steps: Vec::new(),
                    examples: Vec::new(),
                    outline_keyword: kind == BlockKind::ScenarioOutline,
                    rule: self.rule,
                    range,
                    end_line: line,
                });
                self.cur = Cur::Scenario(self.f.scenarios.len() - 1);
                self.last_kind = None;
            }
            BlockKind::Examples => {
                let tags = self.take_tags();
                match self.current_scenario() {
                    Some(s) => {
                        let ex = Examples {
                            keyword: kw,
                            name: name_text,
                            description: String::new(),
                            line,
                            tags,
                            header: None,
                            rows: Vec::new(),
                        };
                        self.f.scenarios[s].examples.push(ex);
                        let e = self.f.scenarios[s].examples.len() - 1;
                        self.cur = Cur::Examples(s, e);
                        self.extend_scenario(line);
                    }
                    None => self.diag(l, keyword, "'Examples:' must belong to a scenario outline"),
                }
            }
        }
    }

    fn step(
        &mut self,
        l: &LineTok,
        keyword_type: StepKeyword,
        keyword: Range<usize>,
        text: Range<usize>,
    ) {
        let kind = match keyword_type {
            StepKeyword::Given => StepKind::Given,
            StepKeyword::When => StepKind::When,
            StepKeyword::Then => StepKind::Then,
            StepKeyword::And | StepKeyword::But | StepKeyword::Star => {
                let inherited = match self.cur {
                    Cur::Scenario(_) => self.last_kind.or(self.background_kind),
                    _ => self.last_kind,
                };
                inherited.unwrap_or(StepKind::Given)
            }
        };
        self.last_kind = Some(kind);
        let step = Step {
            keyword: self.text(&keyword),
            keyword_type,
            kind,
            text: self.text(&text),
            line: l.line + 1,
            range: keyword.start..l.trim_end,
            text_range: text,
            doc_string: None,
            table: Vec::new(),
        };
        let r = match self.cur {
            Cur::FeatureBackground => self.f.background.as_mut().map(|b| {
                b.steps.push(step);
                StepRef::FeatureBackground(b.steps.len() - 1)
            }),
            Cur::RuleBackground(ru) => self.f.rules[ru].background.as_mut().map(|b| {
                b.steps.push(step);
                StepRef::RuleBackground(ru, b.steps.len() - 1)
            }),
            Cur::Scenario(s) => {
                let sc = &mut self.f.scenarios[s];
                sc.steps.push(step);
                sc.end_line = sc.end_line.max(l.line + 1);
                Some(StepRef::Scenario(s, sc.steps.len() - 1))
            }
            _ => None,
        };
        if matches!(self.cur, Cur::FeatureBackground | Cur::RuleBackground(_)) {
            self.background_kind = Some(kind);
        }
        self.arg_step = r;
    }

    fn row(&mut self, l: &LineTok, cells: &[Range<usize>], examples: bool) {
        let row = TableRow {
            line: l.line + 1,
            cells: cells
                .iter()
                .map(|c| unescape_cell(&self.src[c.clone()]))
                .collect(),
            range: l.indent..l.trim_end,
            cell_ranges: cells.to_vec(),
        };
        if examples {
            if let Cur::Examples(s, e) = self.cur {
                let width = {
                    let ex = &mut self.f.scenarios[s].examples[e];
                    match &ex.header {
                        None => {
                            ex.header = Some(row);
                            None
                        }
                        Some(h) => {
                            let w = h.cells.len();
                            let n = row.cells.len();
                            ex.rows.push(row);
                            (w != n).then_some((w, n))
                        }
                    }
                };
                if let Some((w, n)) = width {
                    self.diag(
                        l,
                        l.indent..l.trim_end,
                        format!("Inconsistent cell count: expected {w}, found {n}"),
                    );
                }
                self.extend_scenario(l.line + 1);
                return;
            }
        }
        match self.arg_step {
            Some(r) => {
                let mismatch = {
                    let Some(step) = self.step_mut(r) else { return };
                    let w = step.table.first().map(|h| h.cells.len());
                    let n = row.cells.len();
                    step.table.push(row);
                    w.filter(|w| *w != n).map(|w| (w, n))
                };
                if let Some((w, n)) = mismatch {
                    self.diag(
                        l,
                        l.indent..l.trim_end,
                        format!("Inconsistent cell count: expected {w}, found {n}"),
                    );
                }
                self.extend_scenario(l.line + 1);
            }
            None => self.diag(
                l,
                l.indent..l.trim_end,
                "A table must follow a step or an 'Examples:' line",
            ),
        }
    }

    fn finish(mut self) -> Feature {
        self.flush_description();
        if let Some(doc) = &self.doc {
            let (line, start) = (doc.line, doc.start);
            self.f.diagnostics.push(Diagnostic {
                line,
                range: start..self.src.len(),
                message: "Unterminated doc string".into(),
            });
            let end = self.src.len();
            self.close_doc(end);
        }
        if !self.pending_tags.is_empty() && self.seen_feature {
            let line = self.src.lines().count().max(1);
            self.f.diagnostics.push(Diagnostic {
                line,
                range: self.src.len()..self.src.len(),
                message: "Tags must be followed by a section".into(),
            });
        }
        self.f.diagnostics.sort_by_key(|d| d.line);
        self.f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE: &str = r#"@calc @fast
Feature: Calculator
  As a user
  I want sums

  Background:
    Given a fresh calculator

  @smoke
  Scenario: Add two numbers
    When I add 2 and 3
    And I press "=" twice
    Then the result is 5
      | value | label |
      | 5     | a\|b  |

  Scenario Outline: Add <a> and <b>
    Given the inputs:
      """json
      {"a": <a>}
        indented
      \"\"\"
      """
    When I add <a> and <b>
    Then the result is <sum>

    @small
    Examples: Small
      | a | b | sum |
      | 1 | 2 | 3   |
      | 2 | 2 | 4   |

  Rule: Negative numbers
    Background:
      Given negative mode

    Example: Subtract
      But nothing else
      * a bullet
"#;

    #[test]
    fn parses_the_outline() {
        let f = parse(SAMPLE);
        assert_eq!(f.keyword, "Feature");
        assert_eq!(f.name, "Calculator");
        assert_eq!(f.line, 2);
        assert_eq!(f.tags, vec!["@calc", "@fast"]);
        assert_eq!(f.description, "As a user\nI want sums");
        assert_eq!(f.language, "en");
        let bg = f.background.as_ref().unwrap();
        assert_eq!(bg.line, 6);
        assert_eq!(bg.steps[0].text, "a fresh calculator");
        assert_eq!(f.scenarios.len(), 3);

        let s = &f.scenarios[0];
        assert_eq!(
            (s.keyword.as_str(), s.name.as_str(), s.line, s.end_line),
            ("Scenario", "Add two numbers", 10, 15)
        );
        assert_eq!(s.tags, vec!["@smoke"]);
        assert!(!s.is_outline());
        let steps: Vec<(&str, StepKind, &str, usize)> = s
            .steps
            .iter()
            .map(|st| (st.keyword.as_str(), st.kind, st.text.as_str(), st.line))
            .collect();
        assert_eq!(
            steps,
            vec![
                ("When", StepKind::When, "I add 2 and 3", 11),
                ("And", StepKind::When, "I press \"=\" twice", 12),
                ("Then", StepKind::Then, "the result is 5", 13)
            ]
        );
        let then = &s.steps[2];
        assert_eq!(&SAMPLE[then.range.clone()], "Then the result is 5");
        assert_eq!(&SAMPLE[then.text_range.clone()], "the result is 5");
        assert_eq!(then.table.len(), 2);
        assert_eq!(then.table[1].cells, vec!["5", "a|b"]);
        assert_eq!(&SAMPLE[then.table[1].cell_ranges[1].clone()], "a\\|b");

        let o = &f.scenarios[1];
        assert!(o.is_outline() && o.outline_keyword);
        assert_eq!(o.name, "Add <a> and <b>");
        let doc = o.steps[0].doc_string.as_ref().unwrap();
        assert_eq!(doc.media_type, "json");
        assert_eq!(doc.content, "{\"a\": <a>}\n  indented\n\"\"\"");
        assert_eq!(doc.line, 19);
        assert_eq!(o.examples.len(), 1);
        let ex = &o.examples[0];
        assert_eq!((ex.name.as_str(), ex.line), ("Small", 28));
        assert_eq!(ex.tags, vec!["@small"]);
        assert_eq!(ex.header.as_ref().unwrap().cells, vec!["a", "b", "sum"]);
        assert_eq!(
            ex.rows.iter().map(|r| r.cells.clone()).collect::<Vec<_>>(),
            vec![vec!["1", "2", "3"], vec!["2", "2", "4"]]
        );
        assert_eq!(ex.rows[0].line, 30);
        assert_eq!(o.end_line, 31);

        assert_eq!(f.rules.len(), 1);
        let rule = &f.rules[0];
        assert_eq!((rule.name.as_str(), rule.line), ("Negative numbers", 33));
        assert_eq!(
            rule.background.as_ref().unwrap().steps[0].text,
            "negative mode"
        );
        let sub = &f.scenarios[2];
        assert_eq!((sub.keyword.as_str(), sub.rule), ("Example", Some(0)));
        // A leading `But` inherits the rule background's step type; `*` the previous one.
        assert_eq!(
            sub.steps
                .iter()
                .map(|s| (s.keyword_type, s.kind))
                .collect::<Vec<_>>(),
            vec![
                (StepKeyword::But, StepKind::Given),
                (StepKeyword::Star, StepKind::Given)
            ]
        );
        assert_eq!(
            f.effective_steps(sub)
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "a fresh calculator",
                "negative mode",
                "nothing else",
                "a bullet"
            ]
        );
        assert_eq!(f.all_steps().len(), 10);
        assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
        assert_eq!(f.scenario_at_line(12).map(|s| s.line), Some(10));
        assert_eq!(f.scenario_at_line(31).map(|s| s.line), Some(17));
        assert!(f.scenario_at_line(3).is_none());
    }

    #[test]
    fn localized_feature() {
        let f = parse("# language: de\n@wip\nFunktionalität: Rechner\n  Szenario: Addieren\n    Angenommen ich habe 5 Äpfel\n    Wenn ich 2 esse\n    Dann habe ich 3\n");
        assert_eq!(f.language, "de");
        assert_eq!(
            (f.keyword.as_str(), f.name.as_str()),
            ("Funktionalität", "Rechner")
        );
        let kinds: Vec<StepKind> = f.scenarios[0].steps.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![StepKind::Given, StepKind::When, StepKind::Then]);
        assert_eq!(f.scenarios[0].steps[0].text, "ich habe 5 Äpfel");
        let fr = parse("# language: fr\nFonctionnalité: F\n  Plan du scénario: P\n    Soit <x>\n    Exemples:\n      | x |\n      | 1 |\n");
        assert!(fr.scenarios[0].is_outline());
        assert_eq!(fr.scenarios[0].examples[0].rows.len(), 1);
    }

    #[test]
    fn diagnostics_and_recovery() {
        let f = parse("stray\nFeature: F\n  Scenario: s\n    Given x\n    oops\n      | a |\n  Examples:\n  Feature: again\n    Given y\n      \"\"\"\n      open");
        let msgs: Vec<(usize, &str)> = f
            .diagnostics
            .iter()
            .map(|d| (d.line, d.message.as_str()))
            .collect();
        assert_eq!(msgs[0], (1, "Expected 'Feature:' before this line"));
        assert!(
            msgs.iter()
                .any(|(l, m)| *l == 5 && m.starts_with("Unexpected text")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|(l, m)| *l == 6 && m.starts_with("A table must follow")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|(l, m)| *l == 8 && m.starts_with("Only one 'Feature:'")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter().any(|(_, m)| *m == "Unterminated doc string"),
            "{msgs:?}"
        );
        assert_eq!(f.name, "F");
        assert_eq!(f.scenarios.len(), 1);
        assert_eq!(f.scenarios[0].steps.len(), 1);
    }

    #[test]
    fn empty_and_unknown_language() {
        let f = parse("");
        assert_eq!(f.line, 0);
        assert!(f.scenarios.is_empty() && f.diagnostics.is_empty());
        assert_eq!(f.language, "en");
        let u = parse("# language: xx\nFeature: F\n");
        assert_eq!(u.language, "en");
        assert!(u.diagnostics[0].message.contains("Unknown language 'xx'"));
    }

    #[test]
    fn scenario_with_examples_and_cell_counts() {
        let f = parse("Feature: F\n  Scenario: s <x>\n    Given <x>\n    Examples:\n      | x | y |\n      | 1 |\n");
        assert!(f.scenarios[0].is_outline());
        assert!(!f.scenarios[0].outline_keyword);
        assert_eq!(f.diagnostics.len(), 1);
        assert!(f.diagnostics[0].message.contains("expected 2, found 1"));
    }
}

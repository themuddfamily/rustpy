//! The outline of a `.feature` file produced by [`crate::parse`].
//!
//! Lines are 1-based; byte ranges index into the parsed source text.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// The step keyword as written in the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepKeyword {
    Given,
    When,
    Then,
    And,
    But,
    /// `*` (a bullet step that inherits the previous step type).
    Star,
}

/// Step type used for matching: the effective type of a step (`And` / `But`
/// / `*` inherit the previous step type, `Given` at the start of a block),
/// or the kind of a step definition (`Step` = matches every step type, like
/// behave's `@step` / pytest-bdd's `@step`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepKind {
    Given,
    When,
    Then,
    Step,
}

impl StepKind {
    /// Lower-case decorator name: `given`, `when`, `then`, `step`.
    pub fn decorator(self) -> &'static str {
        match self {
            StepKind::Given => "given",
            StepKind::When => "when",
            StepKind::Then => "then",
            StepKind::Step => "step",
        }
    }

    /// English keyword (`Given` / `When` / `Then` / `Step`).
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Given => "Given",
            StepKind::When => "When",
            StepKind::Then => "Then",
            StepKind::Step => "Step",
        }
    }

    /// True when a definition of this kind applies to a step of type `step`
    /// (`Step` definitions apply to everything; a `Step` *step* only matches
    /// `Step` definitions).
    pub fn applies_to(self, step: StepKind) -> bool {
        self == StepKind::Step || self == step
    }
}

/// Block keyword families.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockKind {
    Feature,
    Rule,
    Background,
    Scenario,
    ScenarioOutline,
    Examples,
}

/// A parse problem (the parser never fails; it reports and recovers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub line: usize,
    pub range: Range<usize>,
    pub message: String,
}

/// One row of a data table or an Examples table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRow {
    pub line: usize,
    /// Cell values: trimmed, with `\|`, `\\` and `\n` escapes resolved.
    pub cells: Vec<String>,
    /// Byte range of the row (from the first `|` to the end of the last cell).
    pub range: Range<usize>,
    /// Byte ranges of the cell contents (trimmed), parallel to `cells`.
    pub cell_ranges: Vec<Range<usize>>,
}

/// A doc string (`"""` or ```` ``` ````) step argument.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocString {
    /// `"""` or ```` ``` ````.
    pub delimiter: String,
    /// Content type after the opening delimiter (`"""json`), or empty.
    pub media_type: String,
    /// Content with the delimiter's indentation removed and escaped delimiters resolved.
    pub content: String,
    /// Line of the opening delimiter.
    pub line: usize,
    /// From the opening delimiter to the end of the closing one (or of the file
    /// when the doc string is not terminated).
    pub range: Range<usize>,
}

/// A step line (`Given I have 5 apples`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Keyword as written, without the trailing space (`Given`, `Und`, `*`, `Lorsqu'`).
    pub keyword: String,
    pub keyword_type: StepKeyword,
    /// Effective step type (never [`StepKind::Step`] for parsed steps).
    pub kind: StepKind,
    /// Step text after the keyword (trimmed).
    pub text: String,
    pub line: usize,
    /// Byte range of the whole step line (keyword start .. text end).
    pub range: Range<usize>,
    /// Byte range of [`Step::text`].
    pub text_range: Range<usize>,
    pub doc_string: Option<DocString>,
    /// Data table rows (behave treats the first row as the heading).
    pub table: Vec<TableRow>,
}

impl Step {
    /// `Given I have 5 apples` (keyword as written).
    pub fn display(&self) -> String {
        if self.keyword.ends_with('\'') {
            format!("{}{}", self.keyword, self.text)
        } else {
            format!("{} {}", self.keyword, self.text)
        }
    }
}

/// `Background:` of a feature or rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background {
    pub keyword: String,
    pub name: String,
    pub description: String,
    pub line: usize,
    pub steps: Vec<Step>,
}

/// An `Examples:` section of a scenario outline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Examples {
    pub keyword: String,
    pub name: String,
    pub description: String,
    pub line: usize,
    pub tags: Vec<String>,
    /// The first table row (column names).
    pub header: Option<TableRow>,
    /// Data rows (one scenario each).
    pub rows: Vec<TableRow>,
}

/// A `Scenario` / `Example` / `Scenario Outline` / `Scenario Template`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// Keyword as written (`Scenario`, `Scenario Outline`, `Szenario`, ...).
    pub keyword: String,
    pub name: String,
    pub description: String,
    pub line: usize,
    /// Tags as written (`@slow`), not including the feature's / rule's tags.
    pub tags: Vec<String>,
    pub steps: Vec<Step>,
    pub examples: Vec<Examples>,
    /// The keyword is an outline keyword (`Scenario Outline` / `Scenario Template`).
    pub outline_keyword: bool,
    /// Index into [`Feature::rules`] when the scenario is inside a `Rule:`.
    pub rule: Option<usize>,
    /// Byte range of the header line (`Scenario: name`).
    pub range: Range<usize>,
    /// Last line (1-based) that belongs to this scenario (steps, examples, tables).
    pub end_line: usize,
}

impl Scenario {
    /// True for outlines: an outline keyword, or a `Scenario` with `Examples`
    /// (Gherkin 6+).
    pub fn is_outline(&self) -> bool {
        self.outline_keyword || !self.examples.is_empty()
    }
}

/// `Rule:` (Gherkin 6+). Its scenarios are in [`Feature::scenarios`] with
/// [`Scenario::rule`] pointing here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub keyword: String,
    pub name: String,
    pub description: String,
    pub line: usize,
    pub tags: Vec<String>,
    pub background: Option<Background>,
}

/// The parsed feature file. A file without a `Feature:` line yields an empty
/// feature with `line == 0` (and diagnostics for any stray content).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feature {
    /// Keyword as written (`Feature`, `Funktionalität`, ...).
    pub keyword: String,
    pub name: String,
    pub description: String,
    pub line: usize,
    /// Tags as written (`@wip`).
    pub tags: Vec<String>,
    /// Language code from the `# language:` header (`en` by default).
    pub language: String,
    pub background: Option<Background>,
    /// Every scenario in document order, including those inside rules.
    pub scenarios: Vec<Scenario>,
    pub rules: Vec<Rule>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Feature {
    /// Backgrounds that run before `scenario`: the feature's, then its rule's.
    pub fn backgrounds_for(&self, scenario: &Scenario) -> Vec<&Background> {
        let mut out: Vec<&Background> = self.background.iter().collect();
        if let Some(rule) = scenario.rule.and_then(|r| self.rules.get(r)) {
            out.extend(rule.background.iter());
        }
        out
    }

    /// Steps of a scenario as they run: backgrounds first, then its own steps.
    pub fn effective_steps<'a>(&'a self, scenario: &'a Scenario) -> Vec<&'a Step> {
        let mut out: Vec<&Step> = self
            .backgrounds_for(scenario)
            .into_iter()
            .flat_map(|b| b.steps.iter())
            .collect();
        out.extend(scenario.steps.iter());
        out
    }

    /// Every step in the file in document order (backgrounds, scenarios,
    /// rule backgrounds), each exactly once.
    pub fn all_steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = Vec::new();
        if let Some(b) = &self.background {
            out.extend(b.steps.iter());
        }
        for r in &self.rules {
            if let Some(b) = &r.background {
                out.extend(b.steps.iter());
            }
        }
        for s in &self.scenarios {
            out.extend(s.steps.iter());
        }
        out.sort_by_key(|s| s.line);
        out
    }

    /// The scenario whose block contains `line` (1-based), if any.
    pub fn scenario_at_line(&self, line: usize) -> Option<&Scenario> {
        self.scenarios
            .iter()
            .find(|s| line >= s.line && line <= s.end_line.max(s.line))
    }
}

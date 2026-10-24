//! RustPy IDE — Gherkin / BDD support (roadmap §3.4 "Behave/BDD", §6
//! "Behave/Gherkin").
//!
//! * [`highlight`]: token highlighting of `.feature` files (keywords in every
//!   bundled language, tags, comments, doc strings, tables, outline
//!   `<placeholders>`), as `(byte range, tag name)` pairs.
//! * [`parse`]: the outline of a feature file ([`Feature`] → [`Scenario`] →
//!   [`Step`], backgrounds, rules, Examples), with diagnostics instead of
//!   failures; [`outline_steps_expanded`] expands outlines per Examples row
//!   (with behave's generated scenario names).
//! * [`scan_step_definitions`]: step definitions of behave and pytest-bdd
//!   modules ([`StepDef`]), honoring `use_step_matcher(...)`.
//! * [`match_step`] / [`find_step_definitions`]: step text ↔ definitions
//!   (`parse`/`cfparse` patterns converted to regexes, `re` patterns).
//! * [`unresolved_steps`] and [`step_definition_stub`] /
//!   [`pytest_bdd_step_stub`]: "undefined step" inspection and its quick fix.
//!
//! Lines are 1-based and ranges are byte offsets into the given source.

pub mod dialect;
mod highlight;
mod lexer;
mod matching;
mod model;
mod outline;
mod parser;
mod stepdefs;
mod stubs;

pub use dialect::{default_dialect, dialect, Dialect, DIALECTS};
pub use highlight::{highlight, COMMENT, KEY, KEYWORD, NUMBER, STRING, TAG, TAG_NAMES, VARIABLE};
pub use matching::{
    definitions_for_step, find_step_definitions, match_step, match_step_kind, StepArgument,
};
pub use model::{
    Background, BlockKind, Diagnostic, DocString, Examples, Feature, Rule, Scenario, Step,
    StepKeyword, StepKind, TableRow,
};
pub use outline::{
    behave_outline_name, outline_steps_expanded, render_placeholders, OutlineExample,
};
pub use parser::parse;
pub use stepdefs::{
    default_step_matcher, looks_like_step_module, scan_step_definitions,
    scan_step_definitions_with, BddFramework, Matcher, StepDef,
};
pub use stubs::{
    pytest_bdd_step_stub, step_definition_stub, step_definition_stub_for, unresolved_steps,
};

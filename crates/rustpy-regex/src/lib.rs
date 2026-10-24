//! Regular-expression language injection for Python (roadmap §1 "Language
//! injections"): a tokenizer for `re` syntax (for highlighting), validation
//! (for inspections), discovery of injected pattern literals in a parsed
//! file, and a "Check RegExp" helper that runs a pattern through the
//! interpreter.

pub mod check;
pub mod inject;
pub mod inspection_checks;
pub mod test_match;
pub mod tokenizer;

pub use check::{check, first_error, RegexError};
pub use inject::{injections, Injection};
pub use rustpy_core::{TextRange, TextSize};
pub use test_match::{test_match, MatchResult};
pub use tokenizer::{tokenize, RegexToken, RegexTokenKind};

//! RustPy IDE — TextMate grammar support (roadmap §2.3 "TextMate bundle
//! import", settings page "TextMate Bundles").
//!
//! * **Loading**: grammars are read from `.tmLanguage.json` or XML plist
//!   `.tmLanguage` files ([`RawGrammar`]; the plist reader is built in, no
//!   `plist` crate being available offline) into a [`Registry`] keyed by
//!   scope name, which also loads whole bundle directories and picks
//!   grammars by file name or first line.
//! * **Linking**: [`Registry::grammar`] resolves `include`s — `#name`
//!   (repository, including rule-level repositories), `$self`, `$base`,
//!   `source.other` and `source.other#name` — into a [`Grammar`]. As in
//!   vscode-textmate, rules whose patterns are all unresolvable includes are
//!   dropped.
//! * **Regexes**: TextMate's Oniguruma patterns are translated for the
//!   `regex` crate (no `fancy-regex`/`onig` is available offline): `\h`,
//!   `\A`, `\G`, `\z`, `\Z`, `\K`, POSIX brackets, code point lists, option
//!   groups and `(?x)` are translated; possessive quantifiers and atomic
//!   groups become their backtracking forms; look-arounds are emulated
//!   (exactly at the start and as trailing look-aheads, by checks after the
//!   match elsewhere); subexpression calls (`\g<name>`) are inlined to a
//!   bounded recursion depth; back-references to groups with few possible
//!   texts (`(['"])…\1`) are expanded into alternatives. Patterns that cannot
//!   be expressed (other back-references, look-arounds inside repetitions,
//!   conditionals, absent operators) are skipped and listed by
//!   [`Grammar::unsupported_patterns`]; nothing panics. Against libonig, 95%
//!   of the patterns of 90 bundled VS Code grammars compile and 99.9% of
//!   sampled searches agree.
//! * **Tokenizing**: [`tokenize_line`] implements the TextMate matching
//!   model like vscode-textmate — `match` rules with `captures`, `begin`/`end`
//!   and `begin`/`while` rules with `contentName`, `beginCaptures` /
//!   `endCaptures` / `whileCaptures`, nested `patterns`, captures retokenized
//!   with their own `patterns`, `applyEndPatternLast`, back-references from
//!   `end`/`while` to `begin` captures and `$1` in scope names — with a
//!   [`StackState`] carrying the scope stack across lines.
//! * **Highlighting**: [`scope_to_tag`] maps scopes to rustpy-highlight tag
//!   names.
//!
//! Grammar injections (`injections`, `injectionSelector`) are not supported.
//!
//! ```
//! use rustpy_textmate::{tokenize_line, Grammar, StackState};
//!
//! let grammar = Grammar::from_json(
//!     r#"{ "scopeName": "source.demo", "patterns": [
//!         { "match": "\\b(?:if|else)\\b", "name": "keyword.control.demo" },
//!         { "begin": "\"", "end": "\"", "name": "string.quoted.double.demo" } ] }"#,
//! )
//! .unwrap();
//! let mut state = StackState::default();
//! let tokens = tokenize_line(&grammar, "if \"open", &mut state);
//! assert_eq!(tokens[0].scopes, ["source.demo", "keyword.control.demo"]);
//! assert_eq!(tokens[0].tag(), Some("Keyword"));
//! // The string continues on the next line.
//! let tokens = tokenize_line(&grammar, "still\" else", &mut state);
//! assert_eq!(tokens[0].range, 0..5);
//! assert_eq!(tokens[0].scopes, ["source.demo", "string.quoted.double.demo"]);
//! assert_eq!(tokens.last().unwrap().tag(), Some("Keyword"));
//! ```

mod grammar;
mod matcher;
mod onig;
mod plist;
mod raw;
mod registry;
mod scope;
mod tokenizer;

pub use grammar::{Grammar, UnsupportedPattern};
pub use plist::{parse_plist, PlistError};
pub use raw::{GrammarError, RawGrammar};
pub use registry::{BundleLoad, Registry};
pub use scope::{scope_to_tag, TAG_NAMES};
pub use tokenizer::{StackState, Token, MAX_LINE_LENGTH};

/// Tokenizes one line with `grammar`, continuing from `state` (which is
/// updated for the next line; start with [`StackState::default`]).
///
/// `line` may end with `\n` or `\r\n` (not tokenized) but should not contain
/// other line breaks. The returned tokens cover the line without gaps;
/// ranges are byte offsets into `line`, scopes go from the grammar's scope
/// name to the innermost scope.
pub fn tokenize_line(grammar: &Grammar, line: &str, state: &mut StackState) -> Vec<Token> {
    tokenizer::tokenize_line(grammar, line, state)
}

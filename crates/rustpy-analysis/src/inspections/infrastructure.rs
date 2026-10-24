//! Inspection metadata and policy shared by editor, batch runner and reports.
use super::{Problem, QuickFix};
use rustpy_core::{Severity, TextRange};
use rustpy_settings::inspection_profiles::{HighlightingLevel, Profile, Profiles};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Inspection {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    /// Baseline shown by the profile editor. Providers retain context-sensitive
    /// severity (for example definite errors versus possible incompatibilities);
    /// an explicit profile severity overrides the emitted diagnostic.
    pub default_severity: Severity,
    pub description_html: &'static str,
    pub cleanup: bool,
    pub options: &'static [&'static str],
}
pub static REGISTRY: &[Inspection] = &[
    Inspection { id:"JsonSchemaNonFinite",name:"Non-finite JSON5 number cannot satisfy JSON Schema",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>JSON5 Infinity and NaN do not belong to the finite-number data model used by JSON Schema. Choose a finite value or change the document contract; no replacement value is guessed.</p>",cleanup:false,options:&[] },
    Inspection { id:"TemplateSyntax",name:"Template syntax error",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>The selected template parser found an unmatched delimiter or malformed control block. Repair the highlighted syntax using the configured template engine. Structural checks do not execute the renderer or guess template expressions.</p>",cleanup:false,options:&[] },
    Inspection { id:"UnresolvedStaticFile",name:"Unresolved static resource",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>A literal static resource cannot be found in the configured static roots. Correct the path or add the intended asset. No empty asset is created automatically.</p>",cleanup:false,options:&[] },
    Inspection { id:"UnresolvedTemplate",name:"Unresolved template resource",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>A literal template resource cannot be found in the captured project template roots. Correct its path or use Create template for a safe project-relative target; that action requires project file creation and is not a text cleanup edit.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyPydanticValidator",name:"Validator references an unknown field",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>A canonical Pydantic validator names a field absent from the captured model. Correct the field name, declare the field, or explicitly use check_fields=False for inherited validators. Dynamic model fields are not invented.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyDeprecatedRequirementSpecifier",name:"Deprecated pip requirement syntax",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>Reports legacy #egg URLs, --install-option and the legacy resolver. Use a PEP 508 named direct reference or current build settings. Migration needs package/build intent, so no blind replacement is offered.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyRequirementConflict",name:"Conflicting exact package pins",group:"Frameworks and tools",default_severity:Severity::Warning,description_html:"<p>Two unconditional requirements pin normalized versions of the same package differently. Choose the intended version or use disjoint environment markers. No automatic rewrite chooses a dependency version.</p>",cleanup:false,options:&[] },
    Inspection{id:"PyNestedDefinition",name:"Nested definition can be static",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports nested definitions without enclosing function dependencies.</p>",cleanup:false,options:&[]},
    Inspection { id:"PyStyleBackend",name:"Full PEP 8 backend",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports a configured full style backend that could not run.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyDocstringStyle",name:"Docstring style and section format",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Checks quote style and misspelled structured documentation headings.</p>",cleanup:false,options:&["single_quoted_docstrings"] },
    Inspection { id:"PyDocstringType",name:"Docstring type differs from annotation",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Compares captured canonical documented types against annotations, preserving aliases and declaration identity.</p>",cleanup:false,options:&[] },
    Inspection { id:"PySourceStyle",name:"Source file style",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Configurable encoding/shebang, string quotes, del parentheses and trailing commas.</p>",cleanup:false,options:&["mandatory_encoding", "missing_shebang", "quote_style", "trailing_comma"] },
    Inspection { id:"PyNonAscii",name:"Non-ASCII identifier or comment",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Optional source-text convention for identifiers and comments.</p>",cleanup:false,options:&["non_ascii_identifiers", "non_ascii_comments"] },
    Inspection { id:"PyFunctionMetrics",name:"Function complexity limits",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Optional function arguments, branches, returns, statements and local-variable limits.</p>",cleanup:false,options:&["max_arguments", "max_branches", "max_returns", "max_statements", "max_locals"] },
    Inspection { id:"PyDuplicatedCode",name:"Duplicated project code",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Finds non-overlapping exact token fragments across project sources and unsaved buffers within a bounded scan.</p>",cleanup:false,options:&["duplicate_code", "duplicate_min_lines", "duplicate_min_tokens"] },
    Inspection { id:"PyRedundantDictDefault",name:"Redundant dict.get None default",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports the redundant default on an actual builtin dictionary.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyDictKeysIteration",name:"Dictionary keys iteration",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports explicit keys() when iteration can use the dictionary itself.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyDeprecatedHasKey",name:"Obsolete dictionary has_key",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Suggests membership for the obsolete has_key method.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyUnnecessaryPass",name:"Unnecessary pass",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports pass when the suite contains other statements.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyNoElseReturn",name:"Unnecessary else after return",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports else after a terminating branch.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyRedundantIf",name:"Identical conditional branches",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports redundant conditionals with identical branch bodies.</p>",cleanup:false,options:&[] },
    Inspection { id:"PySimplifyBoolean",name:"Simplifiable boolean return",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Offers the existing guarded boolean-return rewrite.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyEnumerate",name:"Use enumerate",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Suggests enumerate for range(len(sequence)) index loops.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyLoopToComprehension",name:"Loop can be simplified",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Offers existing guarded comprehension, any, all and sum transformations.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyUnnecessaryLambda",name:"Unnecessary forwarding lambda",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports lambdas which only forward their arguments.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyDeprecatedRaise",name:"Deprecated raise syntax",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports old comma exception syntax.</p>",cleanup:false,options:&[] },
    Inspection { id:"PyPrintRedirect",name:"Python 2 print redirection",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>Reports print >> use and suggests print(file=...).</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E101",name:"PEP 8 E101",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E101: indentation contains mixed spaces and tabs.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E111",name:"PEP 8 E111",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E111: indentation is not a multiple of four.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E112",name:"PEP 8 E112",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E112: expected an indented block.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E113",name:"PEP 8 E113",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E113: unexpected indentation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E114",name:"PEP 8 E114",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E114: comment indentation is not a multiple of four.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E115",name:"PEP 8 E115",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E115: expected an indented comment block.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E116",name:"PEP 8 E116",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E116: unexpected comment indentation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E121",name:"PEP 8 E121",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E121: continuation line under-indented for hanging indent.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E122",name:"PEP 8 E122",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E122: continuation line missing indentation or outdented.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E123",name:"PEP 8 E123",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E123: closing bracket does not match the indentation of its opening bracket.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E124",name:"PEP 8 E124",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E124: closing bracket does not match visual indentation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E125",name:"PEP 8 E125",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E125: continuation line has the same indentation as the next logical line.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E126",name:"PEP 8 E126",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E126: continuation line over-indented for hanging indent.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E127",name:"PEP 8 E127",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E127: continuation line over-indented for visual indent.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E128",name:"PEP 8 E128",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E128: continuation line is under-indented for visual indentation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E129",name:"PEP 8 E129",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E129: visually indented continuation has the same indentation as the next logical line.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E131",name:"PEP 8 E131",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E131: continuation line unaligned for hanging indent.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E133",name:"PEP 8 E133",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E133: closing bracket is missing indentation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E201",name:"PEP 8 E201",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E201: whitespace immediately after an opening delimiter.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E202",name:"PEP 8 E202",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E202: whitespace before a closing delimiter.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E203",name:"PEP 8 E203",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E203: whitespace before punctuation.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E204",name:"PEP 8 E204",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E204: whitespace between the decorator marker and its expression.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E211",name:"PEP 8 E211",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E211: whitespace before an opening parenthesis or bracket.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E221",name:"PEP 8 E221",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E221: multiple spaces before operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E222",name:"PEP 8 E222",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E222: multiple spaces after operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E223",name:"PEP 8 E223",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E223: tab before operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E224",name:"PEP 8 E224",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E224: tab after operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E226",name:"PEP 8 E226",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E226: missing whitespace around an arithmetic operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E227",name:"PEP 8 E227",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E227: missing whitespace around a bitwise or shift operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E228",name:"PEP 8 E228",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E228: missing whitespace around modulo operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E241",name:"PEP 8 E241",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E241: multiple spaces after a comma, semicolon or colon.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E242",name:"PEP 8 E242",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E242: tab after a comma, semicolon or colon.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E251",name:"PEP 8 E251",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E251: unexpected spaces around keyword / parameter equals.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E252",name:"PEP 8 E252",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E252: missing whitespace around parameter equals.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E262",name:"PEP 8 E262",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E262: inline comment should start with a hash and one space.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E265",name:"PEP 8 E265",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E265: block comment should start with a hash and one space.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E266",name:"PEP 8 E266",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E266: too many leading hash characters in a block comment.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E271",name:"PEP 8 E271",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E271: multiple spaces after keyword.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E272",name:"PEP 8 E272",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E272: multiple spaces before keyword.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E273",name:"PEP 8 E273",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E273: tab after keyword.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E274",name:"PEP 8 E274",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E274: tab before keyword.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E275",name:"PEP 8 E275",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E275: missing whitespace after keyword.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E301",name:"PEP 8 E301",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E301: missing blank line between method definitions.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E304",name:"PEP 8 E304",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E304: blank lines found after function decorator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E305",name:"PEP 8 E305",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E305: missing blank lines after a function or class definition.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E306",name:"PEP 8 E306",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E306: missing blank line before a nested definition.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E402",name:"PEP 8 E402",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E402: module level import not at top of file.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E502",name:"PEP 8 E502",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E502: the backslash is redundant between brackets.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E701",name:"PEP 8 E701",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E701: multiple statements on one line (colon).</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E702",name:"PEP 8 E702",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E702: multiple statements on one line (semicolon).</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E703",name:"PEP 8 E703",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E703: statement ends with a semicolon.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E704",name:"PEP 8 E704",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E704: multiple statements on one line (def).</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E713",name:"PEP 8 E713",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E713: membership test should use not in instead of not ... in.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E714",name:"PEP 8 E714",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E714: identity test should use is not instead of not ... is.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E721",name:"PEP 8 E721",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E721: use isinstance for subclass-aware type checks or is/is not for deliberate exact type identity.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E722",name:"PEP 8 E722",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E722: bare except catches all exceptions including process-control exceptions.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E731",name:"PEP 8 E731",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E731: prefer a def statement to assigning a lambda expression.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E741",name:"PEP 8 E741",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E741: ambiguous variable name such as l, O or I.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E742",name:"PEP 8 E742",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E742: ambiguous class name such as l, O or I.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E743",name:"PEP 8 E743",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E743: ambiguous function name such as l, O or I.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E901",name:"PEP 8 E901",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E901: source tokenization failed with a syntax or indentation error.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:E902",name:"PEP 8 E902",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>E902: source could not be read because of an I/O error.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W191",name:"PEP 8 W191",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W191: indentation contains tabs.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W391",name:"PEP 8 W391",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W391: blank line at end of file.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W503",name:"PEP 8 W503",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W503: line break before binary operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W504",name:"PEP 8 W504",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W504: line break after binary operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W505",name:"PEP 8 W505",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W505: doc line too long.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },
    Inspection { id:"PEP8:W605",name:"PEP 8 W605",group:"Python",default_severity:Severity::WeakWarning,description_html:"<p>W605: invalid escape sequence.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",cleanup:false,options:&[] },

    Inspection {
        id: "NamespacePackage",
        name: "Incompatible implicit namespace package",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports a captured namespace root containing an incompatible __init__.py. Choose whether this is a namespace or regular package before changing its initializer; no file is deleted automatically.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "NonAsciiPath",
        name: "Non-ASCII path compatibility",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Optionally reports non-ASCII source paths for compatibility with constrained tools. Rename through project refactoring when required; Unicode paths are not inherently invalid.</p>",
        cleanup: false,
        options: &["warn_non_ascii_paths"],
    },
    Inspection {
        id: "HtmlSyntax",
        name: "HTML structure",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks literal HTML element nesting and required closing tags while respecting void elements, quoted attributes and script/style bodies. Correct the highlighted structure; content is not removed automatically.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "CssSyntax",
        name: "CSS structure",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks CSS delimiter structure outside strings and comments. Repair the highlighted rule or declaration; a missing delimiter is not inserted speculatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "HttpClient",
        name: "HTTP request configuration",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks captured HTTP request methods, versions, headers and supported URL schemes. Correct the request; inspection does not send it or infer credentials.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "RstInspection",
        name: "reStructuredText references",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks reStructuredText directives, roles and literal references using the captured document targets. Correct the target or use the intended supported role; extensions remain explicit.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "ShellSyntax",
        name: "Shell syntax",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks shell delimiter and quoting structure without executing the script. Correct the highlighted construct; command semantics are not inferred.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "JavaScriptSyntax",
        name: "JavaScript syntax",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks supported JavaScript/TypeScript lexical structure without running Node. Repair malformed delimiters and strings; the installed language service retains full language/type authority.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "SqlDialect",
        name: "SQL dialect syntax",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks captured SQL against the selected dialect and known syntax restrictions. Select the intended dialect or rewrite the statement; no query executes during inspection.</p>",
        cleanup: false,
        options: &["dialect"],
    },
    Inspection {
        id: "PyDjangoMigration",
        name: "PyDjangoMigration",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the selected trusted project's captured Django catalogue. No diagnostic is inferred from a failed or unavailable framework probe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDjangoSetting",
        name: "PyDjangoSetting",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the selected trusted project's captured Django catalogue. No diagnostic is inferred from a failed or unavailable framework probe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDjangoUrl",
        name: "PyDjangoUrl",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the selected trusted project's captured Django catalogue. No diagnostic is inferred from a failed or unavailable framework probe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "DjangoTemplate",
        name: "DjangoTemplate",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the selected trusted project's captured Django catalogue. No diagnostic is inferred from a failed or unavailable framework probe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "JinjaTemplate",
        name: "Jinja2 template",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports Jinja2 filters and tests that are neither builtin nor statically registered by the project, and macros imported from a template that does not define them. Projects with dynamic registrations are not judged.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PytestUnknownFixture",
        name: "Unknown pytest fixture",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports fixture parameters absent from an explicitly complete captured pytest/plugin inventory. Correct the parameter or declare/register the fixture; unknown plugin state is not diagnosed as absence.</p>",
        cleanup: false,
        options: &["check_unknown_fixtures", "known_fixtures"],
    },
    Inspection {
        id: "PytestUnknownMark",
        name: "Unknown pytest mark",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports pytest marks absent from captured registration and builtin marks. Register the intended mark or correct its name; unrelated decorator objects do not count as pytest.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PytestParametrize",
        name: "Invalid pytest parametrization",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks literal parametrization names and row arity on canonical pytest decorators. Correct the names/values together; a missing test value is not invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PytestYieldFixture",
        name: "Invalid yield fixture",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports illegal yield-fixture control flow on canonical fixtures. Ensure each executed fixture path yields the intended value once; mutually exclusive yields remain valid.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PytestUnusedFixture",
        name: "Unused private fixture",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports supported private fixture declarations without captured consumers. Review autouse and plugin visibility before removing a fixture; dynamic references are not fabricated.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "Copyright",
        name: "Copyright",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the configured copyright text and captured year. Insert or update the project-approved notice; the checker does not invent a copyright owner.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "Dockerfile",
        name: "Dockerfile",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks FROM references and basic Dockerfile contracts. Pin the intended image tag or digest and correct the highlighted directive; image selection requires user intent.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "EditorConfig",
        name: "Editor Config",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports unsupported keys, invalid values and duplicate configuration. The offered value replacement uses the maintained EditorConfig schema; duplicate removal preserves the remaining settings.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "EnvFile",
        name: "Env File",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports repeated environment keys. Review which value should win before removing a duplicate; environment values may be secrets and are not guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "GitIgnorePattern",
        name: "Git Ignore Pattern",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks duplicate and optionally unused ignore patterns against captured project paths. Remove a duplicate using the offered edit; absence of a current match does not prove a pattern will never be needed.</p>",
        cleanup: false,
        options: &["check_unused_patterns"],
    },
    Inspection {
        id: "JsonSchema",
        name: "Json Schema",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Validates captured JSON values against the selected schema, including required fields and value types. Correct the highlighted value or schema; remote schemas are not downloaded by inspection.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "Markdown",
        name: "Markdown",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks heading structure and literal links against the captured document/project. Correct the target or heading level; remote link availability is not inferred.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDjangoModel",
        name: "Py Django Model",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks canonical Django model fields and relationships. Review invalid field options or unresolved targets against the model contract; migrations and database changes are not applied by an inspection.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDjangoOrmLookup",
        name: "Py Django Orm Lookup",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports unknown field names, relations and lookups in Django queryset <code>filter/exclude/get</code> keywords and <code>order_by/values/only/defer/select_related/prefetch_related</code> paths. Only models whose fields are fully declared in the project are checked; inherited, abstract, third-party or dynamically added fields and names introduced by <code>annotate()</code>/<code>alias()</code> are never reported.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyFastApiDependency",
        name: "Py Fast Api Dependency",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks canonical FastAPI dependency declarations and endpoint annotations. Correct the dependency callable or annotation; dynamic injection is not executed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyFastApiResponseModel",
        name: "Py Fast Api Response Model",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks that a FastAPI route's response_model is a Pydantic model or type and agrees with a Pydantic return annotation (the same model or a subclass). Use response_model=None to disable response validation.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyInterpreter",
        name: "Py Interpreter",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports a missing or invalid interpreter only when interpreter state was explicitly checked. Select a valid SDK; the configuration action has no source edits.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyPackageRequirements",
        name: "Py Package Requirements",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Compares declared dependencies against an explicitly captured interpreter inventory and marker environment. Use the install action for the selected requirement or correct the declaration; an unknown inventory is not an empty environment.</p>",
        cleanup: false,
        options: &["require_requirements_file", "active_extras"],
    },
    Inspection {
        id: "PyProjectSchema",
        name: "Py Project Schema",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks the captured pyproject.toml structure and metadata. Repair the highlighted schema obligation; package/build backends are not imported.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyPydantic",
        name: "Py Pydantic",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks canonical Pydantic fields, configuration and validator signatures. Adjust the declaration to the installed API contract; runtime-generated models remain unknown.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRequirementSpecifier",
        name: "Py Requirement Specifier",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports a malformed requirement version or marker expression. Correct the PEP 440/508 expression; no intended version range is guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRoute",
        name: "Py Route",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks captured route decorators, methods and duplicate endpoint identities. Correct the decorator or conflicting route; renaming an endpoint needs application intent.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySetupSchema",
        name: "Py Setup Schema",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks literal setup metadata and package declarations. Repair the highlighted value; setup.py is never executed to obtain missing metadata.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySqlAlchemyModel",
        name: "Py Sql Alchemy Model",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks canonical SQLAlchemy columns and relationship declarations. Correct the mapped field or relationship target; no database mutation is performed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnittestDeprecatedAlias",
        name: "Py Unittest Deprecated Alias",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports deprecated unittest assertion aliases on canonical unittest receivers. The offered rename uses the supported assertion method and preserves arguments.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnlistedDependency",
        name: "Py Unlisted Dependency",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>A canonical imported package is absent from captured project dependency declarations. Declare the intended distribution; import names do not always equal package names.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyVulnerablePackage",
        name: "Py Vulnerable Package",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports installed versions affected by captured vulnerability advisories. Review an available fixed release in Python Packages; automatic upgrades could break dependency constraints.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "TodoComment",
        name: "Todo Comment",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Reports configured unfinished-work markers in selected lexical comments or strings. Finish or deliberately retain the work; removing a marker would not implement it.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "TomlSchema",
        name: "Toml Schema",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks TOML values against the selected captured configuration schema. Correct the highlighted key or value; unknown extension tables are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "YamlSchema",
        name: "Yaml Schema",
        group: "Frameworks and tools",
        default_severity: Severity::Warning,
        description_html: "<p>Checks YAML values against the selected captured schema. Correct required fields and value types; aliases and dynamically supplied values are not executed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityAssert",
        name: "Security: Assert",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe assert behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &["warn_assertions"],
    },
    Inspection {
        id: "PySecurityAutoescape",
        name: "Security: Autoescape",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe autoescape behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityBindAll",
        name: "Security: Bind All",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe bind all behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityDjango",
        name: "Security: Django",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe django behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityInput",
        name: "Security: Input",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe input behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityRandom",
        name: "Security: Random",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe random behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &["warn_all_uses"],
    },
    Inspection {
        id: "PySecuritySqlInjection",
        name: "Security: Sql Injection",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe sql injection behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityTarExtraction",
        name: "Security: Tar Extraction",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe tar extraction behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityTls",
        name: "Security: Tls",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe tls behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityWeakHash",
        name: "Security: Weak Hash",
        group: "Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports potentially unsafe weak hash behavior. Canonical call identities and known configuration are checked; dynamic receivers are not guessed. Configure severity, file scopes or suppression using the inspection profile.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyAugmentedAssignment",
        name: "Augmented Assignment",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports augmented assignment problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyCollectionLiteral",
        name: "Collection Literal",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports collection literal problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyComparisonWithCallable",
        name: "Comparison With Callable",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports comparison with callable problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDataclass",
        name: "Dataclass",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports dataclass problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDivisionByZero",
        name: "Division By Zero",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports division by zero problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDunderSignature",
        name: "Dunder Signature",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports dunder signature problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyEnum",
        name: "Enum",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports enum problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyException",
        name: "Exception",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports exception problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyFinallyTransfer",
        name: "Finally Transfer",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports finally transfer problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyHashDefinition",
        name: "Hash Definition",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports hash definition problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyIncompatibleOverride",
        name: "Incompatible Override",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports incompatible override problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyInitCall",
        name: "Init Call",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports init call problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyModernTypeHint",
        name: "Modern Type Hint",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports modern type hint problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyMutableDefaultCall",
        name: "Mutable Default Call",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports mutable default call problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyNamedTuple",
        name: "Named Tuple",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports named tuple problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyOldStyleClass",
        name: "Old Style Class",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports old style class problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyOverloads",
        name: "Overloads",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports overloads problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyProperty",
        name: "Property",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports property problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRedundantBoolean",
        name: "Redundant Boolean",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports redundant boolean problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRedundantUnion",
        name: "Redundant Union",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports redundant union problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySlotsConflict",
        name: "Slots Conflict",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports slots conflict problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyStringFormat",
        name: "String Format",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports string format problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTrailingCommaTuple",
        name: "Trailing Comma Tuple",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports trailing comma tuple problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTupleAssignmentBalance",
        name: "Tuple Assignment Balance",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports tuple assignment balance problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTypeComparison",
        name: "Type Comparison",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports type comparison problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTypeHints",
        name: "Type Hints",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports type hints problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTypeVarMisuse",
        name: "Type Var Misuse",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports type var misuse problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTypedDict",
        name: "Typed Dict",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports typed dict problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnreachableExcept",
        name: "Unreachable Except",
        group: "Type checking",
        default_severity: Severity::Warning,
        description_html: "<p>Reports unreachable except problems using declared types and supported Python protocols. Unknown or dynamic contracts are retained conservatively.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnusedParameter",
        name: "Unused parameter",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports unused parameters, excluding receiver, placeholder, decorated, abstract and override signatures.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnusedLocalDefinition",
        name: "Unused local definition",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports local functions and classes that are never referenced.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyGlobalUsedBeforeAssignment",
        name: "Global name used before definition",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports eagerly evaluated module references before their global definition.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyImportShadowed",
        name: "Imported name overwritten before use",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports assignments that replace an imported name before its value is used.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyLoopOrWithVariableAssignment",
        name: "Acquired variable overwritten before use",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports loop or context-manager targets overwritten before the acquired value is used.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySuperArguments",
        name: "Invalid super arguments",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks the argument count and nominal class/receiver relationship of builtin super().</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySelfOutsideMethod",
        name: "Self used outside a method",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports unresolved self references outside a declared method receiver.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyInvalidAll",
        name: "Invalid export list",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports __all__ values that are not lists or tuples of strings.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDuplicateImport",
        name: "Redundant import",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports repeated imports of the same binding and offers safe removal.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyLocalImport",
        name: "Local import",
        group: "Python",
        default_severity: Severity::Info,
        description_html: "<p>Identifies imports inside functions or classes so intentional lazy loading remains visible.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyImportSelf",
        name: "Module imports itself",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports imports that resolve to their containing module.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyModuleNotOnPath",
        name: "Module is not in project or on sys.path",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports imports that resolve only relative to the importing file, to a module outside every project content or source root and outside the interpreter's sys.path and stub libraries. Such imports fail when the code runs from the project; add the directory as a source root or move the module.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyImportShadowsModule",
        name: "Import shadows a module or package submodule",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports imports of a standard-library or installed module that resolve to a project file of the same name (a local <code>random.py</code> captures <code>import random</code>), and imports in a package <code>__init__.py</code> that rebind the name of one of the package's own submodules. Rename the project module or bind the import under another name.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyCyclicImport",
        name: "Cyclic import dependency",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports proven import cycles that may expose a partially initialized module.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyWildcardImport",
        name: "Wildcard import",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports wildcard imports and offers removal when no imported binding is used.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnsortedImports",
        name: "Import order",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports imports that differ from configured grouping and sorting, with an optimize-imports fix.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyUnnecessarySemicolon",
        name: "Unnecessary semicolon",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports statement-separating or trailing semicolons and safely removes trailing separators.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyUnnecessaryBackslash",
        name: "Unnecessary continuation backslash",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports redundant explicit continuations inside brackets, with a removal fix.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyRelativeImport",
        name: "Relative import outside its package",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports relative imports in non-package modules or imports that ascend beyond the enclosing package.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyClassCell",
        name: "Unavailable implicit class context",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports references to an unavailable __class__ cell and zero-argument super() calls without a class method receiver.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "StructuralSearchInspection",
        name: "Structural search inspection",
        group: "Structural search",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports matches of saved structural templates enabled as inspections. Templates may supply a replacement quick fix.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "Grammar",
        name: "Grammar",
        group: "Proofreading",
        default_severity: Severity::Typo,
        description_html: "<p>Reports grammar and punctuation issues in supported prose contexts. Suggested replacements are offered only when the language checker supplies them; source syntax is preserved.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E225",
        name: "PEP8 E225",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E225: missing whitespace around operator.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E231",
        name: "PEP8 E231",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E231: missing whitespace after a comma, semicolon or colon.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E261",
        name: "PEP8 E261",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E261: at least two spaces before inline comment.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E302",
        name: "PEP8 E302",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E302: missing blank lines around top-level definitions.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E303",
        name: "PEP8 E303",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E303: too many consecutive blank lines.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E401",
        name: "PEP8 E401",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E401: multiple imports on one line.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:E501",
        name: "PEP8 E501",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E501: line too long.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &["max_line_length"],
    },
    Inspection {
        id: "PEP8:E711",
        name: "PEP8 E711",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E711: comparison with None should use is or is not.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PEP8:E712",
        name: "PEP8 E712",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>E712: comparison with True/False should use a boolean expression or identity check.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PEP8:W291",
        name: "PEP8 W291",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>W291: trailing whitespace.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PEP8:W292",
        name: "PEP8 W292",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>W292: no newline at end of file.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PEP8:W293",
        name: "PEP8 W293",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>W293: blank line contains whitespace.</p><p>Use the highlighted source location to correct this style rule. A source edit is offered only when the maintained checker has a safe rewrite; formatting and profile options may be preferable for layout changes.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyAbstractClass",
        name: "Abstract Class",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports instantiation of a class with unimplemented abstract members. Implement the required members or instantiate a concrete class; method bodies are not guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyArgumentList",
        name: "Incorrect call arguments",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks positional, keyword, unpacked and overload arguments against resolved call signatures. Correct the call or declaration; unknown argument shapes stay conservative.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyAssertTuple",
        name: "Assert Tuple",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>A nonempty tuple in assert is always truthy. The offered parentheses removal converts the assertion to condition/message syntax where that form is supported.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyAsyncCall",
        name: "Async Call",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports coroutine/awaitable results used without the required await. Add await only in a legal async context and review intentional task scheduling.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyAttributeOutsideInit",
        name: "Attribute Outside Init",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports instance attributes first established outside construction. Initialize the intended state in __init__ or retain the deliberate lifecycle; a default value is not guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyAwaitOutsideAsync",
        name: "Await Outside Async",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports await outside an async function. Move it to a legal async context or revise the API; changing a function to async can affect all callers.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyBroadException",
        name: "Unclear exception clauses",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports bare or overly broad exception handlers. Catch the intended exception family; the checker does not guess which failures the application should recover from.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyChainedComparisons",
        name: "Chained Comparisons",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Suggests equivalent chained comparisons when operand evaluation and operators allow a safe rewrite. Review the offered expression edit before applying.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyCompatibility",
        name: "Code is incompatible with specific Python versions",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Checks syntax and known APIs against the configured Python language level. Select the intended SDK/language level or use a supported construct; syntax-specific migrations are offered only when safe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyComprehensionInCollectionCall",
        name: "Comprehension In Collection Call",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports redundant collection construction around a comprehension. Use the offered direct-comprehension rewrite when the target is the canonical builtin.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyDefaultArgument",
        name: "The default argument is mutable",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports mutable default objects shared across calls. The offered None-sentinel rewrite preserves per-call initialization when the function shape supports it.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDeprecation",
        name: "Deprecation",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports canonical deprecated APIs, including captured PEP 702 annotations. Use a documented replacement; a rename is offered only when a maintained equivalent is known.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDictDuplicateKeys",
        name: "Dict Duplicate Keys",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports repeated statically known dictionary keys. Removing an overwritten entry can remove value-expression side effects, so review any offered removal.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyDocstring",
        name: "Incorrect docstring",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Checks configured docstring obligations. Write documentation reflecting the declaration; supplied stubs are starting points rather than invented behavior.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyFromFutureImport",
        name: "From Future Import",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks future imports, recognized features and legal placement. Move a valid import to the module header or remove an unsupported feature without changing unrelated imports.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyFunctionInCondition",
        name: "Function In Condition",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports a resolved callable used as a boolean where a call is likely intended. Add the intended arguments or retain the deliberate truthiness test; arguments are not invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyGlobalAtModuleLevel",
        name: "Global At Module Level",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports global declarations at module scope where they have no effect. Remove the redundant declaration if it does not carry a deliberate tooling contract.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyGlobalUndefined",
        name: "Global Undefined",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports a global declaration with no corresponding module binding. Define the intended module state or correct the name; no value is guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyIncorrectDocstring",
        name: "Incorrect Docstring",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Checks documented parameters and return sections against canonical signatures and annotations. Correct stale sections or use an offered synchronized declaration edit.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyIsLiteral",
        name: "Is Literal",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports identity comparisons against literals that should usually use equality. The offered operator replacement preserves operands; singleton checks such as is None stay valid.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyLenZeroCheck",
        name: "Len Zero Check",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Suggests truthiness instead of comparing canonical len(...) with zero. The guarded rewrite preserves comparison polarity and avoids custom len bindings.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyLoopControlOutsideLoop",
        name: "Loop Control Outside Loop",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports break or continue outside a legal enclosing loop. Move the statement into the intended loop; deleting control flow is not automatically safe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyMethodMayBeStatic",
        name: "Method is not declared static",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>A method does not use its instance receiver. Review conversion to a function or static method; descriptor and override semantics constrain offered fixes.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyMethodParameters",
        name: "Methods having troublesome first parameters",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks receiver parameters and supported method signatures. Correct self/cls or decorator usage; changing a public signature can affect callers.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyMissingConstructor",
        name: "Missed call to '__init__' of the super class",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports a configured class-construction obligation. Add initialization matching the actual class contract; field values are not invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyMissingOrEmptyDocstring",
        name: "Missing or empty docstring",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports missing or blank documentation for configured declaration kinds. The insert-stub fix respects shebang/encoding headers and declaration indentation.</p>",
        cleanup: false,
        options: &["doc_modules", "doc_classes", "doc_functions", "doc_methods", "doc_private"],
    },
    Inspection {
        id: "PyNegatedIn",
        name: "Negated In",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Suggests not in / is not instead of negating in / is expressions. The offered replacement preserves operands and precedence.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyNonlocalDeclaration",
        name: "Nonlocal Declaration",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks nonlocal names against enclosing function bindings. Correct the scope or name; a new enclosing value is not invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyParameterList",
        name: "Incorrect parameter list",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Checks duplicate and illegally ordered parameters, including positional-only and keyword-only boundaries. Repair the signature and its callers deliberately.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyPep8",
        name: "PEP 8 coding style violation",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Groups PEP 8 E/W diagnostics from the configured builtin or full checker. Individual code rules can override the family; fixes are offered only for supported source-preserving rewrites.</p>",
        cleanup: false,
        options: &["max_line_length", "ignoredErrors", "backend"],
    },
    Inspection {
        id: "PyPep8Naming",
        name: "PEP 8 naming convention violation",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Checks configured function, class, argument, variable and constant naming conventions with canonical override exemptions. Rename through the refactoring action to update references.</p>",
        cleanup: false,
        options: &["ignoredNames", "ignoredPatterns", "ignore_overrides", "check_constants"],
    },
    Inspection {
        id: "PyProtectedMember",
        name: "Protected Member",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports protected member access outside the supported ownership context. Prefer a public API or retain the intentional access with suppression; no public replacement is guessed.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyPrivateMember",
        name: "Access to a private member",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports access to a name-mangled <code>__member</code> of a class or instance outside the body of the class that declares it, including from subclasses, where Python mangles the name to a different <code>_Class__member</code> attribute.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRaiseNotImplemented",
        name: "Raise Not Implemented",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>NotImplemented is a comparison sentinel, not an exception. The offered replacement uses NotImplementedError while preserving the raise arguments.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyRedeclaration",
        name: "Redeclared names without usages",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports names redeclared in the same binding context. Correct the unintended declaration; deleting a declaration can change runtime effects.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyRedundantParentheses",
        name: "Redundant Parentheses",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports parentheses that can be removed without changing parsed expression structure. The offered edit preserves required precedence.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyRegex",
        name: "Regular expression errors",
        group: "RegExp",
        default_severity: Severity::Error,
        description_html: "<p>Checks literal regular expressions for malformed groups, escapes and supported lookbehind constraints. Correct the pattern; matching intent is not inferred.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyReturnFromInit",
        name: "Return From Init",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports non-None return values in __init__. Initialize the instance instead or move factory behavior to a separate callable.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyReturnOutsideFunction",
        name: "Return Outside Function",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports return outside a function body. Move it into the intended callable; deleting the statement would guess program control flow.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurity",
        name: "Security",
        group: "Security",
        default_severity: Severity::Warning,
        description_html: "<p>Groups optional security inspections. Review the specific finding and trust boundary; suppression records an intentional exception and does not mitigate a vulnerability.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityEval",
        name: "Use of eval or exec",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports dynamic eval/exec inputs with canonical builtin identity. Prefer a data parser or a constrained design. No automatic replacement claims arbitrary Python execution is equivalent to parsing data.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityHardcodedSecret",
        name: "Hardcoded secret",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports literal credentials in canonical secret-like declarations while excluding placeholder/configuration names. Move the secret to an approved secret source and rotate exposed credentials; no credential value is rewritten automatically.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityOsSystem",
        name: "Use of os.system",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports canonical os.system calls. Prefer subprocess argument vectors and explicit error handling; shell-string parsing cannot safely infer argument boundaries.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityShellInjection",
        name: "Shell command injection",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports canonical subprocess calls using shell=True with unsafe inputs. Review command construction and use an argument vector; simply deleting shell=True can alter command meaning.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityTempfile",
        name: "Insecure temporary file",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports tempfile.mktemp paths that can be raced before creation. Use an atomic temporary-file API; lifetime and ownership determine the safe replacement.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySecurityUnsafeDeserialization",
        name: "Unsafe deserialization",
        group: "Python Security",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports unsafe pickle/YAML deserialization on canonical APIs. The safe_load rename is offered for supported YAML calls; pickle has no equivalent safe textual rewrite.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySetDuplicateItems",
        name: "Set Duplicate Items",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports repeated statically known set elements. Review side effects before removing a duplicate expression.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyShadowingBuiltins",
        name: "Shadowing built-in names",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports a local name masking a canonical builtin. Use Rename to update references; textual declaration-only renaming is not safe.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyShadowingNames",
        name: "Shadowing Names",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports a declaration hiding an outer binding. Rename the intended symbol through refactoring or retain deliberate shadowing.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyStatementEffect",
        name: "Statement has no effect",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports expression statements without an observable supported effect. Remove accidental expressions or perform the intended assignment/call; unknown custom operations remain conservative.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PySyntaxError",
        name: "Syntax errors",
        group: "Syntax",
        default_severity: Severity::Error,
        description_html: "<p>Reports parser errors in the actual source. Repair the syntax first; syntax diagnostics cannot be hidden by inspection suppression.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyTypeChecker",
        name: "Incorrect type",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Checks resolved argument, assignment, return and operation types. Correct the incompatible expression or annotation; Any and unresolved dynamic state are not treated as proof of incompatibility.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnboundLocalVariable",
        name: "Unbound Local Variable",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports reads without a definitely available local binding on a reachable flow path. Initialize the intended state or repair control flow; no default value is invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnreachableCode",
        name: "Unreachable code",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports statements after a terminal control-flow path. Review whether the terminal statement or following code is unintended before removing code.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnresolvedReferences",
        name: "Unresolved references",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports names or members without a canonical declaration. Available actions may add an import, create a declaration or install a captured package; dynamic unknown members are not invented.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyUnusedImports",
        name: "Unused import",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports imports unused by resolved references, respecting exports and package initializer policy. The removal fix preserves remaining imported names and attached source structure.</p>",
        cleanup: true,
        options: &[],
    },
    Inspection {
        id: "PyUnusedLocal",
        name: "Unused local symbols",
        group: "Python",
        default_severity: Severity::WeakWarning,
        description_html: "<p>Reports unused local bindings while retaining evaluation where necessary. Apply only the supplied binding edit; discarding a right-hand side could remove effects.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyYAML",
        name: "YAML",
        group: "Python",
        default_severity: Severity::Warning,
        description_html: "<p>Reports unsafe or invalid captured YAML API usage. Prefer supported safe-loading APIs; changing serialization contracts needs review.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "PyYieldOutsideFunction",
        name: "Yield Outside Function",
        group: "Python",
        default_severity: Severity::Error,
        description_html: "<p>Reports yield outside a legal function body. Move generator behavior into the intended callable; changing an enclosing function affects callers.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "SpellCheckingInspection",
        name: "Typo",
        group: "Proofreading",
        default_severity: Severity::Typo,
        description_html: "<p>Reports misspelled words in configured prose/name contexts. Apply a supplied replacement, rename a symbol through refactoring, or add an intentional spelling to the dictionary.</p>",
        cleanup: false,
        options: &[],
    },
    Inspection {
        id: "syntax",
        name: "Syntax errors",
        group: "Syntax",
        default_severity: Severity::Error,
        description_html: "<p>Reports parser errors in the actual source. Repair the syntax first; syntax diagnostics cannot be hidden by inspection suppression.</p>",
        cleanup: false,
        options: &[],
    },
];
pub fn inspection(id: &str) -> Option<&'static Inspection> {
    REGISTRY.iter().find(|entry| entry.id == id).or_else(|| {
        let suffix = id.strip_prefix("PEP8:")?;
        if suffix.len() != 4
            || !matches!(suffix.as_bytes()[0], b'E' | b'W')
            || !suffix.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        REGISTRY.iter().find(|entry| entry.id == "PyPep8")
    })
}
pub fn canonical_id(code: &str) -> &str {
    if code == "syntax" {
        "PySyntaxError"
    } else if code.starts_with("PEP8:") {
        "PyPep8"
    } else {
        code
    }
}
pub fn matches_inspection(selected: &str, code: &str) -> bool {
    selected == "*"
        || selected == code
        || (selected == "PyPep8" && code.starts_with("PEP8:"))
        || (selected == "PySyntaxError" && code == "syntax")
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Policy {
    pub profile: Profile,
    pub severities: Profiles,
    pub root: PathBuf,
    pub batch: bool,
}
impl Policy {
    pub fn from_settings(
        settings: &Profiles,
        root: Option<&Path>,
        batch: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            profile: settings.resolved(root)?,
            severities: settings.clone(),
            root: root.unwrap_or(Path::new("")).into(),
            batch,
        })
    }
    pub fn level(&self, path: &Path) -> HighlightingLevel {
        if self.batch {
            HighlightingLevel::All
        } else {
            self.severities.level(path)
        }
    }
    pub fn rule(&self, code: &str) -> Option<&rustpy_settings::inspection_profiles::Rule> {
        self.profile
            .inspections
            .get(code)
            .or_else(|| self.profile.inspections.get(canonical_id(code)))
    }
    pub fn enabled(&self, path: &Path, code: &str) -> bool {
        let relative = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        self.rule(code)
            .is_none_or(|rule| rule.enabled && rule.matches(&relative))
    }
    pub fn apply(&self, path: &Path, problems: &mut Vec<Problem>) {
        let level = self.level(path);
        let relative = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        problems.retain_mut(|problem| {
            let code = problem.diagnostic.code.as_str();
            if level == HighlightingLevel::None {
                return false;
            }
            if matches!(code, "syntax" | "PySyntaxError") {
                return true;
            }
            if level == HighlightingLevel::Syntax {
                return false;
            }
            if let Some(rule) = self.rule(code) {
                if !rule.enabled || !rule.matches(&relative) {
                    return false;
                }
                if let Some(severity) = rule
                    .severity
                    .as_deref()
                    .and_then(|name| self.severities.severity(name))
                {
                    problem.diagnostic.severity = severity;
                }
            }
            true
        });
    }
    pub fn severity_name(&self, code: &str) -> Option<String> {
        let name = self.rule(code)?.severity.as_ref()?;
        self.severities.severity(name).map(|_| name.clone())
    }
    pub fn line_length(&self, fallback: usize) -> usize {
        self.rule("PEP8:E501")
            .and_then(|rule| rule.options.get("max_line_length"))
            .and_then(|value| value.parse().ok())
            .filter(|length| (20..=10000).contains(length))
            .unwrap_or(fallback)
    }
}
/// Only identical fixes selected by the user are repeated; overlapping edits
/// are omitted, including two insertions at the same offset.
pub fn batch_edits(
    problems: &[Problem],
    inspection_id: Option<&str>,
    title: Option<&str>,
    cleanup_only: bool,
) -> Vec<(TextRange, String)> {
    let mut edits: Vec<(TextRange, String)> = Vec::new();
    for problem in problems {
        if inspection_id.is_some_and(|id| !matches_inspection(id, problem.diagnostic.code.as_str()))
        {
            continue;
        }
        if cleanup_only && !inspection(&problem.diagnostic.code).is_some_and(|entry| entry.cleanup)
        {
            continue;
        }
        let fix = match title {
            Some(title) => problem
                .fixes
                .iter()
                .find(|fix| fix.title == title && !fix.is_suppression()),
            None => problem.primary_fix(),
        };
        let Some(QuickFix {
            edits: candidate, ..
        }) = fix
        else {
            continue;
        };
        // A single provider fix must itself be coherent before it joins a batch.
        // Insertion order at the same offset and overlapping replacements would
        // otherwise make the result depend on the downstream editor's sort order.
        if candidate.iter().enumerate().any(|(index, (range, _))| {
            candidate[..index].iter().any(|(prior, _)| {
                prior.start() == range.start()
                    || prior
                        .intersect(*range)
                        .is_some_and(|overlap| !overlap.is_empty())
            })
        }) {
            continue;
        }
        if candidate.iter().any(|(range, _)| {
            edits.iter().any(|(prior, _)| {
                prior.start() == range.start()
                    || prior
                        .intersect(*range)
                        .is_some_and(|overlap| !overlap.is_empty())
            })
        }) {
            continue;
        }
        edits.extend(candidate.iter().cloned());
    }
    edits
}
#[derive(Clone, Debug)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub diagnostic: rustpy_core::Diagnostic,
    pub severity_name: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupBy {
    #[default]
    Directory,
    Inspection,
    Severity,
}
impl Finding {
    pub fn group(&self, group: GroupBy) -> String {
        match group {
            GroupBy::Directory => self
                .path
                .parent()
                .unwrap_or(Path::new(""))
                .display()
                .to_string(),
            GroupBy::Inspection => self.diagnostic.code.to_string(),
            GroupBy::Severity => self
                .severity_name
                .clone()
                .unwrap_or_else(|| self.diagnostic.severity.label().into()),
        }
    }
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn uri_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/:._-~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
pub fn export(findings: &[Finding], format: &str, profile: &str) -> Result<String, String> {
    match format {
        "XML" => {
            let mut out = format!("<inspections profile=\"{}\">\n", xml(profile));
            for finding in findings {
                out.push_str(&format!("<problem file=\"{}\" line=\"{}\" column=\"{}\" inspection=\"{}\" severity=\"{}\">{}</problem>\n",xml(&finding.path.display().to_string()),finding.line+1,finding.column+1,xml(&finding.diagnostic.code),xml(finding.severity_name.as_deref().unwrap_or(finding.diagnostic.severity.label())),xml(&finding.diagnostic.message)));
            }
            out.push_str("</inspections>\n");
            Ok(out)
        }
        "HTML" => {
            let mut out = format!(
                "<!doctype html><html><meta charset=\"utf-8\"><title>Inspection results</title><h1>{}</h1><table><tr><th>File</th><th>Position</th><th>Inspection</th><th>Severity</th><th>Message</th></tr>",
                xml(profile)
            );
            for f in findings {
                out.push_str(&format!(
                    "<tr><td>{}</td><td>{}:{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    xml(&f.path.display().to_string()),
                    f.line + 1,
                    f.column + 1,
                    xml(&f.diagnostic.code),
                    xml(f
                        .severity_name
                        .as_deref()
                        .unwrap_or(f.diagnostic.severity.label())),
                    xml(&f.diagnostic.message)
                ));
            }
            out.push_str("</table></html>");
            Ok(out)
        }
        "SARIF" => {
            let rules: BTreeMap<_, _> = findings
                .iter()
                .map(|finding| {
                    (
                        finding.diagnostic.code.to_string(),
                        inspection(&finding.diagnostic.code)
                            .map(|entry| entry.name)
                            .unwrap_or("Inspection"),
                    )
                })
                .collect();
            let results:Vec<_>=findings.iter().map(|f|serde_json::json!({"ruleId":f.diagnostic.code.to_string(),"level":match f.diagnostic.severity{Severity::Error=>"error",Severity::Warning=>"warning",_=>"note"},"message":{"text":f.diagnostic.message},"properties":{"severity":f.severity_name.as_deref().unwrap_or(f.diagnostic.severity.label())},"locations":[{"physicalLocation":{"artifactLocation":{"uri":uri_path(&f.path)},"region":{"startLine":f.line+1,"startColumn":f.column+1}}}]})).collect();
            serde_json::to_string_pretty(&serde_json::json!({"$schema":"https://json.schemastore.org/sarif-2.1.0.json","version":"2.1.0","runs":[{"tool":{"driver":{"name":"RustPy","rules":rules.into_iter().map(|(id,name)|serde_json::json!({"id":id,"name":name})).collect::<Vec<_>>()}},"results":results}]})).map_err(|error|error.to_string())
        }
        _ => Err("Choose HTML, XML or SARIF".into()),
    }
}

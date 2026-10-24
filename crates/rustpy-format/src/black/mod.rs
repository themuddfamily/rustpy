//! A port of Black 26's stable style (`black/linegen.py`, `lines.py`,
//! `brackets.py`, `nodes.py`, `comments.py`, `strings.py`), working on a
//! blib2to3-shaped tree. `FormatOptions::black()` output is meant to be
//! byte-identical to `black -q -`; see `examples/black_diff.rs` for the
//! differential harness that measures this on real code.

mod brackets;
mod comments;
mod fmtoff;
mod fxhash;
mod linegen;
mod lines;
mod nodes;
mod parser;
mod strings;
mod tree;
mod width_table;

pub use lines::Mode;
pub use parser::ParseError;
pub(crate) use strings::{
    normalize_numeric_literal, normalize_unicode_escape_sequences, py_splitlines, py_strip,
    str_width,
};

use linegen::{Feat, Fmt, LgFeat};
use nodes::is_simple_decorator_expression;
use tree::*;

/// Black's `Feature` flags that target-version detection can observe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
enum Feature {
    FStrings = 1 << 0,
    NumericUnderscores = 1 << 1,
    TrailingCommaInCall = 1 << 2,
    TrailingCommaInDef = 1 << 3,
    AsyncKeywords = 1 << 4,
    FutureAnnotations = 1 << 5,
    AssignmentExpressions = 1 << 6,
    PosOnlyArguments = 1 << 7,
    RelaxedDecorators = 1 << 8,
    PatternMatching = 1 << 9,
    UnpackingOnFlow = 1 << 10,
    AnnAssignExtendedRhs = 1 << 11,
    ExceptStar = 1 << 12,
    VariadicGenerics = 1 << 13,
    DebugFStrings = 1 << 14,
    ParenthesizedContextManagers = 1 << 15,
    TypeParams = 1 << 16,
    TypeParamDefaults = 1 << 17,
    UnparenthesizedExceptTypes = 1 << 18,
    TStrings = 1 << 19,
    LazyImports = 1 << 20,
    UnpackingInComprehensions = 1 << 21,
    AsyncIdentifiers = 1 << 22,
}

/// `VERSION_TO_FEATURES` for Python 3.3 … 3.15.
fn version_features() -> Vec<u32> {
    use Feature::*;
    let py35 = TrailingCommaInCall as u32 | AsyncIdentifiers as u32;
    let py36 = py35 | FStrings as u32 | NumericUnderscores as u32 | TrailingCommaInDef as u32;
    let py37 =
        (py36 & !(AsyncIdentifiers as u32)) | AsyncKeywords as u32 | FutureAnnotations as u32;
    let py38 = py37
        | DebugFStrings as u32
        | AssignmentExpressions as u32
        | PosOnlyArguments as u32
        | UnpackingOnFlow as u32
        | AnnAssignExtendedRhs as u32;
    let py39 = py38 | RelaxedDecorators as u32 | ParenthesizedContextManagers as u32;
    let py310 = py39 | PatternMatching as u32;
    let py311 = py310 | ExceptStar as u32 | VariadicGenerics as u32;
    let py312 = py311 | TypeParams as u32;
    let py313 = py312 | TypeParamDefaults as u32;
    let py314 = py313 | UnparenthesizedExceptTypes as u32 | TStrings as u32;
    let py315 = py314 | LazyImports as u32 | UnpackingInComprehensions as u32;
    vec![
        AsyncIdentifiers as u32,
        AsyncIdentifiers as u32,
        py35,
        py36,
        py37,
        py38,
        py39,
        py310,
        py311,
        py312,
        py313,
        py314,
        py315,
    ]
}

fn get_future_imports(tree: &Tree, root: Id) -> Vec<String> {
    let mut imports = Vec::new();
    for &child in tree.children(root) {
        if tree.ty(child) != SIMPLE_STMT {
            break;
        }
        let first = tree.child(child, 0);
        if tree.is_leaf(first) {
            if tree.nchildren(child) == 2
                && tree.ty(first) == STRING
                && tree.ty(tree.child(child, 1)) == NEWLINE
            {
                continue;
            }
            break;
        } else if tree.ty(first) == IMPORT_FROM {
            let module = tree.child(first, 1);
            if !tree.is_leaf(module) || tree.value(module) != "__future__" {
                break;
            }
            fn names(tree: &Tree, ids: &[Id], out: &mut Vec<String>) {
                for &c in ids {
                    if tree.is_leaf(c) {
                        if tree.ty(c) == NAME {
                            out.push(tree.value(c).to_string());
                        }
                    } else if tree.ty(c) == IMPORT_AS_NAME {
                        out.push(tree.value(tree.child(c, 0)).to_string());
                    } else if tree.ty(c) == IMPORT_AS_NAMES {
                        names(tree, tree.children(c), out);
                    }
                }
            }
            let rest = tree.children(first)[3.min(tree.nchildren(first))..].to_vec();
            names(tree, &rest, &mut imports);
        } else {
            break;
        }
    }
    imports
}

fn contains_asexpr(tree: &Tree, node: Id) -> bool {
    match tree.ty(node) {
        ASEXPR_TEST => true,
        ATOM => {
            let ch = tree.children(node);
            ch.len() == 3
                && tree.ty(ch[0]) == LPAR
                && tree.ty(ch[2]) == RPAR
                && contains_asexpr(tree, ch[1])
        }
        TESTLIST_GEXP => tree
            .children(node)
            .iter()
            .any(|c| contains_asexpr(tree, *c)),
        _ => false,
    }
}

fn get_features_used(tree: &Tree, root: Id, tf: parser::TokFeatures) -> u32 {
    let mut f = 0u32;
    if get_future_imports(tree, root)
        .iter()
        .any(|s| s == "annotations")
    {
        f |= Feature::FutureAnnotations as u32;
    }
    if tf.f_strings {
        f |= Feature::FStrings as u32;
    }
    if tf.t_strings {
        f |= Feature::TStrings as u32;
    }
    if tf.debug_f_strings {
        f |= Feature::DebugFStrings as u32;
    }
    for n in tree.pre_order(root) {
        let t = tree.ty(n);
        let ch = tree.children(n);
        if t == NUMBER {
            if tree.value(n).contains('_') {
                f |= Feature::NumericUnderscores as u32;
            }
        } else if t == SLASH {
            if matches!(
                tree.parent_ty(n),
                Some(TYPEDARGSLIST) | Some(ARGLIST) | Some(VARARGSLIST)
            ) {
                f |= Feature::PosOnlyArguments as u32;
            }
        } else if t == COLONEQUAL {
            f |= Feature::AssignmentExpressions as u32;
        } else if t == DECORATOR {
            if ch.len() > 1 && !is_simple_decorator_expression(tree, ch[1]) {
                f |= Feature::RelaxedDecorators as u32;
            }
        } else if matches!(t, LISTMAKER | TESTLIST_GEXP | DICTSETMAKER)
            && ch
                .iter()
                .any(|c| matches!(tree.ty(*c), COMP_FOR | OLD_COMP_FOR))
            && matches!(tree.ty(ch[0]), STAR_EXPR | DOUBLESTAR)
        {
            f |= Feature::UnpackingInComprehensions as u32;
        } else if matches!(t, TYPEDARGSLIST | ARGLIST)
            && ch.last().map(|c| tree.ty(*c)) == Some(COMMA)
        {
            let feature = if t == TYPEDARGSLIST {
                Feature::TrailingCommaInDef
            } else {
                Feature::TrailingCommaInCall
            };
            for &c in ch {
                if matches!(tree.ty(c), STAR | DOUBLESTAR) {
                    f |= feature as u32;
                }
                if tree.ty(c) == ARGUMENT
                    && tree
                        .children(c)
                        .iter()
                        .any(|a| matches!(tree.ty(*a), STAR | DOUBLESTAR))
                {
                    f |= feature as u32;
                }
            }
        } else if matches!(t, RETURN_STMT | YIELD_EXPR)
            && ch.len() >= 2
            && tree.ty(ch[1]) == TESTLIST_STAR_EXPR
            && tree
                .children(ch[1])
                .iter()
                .any(|c| tree.ty(*c) == STAR_EXPR)
        {
            f |= Feature::UnpackingOnFlow as u32;
        } else if t == ANNASSIGN && ch.len() >= 4 && tree.ty(ch[3]) == TESTLIST_STAR_EXPR {
            f |= Feature::AnnAssignExtendedRhs as u32;
        } else if t == WITH_STMT && ch.len() > 2 && tree.ty(ch[1]) == ATOM {
            let ac = tree.children(ch[1]);
            if ac.len() == 3
                && tree.ty(ac[0]) == LPAR
                && contains_asexpr(tree, ac[1])
                && tree.ty(ac[2]) == RPAR
            {
                f |= Feature::ParenthesizedContextManagers as u32;
            }
        } else if t == MATCH_STMT {
            f |= Feature::PatternMatching as u32;
        } else if (matches!(t, SUBSCRIPTLIST | TRAILER)
            && ch.iter().any(|c| tree.ty(*c) == STAR_EXPR))
            || (t == TNAME_STAR && ch.len() == 3 && tree.ty(ch[2]) == STAR_EXPR)
        {
            f |= Feature::VariadicGenerics as u32;
        } else if matches!(t, TYPE_STMT | TYPEPARAMS) {
            f |= Feature::TypeParams as u32;
        } else if matches!(t, TYPEVARTUPLE | PARAMSPEC | TYPEVAR)
            && ch.len() >= 2
            && tree.ty(ch[ch.len() - 2]) == EQUAL
        {
            f |= Feature::TypeParamDefaults as u32;
        } else if t == EXCEPT_CLAUSE
            && ch.len() >= 2
            && (tree.ty(ch[1]) == STAR || tree.ty(ch[1]) == TESTLIST)
        {
            let is_star = tree.ty(ch[1]) == STAR;
            if is_star {
                f |= Feature::ExceptStar as u32;
            }
            let k = is_star as usize;
            let has_as =
                ch.len() >= k + 3 && tree.ty(ch[k + 2]) == NAME && tree.value(ch[k + 2]) == "as";
            if !has_as
                && ((is_star && ch.len() > 2 && tree.ty(ch[2]) == TESTLIST)
                    || (!is_star && tree.ty(ch[1]) == TESTLIST))
            {
                f |= Feature::UnparenthesizedExceptTypes as u32;
            }
        }
    }
    f
}

/// Whether every target version compatible with `features` supports `feature`.
fn supports(features: u32, feature: Feature) -> bool {
    let table = version_features();
    let versions: Vec<u32> = table.into_iter().filter(|v| features & !v == 0).collect();
    !versions.is_empty() && versions.iter().all(|v| v & feature as u32 != 0)
}

fn is_py_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1f' | '\u{85}'
    ) || (c as u32 > 127 && c.is_whitespace())
}

fn format_once(src: &str, mode: &Mode) -> Result<String, ParseError> {
    let mut text = src.trim_start_matches(is_py_space).to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let (mut tree, root, tf) = parser::parse(&text)?;
    let features = get_features_used(&tree, root, tf);
    let lg = LgFeat {
        parenthesized_context_managers: supports(features, Feature::ParenthesizedContextManagers),
        unparenthesized_except_types: supports(features, Feature::UnparenthesizedExceptTypes),
    };
    let split = Feat {
        trailing_comma_in_call: supports(features, Feature::TrailingCommaInCall),
        trailing_comma_in_def: supports(features, Feature::TrailingCommaInDef),
        force_optional_parentheses: false,
    };
    // After target-version detection, like Black.
    fmtoff::normalize_fmt_off(&mut tree, root);
    let mut fmt = Fmt::new(tree, *mode, lg, split);
    fmt.visit(root);
    if fmt.blocks.is_empty() {
        return Ok(if src.contains('\n') {
            "\n".into()
        } else {
            String::new()
        });
    }
    if let Some(last) = fmt.blocks.last_mut() {
        last.after = 0;
    }
    let mut out = String::with_capacity(src.len() + src.len() / 8);
    for b in &fmt.blocks {
        b.all_lines(&mut out);
    }
    Ok(out)
}

/// Black's `assert_stable`: a single further formatting pass (not two, which
/// could hide output bouncing between two versions) leaves `dst` unchanged.
pub fn is_stable(dst: &str, mode: &Mode) -> bool {
    format_once(dst, mode).is_ok_and(|again| again == dst)
}

/// Formats a module like `black.format_str` (two passes when the first one
/// changed something). `src` must use `\n` line endings.
pub fn format_str(src: &str, mode: &Mode) -> Result<String, ParseError> {
    let dst = format_once(src, mode)?;
    if dst != src {
        return format_once(&dst, mode);
    }
    Ok(dst)
}

/// The same canonicalization used by the port's comment renderer.
pub(crate) fn normalized_comment(comment: &str) -> String {
    comments::make_comment(comment)
}

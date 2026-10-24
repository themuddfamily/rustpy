//! Semantic tokens from the IDE's Python highlighter (`rustpy-highlight`),
//! refined with the file's scope model, so semantic highlighting colors parameters, locals, classes and functions at their
//! uses as well as at their definitions.

use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use rustpy_core::{TextRange, TextSize};
use rustpy_highlight::HighlightTag;
use rustpy_semantic::scope::{BindingKind, ScopeKind};
use rustpy_semantic::FileInfo;

use crate::line_index::{Encoding, LineIndex};

/// Token types, in legend order.
const TYPES: &[&str] = &[
    "namespace",
    "class",
    "function",
    "method",
    "parameter",
    "variable",
    "property",
    "decorator",
    "keyword",
    "comment",
    "string",
    "number",
    "operator",
    "selfParameter",
    "typeParameter",
];
/// Token modifiers, in legend order (bit `i` is `MODIFIERS[i]`).
const MODIFIERS: &[&str] = &[
    "declaration",
    "definition",
    "readonly",
    "documentation",
    "defaultLibrary",
    "async",
    "static",
];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TokType {
    Namespace = 0,
    Class,
    Function,
    Method,
    Parameter,
    Variable,
    Property,
    Decorator,
    Keyword,
    Comment,
    String,
    Number,
    Operator,
    SelfParameter,
    TypeParameter,
}

pub const DECLARATION: u32 = 1 << 0;
pub const DEFINITION: u32 = 1 << 1;
pub const DOCUMENTATION: u32 = 1 << 3;
pub const DEFAULT_LIBRARY: u32 = 1 << 4;

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TYPES.iter().map(|t| SemanticTokenType::new(t)).collect(),
        token_modifiers: MODIFIERS
            .iter()
            .map(|m| SemanticTokenModifier::new(m))
            .collect(),
    }
}

/// A classified range of the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Tok {
    pub range: TextRange,
    pub ty: TokType,
    pub mods: u32,
}

/// Classifies the file: highlighter spans first, then names the scope model
/// resolves (parameters, locals, classes, functions, imported modules).
pub fn classify(file: &FileInfo, builtin_classes: &dyn Fn(&str) -> bool) -> Vec<Tok> {
    let text: &str = &file.text;
    let spans = rustpy_highlight::highlight_python(text);
    let mut out: Vec<Tok> = Vec::with_capacity(spans.len());
    let followed_by_paren = |end: TextSize| {
        text[usize::from(end).min(text.len())..]
            .trim_start_matches([' ', '\t'])
            .starts_with('(')
    };
    let after_dot = |start: TextSize| {
        text[..usize::from(start).min(text.len())]
            .trim_end_matches([' ', '\t'])
            .ends_with('.')
    };
    for s in &spans {
        let word = &text[s.range];
        let tok = match s.tag {
            HighlightTag::Keyword | HighlightTag::SoftKeyword => Some((TokType::Keyword, 0)),
            HighlightTag::Comment | HighlightTag::TodoComment => Some((TokType::Comment, 0)),
            HighlightTag::String | HighlightTag::Bytes | HighlightTag::FString => {
                Some((TokType::String, 0))
            }
            HighlightTag::Docstring => Some((TokType::String, DOCUMENTATION)),
            HighlightTag::Number => Some((TokType::Number, 0)),
            HighlightTag::Operator => Some((TokType::Operator, 0)),
            HighlightTag::Decorator if word != "." => Some((TokType::Decorator, 0)),
            HighlightTag::SelfParameter => Some((TokType::SelfParameter, 0)),
            HighlightTag::KeywordArgument | HighlightTag::Parameter => {
                Some((TokType::Parameter, 0))
            }
            HighlightTag::FunctionDefinition => Some((TokType::Function, DECLARATION | DEFINITION)),
            HighlightTag::ClassDefinition => Some((TokType::Class, DECLARATION | DEFINITION)),
            HighlightTag::FunctionCall => Some((
                if after_dot(s.range.start()) {
                    TokType::Method
                } else {
                    TokType::Function
                },
                0,
            )),
            HighlightTag::Builtin => Some((
                if builtin_classes(word) {
                    TokType::Class
                } else {
                    TokType::Function
                },
                DEFAULT_LIBRARY,
            )),
            HighlightTag::Dunder => Some((
                if followed_by_paren(s.range.end()) {
                    TokType::Method
                } else {
                    TokType::Variable
                },
                DEFAULT_LIBRARY,
            )),
            HighlightTag::TypeAnnotation => Some((TokType::Class, 0)),
            HighlightTag::LocalVariable => Some((TokType::Variable, 0)),
            _ => None,
        };
        if let Some((ty, mods)) = tok {
            out.push(Tok {
                range: s.range,
                ty,
                mods,
            });
        }
    }
    refine_with_scopes(file, &mut out);
    out.sort_by_key(|t| t.range.start());
    out.dedup_by_key(|t| t.range.start());
    out
}

fn refine_with_scopes(file: &FileInfo, out: &mut Vec<Tok>) {
    let scopes = &file.scopes;
    let mut by_start: rustc_hash::FxHashMap<TextSize, usize> = out
        .iter()
        .enumerate()
        .map(|(i, t)| (t.range.start(), i))
        .collect();
    let mut set = |range: TextRange, ty: TokType, mods: u32, out: &mut Vec<Tok>| match by_start
        .get(&range.start())
    {
        Some(&i) => {
            let t = &mut out[i];
            // The highlighter knows keywords, `self` and decorators best. A
            // builtin's name bound in the file (a parameter `input`) is that binding.
            if matches!(
                t.ty,
                TokType::Keyword
                    | TokType::SelfParameter
                    | TokType::Decorator
                    | TokType::String
                    | TokType::Comment
            ) {
                return;
            }
            t.ty = ty;
            t.mods = (t.mods & !DEFAULT_LIBRARY) | mods;
        }
        None => {
            by_start.insert(range.start(), out.len());
            out.push(Tok { range, ty, mods });
        }
    };
    let kind_of = |b: &rustpy_semantic::scope::Binding| -> Option<(TokType, u32)> {
        let scope = scopes.scope(b.scope);
        Some(match &b.kind {
            BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam => {
                if matches!(b.name.as_str(), "self" | "cls") {
                    (TokType::SelfParameter, 0)
                } else {
                    (TokType::Parameter, 0)
                }
            }
            BindingKind::Class => (TokType::Class, 0),
            BindingKind::Function => (
                if scope.kind == ScopeKind::Class {
                    TokType::Method
                } else {
                    TokType::Function
                },
                0,
            ),
            BindingKind::Import(info) if info.name.is_none() => (TokType::Namespace, 0),
            BindingKind::TypeParam => (TokType::TypeParameter, 0),
            BindingKind::Import(_) => return None,
            BindingKind::Assignment
            | BindingKind::AugAssignment
            | BindingKind::Annotation
            | BindingKind::For
            | BindingKind::With
            | BindingKind::Except
            | BindingKind::Walrus
            | BindingKind::MatchCapture
            | BindingKind::CompTarget
            | BindingKind::TypeAlias => {
                if scope.kind == ScopeKind::Class {
                    (TokType::Property, 0)
                } else {
                    (TokType::Variable, 0)
                }
            }
            BindingKind::Global | BindingKind::Nonlocal | BindingKind::Del => return None,
        })
    };
    for b in &scopes.bindings {
        if b.range.is_empty() || !b.is_definition() {
            continue;
        }
        // The name of a `def` / `class` statement: the highlighter tagged it already.
        if matches!(b.kind, BindingKind::Function | BindingKind::Class) {
            if let Some((ty, _)) = kind_of(b) {
                set(b.range, ty, DECLARATION | DEFINITION, out);
            }
            continue;
        }
        if let Some((ty, mods)) = kind_of(b) {
            let decl = if matches!(
                b.kind,
                BindingKind::Param
                    | BindingKind::StarParam
                    | BindingKind::StarStarParam
                    | BindingKind::TypeParam
            ) {
                DECLARATION
            } else {
                0
            };
            set(b.range, ty, mods | decl, out);
        }
    }
    for r in &scopes.references {
        let Some(id) = r.resolved else { continue };
        let b = scopes.binding(id);
        if let Some((ty, mods)) = kind_of(b) {
            set(r.range, ty, mods, out);
        }
    }
}

/// Encodes tokens relative to each other, splitting multi-line tokens into
/// one token per line (clients need not support multi-line tokens).
pub fn encode(toks: &[Tok], index: &LineIndex, enc: Encoding) -> Vec<SemanticToken> {
    let mut data: Vec<SemanticToken> = Vec::with_capacity(toks.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    let text = index.text();
    for t in toks {
        let first = index.line_of(t.range.start());
        let last = index.line_of(t.range.end());
        for line in first..=last {
            let Some(lr) = index.line_range(line) else {
                break;
            };
            let start = t.range.start().max(lr.start());
            let end = t.range.end().min(lr.end());
            if start >= end {
                continue;
            }
            let pos = index.position(start, enc);
            let len = enc.len(&text[usize::from(start)..usize::from(end)]);
            if len == 0 {
                continue;
            }
            let delta_line = pos.line - prev_line;
            let delta_start = if delta_line == 0 {
                pos.character - prev_start
            } else {
                pos.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: len,
                token_type: t.ty as u32,
                token_modifiers_bitset: t.mods,
            });
            prev_line = pos.line;
            prev_start = pos.character;
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn toks(src: &str) -> Vec<(String, TokType, u32)> {
        let db = rustpy_semantic::Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file_for_path(Path::new("/proj/m.py")).unwrap();
        classify(&f, &|n| n == "int")
            .into_iter()
            .map(|t| (src[t.range].to_string(), t.ty, t.mods))
            .collect()
    }

    #[test]
    fn highlighter_and_scopes_combine() {
        let src = "import os\n\n@dec\nclass C:\n    \"\"\"Doc.\"\"\"\n    def m(self, x: int) -> int:\n        y = x + 1\n        return len(os.sep) + y\n";
        let t = toks(src);
        let has = |w: &str, ty: TokType| t.iter().any(|(s, k, _)| s == w && *k == ty);
        assert!(has("import", TokType::Keyword));
        assert!(has("os", TokType::Namespace));
        assert!(has("dec", TokType::Decorator));
        assert!(has("C", TokType::Class));
        assert!(has("\"\"\"Doc.\"\"\"", TokType::String));
        assert!(has("m", TokType::Method));
        assert!(has("self", TokType::SelfParameter));
        assert!(has("x", TokType::Parameter));
        assert!(has("int", TokType::Class));
        assert!(has("y", TokType::Variable));
        assert!(has("len", TokType::Function));
        assert!(has("1", TokType::Number));
        let x_decl = t
            .iter()
            .find(|(s, k, _)| s == "x" && *k == TokType::Parameter)
            .unwrap();
        assert_eq!(x_decl.2 & DECLARATION, DECLARATION);
        let doc = t.iter().find(|(s, _, _)| s.starts_with("\"\"\"")).unwrap();
        assert_eq!(doc.2 & DOCUMENTATION, DOCUMENTATION);
    }

    #[test]
    fn multi_line_tokens_split_per_line() {
        let src = "x = '''a\nbc'''\n";
        let db = rustpy_semantic::Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file_for_path(Path::new("/proj/m.py")).unwrap();
        let index = LineIndex::new(src);
        let data = encode(&classify(&f, &|_| false), &index, Encoding::Utf16);
        // x, =, the string's first line, the string's second line.
        let strings: Vec<&SemanticToken> = data
            .iter()
            .filter(|d| d.token_type == TokType::String as u32)
            .collect();
        assert_eq!(strings.len(), 2);
        assert_eq!(
            (
                strings[1].delta_line,
                strings[1].delta_start,
                strings[1].length
            ),
            (1, 0, 5)
        );
    }
}

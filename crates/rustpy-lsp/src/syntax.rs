//! Syntax-tree features: fold regions, Extend Selection steps and the
//! structure outline. These follow the editor's implementation
//! (`rustpy-ui/src/editor/syntax_support.rs`) so folding, Ctrl+W and the
//! Structure view behave the same in other editors.

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{Parse, SyntaxKind, SyntaxNode};

/// A foldable region: lines `first_line+1 ..= last_line` are hidden when folded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldRegion {
    pub range: TextRange,
    pub first_line: usize,
    pub last_line: usize,
    pub placeholder: String,
    pub kind: FoldKind,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FoldKind {
    Function,
    Class,
    Block,
    Imports,
    Docstring,
    Brackets,
    Region,
}

/// Computes fold regions for a parsed Python document.
pub fn fold_regions(parse: &Parse, line_of: &dyn Fn(TextSize) -> usize) -> Vec<FoldRegion> {
    let root = parse.syntax_node();
    let mut out = Vec::new();
    // Consecutive import statements at module level.
    if let Some(module) = ast::Module::cast(root.clone()) {
        let mut run: Vec<TextRange> = Vec::new();
        let flush = |run: &mut Vec<TextRange>, out: &mut Vec<FoldRegion>| {
            if run.len() >= 2 {
                let range = TextRange::new(run[0].start(), run[run.len() - 1].end());
                let (a, b) = (line_of(range.start()), line_of(range.end()));
                if b > a {
                    out.push(FoldRegion {
                        range,
                        first_line: a,
                        last_line: b,
                        placeholder: "import ...".into(),
                        kind: FoldKind::Imports,
                    });
                }
            }
            run.clear();
        };
        for stmt in module.statements() {
            match stmt {
                ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => run.push(TextRange::new(
                    trimmed_start(stmt.syntax()),
                    trimmed_end(stmt.syntax()),
                )),
                _ => flush(&mut run, &mut out),
            }
        }
        flush(&mut run, &mut out);
    }
    for node in root.descendants() {
        let kind = node.kind();
        let colon_to_body = |node: &SyntaxNode| {
            let colon = node
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::COLON)?;
            let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK)?;
            Some(TextRange::new(
                colon.text_range().end(),
                trimmed_end(&body).max(colon.text_range().end()),
            ))
        };
        let region = match kind {
            SyntaxKind::FUNC_DEF => {
                colon_to_body(&node).map(|r| (r, "...".to_string(), FoldKind::Function))
            }
            SyntaxKind::CLASS_DEF => {
                colon_to_body(&node).map(|r| (r, "...".to_string(), FoldKind::Class))
            }
            SyntaxKind::IF_STMT
            | SyntaxKind::FOR_STMT
            | SyntaxKind::WHILE_STMT
            | SyntaxKind::WITH_STMT
            | SyntaxKind::TRY_STMT
            | SyntaxKind::MATCH_STMT
            | SyntaxKind::ELIF_CLAUSE
            | SyntaxKind::ELSE_CLAUSE
            | SyntaxKind::EXCEPT_CLAUSE
            | SyntaxKind::FINALLY_CLAUSE
            | SyntaxKind::CASE_CLAUSE => {
                colon_to_body(&node).map(|r| (r, "...".to_string(), FoldKind::Block))
            }
            SyntaxKind::STRING_EXPR => {
                let text = node.text().to_string();
                let is_doc = node
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::EXPR_STMT)
                    && (text.contains("\"\"\"") || text.contains("'''"));
                is_doc.then(|| {
                    let first_line = text.lines().next().unwrap_or("").trim().to_string();
                    let placeholder = if first_line.len() > 3 {
                        format!("{}...\"\"\"", first_line.trim_end_matches(['"', '\'']))
                    } else {
                        "\"\"\"...\"\"\"".to_string()
                    };
                    (node.text_range(), placeholder, FoldKind::Docstring)
                })
            }
            SyntaxKind::ARG_LIST
            | SyntaxKind::LIST_EXPR
            | SyntaxKind::DICT_EXPR
            | SyntaxKind::SET_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::PARAM_LIST
            | SyntaxKind::LIST_COMP
            | SyntaxKind::DICT_COMP
            | SyntaxKind::SET_COMP
            | SyntaxKind::GENERATOR_EXPR => {
                let open = node.first_token().filter(|t| {
                    matches!(
                        t.kind(),
                        SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE
                    )
                });
                let close = node.last_token().filter(|t| {
                    matches!(
                        t.kind(),
                        SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE
                    )
                });
                match (open, close) {
                    (Some(o), Some(c)) if o.text_range().end() <= c.text_range().start() => Some((
                        TextRange::new(o.text_range().end(), c.text_range().start()),
                        "...".to_string(),
                        FoldKind::Brackets,
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((range, placeholder, kind)) = region {
            let (a, b) = (line_of(range.start()), line_of(range.end()));
            if b > a {
                out.push(FoldRegion {
                    range,
                    first_line: a,
                    last_line: b,
                    placeholder,
                    kind,
                });
            }
        }
    }
    // `# region` / `# endregion` and `# <editor-fold>` comments.
    let mut stack: Vec<(usize, TextSize, String)> = Vec::new();
    for tok in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMENT)
    {
        let text = tok.text().trim_start_matches('#').trim();
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("region") || lower.starts_with("<editor-fold") {
            let desc = text
                .split_once(' ')
                .map(|(_, d)| d)
                .unwrap_or("")
                .trim()
                .trim_start_matches("desc=")
                .trim_matches('"')
                .trim_end_matches('>')
                .to_string();
            stack.push((
                line_of(tok.text_range().start()),
                tok.text_range().end(),
                desc,
            ));
        } else if lower.starts_with("endregion") || lower.starts_with("</editor-fold") {
            if let Some((first_line, start, desc)) = stack.pop() {
                let end = tok.text_range().end();
                let last_line = line_of(end);
                if last_line > first_line {
                    out.push(FoldRegion {
                        range: TextRange::new(start, end),
                        first_line,
                        last_line,
                        placeholder: if desc.is_empty() {
                            "region...".into()
                        } else {
                            desc
                        },
                        kind: FoldKind::Region,
                    });
                }
            }
        }
    }
    out.sort_by_key(|r| (r.first_line, std::cmp::Reverse(r.last_line)));
    out.dedup_by_key(|r| (r.first_line, r.last_line));
    out
}

fn is_layout(kind: SyntaxKind) -> bool {
    kind.is_trivia()
        || matches!(
            kind,
            SyntaxKind::NEWLINE | SyntaxKind::DEDENT | SyntaxKind::INDENT
        )
}

/// Start of a node ignoring leading newline/indent/trivia tokens.
pub fn trimmed_start(node: &SyntaxNode) -> TextSize {
    let mut start = node.text_range().start();
    let mut tok = node.first_token();
    while let Some(t) =
        tok.filter(|t| is_layout(t.kind()) && node.text_range().contains_range(t.text_range()))
    {
        start = t.text_range().end();
        tok = t.next_token();
    }
    start.min(node.text_range().end())
}

/// End of a node ignoring trailing whitespace/newline tokens.
pub fn trimmed_end(node: &SyntaxNode) -> TextSize {
    let mut end = node.text_range().end();
    let mut tok = node.last_token();
    while let Some(t) =
        tok.filter(|t| is_layout(t.kind()) && node.text_range().contains_range(t.text_range()))
    {
        end = t.text_range().start();
        tok = t.prev_token();
    }
    end.max(node.text_range().start())
}

/// The next *Extend Selection* (Ctrl+W) step around `range`: the smallest
/// syntax range strictly containing it (the inside of brackets counts).
pub fn extend_selection(parse: &Parse, range: TextRange) -> Option<TextRange> {
    let root = parse.syntax_node();
    let tok = root.token_at_offset(range.start()).right_biased()?;
    let mut candidates: Vec<TextRange> = Vec::new();
    for anc in tok.parent_ancestors() {
        // Also offer the "inside the brackets" range for bracketed nodes.
        if let (Some(f), Some(l)) = (anc.first_token(), anc.last_token()) {
            if matches!(
                f.kind(),
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE
            ) && matches!(
                l.kind(),
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE
            ) && f.text_range().end() <= l.text_range().start()
            {
                candidates.push(TextRange::new(f.text_range().end(), l.text_range().start()));
            }
        }
        if matches!(anc.kind(), SyntaxKind::BLOCK | SyntaxKind::MODULE) {
            let (s, e) = (trimmed_start(&anc), trimmed_end(&anc));
            if s < e {
                candidates.push(TextRange::new(s, e));
            }
        } else {
            candidates.push(TextRange::new(anc.text_range().start(), trimmed_end(&anc)));
        }
    }
    candidates.sort_by_key(|r| (r.len(), r.start()));
    candidates
        .into_iter()
        .find(|c| c.contains_range(range) && *c != range && c.len() > range.len())
}

/// The chain of selection ranges at `offset`, innermost first: the word or
/// token under the caret, then every Extend Selection step.
pub fn selection_chain(parse: &Parse, offset: TextSize) -> Vec<TextRange> {
    let root = parse.syntax_node();
    let at = root.token_at_offset(offset);
    let word = at
        .clone()
        .right_biased()
        .filter(|t| !is_layout(t.kind()))
        .or_else(|| at.left_biased().filter(|t| !is_layout(t.kind())));
    let mut out: Vec<TextRange> = Vec::new();
    let mut cur = match word {
        Some(t) => t.text_range(),
        None => TextRange::empty(offset),
    };
    if !cur.is_empty() {
        out.push(cur);
    }
    while let Some(next) = extend_selection(parse, cur) {
        if out.last() == Some(&next) {
            break;
        }
        out.push(next);
        cur = next;
        if out.len() > 256 {
            break;
        }
    }
    if out.is_empty() {
        out.push(TextRange::empty(offset));
    }
    out
}

/// Breadcrumb path (outermost first) of definitions enclosing `offset`:
/// `("ClassName", range)` and `("method()", range)` entries.
pub fn breadcrumbs(parse: &Parse, offset: TextSize) -> Vec<(String, TextRange)> {
    let root = parse.syntax_node();
    let at = root.token_at_offset(offset);
    let tok = match (at.clone().left_biased(), at.right_biased()) {
        (Some(l), Some(r))
            if l != r
                && !r.kind().is_trivia()
                && !matches!(r.kind(), SyntaxKind::NEWLINE | SyntaxKind::DEDENT) =>
        {
            Some(r)
        }
        (l, r) => l.or(r),
    };
    let Some(tok) = tok else { return Vec::new() };
    let mut crumbs: Vec<(String, TextRange)> = tok
        .parent_ancestors()
        .filter_map(|n| match n.kind() {
            SyntaxKind::FUNC_DEF => ast::FuncDef::cast(n.clone())
                .map(|f| (format!("{}()", f.name_text()), n.text_range())),
            SyntaxKind::CLASS_DEF => {
                ast::ClassDef::cast(n.clone()).map(|c| (c.name_text(), n.text_range()))
            }
            _ => None,
        })
        .collect();
    crumbs.reverse();
    crumbs
}

/// An entry of the structure outline (Structure tool window / File Structure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub name: String,
    pub detail: String,
    pub kind: OutlineKind,
    /// The whole definition (decorators included), without trailing blank lines.
    pub range: TextRange,
    pub name_range: TextRange,
    pub children: Vec<OutlineItem>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutlineKind {
    Class,
    Function,
    Method,
    Constructor,
    Property,
    /// A module-level variable.
    Variable,
    /// An ALL_CAPS module-level name.
    Constant,
    /// A variable in a class body.
    Field,
}

/// The structure outline: classes with their members, functions with nested
/// definitions, module-level and class-level variables. Definitions inside
/// module-level `if` blocks (`if TYPE_CHECKING:`, `if __name__ == ...`) are
/// listed at module level, as in the Structure view.
pub fn outline(parse: &Parse) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    if let Some(module) = ast::Module::cast(parse.syntax_node()) {
        for stmt in module.statements() {
            collect(&stmt, Scope::Module, &mut out);
        }
    }
    out
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Scope {
    Module,
    Class,
    Function,
}

fn collect(stmt: &ast::Stmt, scope: Scope, out: &mut Vec<OutlineItem>) {
    let range = |n: &SyntaxNode| TextRange::new(trimmed_start(n), trimmed_end(n));
    match stmt {
        ast::Stmt::ClassDef(c) => {
            let Some(name) = c.name() else { return };
            let bases: Vec<String> = c
                .bases()
                .iter()
                .map(|b| b.syntax().text().to_string())
                .collect();
            let detail = if bases.is_empty() {
                String::new()
            } else {
                format!("({})", bases.join(", "))
            };
            let mut item = OutlineItem {
                name: name.text().to_string(),
                detail,
                kind: OutlineKind::Class,
                range: range(c.syntax()),
                name_range: name.text_range(),
                children: Vec::new(),
            };
            if let Some(body) = c.body() {
                for s in body.statements() {
                    collect(&s, Scope::Class, &mut item.children);
                }
            }
            out.push(item);
        }
        ast::Stmt::FuncDef(f) => {
            let Some(name) = f.name() else { return };
            let params = f
                .params()
                .map(|p| p.syntax().text().to_string())
                .unwrap_or_else(|| "()".into());
            let detail = format!(
                "{}{}",
                params.split_whitespace().collect::<Vec<_>>().join(" "),
                f.return_annotation()
                    .map(|r| format!(" -> {}", r.syntax().text()))
                    .unwrap_or_default()
            );
            let kind = if scope != Scope::Class {
                OutlineKind::Function
            } else if f.name_text() == "__init__" {
                OutlineKind::Constructor
            } else if f.has_decorator("property")
                || f.decorators().any(|d| {
                    d.name_text().is_some_and(|n| {
                        n.ends_with(".setter")
                            || n.ends_with(".getter")
                            || n.ends_with(".deleter")
                            || n.ends_with("cached_property")
                    })
                })
            {
                OutlineKind::Property
            } else {
                OutlineKind::Method
            };
            let mut item = OutlineItem {
                name: name.text().to_string(),
                detail,
                kind,
                range: range(f.syntax()),
                name_range: name.text_range(),
                children: Vec::new(),
            };
            if let Some(body) = f.body() {
                for s in body.statements() {
                    if matches!(s, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)) {
                        collect(&s, Scope::Function, &mut item.children);
                    }
                }
            }
            out.push(item);
        }
        ast::Stmt::Assign(a) if scope != Scope::Function => {
            for t in a.targets() {
                let names: Vec<ast::NameExpr> = match t {
                    ast::Expr::Name(n) => vec![n],
                    ast::Expr::Tuple(tu) => tu
                        .elements()
                        .filter_map(|e| match e {
                            ast::Expr::Name(n) => Some(n),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for n in names {
                    if out.iter().any(|o| {
                        o.name == n.name_text()
                            && matches!(
                                o.kind,
                                OutlineKind::Variable | OutlineKind::Constant | OutlineKind::Field
                            )
                    }) {
                        continue;
                    }
                    let kind = variable_kind(&n.name_text(), scope);
                    out.push(OutlineItem {
                        name: n.name_text(),
                        detail: String::new(),
                        kind,
                        range: range(a.syntax()),
                        name_range: n.syntax().text_range(),
                        children: Vec::new(),
                    });
                }
            }
        }
        ast::Stmt::AnnAssign(a) if scope != Scope::Function => {
            if let Some(ast::Expr::Name(n)) = a.target() {
                if out.iter().any(|o| {
                    o.name == n.name_text()
                        && matches!(
                            o.kind,
                            OutlineKind::Variable | OutlineKind::Constant | OutlineKind::Field
                        )
                }) {
                    return;
                }
                let detail = a
                    .annotation()
                    .map(|t| format!(": {}", t.syntax().text()))
                    .unwrap_or_default();
                out.push(OutlineItem {
                    name: n.name_text(),
                    detail,
                    kind: variable_kind(&n.name_text(), scope),
                    range: range(a.syntax()),
                    name_range: n.syntax().text_range(),
                    children: Vec::new(),
                });
            }
        }
        ast::Stmt::If(i) if scope == Scope::Module => {
            let mut blocks: Vec<ast::Block> = i.body().into_iter().collect();
            blocks.extend(i.elif_clauses().filter_map(|c| c.body()));
            blocks.extend(i.else_clause().and_then(|c| c.body()));
            for b in blocks {
                for s in b.statements() {
                    if matches!(s, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)) {
                        collect(&s, scope, out);
                    }
                }
            }
        }
        ast::Stmt::Try(t) if scope == Scope::Module => {
            // `try: from x import y` / `except ImportError: def y(): ...` fallbacks.
            let mut blocks: Vec<ast::Block> = t.body().into_iter().collect();
            blocks.extend(t.except_clauses().filter_map(|c| c.body()));
            for b in blocks {
                for s in b.statements() {
                    if matches!(s, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)) {
                        collect(&s, scope, out);
                    }
                }
            }
        }
        _ => {}
    }
}

fn variable_kind(name: &str, scope: Scope) -> OutlineKind {
    if scope == Scope::Class {
        OutlineKind::Field
    } else if name.len() > 1
        && name
            .chars()
            .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
        && name.chars().any(|c| c.is_alphabetic())
    {
        OutlineKind::Constant
    } else {
        OutlineKind::Variable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_syntax::parse;

    fn lines(src: &str) -> impl Fn(TextSize) -> usize + '_ {
        move |off: TextSize| src[..usize::from(off)].matches('\n').count()
    }

    #[test]
    fn folds_match_the_editor() {
        let src = "import a\nimport b\n\nclass C:\n    \"\"\"Doc\n    more\"\"\"\n    def m(self):\n        return [\n            1,\n        ]\n\nx = 1\n# region helpers\ndef f():\n    pass\n# endregion\n";
        let p = parse(src);
        let line_of = lines(src);
        let folds = fold_regions(&p, &line_of);
        let kinds: Vec<(FoldKind, usize, usize)> = folds
            .iter()
            .map(|f| (f.kind, f.first_line, f.last_line))
            .collect();
        assert!(kinds.contains(&(FoldKind::Imports, 0, 1)), "{kinds:?}");
        assert!(kinds.contains(&(FoldKind::Class, 3, 9)), "{kinds:?}");
        assert!(kinds.contains(&(FoldKind::Docstring, 4, 5)), "{kinds:?}");
        assert!(kinds.contains(&(FoldKind::Function, 6, 9)), "{kinds:?}");
        assert!(kinds.contains(&(FoldKind::Brackets, 7, 9)), "{kinds:?}");
        assert!(kinds.contains(&(FoldKind::Region, 12, 15)), "{kinds:?}");
    }

    #[test]
    fn selection_chain_grows_like_extend_selection() {
        let src = "def f(a):\n    return foo(a + 1)\n";
        let p = parse(src);
        let at = TextSize::new(src.find("a + 1").unwrap() as u32);
        let chain: Vec<&str> = selection_chain(&p, at)
            .into_iter()
            .map(|r| &src[r])
            .collect();
        assert_eq!(
            chain[..5],
            ["a", "a + 1", "(a + 1)", "foo(a + 1)", "return foo(a + 1)"]
        );
        assert_eq!(*chain.last().unwrap(), "def f(a):\n    return foo(a + 1)");
    }

    #[test]
    fn outline_nests_members() {
        let src = "XS = 1\nclass C(Base):\n    y: int = 2\n    def __init__(self):\n        def inner():\n            pass\n    @property\n    def p(self):\n        return 1\nif TYPE_CHECKING:\n    def g(): ...\ndef f(a, b) -> int:\n    z = 1\n";
        let items = outline(&parse(src));
        let top: Vec<(&str, OutlineKind)> =
            items.iter().map(|i| (i.name.as_str(), i.kind)).collect();
        assert_eq!(
            top,
            vec![
                ("XS", OutlineKind::Constant),
                ("C", OutlineKind::Class),
                ("g", OutlineKind::Function),
                ("f", OutlineKind::Function)
            ]
        );
        let c = &items[1];
        assert_eq!(c.detail, "(Base)");
        let members: Vec<(&str, OutlineKind)> = c
            .children
            .iter()
            .map(|i| (i.name.as_str(), i.kind))
            .collect();
        assert_eq!(
            members,
            vec![
                ("y", OutlineKind::Field),
                ("__init__", OutlineKind::Constructor),
                ("p", OutlineKind::Property)
            ]
        );
        assert_eq!(c.children[1].children[0].name, "inner");
        assert_eq!(items[3].detail, "(a, b) -> int");
        assert!(items[3].children.is_empty(), "locals are not listed");
        assert_eq!(&src[c.name_range], "C");
        assert!(src[c.range].starts_with("class C(Base):") && src[c.range].ends_with("return 1"));
    }
}

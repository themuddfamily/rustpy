//! Syntax-tree powered editor features: fold regions, structure outline,
//! syntax-aware selection, method navigation, error diagnostics
//! (roadmap §1.1 folding, §2.7 structure/breadcrumbs, §1.2 extend selection).

use rustpy_core::{Diagnostic, TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::rowan::TokenAtOffset;
use rustpy_syntax::{Parse, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// `root.token_at_offset(offset)` that finds nothing instead of panicking
/// when `offset` lies outside the tree: large files and non-Python files have
/// an empty tree, and a tree can briefly be shorter than the document.
pub fn token_at(root: &SyntaxNode, offset: TextSize) -> TokenAtOffset<SyntaxToken> {
    if root.text_range().contains_inclusive(offset) {
        root.token_at_offset(offset)
    } else {
        TokenAtOffset::None
    }
}

/// `root.covering_element(range)` for a range that may lie outside the tree.
pub fn covering(root: &SyntaxNode, range: TextRange) -> Option<SyntaxElement> {
    root.text_range()
        .contains_range(range)
        .then(|| root.covering_element(range))
}

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

/// An entry of the structure view / file structure popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureItem {
    pub name: String,
    pub detail: String,
    pub kind: StructureKind,
    pub range: TextRange,
    pub name_range: TextRange,
    pub depth: usize,
    pub line: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StructureKind {
    Class,
    Function,
    Method,
    Variable,
    Import,
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
                let range = TextRange::new(run[0].start(), run.last().unwrap().end());
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
                ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => run.push(stmt.text_range()),
                _ => flush(&mut run, &mut out),
            }
        }
        flush(&mut run, &mut out);
    }
    for node in root.descendants() {
        let kind = node.kind();
        let region = match kind {
            SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF => {
                // Fold from the end of the header line (the colon) to the end of the body.
                let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
                let colon = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::COLON);
                match (body, colon) {
                    (Some(body), Some(colon)) => {
                        let end = trimmed_end(&body);
                        Some((
                            TextRange::new(colon.text_range().end(), end),
                            "...".to_string(),
                            if kind == SyntaxKind::FUNC_DEF {
                                FoldKind::Function
                            } else {
                                FoldKind::Class
                            },
                        ))
                    }
                    _ => None,
                }
            }
            SyntaxKind::IF_STMT
            | SyntaxKind::FOR_STMT
            | SyntaxKind::WHILE_STMT
            | SyntaxKind::WITH_STMT
            | SyntaxKind::TRY_STMT
            | SyntaxKind::MATCH_STMT => {
                let colon = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::COLON);
                let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
                match (body, colon) {
                    (Some(body), Some(colon)) => Some((
                        TextRange::new(colon.text_range().end(), trimmed_end(&body)),
                        "...".to_string(),
                        FoldKind::Block,
                    )),
                    _ => None,
                }
            }
            SyntaxKind::ELIF_CLAUSE
            | SyntaxKind::ELSE_CLAUSE
            | SyntaxKind::EXCEPT_CLAUSE
            | SyntaxKind::FINALLY_CLAUSE
            | SyntaxKind::CASE_CLAUSE => {
                let colon = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::COLON);
                let body = node.children().find(|c| c.kind() == SyntaxKind::BLOCK);
                match (body, colon) {
                    (Some(body), Some(colon)) => Some((
                        TextRange::new(colon.text_range().end(), trimmed_end(&body)),
                        "...".to_string(),
                        FoldKind::Block,
                    )),
                    _ => None,
                }
            }
            SyntaxKind::STRING_EXPR => {
                let is_doc = node
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::EXPR_STMT)
                    && node
                        .text()
                        .to_string()
                        .starts_with(['"', '\'', 'r', 'R', 'u', 'U'])
                    && (node.text().to_string().contains("\"\"\"")
                        || node.text().to_string().contains("'''"));
                if is_doc {
                    let text = node.text().to_string();
                    let first_line = text.lines().next().unwrap_or("").trim().to_string();
                    let placeholder = if first_line.len() > 3 {
                        format!("{}...\"\"\"", first_line.trim_end_matches(['"', '\'']))
                    } else {
                        "\"\"\"...\"\"\"".to_string()
                    };
                    Some((node.text_range(), placeholder, FoldKind::Docstring))
                } else {
                    None
                }
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
                    (Some(o), Some(c)) => Some((
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

/// Start of a node ignoring leading newline/indent/trivia tokens.
fn trimmed_start(node: &SyntaxNode) -> TextSize {
    let mut start = node.text_range().start();
    let mut tok = node.first_token();
    while let Some(t) = tok {
        if t.kind().is_trivia()
            || matches!(
                t.kind(),
                SyntaxKind::NEWLINE | SyntaxKind::DEDENT | SyntaxKind::INDENT
            )
        {
            start = t.text_range().end();
            tok = t.next_token();
        } else {
            break;
        }
    }
    start
}

/// End of a node ignoring trailing whitespace/newline tokens.
fn trimmed_end(node: &SyntaxNode) -> TextSize {
    let mut end = node.text_range().end();
    let mut tok = node.last_token();
    while let Some(t) = tok {
        if t.kind().is_trivia()
            || matches!(
                t.kind(),
                SyntaxKind::NEWLINE | SyntaxKind::DEDENT | SyntaxKind::INDENT
            )
        {
            end = t.text_range().start();
            tok = t.prev_token();
        } else {
            break;
        }
    }
    end
}

/// Builds the structure outline (classes, functions, module-level variables, imports).
pub fn structure(parse: &Parse, line_of: &dyn Fn(TextSize) -> usize) -> Vec<StructureItem> {
    let root = parse.syntax_node();
    let mut out = Vec::new();
    if let Some(module) = ast::Module::cast(root) {
        for stmt in module.statements() {
            collect_structure(&stmt, 0, &mut out, line_of);
        }
    }
    out
}

fn collect_structure(
    stmt: &ast::Stmt,
    depth: usize,
    out: &mut Vec<StructureItem>,
    line_of: &dyn Fn(TextSize) -> usize,
) {
    match stmt {
        ast::Stmt::ClassDef(c) => {
            let name_range = c
                .name()
                .map(|t| t.text_range())
                .unwrap_or_else(|| c.text_range());
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
            out.push(StructureItem {
                name: c.name_text(),
                detail,
                kind: StructureKind::Class,
                range: c.text_range(),
                name_range,
                depth,
                line: line_of(name_range.start()),
            });
            if let Some(body) = c.body() {
                for s in body.statements() {
                    collect_structure(&s, depth + 1, out, line_of);
                }
            }
        }
        ast::Stmt::FuncDef(f) => {
            let name_range = f
                .name()
                .map(|t| t.text_range())
                .unwrap_or_else(|| f.text_range());
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
            let kind = if f.is_method() {
                StructureKind::Method
            } else {
                StructureKind::Function
            };
            out.push(StructureItem {
                name: f.name_text(),
                detail,
                kind,
                range: f.text_range(),
                name_range,
                depth,
                line: line_of(name_range.start()),
            });
            if let Some(body) = f.body() {
                for s in body.statements() {
                    if matches!(s, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)) {
                        collect_structure(&s, depth + 1, out, line_of);
                    }
                }
            }
        }
        ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => {
            let aliases: Vec<_> = match stmt {
                ast::Stmt::Import(import) => import.aliases().collect(),
                ast::Stmt::ImportFrom(import) => import.aliases().collect(),
                _ => unreachable!(),
            };
            for alias in aliases {
                let name = alias
                    .alias()
                    .map(|token| token.text().to_owned())
                    .unwrap_or_else(|| alias.name());
                let name_range = alias
                    .alias()
                    .or_else(|| alias.name_token())
                    .map(|token| token.text_range())
                    .unwrap_or_else(|| alias.text_range());
                out.push(StructureItem {
                    name,
                    detail: format!(" ← {}", stmt.syntax().text().to_string().trim()),
                    kind: StructureKind::Import,
                    range: stmt.text_range(),
                    name_range,
                    depth,
                    line: line_of(name_range.start()),
                });
            }
            if matches!(stmt,ast::Stmt::ImportFrom(import) if import.is_star()) {
                out.push(StructureItem {
                    name: "*".into(),
                    detail: stmt.syntax().text().to_string(),
                    kind: StructureKind::Import,
                    range: stmt.text_range(),
                    name_range: stmt.text_range(),
                    depth,
                    line: line_of(stmt.text_range().start()),
                });
            }
        }
        ast::Stmt::Assign(a) if depth <= 1 => {
            for t in a.targets() {
                if let ast::Expr::Name(n) = t {
                    let r = n.text_range();
                    out.push(StructureItem {
                        name: n.name_text(),
                        detail: String::new(),
                        kind: StructureKind::Variable,
                        range: a.text_range(),
                        name_range: r,
                        depth,
                        line: line_of(r.start()),
                    });
                }
            }
        }
        ast::Stmt::AnnAssign(a) if depth <= 1 => {
            if let Some(ast::Expr::Name(n)) = a.target() {
                let r = n.text_range();
                let detail = a
                    .annotation()
                    .map(|t| format!(": {}", t.syntax().text()))
                    .unwrap_or_default();
                out.push(StructureItem {
                    name: n.name_text(),
                    detail,
                    kind: StructureKind::Variable,
                    range: a.text_range(),
                    name_range: r,
                    depth,
                    line: line_of(r.start()),
                });
            }
        }
        ast::Stmt::If(i) if depth == 0 => {
            // Hoist definitions inside `if TYPE_CHECKING:` / `if __name__ == ...` blocks.
            if let Some(b) = i.body() {
                for s in b.statements() {
                    if matches!(s, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)) {
                        collect_structure(&s, depth, out, line_of);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Breadcrumb path (outermost first) of definitions enclosing `offset`.
pub fn breadcrumbs(parse: &Parse, offset: TextSize) -> Vec<(String, TextRange)> {
    let root = parse.syntax_node();
    // Between two tokens prefer the one to the right unless it is only a line
    // break or dedent: at column 1 of a `def`/`class` line the left token is
    // the end of the previous block.
    let at = token_at(&root, offset);
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

/// Successive selection ranges for *Extend Selection* starting at `range`.
pub fn extend_selection(parse: &Parse, range: TextRange) -> Option<TextRange> {
    let root = parse.syntax_node();
    let tok = token_at(&root, range.start()).right_biased()?;
    let mut candidates: Vec<TextRange> = Vec::new();
    for anc in tok.parent_ancestors() {
        let r = anc.text_range();
        // Also offer the "inside the brackets" range for bracketed nodes.
        if let (Some(f), Some(l)) = (anc.first_token(), anc.last_token()) {
            if matches!(
                f.kind(),
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE
            ) && matches!(
                l.kind(),
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE
            ) {
                candidates.push(TextRange::new(f.text_range().end(), l.text_range().start()));
            }
        }
        if anc.kind() == SyntaxKind::BLOCK || anc.kind() == SyntaxKind::MODULE {
            let (s, e) = (trimmed_start(&anc), trimmed_end(&anc));
            if s < e {
                candidates.push(TextRange::new(s, e));
            }
        } else {
            candidates.push(TextRange::new(r.start(), trimmed_end(&anc)));
        }
    }
    candidates.sort_by_key(|r| (r.len(), r.start()));
    candidates
        .into_iter()
        .find(|c| c.contains_range(range) && *c != range && c.len() > range.len())
}

/// Start offsets of all function/class definitions (for Alt+Up/Down navigation).
pub fn definition_starts(parse: &Parse) -> Vec<TextSize> {
    let mut v: Vec<TextSize> = parse
        .syntax_node()
        .descendants()
        .filter(|n| matches!(n.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))
        .map(|n| {
            n.children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| matches!(t.kind(), SyntaxKind::DEF_KW | SyntaxKind::CLASS_KW))
                .map(|t| t.text_range().start())
                .unwrap_or_else(|| n.text_range().start())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Syntax diagnostics of the parse (lexer + parser errors).
pub fn diagnostics(parse: &Parse) -> Vec<Diagnostic> {
    parse.errors().to_vec()
}

/// Finds the matching bracket for the bracket token at/before `offset`,
/// using the token stream so strings and comments are ignored.
pub fn matching_bracket(parse: &Parse, offset: TextSize) -> Option<(TextRange, TextRange)> {
    let root = parse.syntax_node();
    let pick = |off: TextSize| {
        token_at(&root, off)
            .right_biased()
            .filter(|t| is_bracket(t.kind()))
    };
    let tok = pick(offset).or_else(|| {
        if offset > TextSize::new(0) {
            pick(offset - TextSize::new(1))
        } else {
            None
        }
    })?;
    let (open, close) = pair_of(tok.kind())?;
    let forward = tok.kind() == open;
    let mut depth = 0i32;
    let mut cur = Some(tok.clone());
    while let Some(t) = cur {
        if t.kind() == open {
            depth += if forward { 1 } else { -1 };
        } else if t.kind() == close {
            depth += if forward { -1 } else { 1 };
        }
        if depth == 0 {
            return Some(if forward {
                (tok.text_range(), t.text_range())
            } else {
                (t.text_range(), tok.text_range())
            });
        }
        cur = if forward {
            t.next_token()
        } else {
            t.prev_token()
        };
    }
    None
}

/// Ranges *Move Caret to Code Block Start/End* (Ctrl+[ / Ctrl+]) steps
/// through around `offset`, innermost first: the statements of enclosing
/// blocks (and the module) and the inside of enclosing brackets.
pub fn code_block_ranges(parse: &Parse, offset: TextSize) -> Vec<TextRange> {
    let root = parse.syntax_node();
    let at = token_at(&root, offset);
    let Some(tok) = at.clone().right_biased().or_else(|| at.left_biased()) else {
        return Vec::new();
    };
    let mut out: Vec<TextRange> = Vec::new();
    for anc in tok.parent_ancestors() {
        let r = match anc.kind() {
            SyntaxKind::BLOCK | SyntaxKind::MODULE => {
                let (s, e) = (trimmed_start(&anc), trimmed_end(&anc));
                (s <= e).then(|| TextRange::new(s, e))
            }
            _ => match (anc.first_token(), anc.last_token()) {
                (Some(f), Some(l))
                    if matches!(
                        f.kind(),
                        SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE
                    ) && matches!(
                        l.kind(),
                        SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE
                    ) && f.text_range().end() <= l.text_range().start() =>
                {
                    Some(TextRange::new(f.text_range().end(), l.text_range().start()))
                }
                _ => None,
            },
        };
        if let Some(r) = r.filter(|r| r.contains_inclusive(offset) && !out.contains(r)) {
            out.push(r);
        }
    }
    out
}

/// A statement for *Move Statement Up/Down*: its lines (decorators included)
/// and the lines of the neighbouring statements of the same block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementUnit {
    pub lines: (usize, usize),
    pub prev: Option<(usize, usize)>,
    pub next: Option<(usize, usize)>,
    /// Range of the block (or module) holding the statement.
    pub parent: TextRange,
}

fn node_lines(node: &SyntaxNode, line_of: &dyn Fn(TextSize) -> usize) -> (usize, usize) {
    let (s, e) = (trimmed_start(node), trimmed_end(node));
    (line_of(s), line_of(e.max(s)))
}

/// The statement whose code starts in `line_range` (a whole line of `text`,
/// the parsed source). Clause lines (`elif`, `else`, `except`, …) and
/// one-line bodies (`if x: y`) belong to their compound statement.
pub fn statement_unit(
    parse: &Parse,
    text: &str,
    line_range: TextRange,
    line_of: &dyn Fn(TextSize) -> usize,
) -> Option<StatementUnit> {
    let root = parse.syntax_node();
    let mut tok = token_at(&root, line_range.start()).right_biased();
    // First significant token of the line.
    while let Some(t) = tok.clone() {
        if t.text_range().start() >= line_range.end() && !line_range.is_empty() {
            return None;
        }
        if !(t.kind().is_trivia()
            || matches!(
                t.kind(),
                SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT
            ))
        {
            break;
        }
        tok = t.next_token();
    }
    let tok = tok?;
    if tok.kind() == SyntaxKind::EOF || tok.text_range().start() > line_range.end() {
        return None;
    }
    let line = line_of(line_range.start());
    for node in tok.parent_ancestors() {
        let Some(parent) = node.parent() else { break };
        if !node.kind().is_statement()
            || !matches!(parent.kind(), SyntaxKind::BLOCK | SyntaxKind::MODULE)
        {
            continue;
        }
        let lines = node_lines(&node, line_of);
        // A statement sharing its first line with an enclosing header is not a unit of its own.
        let s = usize::from(trimmed_start(&node)).min(text.len());
        let starts_line = text[..s]
            .rsplit('\n')
            .next()
            .is_none_or(|before| before.trim().is_empty());
        if !starts_line || lines.0 > line || lines.1 < line {
            continue;
        }
        let siblings: Vec<(usize, usize)> = parent
            .children()
            .filter(|c| c.kind().is_statement())
            .map(|c| node_lines(&c, line_of))
            .collect();
        let prev = siblings.iter().rev().find(|s| s.1 < lines.0).copied();
        let next = siblings.iter().find(|s| s.0 > lines.1).copied();
        return Some(StatementUnit {
            lines,
            prev,
            next,
            parent: parent.text_range(),
        });
    }
    None
}

fn is_list_element(list: SyntaxKind, child: SyntaxKind) -> bool {
    match list {
        SyntaxKind::ARG_LIST
        | SyntaxKind::PARAM_LIST
        | SyntaxKind::LIST_EXPR
        | SyntaxKind::TUPLE_EXPR
        | SyntaxKind::SET_EXPR => true,
        SyntaxKind::DICT_EXPR => matches!(child, SyntaxKind::DICT_ITEM | SyntaxKind::DICT_UNPACK),
        SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT => child == SyntaxKind::IMPORT_ALIAS,
        _ => false,
    }
}

/// *Move Element Left/Right*: the range of the list element (argument,
/// parameter, collection item, imported name) at `offset` and the range of
/// its neighbour in that direction.
pub fn element_swap(
    parse: &Parse,
    offset: TextSize,
    right: bool,
) -> Option<(TextRange, TextRange)> {
    let root = parse.syntax_node();
    let at = token_at(&root, offset);
    let usable = |t: &rustpy_syntax::SyntaxToken| {
        !t.kind().is_trivia()
            && !matches!(t.kind(), SyntaxKind::COMMA | SyntaxKind::NEWLINE)
            && !is_bracket(t.kind())
    };
    let tok = at
        .clone()
        .right_biased()
        .filter(usable)
        .or_else(|| at.left_biased().filter(usable))?;
    for node in tok.parent_ancestors() {
        let Some(parent) = node.parent() else { break };
        if !is_list_element(parent.kind(), node.kind()) {
            continue;
        }
        let elems: Vec<SyntaxNode> = parent
            .children()
            .filter(|c| is_list_element(parent.kind(), c.kind()))
            .collect();
        let idx = elems.iter().position(|e| *e == node)?;
        let other = if right {
            elems.get(idx + 1)
        } else {
            idx.checked_sub(1).and_then(|i| elems.get(i))
        }?;
        let range = |n: &SyntaxNode| TextRange::new(trimmed_start(n), trimmed_end(n));
        return Some((range(&node), range(other)));
    }
    None
}

fn is_bracket(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::L_PAREN
            | SyntaxKind::R_PAREN
            | SyntaxKind::L_BRACK
            | SyntaxKind::R_BRACK
            | SyntaxKind::L_BRACE
            | SyntaxKind::R_BRACE
    )
}

fn pair_of(k: SyntaxKind) -> Option<(SyntaxKind, SyntaxKind)> {
    Some(match k {
        SyntaxKind::L_PAREN | SyntaxKind::R_PAREN => (SyntaxKind::L_PAREN, SyntaxKind::R_PAREN),
        SyntaxKind::L_BRACK | SyntaxKind::R_BRACK => (SyntaxKind::L_BRACK, SyntaxKind::R_BRACK),
        SyntaxKind::L_BRACE | SyntaxKind::R_BRACE => (SyntaxKind::L_BRACE, SyntaxKind::R_BRACE),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_syntax::parse;

    fn lines(src: &str) -> impl Fn(TextSize) -> usize + '_ {
        move |off: TextSize| src[..usize::from(off)].matches('\n').count()
    }

    #[test]
    fn folds_and_structure() {
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
        let items = structure(&p, &line_of);
        let names: Vec<(String, StructureKind, usize)> = items
            .iter()
            .map(|i| (i.name.clone(), i.kind, i.depth))
            .collect();
        assert_eq!(
            names,
            vec![
                ("a".into(), StructureKind::Import, 0),
                ("b".into(), StructureKind::Import, 0),
                ("C".into(), StructureKind::Class, 0),
                ("m".into(), StructureKind::Method, 1),
                ("x".into(), StructureKind::Variable, 0),
                ("f".into(), StructureKind::Function, 0)
            ]
        );
        let crumbs = breadcrumbs(&p, TextSize::new(src.find("return").unwrap() as u32));
        assert_eq!(
            crumbs.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            vec!["C", "m()"]
        );
        assert_eq!(definition_starts(&p).len(), 3);
    }

    #[test]
    fn extend_selection_levels() {
        let src = "def f(a):\n    return foo(a + 1)\n";
        let p = parse(src);
        let at = TextSize::new(src.find("a + 1").unwrap() as u32);
        let r0 = TextRange::at(at, TextSize::new(1));
        let r1 = extend_selection(&p, r0).unwrap();
        assert_eq!(&src[r1], "a + 1");
        let r2 = extend_selection(&p, r1).unwrap();
        assert_eq!(&src[r2], "(a + 1)");
        let r3 = extend_selection(&p, r2).unwrap();
        assert_eq!(&src[r3], "foo(a + 1)");
        let r4 = extend_selection(&p, r3).unwrap();
        assert_eq!(&src[r4], "return foo(a + 1)");
        let r5 = extend_selection(&p, r4).unwrap();
        assert_eq!(&src[r5], "def f(a):\n    return foo(a + 1)");
    }

    #[test]
    fn bracket_matching_ignores_strings() {
        let src = "f(')', [1])\n";
        let p = parse(src);
        let (a, b) = matching_bracket(&p, TextSize::new(1)).unwrap();
        assert_eq!(usize::from(a.start()), 1);
        assert_eq!(usize::from(b.start()), 10);
    }

    #[test]
    fn queries_past_an_empty_or_stale_tree_find_nothing() {
        // Large-file and non-Python editors keep an empty tree; a stale tree
        // can be shorter than the document.
        for src in ["", "def f(a):\n    return (a)\n"] {
            let p = parse(src);
            let past = TextSize::new(src.len() as u32 + 5_880_000);
            assert!(token_at(&p.syntax_node(), past).next().is_none());
            assert!(covering(&p.syntax_node(), TextRange::empty(past)).is_none());
            assert!(breadcrumbs(&p, past).is_empty());
            assert_eq!(extend_selection(&p, TextRange::empty(past)), None);
            assert_eq!(matching_bracket(&p, past), None);
            assert!(code_block_ranges(&p, past).is_empty());
            assert_eq!(element_swap(&p, past, true), None);
            let line = TextRange::at(past, TextSize::new(4));
            assert_eq!(statement_unit(&p, src, line, &|_| 0), None);
        }
    }

    #[test]
    fn large_file_editor_breadcrumbs_at_document_end() {
        let text = format!(
            "class C:\n    def m(self):\n        pass\n# {}\n",
            "x".repeat(crate::editor::EditorState::LARGE_FILE_BYTES)
        );
        let mut editor =
            crate::editor::EditorState::from_text(&text, crate::editor::Language::Python);
        assert!(editor.is_large_file());
        let end = editor.doc.len();
        editor.set_carets(vec![crate::editor::Caret::at(end)]);
        assert!(editor.breadcrumbs().is_empty());
        editor.move_to_code_block(true);
        editor.move_element(true);
        editor.move_statement(false);
    }
}

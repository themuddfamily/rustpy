//! Port of `black/linegen.py`: the visitor that turns the tree into logical
//! lines (normalizing parentheses, strings and numbers on the way) and the
//! transforms that split over-long lines (delimiter split, right-hand split
//! with optional-parentheses omission, left-hand split for `def`s, power
//! operator hugging). Each emitted line is processed immediately (blank lines,
//! splitting, rendering), in the same order as Black's generators.

use super::fxhash::HashSet;

use super::brackets::*;
use super::comments::*;
use super::fmtoff::{FMT_OFF, FMT_ON};
use super::lines::*;
use super::nodes::*;
use super::strings::*;
use super::tree::*;

/// Features used while splitting (`split_line_features` plus FOP).
#[derive(Clone, Copy, Debug, Default)]
pub struct Feat {
    pub trailing_comma_in_call: bool,
    pub trailing_comma_in_def: bool,
    pub force_optional_parentheses: bool,
}

/// Features used while generating lines.
#[derive(Clone, Copy, Debug, Default)]
pub struct LgFeat {
    pub parenthesized_context_managers: bool,
    pub unparenthesized_except_types: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transform {
    Delimiter,
    TypeIgnore,
    Standalone,
    Rhs,
    Lhs,
    HugPower,
}

type TResult = Result<Vec<Line>, ()>;

/// Nesting limit for `transform_line` (see there).
const MAX_TRANSFORM_DEPTH: usize = 300;

pub struct Fmt {
    pub tree: Tree,
    pub mode: Mode,
    lg: LgFeat,
    split: Feat,
    transform_depth: usize,
    current_line: Line,
    elt: EmptyLineTracker,
    pub blocks: Vec<LinesBlock>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Component {
    Head,
    Body,
    Tail,
}

struct OmitGen {
    started: bool,
    done: bool,
    omit: HashSet<Id>,
    length: usize,
    opening_bracket: Option<Id>,
    closing_bracket: Option<Id>,
    inner_brackets: HashSet<Id>,
    index: isize,
    resume: Option<(usize, Id)>,
}

fn has_backslash_newline(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    for i in 0..c.len() {
        if c[i] == '\\' {
            let mut j = i + 1;
            while j < c.len() && c[j].is_whitespace() {
                if c[j] == '\n' {
                    return true;
                }
                j += 1;
            }
        }
    }
    false
}

impl Fmt {
    pub fn new(tree: Tree, mode: Mode, lg: LgFeat, split: Feat) -> Self {
        Fmt {
            tree,
            mode,
            lg,
            split,
            transform_depth: 0,
            current_line: Line::default(),
            elt: EmptyLineTracker::default(),
            blocks: Vec::new(),
        }
    }

    fn append(&mut self, line: &mut Line, leaf: Id, preformatted: bool, track_bracket: bool) {
        line.append(
            &mut self.tree,
            &self.mode,
            leaf,
            preformatted,
            track_bracket,
        );
    }

    fn short_enough(&self, line: &Line) -> bool {
        is_line_short_enough(&self.tree, &self.mode, line, None)
    }

    fn line_str(&self, line: &Line) -> String {
        line.to_str(&self.tree, &self.mode)
    }

    // -----------------------------------------------------------------------
    // Line generation

    fn line(&mut self, indent: isize) {
        if !self.current_line.is_truthy() {
            self.current_line.depth = (self.current_line.depth as isize + indent).max(0) as usize;
            return;
        }
        if self.current_line.leaves.len() == 1
            && is_async_stmt_or_funcdef(&self.tree, self.current_line.leaves[0])
        {
            return;
        }
        let depth = (self.current_line.depth as isize + indent).max(0) as usize;
        let complete = std::mem::replace(&mut self.current_line, Line::new(depth, false));
        self.emit(complete);
    }

    fn emit(&mut self, line: Line) {
        let block = self
            .elt
            .maybe_empty_lines(&mut self.tree, &mut self.blocks, &line);
        let lines = self.transform_line(line, self.split);
        for l in lines {
            let s = l.render(&self.tree, &self.mode);
            self.blocks[block].content_lines.push(s);
        }
    }

    pub fn visit(&mut self, node: Id) {
        let t = self.tree.ty(node);
        match t {
            INDENT => {
                self.line(1);
                self.visit_default(node);
            }
            DEDENT => {
                self.line(0);
                self.visit_default(node);
                self.line(-1);
            }
            SEMI => self.line(0),
            ENDMARKER => {
                self.visit_default(node);
                self.line(0);
            }
            STRING => self.visit_string(node),
            STANDALONE_COMMENT => self.visit_standalone_comment(node),
            NUMBER => {
                let v = normalize_numeric_literal(self.tree.value(node));
                self.tree.set_value(node, v);
                self.visit_default(node);
            }
            TEST => self.visit_test(node),
            ASSERT_STMT => self.visit_stmt(node, &["assert"], &["assert", ","]),
            IF_STMT => self.visit_stmt(node, &["if", "else", "elif"], &["if", "elif"]),
            WHILE_STMT => self.visit_stmt(node, &["while", "else"], &["while"]),
            FOR_STMT => self.visit_stmt(node, &["for", "else"], &["for", "in"]),
            TRY_STMT => self.visit_stmt(node, &["try", "except", "else", "finally"], &[]),
            EXCEPT_CLAUSE => self.visit_stmt(node, &["except"], &["except"]),
            WITH_STMT => self.visit_stmt(node, &["with"], &["with"]),
            CLASSDEF => self.visit_stmt(node, &["class"], &[]),
            EXPR_STMT => self.visit_stmt(node, &[], ASSIGNMENTS),
            RETURN_STMT => self.visit_stmt(node, &["return"], &["return"]),
            YIELD_EXPR => self.visit_stmt(node, &[], &[]),
            IMPORT_FROM => self.visit_stmt(node, &[], &["import"]),
            DEL_STMT => self.visit_stmt(node, &[], &["del"]),
            GUARD => self.visit_stmt(node, &[], &["if"]),
            TYPEPARAMS => {
                self.visit_default(node);
                let c = self.tree.child(node, 0);
                self.tree.set_prefix(c, "");
            }
            TYPEVARTUPLE | PARAMSPEC => {
                self.visit_default(node);
                let c = self.tree.child(node, 1);
                self.tree.set_prefix(c, "");
            }
            FUNCDEF => self.visit_funcdef(node),
            MATCH_STMT | CASE_BLOCK => {
                self.normalize_invisible_parens(node, &[]);
                self.line(0);
                self.visit_children(node);
            }
            SUITE => {
                if is_stub_suite(&self.tree, node) {
                    let c = self.tree.child(node, 2);
                    self.visit(c);
                } else {
                    self.visit_default(node);
                }
            }
            SIMPLE_STMT => self.visit_simple_stmt(node),
            ASYNC_STMT | ASYNC_FUNCDEF => self.visit_async_stmt(node),
            DECORATORS | DECORATED => {
                let mut i = 0;
                while i < self.tree.nchildren(node) {
                    let c = self.tree.child(node, i);
                    self.line(0);
                    self.visit(c);
                    i += 1;
                }
            }
            POWER => self.visit_power(node),
            FACTOR => self.visit_factor(node),
            TNAME => {
                if self.tree.nchildren(node) == 3 {
                    let c = self.tree.child(node, 2);
                    if self.maybe_make_parens_invisible_in_atom(c, node, false, false) {
                        wrap_in_parentheses(&mut self.tree, node, c, false, None);
                    }
                }
                self.visit_default(node);
            }
            ATOM => {
                if self.tree.nchildren(node) == 3 {
                    let first = self.tree.ty(self.tree.child(node, 0));
                    let last = self.tree.ty(self.tree.child(node, 2));
                    if (first == LSQB && last == RSQB) || (first == LBRACE && last == RBRACE) {
                        let c = self.tree.child(node, 1);
                        self.maybe_make_parens_invisible_in_atom(c, node, false, false);
                    }
                }
                self.visit_default(node);
            }
            _ => self.visit_default(node),
        }
    }

    fn visit_children(&mut self, node: Id) {
        let mut i = 0;
        while i < self.tree.nchildren(node) {
            let c = self.tree.child(node, i);
            self.visit(c);
            i += 1;
        }
    }

    fn visit_default(&mut self, node: Id) {
        if !self.tree.is_leaf(node) {
            self.visit_children(node);
            return;
        }
        let any_open = self.current_line.bt.any_open_brackets();
        let comments = generate_comments(&mut self.tree, node);
        for comment in comments {
            if any_open {
                let mut cl = std::mem::take(&mut self.current_line);
                self.append(&mut cl, comment, false, false);
                self.current_line = cl;
            } else if self.tree.ty(comment) == COMMENT {
                let mut cl = std::mem::take(&mut self.current_line);
                self.append(&mut cl, comment, false, false);
                self.current_line = cl;
                self.line(0);
            } else {
                self.line(0);
                let mut cl = std::mem::take(&mut self.current_line);
                self.append(&mut cl, comment, false, false);
                self.current_line = cl;
                self.line(0);
            }
        }
        if any_open {
            self.tree.nm(node).prefix.clear();
        }
        if !is_whitespace_token(self.tree.ty(node)) {
            let mut cl = std::mem::take(&mut self.current_line);
            self.append(&mut cl, node, false, false);
            self.current_line = cl;
        }
    }

    fn visit_standalone_comment(&mut self, leaf: Id) {
        let any_open = self.current_line.bt.any_open_brackets();
        if !any_open {
            self.line(0);
        }
        let value = self.tree.value(leaf).to_string();
        let lines = py_splitlines(&value);
        let is_fmt_off_block = lines.len() >= 2
            && contains_directive(&lines[0], FMT_OFF)
            && contains_directive(&lines[lines.len() - 1], FMT_ON);
        if !is_fmt_off_block {
            self.visit_default(leaf);
            return;
        }
        let is_after_invisible_lpar = self.tree.n(leaf).fmt_first_leaf.is_none()
            && self.current_line.leaves.len() == 1
            && self.tree.ty(self.current_line.leaves[0]) == LPAR
            && self.tree.value(self.current_line.leaves[0]).is_empty();
        let prefix = self.tree.n(leaf).prefix.clone();
        // Prefix comments are outside the preserved off/on block. This
        // includes a preceding fmt:on: leaving it in the prefix lets the
        // empty-line tracker clear it as whitespace and silently loses it.
        if prefix
            .split('\n')
            .any(|line| py_strip(line).starts_with('#'))
        {
            for comment in generate_comments(&mut self.tree, leaf) {
                self.line(0);
                let mut cl = std::mem::take(&mut self.current_line);
                self.append(&mut cl, comment, false, false);
                self.current_line = cl;
                self.line(0);
            }
            self.tree.nm(leaf).prefix.clear();
        }
        let mut cl = std::mem::take(&mut self.current_line);
        self.append(&mut cl, leaf, false, false);
        self.current_line = cl;
        if !any_open || is_after_invisible_lpar {
            self.line(0);
        }
    }

    fn visit_test(&mut self, node: Id) {
        let already = self
            .tree
            .prev_sibling(node)
            .map(|p| self.tree.ty(p) == LPAR)
            .unwrap_or(false);
        if !already {
            let lpar = self.tree.leaf(LPAR, "");
            let rpar = self.tree.leaf(RPAR, "");
            let prefix = self.tree.prefix(node).to_string();
            self.tree.set_prefix(node, "");
            self.tree.nm(lpar).prefix = prefix;
            self.tree.insert_child(node, 0, lpar);
            self.tree.append_child(node, rpar);
        }
        self.visit_default(node);
    }

    fn visit_stmt(&mut self, node: Id, keywords: &[&str], parens: &[&str]) {
        self.normalize_invisible_parens(node, parens);
        let mut i = 0;
        while i < self.tree.nchildren(node) {
            let child = self.tree.child(node, i);
            if self.tree.ty(child) == NAME && keywords.contains(&self.tree.value(child)) {
                self.line(0);
            }
            self.visit(child);
            i += 1;
        }
    }

    fn visit_funcdef(&mut self, node: Id) {
        self.line(0);
        let mut is_return_annotation = false;
        let mut i = 0;
        while i < self.tree.nchildren(node) {
            let child = self.tree.child(node, i);
            if self.tree.ty(child) == RARROW {
                is_return_annotation = true;
            } else if is_return_annotation {
                if self.tree.ty(child) == ATOM && self.tree.ty(self.tree.child(child, 0)) == LPAR {
                    if self.maybe_make_parens_invisible_in_atom(child, node, false, false) {
                        wrap_in_parentheses(&mut self.tree, node, child, false, None);
                    }
                } else {
                    wrap_in_parentheses(&mut self.tree, node, child, false, None);
                }
                is_return_annotation = false;
            }
            i += 1;
        }
        self.visit_children(node);
    }

    fn visit_simple_stmt(&mut self, node: Id) {
        let mut prev_type: Option<u16> = None;
        let mut i = 0;
        while i < self.tree.nchildren(node) {
            let child = self.tree.child(node, i);
            let ct = self.tree.ty(child);
            if (prev_type.is_none() || prev_type == Some(SEMI)) && is_arith_like(&self.tree, child)
            {
                wrap_in_parentheses(&mut self.tree, node, child, false, Some(i));
            }
            prev_type = Some(ct);
            i += 1;
        }
        let parent = self.tree.parent(node);
        if parent
            .map(|p| is_statement(self.tree.ty(p)))
            .unwrap_or(false)
        {
            if is_parent_function_or_class(&self.tree, node) && is_stub_body(&self.tree, node) {
                self.visit_default(node);
            } else {
                self.line(1);
                self.visit_default(node);
                self.line(-1);
            }
        } else {
            if let Some(p) = parent {
                if is_stub_suite(&self.tree, p) {
                    self.tree.set_prefix(node, "");
                    self.visit_default(node);
                    return;
                }
            }
            self.line(0);
            self.visit_default(node);
        }
    }

    fn visit_async_stmt(&mut self, node: Id) {
        self.line(0);
        let n = self.tree.nchildren(node);
        let mut i = 0;
        while i < n {
            let child = self.tree.child(node, i);
            self.visit(child);
            i += 1;
            let ct = self.tree.ty(child);
            if ct == ASYNC || ct == STANDALONE_COMMENT {
                break;
            }
        }
        if i < self.tree.nchildren(node) {
            let internal = self.tree.child(node, i);
            self.visit(internal);
        }
    }

    fn visit_power(&mut self, node: Id) {
        let children: Vec<Id> = self.tree.children(node).to_vec();
        for idx in 0..children.len().saturating_sub(1) {
            let leaf = children[idx];
            let next_leaf = children[idx + 1];
            if !self.tree.is_leaf(leaf) {
                continue;
            }
            let value = self.tree.value(leaf).to_lowercase();
            if self.tree.ty(leaf) == NUMBER
                && self.tree.ty(next_leaf) == TRAILER
                && self.tree.ty(self.tree.child(next_leaf, 0)) == DOT
                && !(value.starts_with("0x") || value.starts_with("0b") || value.starts_with("0o"))
                && !value.contains('j')
            {
                wrap_in_parentheses(&mut self.tree, node, leaf, true, None);
            }
        }
        self.remove_await_parens(node);
        self.visit_default(node);
    }

    fn visit_factor(&mut self, node: Id) {
        if self.tree.nchildren(node) == 2 {
            let operand = self.tree.child(node, 1);
            if self.tree.ty(operand) == POWER
                && self.tree.nchildren(operand) == 3
                && self.tree.ty(self.tree.child(operand, 1)) == DOUBLESTAR
            {
                let lpar = self.tree.leaf(LPAR, "(");
                let rpar = self.tree.leaf(RPAR, ")");
                let index = self.tree.remove(operand).unwrap_or(0);
                let atom = self.tree.node(ATOM, vec![lpar, operand, rpar]);
                self.tree.insert_child(node, index, atom);
            }
        }
        self.visit_default(node);
    }

    fn visit_string(&mut self, leaf: Id) {
        if self.tree.n(leaf).keep_string {
            self.visit_default(leaf);
            return;
        }
        let v = normalize_unicode_escape_sequences(self.tree.value(leaf));
        self.tree.set_value(leaf, v);
        if is_docstring(&self.tree, leaf) && !has_backslash_newline(self.tree.value(leaf)) {
            let value = self.tree.value(leaf).to_string();
            let mut docstring = if self.mode.string_normalization {
                normalize_string_quotes(&normalize_string_prefix(&value))
            } else {
                value.clone()
            };
            let prefix = get_string_prefix(&docstring).to_string();
            docstring = docstring[prefix.len()..].to_string();
            let dchars: Vec<char> = docstring.chars().collect();
            let quote_char = dchars[0];
            let quote_len = if dchars.get(1) != Some(&quote_char) {
                1
            } else {
                3
            };
            docstring = if dchars.len() >= 2 * quote_len {
                dchars[quote_len..dchars.len() - quote_len].iter().collect()
            } else {
                String::new()
            };
            let started_empty = docstring.is_empty();
            let indent = " ".repeat(self.mode.indent * self.current_line.depth);
            if is_multiline_string(&self.tree, leaf) {
                docstring = fix_multiline_docstring(&docstring, &indent);
            } else {
                docstring = py_strip(&docstring).to_string();
            }
            let mut has_trailing_backslash = false;
            if !docstring.is_empty() {
                if docstring.starts_with(quote_char) {
                    docstring.insert(0, ' ');
                }
                if docstring.ends_with(quote_char) {
                    docstring.push(' ');
                }
                if docstring.ends_with('\\') {
                    let count = docstring.len() - docstring.trim_end_matches('\\').len();
                    if count % 2 == 1 {
                        docstring.push(' ');
                        has_trailing_backslash = true;
                    }
                }
            } else if !started_empty {
                docstring = " ".into();
            }
            let quote: String = std::iter::repeat_n(quote_char, quote_len).collect();
            let new_value = if quote_len == 3 {
                let lines = py_splitlines(&docstring);
                let last_line_length = if docstring.is_empty() {
                    0
                } else {
                    lines.last().map(|l| l.chars().count()).unwrap_or(0)
                };
                if lines.len() > 1
                    && last_line_length + quote_len > self.mode.line_length
                    && indent.len() + quote_len <= self.mode.line_length
                    && !has_trailing_backslash
                {
                    let vchars: Vec<char> = value.chars().collect();
                    let before_quote = vchars.len().checked_sub(1 + quote_len).map(|i| vchars[i]);
                    if before_quote == Some('\n') {
                        format!("{prefix}{quote}{docstring}{quote}")
                    } else {
                        format!("{prefix}{quote}{docstring}\n{indent}{quote}")
                    }
                } else {
                    format!("{prefix}{quote}{docstring}{quote}")
                }
            } else {
                format!("{prefix}{quote}{docstring}{quote}")
            };
            self.tree.set_value(leaf, new_value);
        }
        if self.mode.string_normalization {
            let v = normalize_string_quotes(&normalize_string_prefix(self.tree.value(leaf)));
            self.tree.set_value(leaf, v);
        }
        self.visit_default(leaf);
    }

    // -----------------------------------------------------------------------
    // Parentheses normalization

    fn normalize_invisible_parens(&mut self, node: Id, parens_after: &[&str]) {
        if list_comments(self.tree.prefix(node), false)
            .iter()
            .any(|pc| contains_directive(&pc.value, FMT_OFF))
        {
            // This `node` has a prefix with `# fmt: off`, don't mess with parens.
            return;
        }
        if self.tree.ty(node) == WITH_STMT {
            self.maybe_wrap_cms_in_parens(node);
        }
        let mut check_lpar = false;
        let children: Vec<Id> = self.tree.children(node).to_vec();
        let nt = self.tree.ty(node);
        for (index, &child) in children.iter().enumerate() {
            let is_node = !self.tree.is_leaf(child);
            let ct = self.tree.ty(child);
            if is_node && ct == ANNASSIGN {
                self.normalize_invisible_parens(child, parens_after);
            }
            if is_node && ct == CASE_BLOCK {
                self.normalize_invisible_parens(child, &["case"]);
            }
            if is_node && ct == GUARD {
                self.normalize_invisible_parens(child, &["if"]);
            }
            if index == 0 && is_node && ct == TESTLIST_STAR_EXPR {
                check_lpar = true;
            }
            if index == 0
                && is_node
                && ct == ATOM
                && nt == EXPR_STMT
                && !(self.mode.magic_trailing_comma && is_one_tuple(&self.tree, child))
                && !self.is_atom_multiline(child)
            {
                if self.is_parenthesized_annotation_target(node, child) {
                    let inner = self.tree.child(child, 1);
                    if !self.tree.is_leaf(inner) && self.tree.ty(inner) == ATOM {
                        self.maybe_make_parens_invisible_in_atom(inner, child, false, false);
                    }
                } else if self.maybe_make_parens_invisible_in_atom(child, node, true, true) {
                    wrap_in_parentheses(&mut self.tree, node, child, false, Some(index));
                }
            }
            if check_lpar {
                let prev_is_for = self
                    .tree
                    .prev_sibling(child)
                    .map(|p| {
                        self.tree.is_leaf(p)
                            && self.tree.ty(p) == NAME
                            && self.tree.value(p) == "for"
                    })
                    .unwrap_or(false);
                if ct == ATOM && nt == FOR_STMT && prev_is_for {
                    if self.maybe_make_parens_invisible_in_atom(child, node, true, false) {
                        wrap_in_parentheses(&mut self.tree, node, child, false, Some(index));
                    }
                } else if is_node && nt == WITH_STMT {
                    self.remove_with_parens(child, node);
                } else if ct == ATOM {
                    if parens_after.contains(&"in")
                        && self.is_parenthesized_lambda_or_ternary(child)
                    {
                        self.maybe_make_parens_invisible_in_atom(child, node, false, false);
                        let opening = self.tree.child(child, 0);
                        let n = self.tree.nchildren(child);
                        let closing = self.tree.child(child, n - 1);
                        if self.tree.ty(opening) == LPAR && self.tree.ty(closing) == RPAR {
                            self.tree.set_value(opening, "(");
                            self.tree.set_value(closing, ")");
                        }
                    } else if self.maybe_make_parens_invisible_in_atom(child, node, false, false) {
                        wrap_in_parentheses(&mut self.tree, node, child, false, Some(index));
                    }
                } else if is_one_tuple(&self.tree, child) {
                    wrap_in_parentheses(&mut self.tree, node, child, true, Some(index));
                } else if nt == IMPORT_FROM {
                    self.normalize_import_from(node, child, index);
                    break;
                } else if index == 1 && ct == STAR && nt == EXCEPT_CLAUSE {
                    continue;
                } else if !is_node
                    && self.tree.next_sibling(child).map(|s| self.tree.ty(s)) == Some(COLON)
                    && self.tree.value(child) == "case"
                {
                    break;
                } else if is_node && ct == GUARD {
                    // Guards wrap their own condition.
                } else if !is_multiline_string(&self.tree, child) {
                    wrap_in_parentheses(&mut self.tree, node, child, false, Some(index));
                }
            }
            let comma_check = ct == COMMA;
            check_lpar =
                !is_node && (parens_after.contains(&self.tree.value(child)) || comma_check);
        }
    }

    fn normalize_import_from(&mut self, parent: Id, child: Id, index: usize) {
        if self.tree.ty(child) == LPAR {
            self.tree.set_value(child, "");
            let n = self.tree.nchildren(parent);
            let last = self.tree.child(parent, n - 1);
            self.tree.set_value(last, "");
        } else if self.tree.ty(child) != STAR {
            let l = self.tree.leaf(LPAR, "");
            self.tree.insert_child(parent, index, l);
            let r = self.tree.leaf(RPAR, "");
            self.tree.append_child(parent, r);
        }
    }

    fn remove_await_parens(&mut self, node: Id) {
        let t = &self.tree;
        if t.ty(t.child(node, 0)) != AWAIT || t.nchildren(node) <= 1 {
            return;
        }
        let c1 = t.child(node, 1);
        if !(t.ty(c1) == ATOM && t.ty(t.child(c1, 0)) == LPAR) {
            return;
        }
        if self.maybe_make_parens_invisible_in_atom(c1, node, true, false) {
            wrap_in_parentheses(&mut self.tree, node, c1, false, None);
        }
        let t = &self.tree;
        let c1 = t.child(node, 1);
        let n = t.nchildren(c1);
        if n < 3 {
            return;
        }
        let opening = t.child(c1, 0);
        let closing = t.child(c1, n - 1);
        let contents = t.child(c1, 1);
        if !t.is_leaf(contents)
            && (t.ty(contents) != POWER
                || t.ty(t.child(contents, 0)) == AWAIT
                || t.children(contents)
                    .iter()
                    .any(|c| t.is_leaf(*c) && t.ty(*c) == DOUBLESTAR))
        {
            ensure_visible(&mut self.tree, opening);
            ensure_visible(&mut self.tree, closing);
        }
    }

    fn maybe_wrap_cms_in_parens(&mut self, node: Id) {
        if !self.lg.parenthesized_context_managers
            || self.tree.nchildren(node) <= 2
            || self.tree.ty(self.tree.child(node, 1)) == ATOM
        {
            return;
        }
        let n = self.tree.nchildren(node);
        let colon_index = (2..n).find(|&i| self.tree.ty(self.tree.child(node, i)) == COLON);
        if let Some(ci) = colon_index {
            let lpar = self.tree.leaf(LPAR, "");
            let rpar = self.tree.leaf(RPAR, "");
            let cms: Vec<Id> = self.tree.children(node)[1..ci].to_vec();
            for &c in &cms {
                self.tree.remove(c);
            }
            let gexp = self.tree.node(TESTLIST_GEXP, cms);
            let atom = self.tree.node(ATOM, vec![lpar, gexp, rpar]);
            self.tree.insert_child(node, 1, atom);
        }
    }

    fn remove_with_parens(&mut self, node: Id, parent: Id) {
        match self.tree.ty(node) {
            ATOM => {
                if self.maybe_make_parens_invisible_in_atom(node, parent, true, false) {
                    wrap_in_parentheses(&mut self.tree, parent, node, false, None);
                }
                if self.tree.nchildren(node) > 1 {
                    let c = self.tree.child(node, 1);
                    if !self.tree.is_leaf(c) {
                        self.remove_with_parens(c, node);
                    }
                }
            }
            TESTLIST_GEXP => {
                let children: Vec<Id> = self.tree.children(node).to_vec();
                for c in children {
                    if !self.tree.is_leaf(c) {
                        self.remove_with_parens(c, node);
                    }
                }
            }
            ASEXPR_TEST => {
                if self
                    .tree
                    .leaves(node)
                    .iter()
                    .any(|l| self.tree.ty(*l) == COLONEQUAL)
                {
                    return;
                }
                let c0 = self.tree.child(node, 0);
                if self.maybe_make_parens_invisible_in_atom(c0, node, true, false) {
                    wrap_in_parentheses(&mut self.tree, node, c0, false, None);
                }
            }
            _ => {}
        }
    }

    fn is_atom_multiline(&self, node: Id) -> bool {
        if self.tree.is_leaf(node) || self.tree.nchildren(node) < 3 {
            return false;
        }
        let middle = self.tree.child(node, 1);
        self.tree
            .pre_order(middle)
            .iter()
            .any(|c| self.tree.is_leaf(*c) && self.tree.n(*c).prefix.contains('\n'))
    }

    fn is_parenthesized_annotation_target(&self, node: Id, child: Id) -> bool {
        let t = &self.tree;
        if t.nchildren(node) < 2 {
            return false;
        }
        let annassign = t.child(node, 1);
        if t.is_leaf(annassign) || t.ty(annassign) != ANNASSIGN {
            return false;
        }
        let mut target = child;
        while !t.is_leaf(target) && t.ty(target) == ATOM && t.nchildren(target) == 3 {
            target = t.child(target, 1);
        }
        t.is_leaf(target) && t.ty(target) == NAME
    }

    fn is_parenthesized_lambda_or_ternary(&self, node: Id) -> bool {
        let t = &self.tree;
        let mut node = node;
        while t.ty(node) == ATOM
            && t.nchildren(node) == 3
            && t.ty(t.child(node, 0)) == LPAR
            && t.ty(t.child(node, 2)) == RPAR
        {
            let middle = t.child(node, 1);
            if matches!(t.ty(middle), TEST | LAMBDEF) {
                return true;
            }
            node = middle;
        }
        false
    }

    /// Makes the parentheses of an atom invisible when safe; returns whether
    /// the node should itself be wrapped in invisible parentheses.
    fn maybe_make_parens_invisible_in_atom(
        &mut self,
        node: Id,
        parent: Id,
        remove_brackets_around_comma: bool,
        allow_star_expr: bool,
    ) -> bool {
        let nt = self.tree.ty(node);
        let pt = self.tree.ty(parent);
        if !matches!(nt, ATOM | EXPR)
            || is_empty_tuple(&self.tree, node)
            || is_one_tuple(&self.tree, node)
            || (is_tuple(&self.tree, node) && pt == ASEXPR_TEST)
            || (is_tuple(&self.tree, node)
                && pt == WITH_STMT
                && has_sibling_with_type(&self.tree, node, COMMA))
            || (is_yield(&self.tree, node) && pt != EXPR_STMT)
        {
            return false;
        }
        if !remove_brackets_around_comma
            && max_delimiter_priority_in_atom(&mut self.tree, node) >= COMMA_PRIORITY
        {
            let t = &self.tree;
            let except_ok = self.lg.unparenthesized_except_types
                && is_tuple(t, node)
                && t.parent_ty(node) == Some(EXCEPT_CLAUSE)
                && !t
                    .next_sibling(node)
                    .map(|s| t.ty(s) == NAME && t.value(s) == "as")
                    .unwrap_or(false);
            if !except_ok {
                return false;
            }
        }
        if is_tuple_containing_walrus(&self.tree, node)
            || (!allow_star_expr && is_tuple_containing_star(&self.tree, node))
            || is_generator(&self.tree, node)
        {
            return false;
        }
        if is_walrus_assignment(&self.tree, node)
            && matches!(
                pt,
                ANNASSIGN
                    | EXPR_STMT
                    | ASSERT_STMT
                    | RETURN_STMT
                    | YIELD_ARG
                    | YIELD_EXPR
                    | EXCEPT_CLAUSE
                    | FUNCDEF
                    | WITH_STMT
                    | TESTLIST_GEXP
                    | TNAME
                    | FOR_STMT
                    | DEL_STMT
            )
        {
            return false;
        }
        let n = self.tree.nchildren(node);
        let first = self.tree.child(node, 0);
        let last = self.tree.child(node, n - 1);
        if self.tree.ty(first) == LPAR && self.tree.ty(last) == RPAR {
            let middle = self.tree.child(node, 1);
            if !is_type_ignore_comment_string(py_strip(self.tree.prefix(middle))) {
                self.tree.set_value(first, "");
                self.tree.set_value(last, "");
            }
            self.maybe_make_parens_invisible_in_atom(
                middle,
                parent,
                remove_brackets_around_comma,
                false,
            );
            if is_atom_with_invisible_parens(&self.tree, middle) {
                let m_children = self.tree.children(middle).to_vec();
                let inner = m_children[1];
                self.tree.replace(middle, inner);
                let mfirst = m_children[0];
                let mlast = m_children[m_children.len() - 1];
                if !py_strip(self.tree.prefix(mfirst)).is_empty() {
                    let p = format!("{}{}", self.tree.prefix(mfirst), self.tree.prefix(inner));
                    self.tree.set_prefix(inner, p);
                }
                if !py_strip(self.tree.prefix(mlast)).is_empty() {
                    let p = format!("{}{}", self.tree.prefix(mlast), self.tree.prefix(last));
                    self.tree.set_prefix(last, p);
                }
            }
            return false;
        }
        true
    }

    // -----------------------------------------------------------------------
    // Transforms

    pub fn transform_line(&mut self, line: Line, features: Feat) -> Vec<Line> {
        // Black recurses once per split level; Python's recursion limit makes
        // it give up far below this depth, so stop splitting instead of
        // exhausting the stack on pathological input.
        if self.transform_depth >= MAX_TRANSFORM_DEPTH {
            return vec![line];
        }
        self.transform_depth += 1;
        let out = self.transform_line_inner(line, features);
        self.transform_depth -= 1;
        out
    }

    fn transform_line_inner(&mut self, line: Line, features: Feat) -> Vec<Line> {
        if line.is_comment(&self.tree) {
            return vec![line];
        }
        let line_str = self.line_str(&line);
        let hug_str = match self.hug_power_op(&line) {
            Ok(l) => self.line_str(&l),
            Err(()) => line_str.clone(),
        };
        let mut transformers: Vec<Transform> = if !line
            .contains_uncollapsable_type_comments(&self.tree)
            && !line.should_split_rhs
            && line.magic_trailing_comma.is_none()
            && (is_line_short_enough(&self.tree, &self.mode, &line, Some(&hug_str))
                || line.contains_unsplittable_type_ignore(&self.tree))
            && !(line.inside_brackets && line.contains_standalone_comments(&self.tree))
            && !line.contains_implicit_multiline_string_with_comments(&self.tree)
        {
            vec![]
        } else if line.is_def(&self.tree) && !self.should_split_funcdef_with_rhs(&line) {
            vec![Transform::Lhs]
        } else if line.inside_brackets {
            vec![
                Transform::Delimiter,
                Transform::TypeIgnore,
                Transform::Standalone,
                Transform::Rhs,
            ]
        } else {
            vec![Transform::Rhs]
        };
        transformers.push(Transform::HugPower);
        for t in transformers {
            if let Ok(result) = self.run_transformer(&line, t, features, &line_str) {
                return result;
            }
        }
        if line.contains_standalone_comments(&self.tree) {
            return self.force_standalone_comment_split(&line);
        }
        vec![line]
    }

    fn apply_transform(&mut self, line: &Line, t: Transform, features: Feat) -> TResult {
        match t {
            Transform::Delimiter => self.delimiter_split(line, features),
            Transform::TypeIgnore => self.type_ignore_comment_split(line),
            Transform::Standalone => self.standalone_comment_split(line),
            Transform::Rhs => self.right_hand_split_with_omits(line, features),
            Transform::Lhs => self.left_hand_split(line),
            Transform::HugPower => self.hug_power_op(line).map(|l| vec![l]),
        }
    }

    fn run_transformer(
        &mut self,
        line: &Line,
        t: Transform,
        features: Feat,
        line_str: &str,
    ) -> TResult {
        let transformed = self.apply_transform(line, t, features)?;
        let mut result = Vec::new();
        for tl in transformed {
            if self.line_str(&tl) == line_str {
                return Err(());
            }
            result.extend(self.transform_line(tl, features));
        }
        if result.is_empty() {
            return Ok(result);
        }
        if features.force_optional_parentheses
            || t != Transform::Rhs
            || line.bt.invisible.is_empty()
            || line
                .bt
                .invisible
                .iter()
                .any(|b| !self.tree.value(*b).is_empty())
            || line.contains_multiline_strings(&self.tree)
            || result[0].contains_uncollapsable_type_comments(&self.tree)
            || result[0].contains_unsplittable_type_ignore(&self.tree)
            || self.short_enough(&result[0])
            || self.over_length_only_due_to_subscript_comment(&result[0])
            || line.leaves.iter().any(|l| self.tree.parent(*l).is_none())
        {
            return Ok(result);
        }
        let mut line_copy = line.clone_empty();
        let leaves = line.leaves.clone();
        append_leaves(
            &mut self.tree,
            &self.mode,
            &mut line_copy,
            line,
            &leaves,
            false,
        );
        let fop = Feat {
            force_optional_parentheses: true,
            ..features
        };
        let second_opinion = self.run_transformer(&line_copy, t, fop, line_str)?;
        if second_opinion.iter().all(|l| self.short_enough(l)) {
            result = second_opinion;
        }
        Ok(result)
    }

    fn over_length_only_due_to_subscript_comment(&self, line: &Line) -> bool {
        if line.leaves.is_empty() {
            return false;
        }
        let t = &self.tree;
        let first = line.leaves[0];
        let mut text = format!(
            "{}{}{}",
            t.n(first).prefix,
            " ".repeat(self.mode.indent * line.depth),
            t.value(first)
        );
        for &l in &line.leaves[1..] {
            text.push_str(&t.n(l).prefix);
            text.push_str(t.value(l));
        }
        if str_width(&text) > self.mode.line_length {
            return false;
        }
        for (leaf_id, comments) in &line.comments {
            if comments.is_empty() {
                continue;
            }
            if !line.leaves.contains(leaf_id) || t.ty(*leaf_id) != LSQB {
                return false;
            }
        }
        true
    }

    fn should_split_funcdef_with_rhs(&mut self, line: &Line) -> bool {
        let mut return_type_leaves = Vec::new();
        let mut in_return_type = false;
        for &leaf in &line.leaves {
            let t = self.tree.ty(leaf);
            if t == COLON {
                in_return_type = false;
            }
            if in_return_type {
                return_type_leaves.push(leaf);
            }
            if t == RARROW {
                in_return_type = true;
            }
        }
        let mut result = Line::new(line.depth, false);
        let to_track = get_leaves_inside_matching_brackets(&self.tree, &return_type_leaves);
        for &leaf in &return_type_leaves {
            self.append(&mut result, leaf, true, to_track.contains(&leaf));
        }
        let first_visible = return_type_leaves
            .iter()
            .find(|l| !self.tree.value(**l).is_empty())
            .copied();
        result.magic_trailing_comma.is_some()
            || (first_visible
                .map(|f| self.tree.ty(f) == STRING)
                .unwrap_or(false)
                && !self.short_enough(&result))
    }

    fn bracket_split_build_line(
        &mut self,
        mut leaves: Vec<Id>,
        original: &Line,
        opening_bracket: Id,
        component: Component,
    ) -> Line {
        let mut result = Line::new(original.depth, false);
        if component == Component::Body {
            result.inside_brackets = true;
            result.depth += 1;
            if self.ensure_trailing_comma(&leaves, original, opening_bracket) {
                for i in (0..leaves.len()).rev() {
                    if self.tree.ty(leaves[i]) == STANDALONE_COMMENT {
                        continue;
                    }
                    if self.tree.ty(leaves[i]) != COMMA {
                        let comma = self.tree.leaf(COMMA, ",");
                        leaves.insert(i + 1, comma);
                    }
                    break;
                }
            }
        }
        let to_track = if component == Component::Head {
            get_leaves_inside_matching_brackets(&self.tree, &leaves)
        } else {
            HashSet::default()
        };
        for &leaf in &leaves {
            self.append(&mut result, leaf, true, to_track.contains(&leaf));
            for &c in original.comments_after(leaf) {
                self.append(&mut result, c, true, false);
            }
        }
        if component == Component::Body && self.should_split_line(&result, opening_bracket) {
            result.should_split_rhs = true;
        }
        result
    }

    fn ensure_trailing_comma(&self, leaves: &[Id], original: &Line, opening_bracket: Id) -> bool {
        let t = &self.tree;
        if leaves.is_empty() {
            return false;
        }
        if original.is_import(t) {
            return true;
        }
        if !original.is_def(t) {
            return false;
        }
        if t.value(opening_bracket) != "(" {
            return false;
        }
        if leaves
            .iter()
            .any(|l| t.ty(*l) == COMMA && !(t.parent(*l).is_some() && is_part_of_annotation(t, *l)))
        {
            return false;
        }
        let Some(lwp) = leaves.iter().find(|l| t.parent(**l).is_some()).copied() else {
            return true;
        };
        if get_annotation_type(t, lwp) == Some(AnnotationType::Return) {
            return false;
        }
        if let Some(p) = t.parent(lwp) {
            if t.next_sibling(p).map(|s| t.ty(s)) == Some(VBAR) {
                return false;
            }
        }
        true
    }

    fn should_split_line(&self, line: &Line, opening_bracket: Id) -> bool {
        let t = &self.tree;
        let Some(obp) = t.parent(opening_bracket) else {
            return false;
        };
        if !matches!(t.value(opening_bracket), "" | "[" | "{" | "(") {
            return false;
        }
        let Some(&last_leaf) = line.leaves.last() else {
            return false;
        };
        let mut exclude = Vec::new();
        let mut trailing_comma = false;
        if t.ty(last_leaf) == COMMA {
            trailing_comma = true;
            exclude.push(last_leaf);
        }
        let Some(max_priority) = line.bt.max_delimiter_priority(&exclude) else {
            return false;
        };
        max_priority == COMMA_PRIORITY
            && ((self.mode.magic_trailing_comma && trailing_comma)
                || matches!(t.ty(obp), ATOM | IMPORT_FROM))
    }

    fn bracket_split_succeeded_or_raise(
        &self,
        head: &Line,
        body: &Line,
        tail: &Line,
    ) -> Result<(), ()> {
        let _ = head;
        let tail_len = tail.render(&self.tree, &self.mode).trim().chars().count();
        if !body.is_truthy() && tail_len < 3 {
            return Err(());
        }
        Ok(())
    }

    fn left_hand_split(&mut self, line: &Line) -> TResult {
        let mut head_leaves = Vec::new();
        let mut body_leaves = Vec::new();
        let mut tail_leaves = Vec::new();
        let mut matching_bracket: Option<Id> = None;
        for leaf_type in [LPAR, LSQB] {
            tail_leaves = Vec::new();
            body_leaves = Vec::new();
            head_leaves = Vec::new();
            #[derive(PartialEq)]
            enum Cur {
                Head,
                Body,
                Tail,
            }
            let mut cur = Cur::Head;
            matching_bracket = None;
            let mut depth = 0i32;
            for (index, &leaf) in line.leaves.iter().enumerate() {
                let lt = self.tree.ty(leaf);
                if index == 2 && lt == LSQB {
                    depth += 1;
                } else if depth > 0 {
                    if lt == LSQB {
                        depth += 1;
                    } else if lt == RSQB {
                        depth -= 1;
                    }
                }
                if let Some(mb) = matching_bracket {
                    if cur == Cur::Body
                        && is_closing(lt)
                        && self.tree.n(leaf).opening_bracket == Some(mb)
                        && !(leaf_type == LPAR && depth > 0)
                    {
                        ensure_visible(&mut self.tree, leaf);
                        ensure_visible(&mut self.tree, mb);
                        cur = if !body_leaves.is_empty() {
                            Cur::Tail
                        } else {
                            Cur::Head
                        };
                    }
                }
                match cur {
                    Cur::Head => head_leaves.push(leaf),
                    Cur::Body => body_leaves.push(leaf),
                    Cur::Tail => tail_leaves.push(leaf),
                }
                if cur == Cur::Head && lt == leaf_type && !(leaf_type == LPAR && depth > 0) {
                    matching_bracket = Some(leaf);
                    cur = Cur::Body;
                }
            }
            if matching_bracket.is_some() && !tail_leaves.is_empty() {
                break;
            }
        }
        let Some(mb) = matching_bracket else {
            return Err(());
        };
        if tail_leaves.is_empty() {
            return Err(());
        }
        let head = self.bracket_split_build_line(head_leaves, line, mb, Component::Head);
        let body = self.bracket_split_build_line(body_leaves, line, mb, Component::Body);
        let tail = self.bracket_split_build_line(tail_leaves, line, mb, Component::Tail);
        self.bracket_split_succeeded_or_raise(&head, &body, &tail)?;
        Ok([head, body, tail]
            .into_iter()
            .filter(|l| l.is_truthy())
            .collect())
    }

    fn first_right_hand_split(&mut self, line: &Line, omit: &HashSet<Id>) -> Result<RhsResult, ()> {
        let mut tail_leaves = Vec::new();
        let mut body_leaves = Vec::new();
        let mut head_leaves = Vec::new();
        #[derive(PartialEq, Clone, Copy)]
        enum Cur {
            Head,
            Body,
            Tail,
        }
        let mut cur = Cur::Tail;
        let mut opening_bracket: Option<Id> = None;
        let mut closing_bracket: Option<Id> = None;
        for &leaf in line.leaves.iter().rev() {
            if cur == Cur::Body && Some(leaf) == opening_bracket {
                cur = if !body_leaves.is_empty() {
                    Cur::Head
                } else {
                    Cur::Tail
                };
            }
            match cur {
                Cur::Head => head_leaves.push(leaf),
                Cur::Body => body_leaves.push(leaf),
                Cur::Tail => tail_leaves.push(leaf),
            }
            if cur == Cur::Tail && is_closing(self.tree.ty(leaf)) && !omit.contains(&leaf) {
                opening_bracket = self.tree.n(leaf).opening_bracket;
                closing_bracket = Some(leaf);
                cur = Cur::Body;
            }
        }
        let (Some(ob), Some(cb)) = (opening_bracket, closing_bracket) else {
            return Err(());
        };
        if head_leaves.is_empty() {
            return Err(());
        }
        tail_leaves.reverse();
        body_leaves.reverse();
        head_leaves.reverse();
        let head = self.bracket_split_build_line(head_leaves, line, ob, Component::Head);
        let body = self.bracket_split_build_line(body_leaves, line, ob, Component::Body);
        let tail = self.bracket_split_build_line(tail_leaves, line, ob, Component::Tail);
        self.bracket_split_succeeded_or_raise(&head, &body, &tail)?;
        Ok(RhsResult {
            head,
            body,
            tail,
            opening_bracket: ob,
            closing_bracket: cb,
        })
    }

    /// `_maybe_split_omitting_optional_parens`, iteratively. Each level may
    /// descend into a split that omits its optional parentheses (Black
    /// recurses, hundreds of levels deep on long chained assignments); a
    /// failure unwinds through the levels' exception handlers in order.
    fn maybe_split_omitting_optional_parens(
        &mut self,
        rhs: RhsResult,
        line: &Line,
        features: Feat,
        omit: &HashSet<Id>,
    ) -> TResult {
        let mut levels: Vec<RhsResult> = vec![rhs];
        let mut omit = omit.clone();
        let mut failed = false;
        loop {
            let cur = levels.last().unwrap();
            let t = &self.tree;
            let try_omitting = !features.force_optional_parentheses
                && t.ty(cur.opening_bracket) == LPAR
                && t.value(cur.opening_bracket).is_empty()
                && t.ty(cur.closing_bracket) == RPAR
                && t.value(cur.closing_bracket).is_empty()
                && !line.is_import(t)
                && can_omit_invisible_parens(t, &self.mode, cur, self.mode.line_length);
            if !try_omitting {
                break;
            }
            let mut omit2 = omit.clone();
            omit2.insert(cur.closing_bracket);
            match self.first_right_hand_split(line, &omit2) {
                Ok(rhs_oop) => {
                    if self.prefer_split_rhs_oop_over_rhs(&rhs_oop, levels.last().unwrap()) {
                        levels.push(rhs_oop);
                        omit = omit2;
                        continue;
                    }
                    break;
                }
                Err(()) => {
                    failed = true;
                    break;
                }
            }
        }
        while let Some(rhs) = levels.pop() {
            if failed && self.omission_failure_raises(&rhs, line) {
                // The error propagates to the enclosing level's handler.
                continue;
            }
            ensure_visible(&mut self.tree, rhs.opening_bracket);
            ensure_visible(&mut self.tree, rhs.closing_bracket);
            return Ok([rhs.head, rhs.body, rhs.tail]
                .into_iter()
                .filter(|l| l.is_truthy())
                .collect());
        }
        Err(())
    }

    /// The `except CannotSplit` handler of `_maybe_split_omitting_optional_parens`:
    /// whether the failure to split without the optional parens is final.
    fn omission_failure_raises(&self, rhs: &RhsResult, line: &Line) -> bool {
        if line.is_chained_assignment(&self.tree) {
            return false;
        }
        let in_case_block = self
            .tree
            .parent(rhs.opening_bracket)
            .and_then(|p| self.tree.parent(p))
            .map(|gp| self.tree.ty(gp) == CASE_BLOCK)
            .unwrap_or(false);
        if !can_be_split(&self.tree, &rhs.body) && !self.short_enough(&rhs.body) && !in_case_block {
            return true;
        }
        rhs.head.contains_multiline_strings(&self.tree)
            || rhs.tail.contains_multiline_strings(&self.tree)
    }

    fn prefer_split_rhs_oop_over_rhs(&self, rhs_oop: &RhsResult, rhs: &RhsResult) -> bool {
        let t = &self.tree;
        if rhs_oop.head.contains_unsplittable_type_ignore(t)
            || rhs_oop.body.contains_unsplittable_type_ignore(t)
            || rhs_oop.tail.contains_unsplittable_type_ignore(t)
        {
            return true;
        }
        let hl = &rhs.head.leaves;
        if !(hl.len() >= 2 && t.ty(hl[hl.len() - 2]) == EQUAL) {
            return true;
        }
        if !hl[..hl.len() - 1].iter().any(|l| is_bracket(t.ty(*l))) {
            return true;
        }
        let shorter = Mode {
            line_length: self.mode.line_length.saturating_sub(1),
            ..self.mode
        };
        if !is_line_short_enough(t, &shorter, &rhs.head, None) {
            return true;
        }
        if rhs.head.magic_trailing_comma.is_some() {
            return true;
        }
        let rhs_head_equal_count = hl.iter().filter(|l| t.ty(**l) == EQUAL).count();
        let rhs_oop_head_equal_count = rhs_oop
            .head
            .leaves
            .iter()
            .filter(|l| t.ty(**l) == EQUAL)
            .count();
        if rhs_head_equal_count > 1 && rhs_head_equal_count > rhs_oop_head_equal_count {
            return false;
        }
        let mut has_closing_bracket_after_assign = false;
        for &leaf in rhs_oop.head.leaves.iter().rev() {
            let lt = t.ty(leaf);
            if lt == EQUAL {
                break;
            }
            if is_closing(lt) {
                has_closing_bracket_after_assign = true;
                break;
            }
        }
        has_closing_bracket_after_assign
            || (rhs_oop.head.leaves.iter().any(|l| t.ty(*l) == EQUAL)
                && self.short_enough(&rhs_oop.head))
    }

    fn right_hand_split(&mut self, line: &Line, features: Feat, omit: &HashSet<Id>) -> TResult {
        let rhs = self.first_right_hand_split(line, omit)?;
        self.maybe_split_omitting_optional_parens(rhs, line, features, omit)
    }

    fn right_hand_split_with_omits(&mut self, line: &Line, features: Feat) -> TResult {
        let mut g = OmitGen {
            started: false,
            done: false,
            omit: HashSet::default(),
            length: 0,
            opening_bracket: None,
            closing_bracket: None,
            inner_brackets: HashSet::default(),
            index: 0,
            resume: None,
        };
        while let Some(omit) = self.omit_gen_next(&mut g, line) {
            let lines = self.right_hand_split(line, features, &omit)?;
            if lines.is_empty() {
                continue;
            }
            if self.short_enough(&lines[0])
                || (!omit.is_empty() && self.over_length_only_due_to_subscript_comment(&lines[0]))
            {
                return Ok(lines);
            }
        }
        self.right_hand_split(line, features, &HashSet::default())
    }

    /// `generate_trailers_to_omit` as a resumable state machine (the
    /// generator observes the effects of each split attempt on the leaves).
    fn omit_gen_next(&mut self, g: &mut OmitGen, line: &Line) -> Option<HashSet<Id>> {
        if g.done {
            return None;
        }
        if !g.started {
            g.started = true;
            g.length = self.mode.indent * line.depth;
            g.index = line.leaves.len() as isize - 1;
            if line.magic_trailing_comma.is_none() {
                return Some(g.omit.clone());
            }
        }
        let line_length = self.mode.line_length;
        if let Some((index, leaf)) = g.resume.take() {
            if !self.after_closing_bracket(g, line, index, leaf) {
                g.done = true;
                return None;
            }
        }
        while g.index >= 0 {
            let index = g.index as usize;
            g.index -= 1;
            let Some(leaf_length) = line.leaf_length(&self.tree, index) else {
                g.done = true;
                return None;
            };
            let leaf = line.leaves[index];
            g.length += leaf_length;
            if g.length > line_length {
                g.done = true;
                return None;
            }
            let n = self.tree.n(leaf);
            let has_inline_comment =
                leaf_length > n.value.chars().count() + n.prefix.chars().count();
            if n.ty == STANDALONE_COMMENT || has_inline_comment {
                g.done = true;
                return None;
            }
            let lt = n.ty;
            let prev = if index > 0 {
                Some(line.leaves[index - 1])
            } else {
                None
            };
            if let Some(ob) = g.opening_bracket {
                if leaf == ob {
                    g.opening_bracket = None;
                } else if is_closing(lt) {
                    if self.trailing_comma_blocks(line, prev, leaf) {
                        g.done = true;
                        return None;
                    }
                    g.inner_brackets.insert(leaf);
                }
            } else if is_closing(lt) {
                if prev.map(|p| is_opening(self.tree.ty(p))).unwrap_or(false) {
                    g.inner_brackets.insert(leaf);
                    continue;
                }
                if let Some(cb) = g.closing_bracket {
                    g.omit.insert(cb);
                    let inner: Vec<Id> = g.inner_brackets.drain().collect();
                    g.omit.extend(inner);
                    g.resume = Some((index, leaf));
                    return Some(g.omit.clone());
                }
                if !self.after_closing_bracket(g, line, index, leaf) {
                    g.done = true;
                    return None;
                }
            }
        }
        g.done = true;
        None
    }

    fn trailing_comma_blocks(&self, line: &Line, prev: Option<Id>, leaf: Id) -> bool {
        let t = &self.tree;
        match (prev, t.n(leaf).opening_bracket) {
            (Some(p), Some(ob)) if t.ty(p) == COMMA => {
                !is_one_sequence_between(t, ob, leaf, &line.leaves, (LPAR, RPAR))
            }
            _ => false,
        }
    }

    /// The tail of a closing-bracket iteration; false means `break`.
    fn after_closing_bracket(&self, g: &mut OmitGen, line: &Line, index: usize, leaf: Id) -> bool {
        let prev = if index > 0 {
            Some(line.leaves[index - 1])
        } else {
            None
        };
        if self.trailing_comma_blocks(line, prev, leaf) {
            return false;
        }
        if !self.tree.value(leaf).is_empty() {
            g.opening_bracket = self.tree.n(leaf).opening_bracket;
            g.closing_bracket = Some(leaf);
        }
        true
    }

    fn delimiter_split(&mut self, line: &Line, features: Feat) -> TResult {
        let Some(&last_leaf) = line.leaves.last() else {
            return Err(());
        };
        let bt = &line.bt;
        let Some(delimiter) = bt.max_delimiter_priority(&[last_leaf]) else {
            return Err(());
        };
        if delimiter == DOT_PRIORITY && bt.delimiter_count_with_priority(delimiter) == 1 {
            return Err(());
        }
        let migrate = |p: u8| p == STRING_PRIORITY || p == COMMA_PRIORITY;
        let mut out: Vec<Line> = Vec::new();
        let mut current = Line::new(line.depth, line.inside_brackets);
        let mut lowest_depth = i32::MAX;
        let mut trailing_comma_safe = true;
        let last_non_comment_leaf = (1..line.leaves.len())
            .rev()
            .find(|&i| self.tree.ty(line.leaves[i]) != STANDALONE_COMMENT);
        let delims = line.bt.delimiters.clone();
        for (leaf_idx, &leaf) in line.leaves.iter().enumerate() {
            self.append_to_line(&mut out, &mut current, line, leaf);
            let previous_priority = if leaf_idx > 0 {
                delims.get(&line.leaves[leaf_idx - 1]).copied()
            } else {
                None
            };
            if previous_priority != Some(delimiter) || migrate(delimiter) {
                for &c in line.comments_after(leaf) {
                    self.append_to_line(&mut out, &mut current, line, c);
                }
            }
            let bd = self.tree.n(leaf).bracket_depth;
            lowest_depth = lowest_depth.min(bd);
            if trailing_comma_safe && bd == lowest_depth {
                trailing_comma_safe = self.can_add_trailing_comma(leaf, features);
            }
            if self.tree.ty(last_leaf) == STANDALONE_COMMENT
                && Some(leaf_idx) == last_non_comment_leaf
            {
                self.safe_add_trailing_comma(trailing_comma_safe, delimiter, &mut current);
            }
            if delims.get(&leaf).copied() == Some(delimiter) {
                if leaf_idx + 1 < line.leaves.len() && !migrate(delimiter) {
                    let next = line.leaves[leaf_idx + 1];
                    for &c in line.comments_after(next) {
                        self.append_to_line(&mut out, &mut current, line, c);
                    }
                }
                out.push(std::mem::replace(
                    &mut current,
                    Line::new(line.depth, line.inside_brackets),
                ));
            }
        }
        if current.is_truthy() {
            self.safe_add_trailing_comma(trailing_comma_safe, delimiter, &mut current);
            out.push(current);
        }
        for l in &out {
            if let Some(&f) = l.leaves.first() {
                self.tree.nm(f).prefix.clear();
            }
        }
        Ok(out)
    }

    fn append_to_line(&mut self, out: &mut Vec<Line>, current: &mut Line, line: &Line, leaf: Id) {
        if current
            .append_safe(&mut self.tree, &self.mode, leaf, true)
            .is_err()
        {
            let full = std::mem::replace(current, Line::new(line.depth, line.inside_brackets));
            out.push(full);
            self.append(current, leaf, false, false);
        }
    }

    fn can_add_trailing_comma(&self, leaf: Id, features: Feat) -> bool {
        if is_vararg(&self.tree, leaf, &[TYPEDARGSLIST], &[]) {
            return features.trailing_comma_in_def;
        }
        if is_vararg(&self.tree, leaf, &[ARGLIST, ARGUMENT], &[]) {
            return features.trailing_comma_in_call;
        }
        true
    }

    fn safe_add_trailing_comma(&mut self, safe: bool, delimiter: u8, line: &mut Line) {
        if safe && delimiter == COMMA_PRIORITY {
            if let Some(&last) = line.leaves.last() {
                let lt = self.tree.ty(last);
                if lt != COMMA && lt != STANDALONE_COMMENT {
                    let comma = self.tree.leaf(COMMA, ",");
                    self.append(line, comma, false, false);
                }
            }
        }
    }

    fn standalone_comment_split(&mut self, line: &Line) -> TResult {
        if !line.contains_standalone_comments(&self.tree) {
            return Err(());
        }
        let mut out = Vec::new();
        let mut current = Line::new(line.depth, line.inside_brackets);
        for &leaf in &line.leaves {
            self.append_to_line(&mut out, &mut current, line, leaf);
            for &c in line.comments_after(leaf) {
                self.append_to_line(&mut out, &mut current, line, c);
            }
        }
        if current.is_truthy() {
            out.push(current);
        }
        for l in &out {
            if let Some(&f) = l.leaves.first() {
                self.tree.nm(f).prefix.clear();
            }
        }
        Ok(out)
    }

    fn type_ignore_comment_split(&mut self, line: &Line) -> TResult {
        if !line.contains_multiple_type_ignores_at_current_depth(&self.tree)
            || !line.leaves.iter().any(|l| self.tree.ty(*l) == DOT)
        {
            return Err(());
        }
        let mut out = Vec::new();
        let mut current = Line::new(line.depth, line.inside_brackets);
        for &leaf in &line.leaves {
            self.append(&mut current, leaf, true, false);
            let comments = line.comments_after(leaf).to_vec();
            for &c in &comments {
                self.append(&mut current, c, true, false);
            }
            if comments
                .iter()
                .any(|c| is_type_ignore_comment(&self.tree, *c))
            {
                out.push(std::mem::replace(
                    &mut current,
                    Line::new(line.depth, line.inside_brackets),
                ));
            }
        }
        if current.is_truthy() {
            out.push(current);
        }
        for l in &out {
            if let Some(&f) = l.leaves.first() {
                self.tree.nm(f).prefix.clear();
            }
        }
        Ok(out)
    }

    fn force_standalone_comment_split(&mut self, line: &Line) -> Vec<Line> {
        let mut out = Vec::new();
        let mut current = Line::new(line.depth, line.inside_brackets);
        for &leaf in &line.leaves {
            if !current.leaves.is_empty()
                && (self.tree.ty(leaf) == STANDALONE_COMMENT || current.is_comment(&self.tree))
                && !(current.is_comment(&self.tree) && self.tree.ty(leaf) == COLON)
            {
                out.push(std::mem::replace(
                    &mut current,
                    Line::new(line.depth, line.inside_brackets),
                ));
            }
            self.append(&mut current, leaf, true, false);
            for &c in line.comments_after(leaf) {
                self.append(&mut current, c, true, false);
            }
        }
        if current.is_truthy() {
            out.push(current);
        }
        out
    }

    fn hug_power_op(&mut self, line: &Line) -> Result<Line, ()> {
        if !line.leaves.iter().any(|l| self.tree.ty(*l) == DOUBLESTAR) {
            return Err(());
        }
        let n = line.leaves.len();
        let mut new_line = line.clone_empty();
        let mut should_hug = false;
        for idx in 0..n {
            let leaf = line.leaves[idx];
            let hug_this_leaf = should_hug;
            should_hug = 0 < idx
                && idx < n - 1
                && self.tree.ty(leaf) == DOUBLESTAR
                && self.is_simple_power_operand(line, idx - 1, -1)
                && self.tree.value(line.leaves[idx - 1]) != "lambda"
                && self.is_simple_power_operand(line, idx + 1, 1);
            let new_leaf = if hug_this_leaf || should_hug {
                let c = self.tree.clone_leaf(leaf);
                self.tree.nm(c).prefix.clear();
                c
            } else {
                leaf
            };
            self.append(&mut new_line, new_leaf, true, false);
            for &c in line.comments_after(leaf) {
                self.append(&mut new_line, c, true, false);
            }
        }
        Ok(new_line)
    }

    fn is_simple_power_operand(&self, line: &Line, index: usize, kind: i32) -> bool {
        let t = &self.tree;
        let start = line.leaves[index];
        let st = t.ty(start);
        if st == NAME || st == NUMBER {
            return self.is_simple_lookup(line, index, kind);
        }
        if matches!(st, PLUS | MINUS | TILDE) {
            if let Some(&next) = line.leaves.get(index + 1) {
                if matches!(t.ty(next), NAME | NUMBER) {
                    return self.is_simple_lookup(line, index + 1, 1);
                }
            }
        }
        false
    }

    fn is_simple_lookup(&self, line: &Line, index: usize, kind: i32) -> bool {
        let t = &self.tree;
        if kind == -1 {
            let mut contains_disallowed = false;
            let mut chain: Vec<u16> = Vec::new();
            let mut i = index as isize;
            while i >= 0 && (i as usize) < line.leaves.len() {
                let cur = t.ty(line.leaves[i as usize]);
                chain.push(cur);
                if !contains_disallowed && matches!(cur, RPAR | RSQB) {
                    contains_disallowed = true;
                }
                if !is_expression_chained(&chain) {
                    return !contains_disallowed;
                }
                i -= 1;
            }
            true
        } else {
            let mut i = index;
            while i < line.leaves.len() {
                let cur = line.leaves[i];
                let ct = t.ty(cur);
                if matches!(ct, LPAR | LSQB) {
                    return false;
                }
                if !(ct == NAME || ct == DOT) || (ct == NAME && t.value(cur) == "for") {
                    return true;
                }
                i += 1;
            }
            true
        }
    }
}

fn is_expression_chained(chain: &[u16]) -> bool {
    if chain.len() < 2 {
        return true;
    }
    let current = chain[chain.len() - 1];
    let past = chain[chain.len() - 2];
    match past {
        NAME => current == DOT,
        RPAR | RSQB => matches!(current, RSQB | RPAR),
        LPAR | LSQB => matches!(current, NAME | LPAR | LSQB),
        _ => false,
    }
}

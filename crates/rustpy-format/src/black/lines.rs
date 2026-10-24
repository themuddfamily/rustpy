//! Port of `black/lines.py`: logical lines, blank-line tracking and the
//! length heuristics used by the splitters.

use super::fxhash::{HashMap, HashSet};

use super::brackets::*;
use super::comments::contains_directive;
use super::fmtoff::FMT_ON;
use super::nodes::*;
use super::strings::str_width;
use super::tree::*;

#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub line_length: usize,
    pub string_normalization: bool,
    pub magic_trailing_comma: bool,
    /// Spaces per indentation level (Black: 4).
    pub indent: usize,
}

#[derive(Clone, Default, Debug)]
pub struct Line {
    pub depth: usize,
    pub leaves: Vec<Id>,
    /// Comments after each leaf, in insertion order (a Python dict).
    pub comments: Vec<(Id, Vec<Id>)>,
    pub bt: BracketTracker,
    pub inside_brackets: bool,
    pub should_split_rhs: bool,
    pub magic_trailing_comma: Option<Id>,
    subscript_cache: HashMap<Id, bool>,
}

fn has_value(tree: &Tree, leaf: Id) -> bool {
    let t = tree.ty(leaf);
    is_bracket(t) || !super::strings::py_strip(tree.value(leaf)).is_empty()
}

/// Leaf equality as in blib2to3 (`(type, value)`).
fn leaf_eq(tree: &Tree, a: Id, ty: u16, value: &str) -> bool {
    tree.ty(a) == ty && tree.value(a) == value
}

// ---- line properties shared by `Line` and `LineSnap`

pub fn leaves_is_comment(tree: &Tree, leaves: &[Id]) -> bool {
    leaves.len() == 1 && tree.ty(leaves[0]) == STANDALONE_COMMENT
}

pub fn leaves_is_decorator(tree: &Tree, leaves: &[Id]) -> bool {
    !leaves.is_empty() && tree.ty(leaves[0]) == AT
}

pub fn leaves_is_import(tree: &Tree, leaves: &[Id]) -> bool {
    !leaves.is_empty() && is_import(tree, leaves[0])
}

pub fn leaves_is_class(tree: &Tree, leaves: &[Id]) -> bool {
    !leaves.is_empty() && tree.ty(leaves[0]) == NAME && tree.value(leaves[0]) == "class"
}

pub fn leaves_is_def(tree: &Tree, leaves: &[Id]) -> bool {
    let Some(&first) = leaves.first() else {
        return false;
    };
    let second = leaves.get(1).copied();
    (tree.ty(first) == NAME && tree.value(first) == "def")
        || (tree.ty(first) == ASYNC
            && second
                .map(|s| tree.ty(s) == NAME && tree.value(s) == "def")
                .unwrap_or(false))
}

pub fn leaves_is_stub_def(tree: &Tree, leaves: &[Id]) -> bool {
    leaves_is_def(tree, leaves) && leaves.len() >= 4 && {
        let t = &leaves[leaves.len() - 4..];
        leaf_eq(tree, t[0], COLON, ":") && t[1..].iter().all(|l| leaf_eq(tree, *l, DOT, "."))
    }
}

pub fn leaves_is_docstring(tree: &Tree, leaves: &[Id]) -> bool {
    !leaves.is_empty() && is_docstring(tree, leaves[0])
}

/// `is_fmt_pass_converted`: the first leaf of the code a one-leaf line of
/// `# fmt: off` / `# fmt: skip` output stands for.
pub fn leaves_fmt_converted_first_leaf(tree: &Tree, leaves: &[Id]) -> Option<Id> {
    if leaves.len() != 1 || tree.ty(leaves[0]) != STANDALONE_COMMENT {
        return None;
    }
    tree.n(leaves[0]).fmt_first_leaf
}

pub fn leaves_opens_block(tree: &Tree, leaves: &[Id]) -> bool {
    leaves.last().map(|l| tree.ty(*l) == COLON).unwrap_or(false)
}

/// A line kept for later inspection by the empty line tracker.
#[derive(Clone, Debug)]
pub struct LineSnap {
    pub depth: usize,
    pub leaves: Vec<Id>,
}

impl LineSnap {
    pub fn is_comment(&self, t: &Tree) -> bool {
        leaves_is_comment(t, &self.leaves)
    }
    pub fn is_decorator(&self, t: &Tree) -> bool {
        leaves_is_decorator(t, &self.leaves)
    }
    pub fn is_import(&self, t: &Tree) -> bool {
        leaves_is_import(t, &self.leaves)
    }
    pub fn is_class(&self, t: &Tree) -> bool {
        leaves_is_class(t, &self.leaves)
    }
    pub fn is_def(&self, t: &Tree) -> bool {
        leaves_is_def(t, &self.leaves)
    }
    pub fn is_stub_def(&self, t: &Tree) -> bool {
        leaves_is_stub_def(t, &self.leaves)
    }
    pub fn is_docstring(&self, t: &Tree) -> bool {
        leaves_is_docstring(t, &self.leaves)
    }
    pub fn opens_block(&self, t: &Tree) -> bool {
        leaves_opens_block(t, &self.leaves)
    }
}

impl Line {
    pub fn new(depth: usize, inside_brackets: bool) -> Self {
        Line {
            depth,
            inside_brackets,
            ..Line::default()
        }
    }

    /// `line.clone()`: an empty line with the same settings.
    pub fn clone_empty(&self) -> Line {
        Line {
            depth: self.depth,
            inside_brackets: self.inside_brackets,
            should_split_rhs: self.should_split_rhs,
            magic_trailing_comma: self.magic_trailing_comma,
            ..Line::default()
        }
    }

    pub fn snap(&self) -> LineSnap {
        LineSnap {
            depth: self.depth,
            leaves: self.leaves.clone(),
        }
    }

    pub fn is_truthy(&self) -> bool {
        !self.leaves.is_empty() || !self.comments.is_empty()
    }

    pub fn append(
        &mut self,
        tree: &mut Tree,
        mode: &Mode,
        leaf: Id,
        preformatted: bool,
        track_bracket: bool,
    ) {
        if !has_value(tree, leaf) {
            return;
        }
        if tree.ty(leaf) == COLON && self.is_class_paren_empty(tree) {
            let n = self.leaves.len();
            self.leaves.truncate(n - 2);
        }
        if !self.leaves.is_empty() && !preformatted {
            let cs = self.is_complex_subscript(tree, leaf);
            let ws = whitespace(tree, leaf, cs);
            tree.nm(leaf).prefix.push_str(&ws);
        }
        if self.inside_brackets || !preformatted || track_bracket {
            self.bt.mark(tree, leaf);
            if mode.magic_trailing_comma {
                if self.has_magic_trailing_comma(tree, leaf) {
                    self.magic_trailing_comma = Some(leaf);
                }
            } else if self.has_magic_trailing_comma(tree, leaf)
                && !tree
                    .parent(leaf)
                    .map(|p| is_one_tuple(tree, p))
                    .unwrap_or(false)
            {
                self.remove_trailing_comma();
            }
        }
        if !self.append_comment(tree, leaf) {
            self.leaves.push(leaf);
        }
    }

    /// Like `append` but refuses invalid standalone comment structure.
    pub fn append_safe(
        &mut self,
        tree: &mut Tree,
        mode: &Mode,
        leaf: Id,
        preformatted: bool,
    ) -> Result<(), ()> {
        if self.bt.depth == 0 || self.bt.any_open_for_or_lambda() {
            if self.is_comment(tree) {
                return Err(());
            }
            if !self.leaves.is_empty() && tree.ty(leaf) == STANDALONE_COMMENT {
                return Err(());
            }
        }
        self.append(tree, mode, leaf, preformatted, false);
        Ok(())
    }

    pub fn is_comment(&self, tree: &Tree) -> bool {
        leaves_is_comment(tree, &self.leaves)
    }
    pub fn is_decorator(&self, tree: &Tree) -> bool {
        leaves_is_decorator(tree, &self.leaves)
    }
    pub fn is_import(&self, tree: &Tree) -> bool {
        leaves_is_import(tree, &self.leaves)
    }
    pub fn is_with_or_async_with_stmt(&self, tree: &Tree) -> bool {
        !self.leaves.is_empty() && is_with_or_async_with_stmt(tree, self.leaves[0])
    }
    pub fn is_class(&self, tree: &Tree) -> bool {
        leaves_is_class(tree, &self.leaves)
    }
    pub fn is_def(&self, tree: &Tree) -> bool {
        leaves_is_def(tree, &self.leaves)
    }
    pub fn is_docstring(&self, tree: &Tree) -> bool {
        leaves_is_docstring(tree, &self.leaves)
    }
    pub fn is_chained_assignment(&self, tree: &Tree) -> bool {
        self.leaves.iter().filter(|l| tree.ty(**l) == EQUAL).count() > 1
    }

    fn is_class_paren_empty(&self, tree: &Tree) -> bool {
        self.leaves.len() == 4
            && self.is_class(tree)
            && leaf_eq(tree, self.leaves[2], LPAR, "(")
            && leaf_eq(tree, self.leaves[3], RPAR, ")")
    }

    pub fn contains_standalone_comments(&self, tree: &Tree) -> bool {
        self.leaves
            .iter()
            .any(|l| tree.ty(*l) == STANDALONE_COMMENT)
    }

    pub fn contains_implicit_multiline_string_with_comments(&self, tree: &Tree) -> bool {
        let mut i = 0;
        while i < self.leaves.len() {
            let t = tree.ty(self.leaves[i]);
            let mut j = i;
            while j < self.leaves.len() && tree.ty(self.leaves[j]) == t {
                j += 1;
            }
            if t == STRING && j - i > 1 {
                for &leaf in &self.leaves[i..j] {
                    if !self.comments_after(leaf).is_empty() {
                        return true;
                    }
                }
            }
            i = j;
        }
        false
    }

    pub fn contains_uncollapsable_type_comments(&self, tree: &Tree) -> bool {
        let mut ignored_ids = Vec::new();
        let n = self.leaves.len();
        if n == 0 {
            return false;
        }
        let mut last_leaf = self.leaves[n - 1];
        ignored_ids.push(last_leaf);
        if tree.ty(last_leaf) == COMMA
            || (tree.ty(last_leaf) == RPAR && tree.value(last_leaf).is_empty())
        {
            if n < 2 {
                return false;
            }
            last_leaf = self.leaves[n - 2];
            ignored_ids.push(last_leaf);
        }
        let mut comment_seen = false;
        for (leaf_id, comments) in &self.comments {
            for &comment in comments {
                if is_type_comment(tree, comment)
                    && (comment_seen
                        || (!is_type_ignore_comment(tree, comment)
                            && !ignored_ids.contains(leaf_id)))
                {
                    return true;
                }
                comment_seen = true;
            }
        }
        false
    }

    pub fn contains_multiple_type_ignores_at_current_depth(&self, tree: &Tree) -> bool {
        let mut count = 0;
        for &leaf in &self.leaves {
            if tree.n(leaf).bracket_depth != 0 {
                continue;
            }
            for &c in self.comments_after(leaf) {
                if is_type_ignore_comment(tree, c) {
                    count += 1;
                    if count > 1 {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn contains_unsplittable_type_ignore(&self, tree: &Tree) -> bool {
        if self.leaves.is_empty() {
            return false;
        }
        let first_line = self
            .leaves
            .iter()
            .map(|l| tree.n(*l).lineno)
            .find(|n| *n != 0)
            .unwrap_or(0);
        let last_line = self
            .leaves
            .iter()
            .rev()
            .map(|l| tree.n(*l).lineno)
            .find(|n| *n != 0)
            .unwrap_or(0);
        if first_line == last_line && first_line != 0 {
            let start = self.leaves.len().saturating_sub(2);
            for &node in &self.leaves[start..] {
                for &c in self.comments_after(node) {
                    if is_type_ignore_comment(tree, c) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn contains_multiline_strings(&self, tree: &Tree) -> bool {
        self.leaves.iter().any(|l| is_multiline_string(tree, *l))
    }

    pub fn has_magic_trailing_comma(&self, tree: &Tree, closing: Id) -> bool {
        let ct = tree.ty(closing);
        if !(is_closing(ct)
            && self
                .leaves
                .last()
                .map(|l| tree.ty(*l) == COMMA)
                .unwrap_or(false))
        {
            return false;
        }
        if ct == RBRACE {
            return true;
        }
        let opening = tree.n(closing).opening_bracket;
        if ct == RSQB {
            if tree.parent_ty(closing) == Some(TRAILER)
                && opening.is_some()
                && is_one_sequence_between(
                    tree,
                    opening.unwrap(),
                    closing,
                    &self.leaves,
                    (LSQB, RSQB),
                )
            {
                return false;
            }
            return true;
        }
        if self.is_import(tree) {
            return true;
        }
        if let Some(o) = opening {
            if !is_one_sequence_between(tree, o, closing, &self.leaves, (LPAR, RPAR)) {
                return true;
            }
        }
        false
    }

    fn comments_entry(&mut self, leaf: Id) -> &mut Vec<Id> {
        if let Some(i) = self.comments.iter().position(|(k, _)| *k == leaf) {
            &mut self.comments[i].1
        } else {
            self.comments.push((leaf, Vec::new()));
            let n = self.comments.len();
            &mut self.comments[n - 1].1
        }
    }

    pub fn append_comment(&mut self, tree: &mut Tree, comment: Id) -> bool {
        let ct = tree.ty(comment);
        if ct == STANDALONE_COMMENT && self.bt.any_open_brackets() {
            tree.nm(comment).prefix.clear();
            return false;
        }
        if ct != COMMENT {
            return false;
        }
        if self.leaves.is_empty() {
            let n = tree.nm(comment);
            n.ty = STANDALONE_COMMENT;
            n.prefix.clear();
            return false;
        }
        let mut last_leaf = *self.leaves.last().unwrap();
        if tree.ty(last_leaf) == RPAR
            && tree.value(last_leaf).is_empty()
            && tree
                .parent(last_leaf)
                .map(|p| tree.leaves(p).len() <= 3)
                .unwrap_or(false)
            && !is_type_comment(tree, comment)
            && !self.contains_standalone_comments(tree)
            && self.leaves.len() >= 2
        {
            last_leaf = self.leaves[self.leaves.len() - 2];
        }
        self.comments_entry(last_leaf).push(comment);
        true
    }

    pub fn comments_after(&self, leaf: Id) -> &[Id] {
        self.comments
            .iter()
            .find(|(k, _)| *k == leaf)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn comments_get(&self, leaf: Id) -> Option<&Vec<Id>> {
        self.comments
            .iter()
            .find(|(k, _)| *k == leaf)
            .map(|(_, v)| v)
    }

    pub fn remove_trailing_comma(&mut self) {
        let trailing_comma = self.leaves.pop().unwrap();
        let moved = match self.comments.iter().position(|(k, _)| *k == trailing_comma) {
            Some(i) => self.comments.remove(i).1,
            None => Vec::new(),
        };
        let last = *self.leaves.last().unwrap();
        self.comments_entry(last).extend(moved);
    }

    fn is_complex_subscript(&mut self, tree: &Tree, leaf: Id) -> bool {
        let Some(open_lsqb) = self.bt.get_open_lsqb() else {
            return false;
        };
        let mut subscript_start = tree.next_sibling(open_lsqb);
        if let Some(s) = subscript_start {
            if !tree.is_leaf(s) {
                if tree.ty(s) == LISTMAKER {
                    return false;
                }
                if tree.ty(s) == SUBSCRIPTLIST {
                    subscript_start = child_towards(tree, s, leaf);
                }
            }
        }
        let Some(start) = subscript_start else {
            return false;
        };
        if let Some(c) = self.subscript_cache.get(&start) {
            return *c;
        }
        let v = tree
            .pre_order(start)
            .iter()
            .any(|n| is_test_descendant(tree.ty(*n)));
        self.subscript_cache.insert(start, v);
        v
    }

    /// `enumerate_with_length`: (index, leaf, length) until a multiline string.
    pub fn enumerate_with_length(&self, tree: &Tree, reversed: bool) -> Vec<(usize, Id, usize)> {
        let mut out = Vec::new();
        let n = self.leaves.len();
        for k in 0..n {
            let index = if reversed { n - 1 - k } else { k };
            match self.leaf_length(tree, index) {
                Some(len) => out.push((index, self.leaves[index], len)),
                None => break,
            }
        }
        out
    }

    /// Length of a leaf with its prefix and comments; `None` for multiline strings.
    pub fn leaf_length(&self, tree: &Tree, index: usize) -> Option<usize> {
        let leaf = self.leaves[index];
        let n = tree.n(leaf);
        if n.value.contains('\n') {
            return None;
        }
        let mut length = n.prefix.chars().count() + n.value.chars().count();
        for &c in self.comments_after(leaf) {
            length += tree.value(c).chars().count();
        }
        Some(length)
    }

    /// `str(line)` (with the trailing newline).
    pub fn render(&self, tree: &Tree, mode: &Mode) -> String {
        if !self.is_truthy() {
            return "\n".into();
        }
        let mut res = String::new();
        if let Some(&first) = self.leaves.first() {
            res.push_str(&tree.n(first).prefix);
            res.push_str(&" ".repeat(mode.indent * self.depth));
            res.push_str(&tree.n(first).value);
            for &leaf in &self.leaves[1..] {
                res.push_str(&tree.n(leaf).prefix);
                res.push_str(&tree.n(leaf).value);
            }
        }
        for (_, comments) in &self.comments {
            for &c in comments {
                res.push_str(&tree.n(c).prefix);
                res.push_str(&tree.n(c).value);
            }
        }
        res.push('\n');
        res
    }

    /// Display width of the rendered line without building it, or `None`
    /// when a piece contains a newline. Stops counting once over `limit`.
    pub fn single_line_width(&self, tree: &Tree, mode: &Mode, limit: usize) -> Option<usize> {
        fn piece(s: &str) -> Option<usize> {
            if s.contains('\n') {
                None
            } else {
                Some(str_width(s))
            }
        }
        let mut width = 0usize;
        if let Some(&first) = self.leaves.first() {
            width += piece(&tree.n(first).prefix)? + piece(&tree.n(first).value)?;
            width += mode.indent * self.depth;
            for &leaf in &self.leaves[1..] {
                width += piece(&tree.n(leaf).prefix)? + piece(&tree.n(leaf).value)?;
                if width > limit {
                    return Some(width);
                }
            }
        }
        for (_, comments) in &self.comments {
            for &c in comments {
                width += piece(&tree.n(c).prefix)? + piece(&tree.n(c).value)?;
            }
        }
        Some(width)
    }

    /// `line_to_string`: the rendered line without surrounding newlines.
    pub fn to_str(&self, tree: &Tree, mode: &Mode) -> String {
        self.render(tree, mode).trim_matches('\n').to_string()
    }
}

/// `append_leaves`: copies `leaves` into `new_line`, replacing them in the tree.
pub fn append_leaves(
    tree: &mut Tree,
    mode: &Mode,
    new_line: &mut Line,
    old_line: &Line,
    leaves: &[Id],
    preformatted: bool,
) {
    for &old in leaves {
        let ty = tree.ty(old);
        let value = tree.value(old).to_string();
        let new = tree.leaf(ty, value);
        tree.nm(new).lineno = tree.n(old).lineno;
        if tree.parent(old).is_some() {
            tree.replace(old, new);
        }
        new_line.append(tree, mode, new, preformatted, false);
        for &c in old_line.comments_after(old) {
            new_line.append(tree, mode, c, true, false);
        }
    }
}

fn tree_eq(tree: &Tree, a: Id, b: Id) -> bool {
    if a == b {
        return true;
    }
    if tree.ty(a) != tree.ty(b) {
        return false;
    }
    if tree.is_leaf(a) {
        return tree.value(a) == tree.value(b);
    }
    let (ca, cb) = (tree.children(a), tree.children(b));
    ca.len() == cb.len() && ca.iter().zip(cb).all(|(x, y)| tree_eq(tree, *x, *y))
}

pub fn is_line_short_enough(tree: &Tree, mode: &Mode, line: &Line, line_str: Option<&str>) -> bool {
    if line_str.is_none() {
        if line.contains_standalone_comments(tree) {
            return false;
        }
        if let Some(width) = line.single_line_width(tree, mode, mode.line_length) {
            return width <= mode.line_length;
        }
    }
    let owned;
    let line_str = match line_str {
        Some(s) => s,
        None => {
            owned = line.to_str(tree, mode);
            &owned
        }
    };
    if line.contains_standalone_comments(tree) {
        return false;
    }
    if !line_str.contains('\n') {
        return str_width(line_str) <= mode.line_length;
    }
    let first = line_str.split('\n').next().unwrap_or("");
    let last = line_str.rsplit('\n').next().unwrap_or("");
    if str_width(first) > mode.line_length || str_width(last) > mode.line_length {
        return false;
    }
    let mut commas: Vec<i64> = Vec::new();
    let mut multiline_string: Option<Id> = None;
    let mut contexts: Vec<Id> = Vec::new();
    let leaf_ids: HashSet<Id> = line.leaves.iter().copied().collect();
    let mut max_level: Option<i32> = None; // None = infinity
    for (i, &leaf) in line.leaves.iter().enumerate() {
        let depth = tree.n(leaf).bracket_depth;
        if max_level.is_none() {
            let mut had_comma = None;
            if (depth + 1) as usize > commas.len() {
                commas.push(0);
            } else if ((depth + 1) as usize) < commas.len() {
                had_comma = commas.pop();
            }
            if let (Some(hc), Some(mls)) = (had_comma, multiline_string) {
                if tree.n(mls).bracket_depth == depth + 1 {
                    max_level = Some(depth);
                    if hc > 0 {
                        return false;
                    }
                }
            }
        }
        if max_level.map(|m| depth <= m).unwrap_or(true) && tree.ty(leaf) == COMMA {
            let prev = tree.prev_sibling(leaf);
            let in_ignore = match prev {
                None => true,
                Some(p) => contexts.iter().any(|c| tree_eq(tree, *c, p)),
            };
            if (line.inside_brackets || depth > 0) && (i != line.leaves.len() - 1 || !in_ignore) {
                let d = depth.max(0) as usize;
                if commas.len() <= d {
                    commas.resize(d + 1, 0);
                }
                commas[d] += 1;
            }
        }
        if let Some(m) = max_level {
            max_level = Some(m.min(depth));
        }
        if is_multiline_string(tree, leaf) {
            if let Some(p) = tree.parent(leaf) {
                if tree.ty(p) == TEST || tree.parent_ty(p) == Some(DICTSETMAKER) {
                    return false;
                }
            }
            if !contexts.is_empty() {
                return false;
            }
            multiline_string = Some(leaf);
            let mut ctx = leaf;
            loop {
                let fl = tree.first_leaf(ctx);
                let ll = tree.last_leaf(ctx);
                let inside = fl.map(|f| leaf_ids.contains(&f)).unwrap_or(false)
                    && ll.map(|l| leaf_ids.contains(&l)).unwrap_or(false);
                if !inside {
                    break;
                }
                contexts.push(ctx);
                match tree.parent(ctx) {
                    Some(p) => ctx = p,
                    None => break,
                }
            }
        }
    }
    if contexts.is_empty() {
        return true;
    }
    commas.iter().all(|v| *v == 0)
}

pub fn can_be_split(tree: &Tree, line: &Line) -> bool {
    let leaves = &line.leaves;
    if leaves.len() < 2 {
        return false;
    }
    if tree.ty(leaves[0]) == STRING && tree.ty(leaves[1]) == DOT {
        let mut call_count = 0;
        let mut dot_count = 0;
        let next = *leaves.last().unwrap();
        for &leaf in leaves[..leaves.len() - 1].iter().rev() {
            let t = tree.ty(leaf);
            if is_opening(t) {
                if !is_closing(tree.ty(next)) {
                    return false;
                }
                call_count += 1;
            } else if t == DOT {
                dot_count += 1;
            } else if t == NAME {
                if !(tree.ty(next) == DOT || is_opening(tree.ty(next))) {
                    return false;
                }
            } else if !is_closing(t) {
                return false;
            }
            if dot_count > 1 && call_count > 1 {
                return false;
            }
        }
    }
    true
}

pub struct RhsResult {
    pub head: Line,
    pub body: Line,
    pub tail: Line,
    pub opening_bracket: Id,
    pub closing_bracket: Id,
}

pub fn can_omit_invisible_parens(
    tree: &Tree,
    mode: &Mode,
    rhs: &RhsResult,
    line_length: usize,
) -> bool {
    let line = &rhs.body;
    if line.contains_multiple_type_ignores_at_current_depth(tree)
        && line.bt.delimiters.is_empty()
        && line.leaves.iter().any(|l| tree.ty(*l) == DOT)
    {
        return false;
    }
    if tree.ty(rhs.opening_bracket) == LPAR
        && tree.value(rhs.opening_bracket).is_empty()
        && rhs
            .head
            .comments_get(rhs.opening_bracket)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    {
        return false;
    }
    if let Some(&ob) = rhs.head.leaves.last() {
        let head_comments = rhs.head.comments_after(ob);
        if !head_comments.is_empty() {
            let ti_head = head_comments
                .iter()
                .any(|c| is_type_ignore_comment(tree, *c));
            let other_head = head_comments
                .iter()
                .any(|c| !is_type_ignore_comment(tree, *c));
            let mut ti_body = false;
            let mut other_body = false;
            for &leaf in &rhs.body.leaves {
                for &c in rhs.body.comments_after(leaf) {
                    if is_type_ignore_comment(tree, c) {
                        ti_body = true;
                    } else {
                        other_body = true;
                    }
                }
            }
            if (ti_head && other_body) || (other_head && ti_body) {
                return false;
            }
        }
    }
    let mut closing_bracket: Option<Id> = None;
    for &leaf in line.leaves.iter().rev() {
        if let Some(cb) = closing_bracket {
            if Some(leaf) == tree.n(cb).opening_bracket {
                closing_bracket = None;
            }
        }
        if tree.ty(leaf) == STANDALONE_COMMENT && closing_bracket.is_none() {
            return false;
        }
        if closing_bracket.is_none() && is_closing(tree.ty(leaf)) && !tree.value(leaf).is_empty() {
            if let Some(ob) = tree.n(leaf).opening_bracket {
                let (oty, oval) = (tree.ty(ob), tree.value(ob));
                if line.leaves.iter().any(|l| leaf_eq(tree, *l, oty, oval)) {
                    closing_bracket = Some(leaf);
                }
            }
        }
    }
    let bt = &line.bt;
    if bt.delimiters.is_empty() {
        return true;
    }
    let max_priority = bt.max_delimiter_priority(&[]).unwrap_or(0);
    let delimiter_count = bt.delimiter_count_with_priority(max_priority);
    if delimiter_count > 1 {
        return false;
    }
    if delimiter_count == 1
        && max_priority == COMMA_PRIORITY
        && rhs.head.is_with_or_async_with_stmt(tree)
    {
        return false;
    }
    if max_priority == DOT_PRIORITY {
        return true;
    }
    if line.leaves.len() < 2 {
        return false;
    }
    let first = line.leaves[0];
    let second = line.leaves[1];
    if is_opening(tree.ty(first))
        && !is_closing(tree.ty(second))
        && can_omit_opening_paren(tree, mode, line, first, line_length)
    {
        return true;
    }
    let penultimate = line.leaves[line.leaves.len() - 2];
    let last = line.leaves[line.leaves.len() - 1];
    let lt = tree.ty(last);
    if lt == RPAR
        || lt == RBRACE
        || (lt == RSQB && tree.parent(last).is_some() && tree.parent_ty(last) != Some(TRAILER))
    {
        if is_opening(tree.ty(penultimate)) {
            return false;
        }
        if is_multiline_string(tree, first) {
            return true;
        }
        if can_omit_closing_paren(tree, mode, line, last, line_length) {
            return true;
        }
    }
    false
}

fn can_omit_opening_paren(
    tree: &Tree,
    mode: &Mode,
    line: &Line,
    first: Id,
    line_length: usize,
) -> bool {
    let mut remainder = false;
    let mut length = mode.indent * line.depth;
    let mut index: isize = -1;
    let items = line.enumerate_with_length(tree, false);
    let complete = items.len() == line.leaves.len();
    for (i, leaf, leaf_length) in items {
        index = i as isize;
        if is_closing(tree.ty(leaf)) && tree.n(leaf).opening_bracket == Some(first) {
            remainder = true;
        }
        if remainder {
            length += leaf_length;
            if length > line_length {
                return false;
            }
            if is_opening(tree.ty(leaf)) {
                remainder = false;
            }
        }
    }
    let _ = complete;
    line.leaves.len() as isize == index + 1
}

fn can_omit_closing_paren(
    tree: &Tree,
    mode: &Mode,
    line: &Line,
    last: Id,
    line_length: usize,
) -> bool {
    let mut length = mode.indent * line.depth;
    let mut seen_other_brackets = false;
    let target = tree.n(last).opening_bracket;
    for (_, leaf, leaf_length) in line.enumerate_with_length(tree, false) {
        length += leaf_length;
        if Some(leaf) == target {
            if seen_other_brackets || length <= line_length {
                return true;
            }
        } else if is_opening(tree.ty(leaf)) {
            seen_other_brackets = true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Empty lines

pub struct LinesBlock {
    pub previous_block: Option<usize>,
    pub original: LineSnap,
    pub before: isize,
    pub content_lines: Vec<String>,
    pub after: isize,
    pub form_feed: bool,
}

impl LinesBlock {
    pub fn all_lines(&self, out: &mut String) {
        let before = self.before.max(0) as usize;
        if self.form_feed {
            out.push_str(&"\n".repeat(before.saturating_sub(1)));
            out.push('\x0c');
            out.push('\n');
        } else {
            out.push_str(&"\n".repeat(before));
        }
        for l in &self.content_lines {
            out.push_str(l);
        }
        out.push_str(&"\n".repeat(self.after.max(0) as usize));
    }
}

#[derive(Default)]
pub struct EmptyLineTracker {
    previous_line: Option<LineSnap>,
    previous_block: Option<usize>,
    previous_defs: Vec<LineSnap>,
    semantic_leading_comment: Option<usize>,
}

impl EmptyLineTracker {
    pub fn maybe_empty_lines(
        &mut self,
        tree: &mut Tree,
        blocks: &mut Vec<LinesBlock>,
        line: &Line,
    ) -> usize {
        let form_feed = line.depth == 0
            && !line.leaves.is_empty()
            && tree.n(line.leaves[0]).prefix.contains("\x0c\n");
        let (before, after) = self.inner(tree, blocks, line);
        let previous_after = self.previous_block.map(|b| blocks[b].after).unwrap_or(0);
        let mut before = (before - previous_after).max(0);
        if self.line_is_module_docstring(tree, blocks, line) {
            before = 1;
        }
        blocks.push(LinesBlock {
            previous_block: self.previous_block,
            original: line.snap(),
            before,
            content_lines: Vec::new(),
            after,
            form_feed,
        });
        let block = blocks.len() - 1;
        if line.is_comment(tree) {
            let ok = match &self.previous_line {
                None => true,
                Some(prev) => {
                    !prev.is_decorator(tree)
                        && (!prev.is_comment(tree) || before > 0)
                        && (self.semantic_leading_comment.is_none() || before > 0)
                }
            };
            if ok {
                self.semantic_leading_comment = Some(block);
            }
        } else if !line.is_decorator(tree) || before > 0 {
            self.semantic_leading_comment = None;
        }
        self.previous_line = Some(line.snap());
        self.previous_block = Some(block);
        block
    }

    fn line_is_module_docstring(&self, tree: &Tree, blocks: &[LinesBlock], line: &Line) -> bool {
        let Some(mut pb) = self.previous_block else {
            return false;
        };
        let o = &blocks[pb].original;
        if o.leaves.len() != 1
            || !o.is_docstring(tree)
            || o.depth != 0
            || line.is_class(tree)
            || line.is_def(tree)
        {
            return false;
        }
        while let Some(p) = blocks[pb].previous_block {
            pb = p;
            if !blocks[pb].original.is_comment(tree) {
                return false;
            }
        }
        true
    }

    fn inner(&mut self, tree: &mut Tree, blocks: &mut [LinesBlock], line: &Line) -> (isize, isize) {
        let max_allowed: usize = if line.depth == 0 { 2 } else { 1 };
        let converted = leaves_fmt_converted_first_leaf(tree, &line.leaves).is_some();
        let mut before: isize = if let Some(&first) = line.leaves.first() {
            let mut b = tree.n(first).prefix.matches('\n').count();
            // Blank lines ending a `# fmt: off` region are part of what was opted out.
            let fmt_on_after_off = tree.ty(first) == STANDALONE_COMMENT
                && contains_directive(tree.value(first), FMT_ON)
                && self
                    .previous_line
                    .as_ref()
                    .map(|p| leaves_fmt_converted_first_leaf(tree, &p.leaves).is_some())
                    .unwrap_or(false);
            if !converted && !fmt_on_after_off {
                b = b.min(max_allowed);
            }
            tree.nm(first).prefix.clear();
            b as isize
        } else {
            0
        };
        let user_had_newline = before > 0;
        let depth = line.depth;
        let mut previous_def: Option<LineSnap> = None;
        while self
            .previous_defs
            .last()
            .map(|d| d.depth >= depth)
            .unwrap_or(false)
        {
            previous_def = self.previous_defs.pop();
        }
        if line.is_def(tree) || line.is_class(tree) {
            self.previous_defs.push(line.snap());
        }
        let Some(prev) = self.previous_line.clone() else {
            return (0, 0);
        };
        if line.is_docstring(tree) {
            if prev.is_class(tree) {
                return (0, 1);
            }
            if prev.opens_block(tree) && prev.is_def(tree) {
                return (0, 0);
            }
        }
        if let Some(pd) = &previous_def {
            // A dependent clause (`else:`, `except:` …) after an indented def is
            // not really top-level: one blank line, like inside blocks.
            let dependent_clause = pd.depth > 0
                && line
                    .leaves
                    .last()
                    .map(|l| tree.ty(*l) == COLON)
                    .unwrap_or(false)
                && !matches!(
                    tree.value(line.leaves[0]),
                    "with" | "try" | "for" | "while" | "if" | "match"
                );
            before = if depth > 0 || dependent_clause { 1 } else { 2 };
        }
        if line.is_decorator(tree) || line.is_def(tree) || line.is_class(tree) {
            return self.class_or_def(tree, blocks, line, &prev, before, user_had_newline);
        }
        let converted_import = leaves_fmt_converted_first_leaf(tree, &line.leaves)
            .map(|l| is_import(tree, l))
            .unwrap_or(false);
        if prev.is_import(tree)
            && prev.depth == 0
            && line.depth == 0
            && !line.is_import(tree)
            && !converted_import
        {
            return (1, 0);
        }
        if prev.is_import(tree) && !line.is_import(tree) && !converted_import && depth == prev.depth
        {
            return (if before > 0 { before } else { 1 }, 0);
        }
        (before, 0)
    }

    fn class_or_def(
        &mut self,
        tree: &Tree,
        blocks: &mut [LinesBlock],
        line: &Line,
        prev: &LineSnap,
        before: isize,
        user_had_newline: bool,
    ) -> (isize, isize) {
        if prev.is_decorator(tree) {
            return (0, 0);
        }
        if prev.depth < line.depth && (prev.is_class(tree) || prev.is_def(tree)) {
            return (if user_had_newline { 1 } else { 0 }, 0);
        }
        let mut comment_to_add_newlines = None;
        if prev.is_comment(tree) && prev.depth == line.depth && before == 0 {
            let slc = self.semantic_leading_comment;
            let ok = slc
                .map(|s| {
                    let b = &blocks[s];
                    match b.previous_block {
                        Some(pb) => {
                            let o = &blocks[pb].original;
                            !o.is_class(tree) && !o.opens_block(tree) && b.before <= 1
                        }
                        None => false,
                    }
                })
                .unwrap_or(false);
            if ok {
                comment_to_add_newlines = slc;
            } else {
                return (0, 0);
            }
        }
        let mut newlines: isize = if line.depth > 0 { 1 } else { 2 };
        if prev.is_stub_def(tree) && !user_had_newline {
            newlines = 0;
        }
        if let Some(c) = comment_to_add_newlines {
            if let Some(pb) = blocks[c].previous_block {
                let pa = blocks[pb].after;
                blocks[c].before = blocks[c].before.max(newlines) - pa;
                newlines = 0;
            }
        }
        (newlines, 0)
    }
}

//! Port of `black/brackets.py`: bracket depth and delimiter priorities.

use super::fxhash::{HashMap, HashSet};

use super::nodes::*;
use super::tree::*;

pub const COMPREHENSION_PRIORITY: u8 = 20;
pub const COMMA_PRIORITY: u8 = 18;
pub const TERNARY_PRIORITY: u8 = 16;
pub const LOGIC_PRIORITY: u8 = 14;
pub const STRING_PRIORITY: u8 = 12;
pub const COMPARATOR_PRIORITY: u8 = 10;
pub const DOT_PRIORITY: u8 = 1;

fn math_priority(t: u16) -> u8 {
    match t {
        VBAR => 9,
        CIRCUMFLEX => 8,
        AMPER => 7,
        LEFTSHIFT | RIGHTSHIFT => 6,
        PLUS | MINUS => 5,
        STAR | SLASH | DOUBLESLASH | PERCENT | AT => 4,
        TILDE => 3,
        DOUBLESTAR => 2,
        _ => 0,
    }
}

#[derive(Clone, Default, Debug)]
pub struct BracketTracker {
    pub depth: i32,
    pub bracket_match: HashMap<(i32, u16), Id>,
    pub delimiters: HashMap<Id, u8>,
    pub previous: Option<Id>,
    for_loop_depths: Vec<i32>,
    lambda_argument_depths: Vec<i32>,
    pub invisible: Vec<Id>,
}

impl BracketTracker {
    pub fn mark(&mut self, tree: &mut Tree, leaf: Id) {
        let t = tree.ty(leaf);
        if t == COMMENT {
            return;
        }
        if self.depth == 0 && is_closing(t) && !self.bracket_match.contains_key(&(self.depth, t)) {
            return;
        }
        self.maybe_decrement_after_for_loop_variable(tree, leaf);
        self.maybe_decrement_after_lambda_arguments(tree, leaf);
        if is_closing(t) {
            self.depth -= 1;
            let opening = self.bracket_match.remove(&(self.depth, t));
            tree.nm(leaf).opening_bracket = opening;
            if tree.value(leaf).is_empty() {
                self.invisible.push(leaf);
            }
        }
        tree.nm(leaf).bracket_depth = self.depth;
        if self.depth == 0 {
            let delim = is_split_before_delimiter(tree, leaf, self.previous);
            if let (true, Some(previous)) = (delim > 0, self.previous) {
                self.delimiters.insert(previous, delim);
            } else {
                let delim = is_split_after_delimiter(tree, leaf);
                if delim > 0 {
                    self.delimiters.insert(leaf, delim);
                }
            }
        }
        if is_opening(t) {
            self.bracket_match.insert((self.depth, closing_of(t)), leaf);
            self.depth += 1;
            if tree.value(leaf).is_empty() {
                self.invisible.push(leaf);
            }
        }
        self.previous = Some(leaf);
        self.maybe_increment_lambda_arguments(tree, leaf);
        self.maybe_increment_for_loop_variable(tree, leaf);
    }

    pub fn any_open_for_or_lambda(&self) -> bool {
        !self.for_loop_depths.is_empty() || !self.lambda_argument_depths.is_empty()
    }

    pub fn any_open_brackets(&self) -> bool {
        !self.bracket_match.is_empty()
    }

    /// Highest delimiter priority, ignoring `exclude`; `None` when there is none.
    pub fn max_delimiter_priority(&self, exclude: &[Id]) -> Option<u8> {
        self.delimiters
            .iter()
            .filter(|(k, _)| !exclude.contains(k))
            .map(|(_, v)| *v)
            .max()
    }

    pub fn delimiter_count_with_priority(&self, priority: u8) -> usize {
        if self.delimiters.is_empty() {
            return 0;
        }
        let priority = if priority == 0 {
            self.max_delimiter_priority(&[]).unwrap_or(0)
        } else {
            priority
        };
        self.delimiters.values().filter(|p| **p == priority).count()
    }

    fn maybe_increment_for_loop_variable(&mut self, tree: &Tree, leaf: Id) {
        if tree.ty(leaf) == NAME && tree.value(leaf) == "for" {
            self.depth += 1;
            self.for_loop_depths.push(self.depth);
        }
    }

    fn maybe_decrement_after_for_loop_variable(&mut self, tree: &Tree, leaf: Id) {
        if self.for_loop_depths.last() == Some(&self.depth)
            && tree.ty(leaf) == NAME
            && tree.value(leaf) == "in"
        {
            self.depth -= 1;
            self.for_loop_depths.pop();
        }
    }

    fn maybe_increment_lambda_arguments(&mut self, tree: &Tree, leaf: Id) {
        if tree.ty(leaf) == NAME && tree.value(leaf) == "lambda" {
            self.depth += 1;
            self.lambda_argument_depths.push(self.depth);
        }
    }

    fn maybe_decrement_after_lambda_arguments(&mut self, tree: &Tree, leaf: Id) {
        if self.lambda_argument_depths.last() == Some(&self.depth) && tree.ty(leaf) == COLON {
            self.depth -= 1;
            self.lambda_argument_depths.pop();
        }
    }

    pub fn get_open_lsqb(&self) -> Option<Id> {
        self.bracket_match.get(&(self.depth - 1, RSQB)).copied()
    }
}

pub fn is_split_after_delimiter(tree: &Tree, leaf: Id) -> u8 {
    if tree.ty(leaf) == COMMA {
        if let Some(parent) = tree.parent(leaf) {
            if tree.ty(parent) == EXPRLIST {
                if let Some(gp) = tree.parent(parent) {
                    if matches!(tree.ty(gp), COMP_FOR | OLD_COMP_FOR | FOR_STMT) {
                        return 0;
                    }
                }
            }
        }
        return COMMA_PRIORITY;
    }
    0
}

pub fn is_split_before_delimiter(tree: &Tree, leaf: Id, previous: Option<Id>) -> u8 {
    if is_vararg(tree, leaf, VARARGS_PARENTS, UNPACKING_PARENTS) {
        return 0;
    }
    let t = tree.ty(leaf);
    let parent = tree.parent(leaf);
    let pt = parent.map(|p| tree.ty(p));
    if t == DOT
        && parent.is_some()
        && !matches!(pt, Some(IMPORT_FROM) | Some(DOTTED_NAME))
        && previous.map(|p| is_closing(tree.ty(p))).unwrap_or(true)
    {
        return DOT_PRIORITY;
    }
    if is_math_operator(t) && parent.is_some() && !matches!(pt, Some(FACTOR) | Some(STAR_EXPR)) {
        return math_priority(t);
    }
    if is_comparator(t) {
        return COMPARATOR_PRIORITY;
    }
    if t == STRING && previous.map(|p| tree.ty(p)) == Some(STRING) {
        return STRING_PRIORITY;
    }
    if t != NAME && t != ASYNC {
        return 0;
    }
    let v = tree.value(leaf);
    if (v == "for" && matches!(pt, Some(COMP_FOR) | Some(OLD_COMP_FOR))) || t == ASYNC {
        let prev_sib = tree.prev_sibling(leaf);
        let prev_is_async = prev_sib
            .map(|p| tree.is_leaf(p) && tree.value(p) == "async")
            .unwrap_or(false);
        if !prev_is_async {
            return COMPREHENSION_PRIORITY;
        }
    }
    if v == "if" && matches!(pt, Some(COMP_IF) | Some(OLD_COMP_IF)) {
        return COMPREHENSION_PRIORITY;
    }
    if (v == "if" || v == "else") && pt == Some(TEST) {
        return TERNARY_PRIORITY;
    }
    if v == "is" {
        return COMPARATOR_PRIORITY;
    }
    let prev_is = |s: &str| {
        previous
            .map(|p| tree.ty(p) == NAME && tree.value(p) == s)
            .unwrap_or(false)
    };
    if v == "in" && matches!(pt, Some(COMP_OP) | Some(COMPARISON)) && !prev_is("not") {
        return COMPARATOR_PRIORITY;
    }
    if v == "not" && pt == Some(COMP_OP) && !prev_is("is") {
        return COMPARATOR_PRIORITY;
    }
    if (v == "and" || v == "or") && parent.is_some() {
        return LOGIC_PRIORITY;
    }
    0
}

fn top_level_leaves(tree: &Tree, children: &[Id], out: &mut Vec<Id>) {
    for &child in children {
        if tree.is_leaf(child) {
            out.push(child);
            continue;
        }
        let ch = tree.children(child);
        if ch.is_empty() {
            continue;
        }
        let (first, last) = (ch[0], ch[ch.len() - 1]);
        if tree.is_leaf(first)
            && tree.is_leaf(last)
            && is_opening(tree.ty(first))
            && is_closing(tree.ty(last))
        {
            out.push(first);
            out.push(last);
        } else {
            let ch = ch.to_vec();
            top_level_leaves(tree, &ch, out);
        }
    }
}

/// Highest delimiter priority directly inside the parentheses of an atom.
pub fn max_delimiter_priority_in_atom(tree: &mut Tree, node: Id) -> u8 {
    if tree.ty(node) != ATOM {
        return 0;
    }
    let ch = tree.children(node).to_vec();
    let (first, last) = (ch[0], ch[ch.len() - 1]);
    if !(tree.ty(first) == LPAR && tree.ty(last) == RPAR) {
        return 0;
    }
    let mut bt = BracketTracker::default();
    let mut leaves = Vec::new();
    top_level_leaves(tree, &ch[1..ch.len() - 1], &mut leaves);
    for leaf in leaves {
        bt.mark(tree, leaf);
    }
    bt.max_delimiter_priority(&[]).unwrap_or(0)
}

/// Leaves inside matching brackets (the brackets included).
pub fn get_leaves_inside_matching_brackets(tree: &Tree, leaves: &[Id]) -> HashSet<Id> {
    let mut ids = HashSet::default();
    let Some(start) = leaves.iter().position(|l| is_opening(tree.ty(*l))) else {
        return ids;
    };
    let mut stack: Vec<(u16, Vec<Id>)> = Vec::new();
    for &leaf in &leaves[start..] {
        let t = tree.ty(leaf);
        if is_opening(t) {
            stack.push((closing_of(t), vec![leaf]));
        } else if is_closing(t) {
            if stack.last().map(|s| s.0) == Some(t) {
                let (_, mut level) = stack.pop().unwrap();
                level.push(leaf);
                ids.extend(level);
            } else {
                break;
            }
        } else if let Some(top) = stack.last_mut() {
            top.1.push(leaf);
        }
    }
    ids
}

//! blib2to3-shaped concrete syntax tree (the tree Black works on), stored in
//! an arena. Token types are below 256, grammar symbols at 256 and above,
//! mirroring `blib2to3.pgen2.token` / `blib2to3.pygram.python_symbols`.

pub type Id = u32;

macro_rules! types {
    ($($name:ident = $val:expr),* $(,)?) => {
        $(pub const $name: u16 = $val;)*
        pub fn type_name(t: u16) -> &'static str {
            match t {
                $($val => stringify!($name),)*
                _ => "?",
            }
        }
    };
}

types! {
    ENDMARKER = 0,
    NAME = 1,
    NUMBER = 2,
    STRING = 3,
    NEWLINE = 4,
    INDENT = 5,
    DEDENT = 6,
    LPAR = 7,
    RPAR = 8,
    LSQB = 9,
    RSQB = 10,
    COLON = 11,
    COMMA = 12,
    SEMI = 13,
    PLUS = 14,
    MINUS = 15,
    STAR = 16,
    SLASH = 17,
    VBAR = 18,
    AMPER = 19,
    LESS = 20,
    GREATER = 21,
    EQUAL = 22,
    DOT = 23,
    PERCENT = 24,
    LBRACE = 26,
    RBRACE = 27,
    EQEQUAL = 28,
    NOTEQUAL = 29,
    LESSEQUAL = 30,
    GREATEREQUAL = 31,
    TILDE = 32,
    CIRCUMFLEX = 33,
    LEFTSHIFT = 34,
    RIGHTSHIFT = 35,
    DOUBLESTAR = 36,
    PLUSEQUAL = 37,
    MINEQUAL = 38,
    STAREQUAL = 39,
    SLASHEQUAL = 40,
    PERCENTEQUAL = 41,
    AMPEREQUAL = 42,
    VBAREQUAL = 43,
    CIRCUMFLEXEQUAL = 44,
    LEFTSHIFTEQUAL = 45,
    RIGHTSHIFTEQUAL = 46,
    DOUBLESTAREQUAL = 47,
    DOUBLESLASH = 48,
    DOUBLESLASHEQUAL = 49,
    AT = 50,
    ATEQUAL = 51,
    RARROW = 52,
    COLONEQUAL = 53,
    BANG = 54,
    COMMENT = 55,
    ASYNC = 57,
    AWAIT = 58,
    STANDALONE_COMMENT = 153,
    // ---- grammar symbols
    FILE_INPUT = 256,
    DECORATOR = 257,
    DECORATORS = 258,
    DECORATED = 259,
    ASYNC_FUNCDEF = 260,
    FUNCDEF = 261,
    PARAMETERS = 262,
    TYPEDARGSLIST = 263,
    TNAME = 264,
    TNAME_STAR = 265,
    VARARGSLIST = 266,
    SIMPLE_STMT = 267,
    EXPR_STMT = 268,
    ANNASSIGN = 269,
    TESTLIST_STAR_EXPR = 270,
    DEL_STMT = 271,
    RETURN_STMT = 272,
    RAISE_STMT = 273,
    IMPORT_NAME = 274,
    IMPORT_FROM = 275,
    IMPORT_AS_NAME = 276,
    DOTTED_AS_NAME = 277,
    IMPORT_AS_NAMES = 278,
    DOTTED_AS_NAMES = 279,
    DOTTED_NAME = 280,
    GLOBAL_STMT = 281,
    ASSERT_STMT = 282,
    TYPE_STMT = 283,
    ASYNC_STMT = 284,
    IF_STMT = 285,
    WHILE_STMT = 286,
    FOR_STMT = 287,
    TRY_STMT = 288,
    WITH_STMT = 289,
    EXCEPT_CLAUSE = 290,
    SUITE = 291,
    TESTLIST_SAFE = 292,
    OLD_LAMBDEF = 293,
    NAMEDEXPR_TEST = 294,
    ASEXPR_TEST = 295,
    TEST = 296,
    OR_TEST = 297,
    AND_TEST = 298,
    NOT_TEST = 299,
    COMPARISON = 300,
    COMP_OP = 301,
    STAR_EXPR = 302,
    EXPR = 303,
    XOR_EXPR = 304,
    AND_EXPR = 305,
    SHIFT_EXPR = 306,
    ARITH_EXPR = 307,
    TERM = 308,
    FACTOR = 309,
    POWER = 310,
    ATOM = 311,
    LISTMAKER = 312,
    TESTLIST_GEXP = 313,
    LAMBDEF = 314,
    TRAILER = 315,
    SUBSCRIPTLIST = 316,
    SUBSCRIPT = 317,
    SLICEOP = 318,
    EXPRLIST = 319,
    TESTLIST = 320,
    DICTSETMAKER = 321,
    CLASSDEF = 322,
    ARGLIST = 323,
    ARGUMENT = 324,
    COMP_FOR = 325,
    COMP_IF = 326,
    OLD_COMP_FOR = 327,
    OLD_COMP_IF = 328,
    YIELD_EXPR = 329,
    YIELD_ARG = 330,
    MATCH_STMT = 331,
    SUBJECT_EXPR = 332,
    CASE_BLOCK = 333,
    GUARD = 334,
    PATTERNS = 335,
    PATTERN = 336,
    TYPEVAR = 337,
    PARAMSPEC = 338,
    TYPEVARTUPLE = 339,
    TYPEPARAMS = 340,
}

#[derive(Clone, Debug, Default)]
pub struct NodeData {
    pub ty: u16,
    /// Token text (leaves only).
    pub value: String,
    /// Whitespace and comments before the token (leaves only).
    pub prefix: String,
    pub parent: Option<Id>,
    pub children: Vec<Id>,
    /// Position of this node in its parent's children.
    pub index: u32,
    pub lineno: u32,
    pub bracket_depth: i32,
    pub opening_bracket: Option<Id>,
    /// f-string whose replacement fields contain a backslash: never normalized.
    pub keep_string: bool,
    /// For a STANDALONE_COMMENT holding code switched off by `# fmt: off` /
    /// `# fmt: skip`: the first leaf of that code.
    pub fmt_first_leaf: Option<Id>,
}

#[derive(Default)]
pub struct Tree {
    pub nodes: Vec<NodeData>,
}

impl Tree {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn leaf(&mut self, ty: u16, value: impl Into<String>) -> Id {
        self.nodes.push(NodeData {
            ty,
            value: value.into(),
            ..NodeData::default()
        });
        (self.nodes.len() - 1) as Id
    }

    pub fn leaf_with_prefix(
        &mut self,
        ty: u16,
        value: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Id {
        let id = self.leaf(ty, value);
        self.nodes[id as usize].prefix = prefix.into();
        id
    }

    pub fn node(&mut self, ty: u16, children: Vec<Id>) -> Id {
        let id = self.nodes.len() as Id;
        for (i, c) in children.iter().enumerate() {
            let n = &mut self.nodes[*c as usize];
            n.parent = Some(id);
            n.index = i as u32;
        }
        self.nodes.push(NodeData {
            ty,
            children,
            ..NodeData::default()
        });
        id
    }

    #[inline]
    pub fn n(&self, id: Id) -> &NodeData {
        &self.nodes[id as usize]
    }

    #[inline]
    pub fn nm(&mut self, id: Id) -> &mut NodeData {
        &mut self.nodes[id as usize]
    }

    #[inline]
    pub fn ty(&self, id: Id) -> u16 {
        self.nodes[id as usize].ty
    }

    #[inline]
    pub fn is_leaf(&self, id: Id) -> bool {
        self.nodes[id as usize].ty < 256
    }

    #[inline]
    pub fn value(&self, id: Id) -> &str {
        &self.nodes[id as usize].value
    }

    pub fn set_value(&mut self, id: Id, v: impl Into<String>) {
        self.nodes[id as usize].value = v.into();
    }

    #[inline]
    pub fn parent(&self, id: Id) -> Option<Id> {
        self.nodes[id as usize].parent
    }

    pub fn parent_ty(&self, id: Id) -> Option<u16> {
        self.parent(id).map(|p| self.ty(p))
    }

    #[inline]
    pub fn children(&self, id: Id) -> &[Id] {
        &self.nodes[id as usize].children
    }

    pub fn child(&self, id: Id, i: usize) -> Id {
        self.nodes[id as usize].children[i]
    }

    pub fn nchildren(&self, id: Id) -> usize {
        self.nodes[id as usize].children.len()
    }

    /// The prefix of a leaf, or of the first leaf of a node.
    pub fn prefix(&self, id: Id) -> &str {
        let mut cur = id;
        loop {
            let n = &self.nodes[cur as usize];
            if n.ty < 256 {
                return &n.prefix;
            }
            match n.children.first() {
                Some(c) => cur = *c,
                None => return "",
            }
        }
    }

    pub fn set_prefix(&mut self, id: Id, prefix: impl Into<String>) {
        let mut cur = id;
        loop {
            let n = &self.nodes[cur as usize];
            if n.ty < 256 {
                self.nodes[cur as usize].prefix = prefix.into();
                return;
            }
            match n.children.first() {
                Some(c) => cur = *c,
                None => return,
            }
        }
    }

    pub fn prev_sibling(&self, id: Id) -> Option<Id> {
        let p = self.parent(id)?;
        let i = self.nodes[id as usize].index as usize;
        if i == 0 {
            None
        } else {
            self.children(p).get(i - 1).copied()
        }
    }

    pub fn next_sibling(&self, id: Id) -> Option<Id> {
        let p = self.parent(id)?;
        let i = self.nodes[id as usize].index as usize;
        self.children(p).get(i + 1).copied()
    }

    fn renumber(&mut self, parent: Id) {
        let children = std::mem::take(&mut self.nodes[parent as usize].children);
        for (i, c) in children.iter().enumerate() {
            let n = &mut self.nodes[*c as usize];
            n.index = i as u32;
            n.parent = Some(parent);
        }
        self.nodes[parent as usize].children = children;
    }

    /// Detaches `id` from its parent; returns its former position.
    pub fn remove(&mut self, id: Id) -> Option<usize> {
        let p = self.parent(id)?;
        let i = self.nodes[id as usize].index as usize;
        self.nodes[p as usize].children.remove(i);
        self.nodes[id as usize].parent = None;
        self.renumber(p);
        Some(i)
    }

    /// Replaces `id` by `new` in `id`'s parent.
    pub fn replace(&mut self, id: Id, new: Id) {
        let Some(p) = self.parent(id) else { return };
        let i = self.nodes[id as usize].index as usize;
        self.nodes[p as usize].children[i] = new;
        self.nodes[id as usize].parent = None;
        let n = &mut self.nodes[new as usize];
        n.parent = Some(p);
        n.index = i as u32;
    }

    pub fn insert_child(&mut self, parent: Id, i: usize, child: Id) {
        self.nodes[parent as usize].children.insert(i, child);
        self.renumber(parent);
    }

    pub fn append_child(&mut self, parent: Id, child: Id) {
        let i = self.nodes[parent as usize].children.len();
        self.nodes[parent as usize].children.push(child);
        let n = &mut self.nodes[child as usize];
        n.parent = Some(parent);
        n.index = i as u32;
    }

    pub fn set_child(&mut self, parent: Id, i: usize, child: Id) {
        let old = self.nodes[parent as usize].children[i];
        self.nodes[old as usize].parent = None;
        self.nodes[parent as usize].children[i] = child;
        let n = &mut self.nodes[child as usize];
        n.parent = Some(parent);
        n.index = i as u32;
    }

    pub fn leaves(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        self.collect_leaves(id, &mut out);
        out
    }

    fn collect_leaves(&self, id: Id, out: &mut Vec<Id>) {
        if self.is_leaf(id) {
            out.push(id);
        } else {
            for c in self.children(id) {
                self.collect_leaves(*c, out);
            }
        }
    }

    pub fn pre_order(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in self.children(n).iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    pub fn first_leaf(&self, id: Id) -> Option<Id> {
        let mut cur = id;
        loop {
            if self.is_leaf(cur) {
                return Some(cur);
            }
            cur = *self.children(cur).first()?;
        }
    }

    pub fn last_leaf(&self, id: Id) -> Option<Id> {
        let mut cur = id;
        loop {
            if self.is_leaf(cur) {
                return Some(cur);
            }
            cur = *self.children(cur).last()?;
        }
    }

    /// `Leaf.clone()`: type, value, prefix, lineno and bracket depth (no parent).
    pub fn clone_leaf(&mut self, id: Id) -> Id {
        let n = self.n(id);
        let data = NodeData {
            ty: n.ty,
            value: n.value.clone(),
            prefix: n.prefix.clone(),
            lineno: n.lineno,
            bracket_depth: n.bracket_depth,
            keep_string: n.keep_string,
            ..NodeData::default()
        };
        self.nodes.push(data);
        (self.nodes.len() - 1) as Id
    }

    /// Debug dump of a subtree.
    #[allow(dead_code)]
    pub fn dump(&self, id: Id, depth: usize, out: &mut String) {
        let n = self.n(id);
        out.push_str(&"  ".repeat(depth));
        if n.ty < 256 {
            out.push_str(&format!(
                "{} {:?} prefix={:?}\n",
                type_name(n.ty),
                n.value,
                n.prefix
            ));
        } else {
            out.push_str(&format!("{}\n", type_name(n.ty)));
            for c in &n.children {
                self.dump(*c, depth + 1, out);
            }
        }
    }
}

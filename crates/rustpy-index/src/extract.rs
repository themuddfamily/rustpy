//! Symbol extraction: what an index records about one Python file.
//!
//! Recorded are the module-level classes, functions, variables and imported
//! names (including those inside module-level `if`/`try`/`with` blocks), the
//! members of classes (methods, nested classes, class variables) and the
//! module's `__all__` and instance attributes stored through method receivers.
//! Other local definitions inside function bodies are
//! not part of a module's API, so Go to Symbol does not list them either.
//!
//! The result depends only on the file's text, never on its path, so it can be
//! cached by content hash and shared between machines (see [`crate::store`]).

use rustc_hash::FxHashMap;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

/// What kind of declaration a [`Symbol`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Class,
    Function,
    /// A function defined in a class body.
    Method,
    Variable,
    /// A variable with an upper-case name (`MAX_SIZE = 3`).
    Constant,
    /// A module-level name bound by an import: a re-export candidate. For a
    /// star import (`from m import *`) the name is `*`.
    Import,
}

impl SymbolKind {
    pub const ALL: [SymbolKind; 6] = [
        SymbolKind::Class,
        SymbolKind::Function,
        SymbolKind::Method,
        SymbolKind::Variable,
        SymbolKind::Constant,
        SymbolKind::Import,
    ];

    /// The stable number stored in index files.
    pub fn code(self) -> u8 {
        match self {
            SymbolKind::Class => 0,
            SymbolKind::Function => 1,
            SymbolKind::Method => 2,
            SymbolKind::Variable => 3,
            SymbolKind::Constant => 4,
            SymbolKind::Import => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<SymbolKind> {
        SymbolKind::ALL.get(code as usize).copied()
    }

    /// Everything but imports: the symbol is declared in its file.
    pub fn is_definition(self) -> bool {
        self != SymbolKind::Import
    }

    pub fn label(self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Variable => "variable",
            SymbolKind::Constant => "constant",
            SymbolKind::Import => "import",
        }
    }
}

/// `from m import x as x` / `import m as m`: an explicit re-export (PEP 484).
pub const FLAG_EXPLICIT_REEXPORT: u8 = 1;
/// `async def`.
pub const FLAG_ASYNC: u8 = 1 << 1;
/// A method decorated with `@property` or `@cached_property`.
pub const FLAG_PROPERTY: u8 = 1 << 2;
/// Flags an extractor may set (the others are reserved for the in-memory index).
pub const STORED_FLAGS: u8 = FLAG_EXPLICIT_REEXPORT | FLAG_ASYNC | FLAG_PROPERTY;

/// One declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// `FLAG_*` bits.
    pub flags: u8,
    /// 0-based line of the declared name.
    pub line: u32,
    /// Index of the enclosing class in [`FileSymbols::symbols`].
    pub parent: Option<u32>,
    /// One-line summary: `(a, b=1) -> int` for functions, `(Base)` for
    /// classes, `: int` for annotated variables, and for imports the module
    /// imported from as written (`.core`, `pkg.mod`).
    pub detail: String,
    /// For `from m import x as y`: the imported name `x`; empty for
    /// `import m as y` (the module itself) and for definitions.
    pub origin: String,
    /// For classes: the declared base expressions ([`declared_bases`]).
    pub bases: Vec<String>,
}

/// Shared bounded module export authority (names plus exactness).
pub use rustpy_syntax::exports::Exports as DunderAll;

/// Everything recorded for one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSymbols {
    pub symbols: Vec<Symbol>,
    pub dunder_all: Option<DunderAll>,
}

/// Generated files can be huge: at most this many symbols are kept per file.
pub const MAX_SYMBOLS_PER_FILE: usize = 20_000;
/// Details longer than this (in bytes) are cut with an ellipsis.
pub const MAX_DETAIL: usize = 160;
const MAX_CLASS_DEPTH: u32 = 12;
/// Statements nested deeper in `if` / `try` / `with` blocks are not visited
/// (CPython allows 100 indentation levels; the walk recurses per level).
const MAX_BLOCK_DEPTH: u32 = 100;

/// Parses `text` and extracts its symbols.
pub fn extract(text: &str) -> FileSymbols {
    let parse = rustpy_syntax::parse(text);
    extract_from(&parse.syntax_node(), text)
}

/// Extracts the symbols of an already parsed file (`root` is the module node of `text`).
pub fn extract_from(root: &SyntaxNode, text: &str) -> FileSymbols {
    let mut ex = Extractor {
        lines: line_starts(text),
        out: FileSymbols::default(),
        seen: FxHashMap::default(),
        nesting: 0,
    };
    if let Some(module) = ast::Module::cast(root.clone()) {
        ex.block(module.statements(), None, 0);
    }
    ex.out.dunder_all = rustpy_syntax::exports::evaluate(root);
    // An index build keeps the symbols of every file it parsed until it
    // assembles the index: no spare capacity from growing the list.
    ex.out.symbols.shrink_to_fit();
    ex.out
}

/// Base expression of a class that is not a (dotted) name: a call
/// (`with_metaclass(M, Base)`), a starred argument, an expression.
pub const DYNAMIC_BASE: &str = "*";

/// The positional bases of `class` as written, as dotted names: `Base`,
/// `pkg.mod.Base`; subscripts are dropped (`Generic[T]` is `Generic`) and any
/// other expression is [`DYNAMIC_BASE`]. Hierarchy queries resolve them
/// lazily, only for classes whose base names can match.
pub fn declared_bases(class: &ast::ClassDef) -> Vec<String> {
    fn dotted(expr: ast::Expr, depth: u32) -> Option<String> {
        if depth > 32 {
            return None;
        }
        match expr.unparen() {
            ast::Expr::Name(n) => Some(n.name_text()).filter(|n| !n.is_empty()),
            ast::Expr::Attribute(a) => {
                let attr = a.attr_text();
                let object = dotted(a.object()?, depth + 1)?;
                (!attr.is_empty()).then(|| format!("{object}.{attr}"))
            }
            ast::Expr::Subscript(s) if depth == 0 => dotted(s.object()?, depth + 1),
            _ => None,
        }
    }
    class
        .bases()
        .into_iter()
        .map(|base| dotted(base, 0).unwrap_or_else(|| DYNAMIC_BASE.to_string()))
        .collect()
}

fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i as u32 + 1),
    );
    starts
}

struct Extractor {
    lines: Vec<u32>,
    out: FileSymbols,
    /// `(parent + 1, name)` -> index of the symbol already declared with that name in that scope.
    seen: FxHashMap<(u32, String), u32>,
    /// Blocks entered by [`Extractor::block`] (bounded by [`MAX_BLOCK_DEPTH`]).
    nesting: u32,
}

impl Extractor {
    fn line_of(&self, offset: u32) -> u32 {
        (self.lines.partition_point(|s| *s <= offset).max(1) - 1) as u32
    }

    /// Records a declaration. A name declared again in the same scope (a
    /// conditional or overloaded definition, `f = wrap(f)`) keeps its first
    /// declaration, except that a definition replaces an import of the same
    /// name (`try: from _speedups import f` / `except ImportError: def f()`).
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        name: &str,
        kind: SymbolKind,
        flags: u8,
        offset: u32,
        parent: Option<u32>,
        detail: String,
        origin: String,
    ) -> Option<u32> {
        if name.is_empty() || self.out.symbols.len() >= MAX_SYMBOLS_PER_FILE {
            return None;
        }
        let symbol = Symbol {
            name: name.to_string(),
            kind,
            flags,
            line: self.line_of(offset),
            parent,
            detail,
            origin,
            bases: Vec::new(),
        };
        // Star imports bind no one name: a package `__init__` has several.
        if name == "*" {
            self.out.symbols.push(symbol);
            return Some(self.out.symbols.len() as u32 - 1);
        }
        let key = (parent.map_or(0, |p| p + 1), name.to_string());
        if let Some(&existing) = self.seen.get(&key) {
            let old = &mut self.out.symbols[existing as usize];
            if old.kind == SymbolKind::Import && kind.is_definition() {
                *old = symbol;
                return Some(existing);
            }
            return None;
        }
        let idx = self.out.symbols.len() as u32;
        self.seen.insert(key, idx);
        self.out.symbols.push(symbol);
        Some(idx)
    }

    fn block(&mut self, stmts: impl Iterator<Item = ast::Stmt>, parent: Option<u32>, depth: u32) {
        if self.nesting >= MAX_BLOCK_DEPTH {
            return;
        }
        self.nesting += 1;
        self.statements(stmts, parent, depth);
        self.nesting -= 1;
    }

    fn statements(
        &mut self,
        stmts: impl Iterator<Item = ast::Stmt>,
        parent: Option<u32>,
        depth: u32,
    ) {
        let module_level = parent.is_none();
        for stmt in stmts {
            if self.out.symbols.len() >= MAX_SYMBOLS_PER_FILE {
                return;
            }
            match stmt {
                ast::Stmt::FuncDef(f) => self.function(&f, parent),
                ast::Stmt::ClassDef(c) => self.class(&c, parent, depth),
                ast::Stmt::Assign(a) => {
                    let targets = a.targets();
                    for t in &targets {
                        self.assign_target(t, parent, None);
                    }
                }
                ast::Stmt::AnnAssign(a) => {
                    let Some(target) = a.target() else { continue };
                    if module_level && is_dunder_all(&target) {
                        // Export evaluation is delegated once for the whole module.
                    } else {
                        self.assign_target(&target, parent, a.annotation());
                    }
                }
                ast::Stmt::TypeAlias(t) => {
                    if let Some(tok) = t.name() {
                        self.push(
                            tok.text(),
                            SymbolKind::Variable,
                            0,
                            tok.text_range().start().into(),
                            parent,
                            String::new(),
                            String::new(),
                        );
                    }
                }
                ast::Stmt::Import(i) if module_level => self.import(&i),
                ast::Stmt::ImportFrom(i) if module_level => self.import_from(&i),
                ast::Stmt::If(i) => {
                    if let Some(b) = i.body() {
                        self.block(b.statements(), parent, depth);
                    }
                    for elif in i.elif_clauses() {
                        if let Some(b) = elif.body() {
                            self.block(b.statements(), parent, depth);
                        }
                    }
                    if let Some(b) = i.else_clause().and_then(|e| e.body()) {
                        self.block(b.statements(), parent, depth);
                    }
                }
                ast::Stmt::Try(t) => {
                    if let Some(b) = t.body() {
                        self.block(b.statements(), parent, depth);
                    }
                    for clause in t.except_clauses() {
                        if let Some(b) = clause.body() {
                            self.block(b.statements(), parent, depth);
                        }
                    }
                    if let Some(b) = t.else_clause().and_then(|e| e.body()) {
                        self.block(b.statements(), parent, depth);
                    }
                    if let Some(b) = t.finally_clause().and_then(|e| e.body()) {
                        self.block(b.statements(), parent, depth);
                    }
                }
                ast::Stmt::With(w) => {
                    if let Some(b) = w.body() {
                        self.block(b.statements(), parent, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn function(&mut self, f: &ast::FuncDef, parent: Option<u32>) {
        let Some(tok) = f.name() else { return };
        let kind = if parent.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let mut flags = 0;
        if f.is_async() {
            flags |= FLAG_ASYNC;
        }
        if f.decorators().any(|d| {
            d.name_text()
                .is_some_and(|n| n == "property" || n.rsplit('.').next() == Some("cached_property"))
        }) {
            flags |= FLAG_PROPERTY;
        }
        let mut detail = f
            .params()
            .map(|p| compact(p.syntax(), MAX_DETAIL))
            .unwrap_or_else(|| "()".to_string());
        if let Some(ret) = f.return_annotation() {
            detail.push_str(" -> ");
            detail.push_str(&compact(ret.syntax(), MAX_DETAIL));
        }
        self.push(
            tok.text(),
            kind,
            flags,
            tok.text_range().start().into(),
            parent,
            cut(detail, MAX_DETAIL),
            String::new(),
        );
        if let Some(class) = parent {
            self.instance_attributes(f, class);
        }
    }

    /// Instance stores belong to the class, not the method's local namespace.
    /// Respect the actual receiver parameter and skip nested executable scopes.
    fn instance_attributes(&mut self, function: &ast::FuncDef, class: u32) {
        if function.decorators().any(|decorator| {
            decorator.name_text().is_some_and(|name| {
                matches!(
                    name.rsplit('.').next(),
                    Some("staticmethod" | "classmethod")
                )
            })
        }) {
            return;
        }
        let Some(receiver) = function
            .params()
            .and_then(|params| params.plain_params().next())
            .map(|parameter| parameter.name_text())
        else {
            return;
        };
        let Some(body) = function.body() else {
            return;
        };
        for node in body.syntax().descendants() {
            if self.out.symbols.len() >= MAX_SYMBOLS_PER_FILE {
                break;
            }
            if node
                .ancestors()
                .skip(1)
                .take_while(|ancestor| ancestor != function.syntax())
                .any(|ancestor| {
                    matches!(
                        ancestor.kind(),
                        SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
                    )
                })
            {
                continue;
            }
            let (targets, annotation) =
                if let Some(assignment) = ast::AssignStmt::cast(node.clone()) {
                    (assignment.targets(), None)
                } else if let Some(assignment) = ast::AnnAssignStmt::cast(node.clone()) {
                    if assignment.value().is_none() {
                        continue;
                    }
                    (
                        assignment.target().into_iter().collect(),
                        assignment.annotation(),
                    )
                } else if let Some(assignment) = ast::AugAssignStmt::cast(node.clone()) {
                    (assignment.target().into_iter().collect(), None)
                } else if let Some(iteration) = ast::ForStmt::cast(node.clone()) {
                    (iteration.target().into_iter().collect(), None)
                } else if let Some(item) = ast::WithItem::cast(node) {
                    (item.target().into_iter().collect(), None)
                } else {
                    continue;
                };
            for target in targets {
                self.instance_target(&target, &receiver, class, annotation.as_ref(), 0);
            }
        }
    }

    fn instance_target(
        &mut self,
        target: &ast::Expr,
        receiver: &str,
        class: u32,
        annotation: Option<&ast::Expr>,
        depth: usize,
    ) {
        if depth > 32 {
            return;
        }
        match target.clone().unparen() {
            ast::Expr::Attribute(attribute) => {
                if !matches!(attribute.object().map(ast::Expr::unparen), Some(ast::Expr::Name(name)) if name.name_text() == receiver)
                {
                    return;
                }
                let Some(token) = attribute.attr() else {
                    return;
                };
                let detail = annotation
                    .map(|value| {
                        cut(
                            format!(": {}", compact(value.syntax(), MAX_DETAIL)),
                            MAX_DETAIL,
                        )
                    })
                    .unwrap_or_default();
                self.push(
                    token.text(),
                    SymbolKind::Variable,
                    0,
                    token.text_range().start().into(),
                    Some(class),
                    detail,
                    String::new(),
                );
            }
            ast::Expr::Tuple(tuple) => {
                for item in tuple.elements() {
                    self.instance_target(&item, receiver, class, annotation, depth + 1);
                }
            }
            ast::Expr::List(list) => {
                for item in list.elements() {
                    self.instance_target(&item, receiver, class, annotation, depth + 1);
                }
            }
            ast::Expr::Star(star) => {
                if let Some(item) = star.expr() {
                    self.instance_target(&item, receiver, class, annotation, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn class(&mut self, c: &ast::ClassDef, parent: Option<u32>, depth: u32) {
        let Some(tok) = c.name() else { return };
        let detail = c
            .arguments()
            .map(|a| compact(a.syntax(), MAX_DETAIL))
            .unwrap_or_default();
        let Some(idx) = self.push(
            tok.text(),
            SymbolKind::Class,
            0,
            tok.text_range().start().into(),
            parent,
            detail,
            String::new(),
        ) else {
            return;
        };
        self.out.symbols[idx as usize].bases = declared_bases(c);
        if depth < MAX_CLASS_DEPTH {
            if let Some(body) = c.body() {
                self.block(body.statements(), Some(idx), depth + 1);
            }
        }
    }

    fn assign_target(
        &mut self,
        target: &ast::Expr,
        parent: Option<u32>,
        annotation: Option<ast::Expr>,
    ) {
        match target.clone().unparen() {
            ast::Expr::Name(n) => {
                let Some(tok) = n.name_token() else { return };
                let name = tok.text();
                // `__slots__`, `__version__`, `__all__`: module plumbing, not API.
                if is_dunder(name) {
                    return;
                }
                let kind = if is_constant_name(name) {
                    SymbolKind::Constant
                } else {
                    SymbolKind::Variable
                };
                let detail = annotation
                    .map(|a| cut(format!(": {}", compact(a.syntax(), MAX_DETAIL)), MAX_DETAIL))
                    .unwrap_or_default();
                self.push(
                    name,
                    kind,
                    0,
                    tok.text_range().start().into(),
                    parent,
                    detail,
                    String::new(),
                );
            }
            ast::Expr::Tuple(t) => {
                for e in t.elements() {
                    self.assign_target(&e, parent, None);
                }
            }
            ast::Expr::List(l) => {
                for e in l.elements() {
                    self.assign_target(&e, parent, None);
                }
            }
            _ => {}
        }
    }

    /// `import a.b as c` binds `c` (the module `a.b`); a plain `import a.b`
    /// binds the top-level package and re-exports nothing, so it is skipped.
    fn import(&mut self, stmt: &ast::ImportStmt) {
        for a in stmt.aliases() {
            let Some(alias) = a.alias() else { continue };
            let name = alias.text();
            if name.starts_with('_') {
                continue;
            }
            let module = a.name();
            let flags = if module == name {
                FLAG_EXPLICIT_REEXPORT
            } else {
                0
            };
            self.push(
                name,
                SymbolKind::Import,
                flags,
                alias.text_range().start().into(),
                None,
                module,
                String::new(),
            );
        }
    }

    fn import_from(&mut self, stmt: &ast::ImportFromStmt) {
        let module = stmt.module_path();
        if module == "__future__" || module.is_empty() {
            return;
        }
        if stmt.is_star() {
            let offset = stmt.syntax().text_range().start().into();
            self.push(
                "*",
                SymbolKind::Import,
                0,
                offset,
                None,
                module,
                "*".to_string(),
            );
            return;
        }
        for a in stmt.aliases() {
            let orig = a.name();
            let (name, tok) = match a.alias() {
                Some(t) => (t.text().to_string(), Some(t)),
                None => (orig.clone(), a.name_token()),
            };
            let Some(tok) = tok else { continue };
            if name.starts_with('_') || orig.is_empty() {
                continue;
            }
            let flags = if a.alias().is_some() && name == orig {
                FLAG_EXPLICIT_REEXPORT
            } else {
                0
            };
            self.push(
                &name,
                SymbolKind::Import,
                flags,
                tok.text_range().start().into(),
                None,
                module.clone(),
                orig,
            );
        }
    }
}

fn is_dunder_all(e: &ast::Expr) -> bool {
    matches!(e.clone().unparen(), ast::Expr::Name(n) if n.name_text() == "__all__")
}

fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

/// `MAX_SIZE`, `PI2`: upper case with at least one letter (a lone `_` or `T` is not a constant).
pub fn is_constant_name(name: &str) -> bool {
    name.len() > 1
        && name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The text of `node` on one line: comments dropped, runs of whitespace and
/// line breaks collapsed into one space (none just inside brackets or before
/// a comma or colon), cut after `max` bytes.
pub fn compact(node: &SyntaxNode, max: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for tok in node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        let kind = tok.kind();
        if kind.is_trivia()
            || matches!(
                kind,
                SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT
            )
        {
            pending_space = !out.is_empty();
            continue;
        }
        let text = tok.text();
        if pending_space
            && !out.ends_with(['(', '[', '{'])
            && !text.starts_with([')', ']', '}', ',', ':'])
        {
            out.push(' ');
        }
        pending_space = false;
        out.push_str(text);
        if out.len() > max {
            break;
        }
    }
    cut(out, max)
}

/// `s` cut to at most `max` bytes (on a character boundary), with an ellipsis when cut.
pub fn cut(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn names(fs: &FileSymbols) -> Vec<(String, SymbolKind, Option<u32>)> {
        fs.symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind, s.parent))
            .collect()
    }

    fn find<'a>(fs: &'a FileSymbols, name: &str) -> &'a Symbol {
        fs.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} not in {:?}", names(fs)))
    }

    #[test]
    fn class_bases_are_recorded_as_dotted_names() {
        let fs = extract(
            "import abc\nclass A(Base, pkg.mod.Mixin, Generic[T], (Paren), with_meta(M), *more, a[0].B, metaclass=abc.ABCMeta):\n    class B(A): pass\n",
        );
        assert_eq!(
            find(&fs, "A").bases,
            ["Base", "pkg.mod.Mixin", "Generic", "Paren", "*", "*"],
            "`*more` is not a positional base (as for ClassDef::bases)"
        );
        assert_eq!(find(&fs, "B").bases, ["A"]);
    }

    #[test]
    fn module_level_declarations() {
        let src = "\"\"\"Doc.\"\"\"\nimport os\nMAX_SIZE = 3\nname: str = 'x'\n\n@decorator\ndef compute(a, b=1, *args, c: int = 2, **kw) -> int:\n    def local():\n        pass\n    return a\n\nasync def fetch(url):\n    pass\n\nclass Widget(Base, metaclass=Meta):\n    \"\"\"A widget.\"\"\"\n    size = 1\n    LIMIT: int = 2\n\n    def __init__(self, x):\n        self.x = x\n\n    @property\n    def area(self):\n        return 1\n\n    class Inner:\n        def deep(self): ...\n\ntype Alias = list[int]\nfirst, (second, third) = 1, (2, 3)\n__version__ = '1.0'\n";
        let fs = extract(src);
        let got = names(&fs);
        let widget = got.iter().position(|(n, _, _)| n == "Widget").unwrap() as u32;
        let inner = got.iter().position(|(n, _, _)| n == "Inner").unwrap() as u32;
        assert_eq!(
            got,
            vec![
                ("MAX_SIZE".into(), SymbolKind::Constant, None),
                ("name".into(), SymbolKind::Variable, None),
                ("compute".into(), SymbolKind::Function, None),
                ("fetch".into(), SymbolKind::Function, None),
                ("Widget".into(), SymbolKind::Class, None),
                ("size".into(), SymbolKind::Variable, Some(widget)),
                ("LIMIT".into(), SymbolKind::Constant, Some(widget)),
                ("__init__".into(), SymbolKind::Method, Some(widget)),
                ("x".into(), SymbolKind::Variable, Some(widget)),
                ("area".into(), SymbolKind::Method, Some(widget)),
                ("Inner".into(), SymbolKind::Class, Some(widget)),
                ("deep".into(), SymbolKind::Method, Some(inner)),
                ("Alias".into(), SymbolKind::Variable, None),
                ("first".into(), SymbolKind::Variable, None),
                ("second".into(), SymbolKind::Variable, None),
                ("third".into(), SymbolKind::Variable, None),
            ]
        );
        let compute = find(&fs, "compute");
        assert_eq!(compute.detail, "(a, b=1, *args, c: int = 2, **kw) -> int");
        assert_eq!(
            compute.line, 6,
            "the line of the name, not of the decorator"
        );
        assert_eq!(find(&fs, "fetch").flags & FLAG_ASYNC, FLAG_ASYNC);
        assert_eq!(find(&fs, "Widget").detail, "(Base, metaclass=Meta)");
        assert_eq!(
            find(&fs, "Widget").bases,
            ["Base"],
            "keywords are not bases"
        );
        assert!(find(&fs, "Inner").bases.is_empty());
        assert!(find(&fs, "compute").bases.is_empty());
        assert_eq!(find(&fs, "area").flags & FLAG_PROPERTY, FLAG_PROPERTY);
        assert_eq!(find(&fs, "name").detail, ": str");
        assert_eq!(find(&fs, "LIMIT").detail, ": int");
        assert!(fs.dunder_all.is_none());
    }

    #[test]
    fn multi_line_signatures_are_compacted() {
        let src = "def f(\n    a,  # the first\n    b: dict[str,\n            int] = {1 : 2},\n) -> tuple[\n    int, str\n]:\n    pass\n";
        let fs = extract(src);
        assert_eq!(
            find(&fs, "f").detail,
            "(a, b: dict[str, int] = {1: 2},) -> tuple[int, str]"
        );
        let long = format!(
            "def g({}):\n    pass\n",
            (0..100)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let g = extract(&long);
        let detail = &find(&g, "g").detail;
        assert!(
            detail.len() <= MAX_DETAIL && detail.ends_with('…'),
            "{detail}"
        );
    }

    #[test]
    fn conditional_definitions_and_duplicates() {
        let src = "import sys\nif sys.version_info >= (3, 8):\n    def f(x): ...\nelse:\n    def f(x, y): ...\ntry:\n    from _speedups import fast\nexcept ImportError:\n    def fast(): ...\nwith ctx():\n    WIDTH = 1\nf = wrap(f)\nfor i in range(3):\n    LOOP = i\n";
        let fs = extract(src);
        assert_eq!(
            names(&fs),
            vec![
                ("f".into(), SymbolKind::Function, None),
                ("fast".into(), SymbolKind::Function, None),
                ("WIDTH".into(), SymbolKind::Constant, None)
            ]
        );
        assert_eq!(find(&fs, "f").detail, "(x)", "the first definition is kept");
        assert_eq!(
            find(&fs, "fast").line,
            8,
            "a definition replaces the import of the same name"
        );
    }

    #[test]
    fn imports_record_reexport_candidates() {
        let src = "from __future__ import annotations\nimport os\nimport numpy as np\nimport json as json\nfrom .core import Frame, Series as Series, _private\nfrom ..util import helper as h\nfrom pkg.sub import *\nfrom . import submodule\n";
        let fs = extract(src);
        let imports: Vec<(String, String, String, u8)> = fs
            .symbols
            .iter()
            .map(|s| (s.name.clone(), s.detail.clone(), s.origin.clone(), s.flags))
            .collect();
        assert_eq!(
            imports,
            vec![
                ("np".into(), "numpy".into(), "".into(), 0),
                (
                    "json".into(),
                    "json".into(),
                    "".into(),
                    FLAG_EXPLICIT_REEXPORT
                ),
                ("Frame".into(), ".core".into(), "Frame".into(), 0),
                (
                    "Series".into(),
                    ".core".into(),
                    "Series".into(),
                    FLAG_EXPLICIT_REEXPORT
                ),
                ("h".into(), "..util".into(), "helper".into(), 0),
                ("*".into(), "pkg.sub".into(), "*".into(), 0),
                ("submodule".into(), ".".into(), "submodule".into(), 0),
            ]
        );
        assert!(fs.symbols.iter().all(|s| s.kind == SymbolKind::Import));
        // Every star import is kept (a package `__init__` usually has several).
        let stars = extract("from .base_events import *\nfrom .coroutines import *\nif sys.platform == 'win32':\n    from .windows_events import *\nelse:\n    from .unix_events import *\n");
        let modules: Vec<&str> = stars
            .symbols
            .iter()
            .filter(|s| s.name == "*")
            .map(|s| s.detail.as_str())
            .collect();
        assert_eq!(
            modules,
            vec![
                ".base_events",
                ".coroutines",
                ".windows_events",
                ".unix_events"
            ]
        );
    }

    #[test]
    fn dunder_all_forms() {
        let exact = extract("__all__ = ['a', \"b\"]\n__all__ += ('c',)\n__all__.extend(['d'])\n__all__.append('e')\n__all__.remove('a')\n");
        assert_eq!(
            exact.dunder_all,
            Some(DunderAll {
                names: vec!["b".into(), "c".into(), "d".into(), "e".into()],
                exact: true
            })
        );
        let sum = extract("__all__ = ['a'] + ['b']\n");
        assert_eq!(
            sum.dunder_all,
            Some(DunderAll {
                names: vec!["a".into(), "b".into()],
                exact: true
            })
        );
        let dynamic =
            extract("from .core import *\nfrom . import core\n__all__ = ['x'] + core.__all__\n");
        assert_eq!(
            dynamic.dunder_all,
            Some(DunderAll {
                names: vec!["x".into()],
                exact: false
            })
        );
        let annotated = extract("__all__: list[str] = ['z']\n");
        assert_eq!(
            annotated.dunder_all,
            Some(DunderAll {
                names: vec!["z".into()],
                exact: true
            })
        );
        let computed = extract("__all__ = [n for n in dir() if not n.startswith('_')]\n");
        assert_eq!(
            computed.dunder_all,
            Some(DunderAll {
                names: vec![],
                exact: false
            })
        );
        // Inside a class it is just a (dunder) class variable.
        let in_class = extract("class C:\n    __all__ = ['q']\n");
        assert!(in_class.dunder_all.is_none());
        assert_eq!(
            names(&in_class),
            vec![("C".into(), SymbolKind::Class, None)]
        );
    }

    /// `__all__ = ['n0'] + ['n1'] + …` with `terms` terms.
    pub(crate) fn dunder_all_sum(terms: usize) -> String {
        let parts: Vec<String> = (0..terms).map(|i| format!("['n{i}']")).collect();
        format!(
            "__all__ = {}\n\ndef after():\n    pass\n",
            parts.join(" + ")
        )
    }

    /// Runs `f` on a thread with the default 2 MiB stack of spawned threads.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .expect("no stack overflow")
    }

    #[test]
    fn huge_dunder_all_sums_do_not_overflow_the_stack() {
        // Valid Python (CPython runs 9,500 terms): the sum is a left-nested
        // chain of that depth, which a recursive walk overflowed on.
        let all = on_small_stack(|| extract(&dunder_all_sum(11_000)));
        let names = &all.dunder_all.as_ref().expect("__all__").names;
        assert_eq!(names.len(), 11_000);
        assert_eq!(
            (names[0].as_str(), names[10_999].as_str()),
            ("n0", "n10999"),
            "in order"
        );
        assert!(all.dunder_all.as_ref().unwrap().exact);
        assert_eq!(
            all.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["after"]
        );
        // Longer than the parser builds as a tree (it flattens the rest): no crash either.
        let flat = on_small_stack(|| extract(&dunder_all_sum(60_000)));
        assert!(flat.symbols.iter().any(|s| s.name == "after"));
        // Right-nested sums (parenthesized) are walked in order too.
        let nested = extract("__all__ = ['a'] + (['b'] + (['c'] + ['d']))\n");
        assert_eq!(
            nested.dunder_all,
            Some(DunderAll {
                names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                exact: true
            })
        );
    }

    #[test]
    fn deeply_nested_blocks_are_cut_off() {
        // 300 nested `if` blocks (beyond CPython's 100 indentation levels):
        // the definitions within the first 100 levels are found.
        let mut src = String::new();
        for depth in 0..300 {
            let pad = " ".repeat(depth);
            src.push_str(&format!("{pad}def f{depth}(): pass\n{pad}if x:\n"));
        }
        src.push_str(&format!("{}pass\n", " ".repeat(300)));
        let fs = on_small_stack(move || extract(&src));
        let found: Vec<&str> = fs.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(found.contains(&"f0") && found.contains(&"f99"), "{found:?}");
        assert!(!found.contains(&"f100"));
    }

    #[test]
    fn broken_code_still_yields_symbols() {
        let src =
            "def ok(a):\n    return a\n\ndef broken(:\n\nclass Fine:\n    def m(self): pass\n";
        let fs = extract(src);
        assert!(fs.symbols.iter().any(|s| s.name == "ok"));
        assert!(fs
            .symbols
            .iter()
            .any(|s| s.name == "Fine" && s.kind == SymbolKind::Class));
        assert!(fs
            .symbols
            .iter()
            .any(|s| s.name == "m" && s.kind == SymbolKind::Method));
        // Non-ASCII names and an empty file.
        let uni = extract("def größe(x):\n    pass\nπ = 3.14\n");
        assert_eq!(
            names(&uni),
            vec![
                ("größe".into(), SymbolKind::Function, None),
                ("π".into(), SymbolKind::Variable, None)
            ]
        );
        assert_eq!(extract(""), FileSymbols::default());
    }

    #[test]
    fn helpers() {
        assert!(
            is_constant_name("MAX_SIZE")
                && is_constant_name("PI2")
                && !is_constant_name("T")
                && !is_constant_name("_")
                && !is_constant_name("Value")
        );
        assert_eq!(cut("abcdef".into(), 10), "abcdef");
        assert_eq!(cut("ééééé".into(), 6), "é…");
        for k in SymbolKind::ALL {
            assert_eq!(SymbolKind::from_code(k.code()), Some(k));
        }
        assert_eq!(SymbolKind::from_code(6), None);
    }
    #[test]
    fn shared_exports_constants_and_aliases_are_identical_to_syntax_authority() {
        let source = "PART=['π']\n__all__=PART\nPART.append('other')\n";
        assert_eq!(
            extract(source).dunder_all,
            rustpy_syntax::exports::evaluate(&rustpy_syntax::parse(source).syntax_node())
        );
        assert_eq!(
            extract(source).dunder_all,
            Some(DunderAll {
                names: vec!["π".into(), "other".into()],
                exact: true
            })
        );
    }
    #[test]
    fn instance_stores_use_actual_receiver_and_store_targets_only() {
        let file = extract("class C:\n def __init__(this):\n  this.x=1\n  this.label: str='text'\n  (this.y,this.z)=(2,3)\n  this.items[this.not_stored]=1\n  def nested():\n   this.hidden=1\n @staticmethod\n def static(self):\n  self.fake=1\n @classmethod\n def class_method(cls):\n  cls.class_only=1\n");
        let attributes: Vec<_> = file
            .symbols
            .iter()
            .filter(|symbol| symbol.parent.is_some() && symbol.kind == SymbolKind::Variable)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(attributes, ["x", "label", "y", "z"]);
        assert_eq!(find(&file, "label").detail, ": str");
        assert!(attributes
            .iter()
            .all(|name| !matches!(*name, "not_stored" | "hidden" | "fake" | "class_only")));
    }
}

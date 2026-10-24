//! Type / method / call hierarchies and implementation navigation (roadmap §2.7).

use std::path::PathBuf;
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::rowan;
use rustpy_syntax::SyntaxKind;

use crate::db::{Analysis, FileInfo};
use crate::features::{self, Location};
use crate::infer::Inferer;
use crate::types::{ClassRef, Type};
use crate::usages;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyNode {
    pub name: String,
    pub path: PathBuf,
    /// Range to select when navigating (the name token).
    pub range: TextRange,
    pub detail: String,
    pub children: Vec<HierarchyNode>,
}

/// The expression node whose range is exactly `range`.
fn expr_at_range(root: &rustpy_syntax::SyntaxNode, range: TextRange) -> Option<ast::Expr> {
    let start = match root.covering_element(range) {
        rowan::NodeOrToken::Node(n) => n,
        rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .find(|a| a.text_range() == range && ast::Expr::can_cast(a.kind()))
        .and_then(ast::Expr::cast)
}

pub(crate) fn class_name_range(file: &FileInfo, class: &ClassRef) -> TextRange {
    let root = file.parse.syntax_node();
    root.covering_element(class.range)
        .into_node()
        .and_then(|n| {
            n.ancestors()
                .find(|a| a.kind() == SyntaxKind::CLASS_DEF)
                .or(Some(n))
        })
        .and_then(|n| {
            n.children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::IDENT)
                .map(|t| t.text_range())
        })
        .unwrap_or(class.range)
}

fn node_for_class(db: &Analysis, class: &ClassRef, detail: String) -> Option<HierarchyNode> {
    let file = db.file(class.file)?;
    Some(HierarchyNode {
        name: class.name.to_string(),
        path: file.path.clone(),
        range: class_name_range(&file, class),
        detail,
        children: Vec::new(),
    })
}

/// The class definition enclosing (or at) `offset`, if any.
pub fn class_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<ClassRef> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    // A reference to a class: `Foo` in an expression.
    if let Some(tok) = features::ident_at(file, offset) {
        if let Some(parent) = tok.parent() {
            if let Some(expr) = ast::Expr::cast(parent.clone()) {
                if let Type::Class(c, _) | Type::Instance(c, _) | Type::SelfType(c) =
                    inf.type_of_expr(file, &expr)
                {
                    return Some(c);
                }
            }
            // The name of a class definition.
            if parent.kind() == SyntaxKind::CLASS_DEF {
                return Some(ClassRef {
                    file: file.id,
                    range: parent.text_range(),
                    name: tok.text().into(),
                });
            }
        }
    }
    // Otherwise the enclosing class definition.
    let root = file.parse.syntax_node();
    let tok = root
        .token_at_offset(offset)
        .left_biased()
        .or_else(|| root.token_at_offset(offset).right_biased())?;
    let def = tok
        .parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::CLASS_DEF)?;
    let name = ast::ClassDef::cast(def.clone())?.name_text();
    Some(ClassRef {
        file: file.id,
        range: def.text_range(),
        name: name.into(),
    })
}

/// Supertypes tree rooted at the class at `offset` (bases recursively).
pub fn supertypes(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<HierarchyNode> {
    let offset = crate::clamp_offset(file, offset);
    let class = class_at(db, file, offset)?;
    let inf = Inferer::new(db);
    fn build(db: &Analysis, inf: &Inferer, c: &ClassRef, depth: usize) -> Option<HierarchyNode> {
        let mut node = node_for_class(db, c, String::new())?;
        if depth > 12 {
            return Some(node);
        }
        if let Some(info) = inf.class_info(c) {
            let file = db.file(c.file)?;
            let root = file.parse.syntax_node();
            for base in &info.base_exprs {
                let Some(expr) = expr_at_range(&root, *base) else {
                    continue;
                };
                if let Type::Class(bc, _) = inf.type_of_expr(&file, &expr) {
                    if let Some(child) = build(db, inf, &bc, depth + 1) {
                        node.children.push(child);
                    }
                }
            }
        }
        Some(node)
    }
    build(db, &inf, &class, 0)
}

/// Class bases recorded by the persisted project index (see
/// [`Analysis::set_project_index`]), by file path once a hierarchy query
/// needs them.
pub(crate) struct PersistedBases {
    index: Arc<rustpy_index::SymbolIndex>,
    files: once_cell::sync::OnceCell<rustc_hash::FxHashMap<PathBuf, rustpy_index::FileBases>>,
}

impl PersistedBases {
    pub(crate) fn new(index: Arc<rustpy_index::SymbolIndex>) -> Self {
        Self {
            index,
            files: Default::default(),
        }
    }

    pub(crate) fn is_for(&self, index: &Arc<rustpy_index::SymbolIndex>) -> bool {
        Arc::ptr_eq(&self.index, index)
    }

    fn get(&self, path: &std::path::Path) -> Option<&rustpy_index::FileBases> {
        self.files
            .get_or_init(|| {
                self.index
                    .class_bases()
                    .into_iter()
                    .map(|file| (file.path.clone(), file))
                    .collect()
            })
            .get(path)
    }
}

/// Builtin and standard-library classes commonly used as bases: they only
/// denote classes of the same name (unless some project module rebinds it).
const COMMON_BASES: &[&str] = &[
    "object",
    "type",
    "int",
    "float",
    "str",
    "bytes",
    "dict",
    "list",
    "tuple",
    "set",
    "frozenset",
    "Exception",
    "BaseException",
    "ValueError",
    "TypeError",
    "RuntimeError",
    "KeyError",
    "LookupError",
    "AttributeError",
    "NotImplementedError",
    "OSError",
    "Warning",
    "Generic",
    "Protocol",
    "NamedTuple",
    "TypedDict",
    "ABC",
    "Enum",
    "IntEnum",
    "StrEnum",
    "Flag",
    "IntFlag",
    "TestCase",
];

/// What one file's declared class bases can denote, by name only.
#[derive(Default)]
struct BaseKeys {
    /// Last components of the declared bases, and the imported originals of
    /// aliased bases (`from m import Base as Parent`).
    names: rustc_hash::FxHashSet<String>,
    /// Some base can denote any class: a call or other expression, a
    /// module-level variable (`Parent = Base`), or a class the record does
    /// not cover (classes inside functions, redeclared names).
    any: bool,
}

impl BaseKeys {
    fn of(record: &rustpy_index::FileBases, classes: usize) -> Self {
        let mut keys = BaseKeys {
            any: record.classes.len() != classes,
            ..Default::default()
        };
        for base in record.classes.iter().flat_map(|class| &class.bases) {
            if base == rustpy_index::extract::DYNAMIC_BASE {
                keys.any = true;
                continue;
            }
            let first = base.split('.').next().unwrap_or(base);
            if record.variables.iter().any(|variable| variable == first) {
                keys.any = true;
            }
            if !base.contains('.') {
                if let Some((_, origin)) = record.imports.iter().find(|(alias, _)| alias == base) {
                    keys.names.insert(origin.clone());
                }
            }
            keys.names
                .insert(base.rsplit('.').next().unwrap_or(base).to_string());
        }
        keys
    }

    /// Whether a base of this file may resolve to a class named `name`.
    /// A base name that is neither a project class nor a builtin may be an
    /// alias re-exported elsewhere, so it is a candidate for every class.
    fn may_denote(&self, name: &str, known: &rustc_hash::FxHashSet<String>) -> bool {
        self.any
            || self.names.contains(name)
            || self.names.iter().any(|base| !known.contains(base.as_str()))
    }
}

/// Resolved `(base, subclass)` edges declared in one file.
type FileEdges = Arc<Vec<(ClassRef, ClassRef)>>;

/// Lazily resolved edges and override declarations for one captured project
/// catalogue. Candidate files come from base names (the persisted project
/// index, or the file text when it changed since indexing); only their bases
/// are inferred, once per file. Weak source guards avoid retaining evicted
/// parse trees through this index.
pub(crate) struct HierarchyIndex {
    generation: u64,
    sources: Vec<std::sync::Weak<FileInfo>>,
    keys: Vec<BaseKeys>,
    /// Base names that denote only the class(es) of that name.
    known: rustc_hash::FxHashSet<String>,
    /// Per file: resolved `(base, subclass)` edges in declaration order.
    edges: parking_lot::Mutex<rustc_hash::FxHashMap<usize, FileEdges>>,
}

impl HierarchyIndex {
    fn matches(&self, db: &Analysis, files: &[Arc<FileInfo>]) -> bool {
        self.generation == db.source_generation()
            && self.sources.len() == files.len()
            && self.sources.iter().zip(files).all(|(cached, current)| {
                cached
                    .upgrade()
                    .is_some_and(|cached| Arc::ptr_eq(&cached, current))
            })
    }

    /// The resolved base edges declared in `files[at]`.
    fn edges_of(&self, db: &Analysis, file: &Arc<FileInfo>, at: usize) -> FileEdges {
        if let Some(edges) = self.edges.lock().get(&at) {
            return edges.clone();
        }
        let inf = Inferer::new(db);
        let root = file.parse.syntax_node();
        let mut edges = Vec::new();
        for candidate in file.class_declarations() {
            let Some(info) = inf.class_info(candidate) else {
                continue;
            };
            for base in &info.base_exprs {
                let Some(expression) = expr_at_range(&root, *base) else {
                    continue;
                };
                if let Type::Class(parent, _) = inf.type_of_expr(file, &expression) {
                    if parent != *candidate {
                        edges.push((parent, candidate.clone()));
                    }
                }
            }
        }
        let edges = Arc::new(edges);
        self.edges.lock().insert(at, edges.clone());
        edges
    }
}

/// The base-name record of `file`: the persisted one while the text is
/// what was indexed, otherwise extracted from the current parse.
fn file_bases(persisted: Option<&PersistedBases>, file: &FileInfo) -> rustpy_index::FileBases {
    let hash = rustpy_index::ContentHash::of(file.text.as_bytes());
    if let Some(record) = persisted
        .and_then(|persisted| persisted.get(&file.path))
        .filter(|record| record.hash == hash)
    {
        return record.clone();
    }
    let symbols = rustpy_index::extract::extract_from(&file.parse.syntax_node(), &file.text);
    rustpy_index::FileBases::from_symbols(file.path.clone(), hash, &symbols)
}

fn hierarchy_index(db: &Analysis, files: &[Arc<FileInfo>]) -> Arc<HierarchyIndex> {
    if let Some(index) = db
        .hierarchy_index
        .read()
        .as_ref()
        .filter(|index| index.matches(db, files))
        .cloned()
    {
        return index;
    }
    let generation = db.source_generation();
    let persisted = db.persisted_bases.read().clone();
    let records: Vec<rustpy_index::FileBases> = files
        .iter()
        .map(|file| file_bases(persisted.as_deref(), file))
        .collect();
    let mut known: rustc_hash::FxHashSet<String> =
        COMMON_BASES.iter().map(|name| name.to_string()).collect();
    for file in files {
        known.extend(file.class_declarations().iter().map(|c| c.name.to_string()));
    }
    // A name some module binds to something else may denote another class.
    for record in &records {
        for (alias, _) in &record.imports {
            known.remove(alias);
        }
        for variable in &record.variables {
            known.remove(variable);
        }
    }
    let index = HierarchyIndex {
        generation,
        sources: files.iter().map(Arc::downgrade).collect(),
        keys: records
            .iter()
            .zip(files)
            .map(|(record, file)| BaseKeys::of(record, file.class_declarations().len()))
            .collect(),
        known,
        edges: Default::default(),
    };
    // Source or import-resolution changes during construction cannot publish a
    // reusable stale graph. The current query still uses its captured sources.
    let index = Arc::new(index);
    if generation == db.source_generation() {
        *db.hierarchy_index.write() = Some(index.clone());
    }
    index
}

/// Direct subclasses among `files`, in file and declaration order. Only
/// files whose base names can denote `class` have their bases resolved.
pub fn direct_subclasses(
    db: &Analysis,
    class: &ClassRef,
    files: &[Arc<FileInfo>],
) -> Vec<ClassRef> {
    let index = hierarchy_index(db, files);
    let mut out: Vec<ClassRef> = Vec::new();
    for (at, file) in files.iter().enumerate() {
        if !index.keys[at].may_denote(&class.name, &index.known) {
            continue;
        }
        for (parent, child) in index.edges_of(db, file, at).iter() {
            if parent == class && !out.contains(child) {
                out.push(child.clone());
            }
        }
    }
    out
}

/// Whether `class` itself declares a method or property `name`.
fn declares_method(db: &Analysis, class: &ClassRef, name: &str) -> bool {
    Inferer::new(db).class_info(class).is_some_and(|info| {
        info.members.get(name).is_some_and(|member| {
            matches!(
                member.kind,
                crate::classes::MemberKind::Method
                    | crate::classes::MemberKind::StaticMethod
                    | crate::classes::MemberKind::ClassMethod
                    | crate::classes::MemberKind::Property
            )
        })
    })
}

/// Subtypes tree rooted at the class at `offset`.
pub fn subtypes(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Option<HierarchyNode> {
    let offset = crate::clamp_offset(file, offset);
    let class = class_at(db, file, offset)?;
    let files = load_files(db, project_files);
    fn build(
        db: &Analysis,
        c: &ClassRef,
        files: &[Arc<FileInfo>],
        depth: usize,
    ) -> Option<HierarchyNode> {
        let mut node = node_for_class(db, c, String::new())?;
        if depth > 12 {
            return Some(node);
        }
        for sub in direct_subclasses(db, c, files) {
            if let Some(child) = build(db, &sub, files, depth + 1) {
                node.children.push(child);
            }
        }
        Some(node)
    }
    build(db, &class, &files, 0)
}

fn load_files(db: &Analysis, project_files: &[PathBuf]) -> Vec<Arc<FileInfo>> {
    let mut files: Vec<Arc<FileInfo>> = Vec::new();
    for p in project_files {
        if let Some(f) = db.file_for_path(p) {
            if !files.iter().any(|existing| existing.id == f.id) {
                files.push(f);
            }
        }
    }
    files
}

/// The method definition enclosing `offset`: (class, method name, name range).
fn method_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<(ClassRef, String)> {
    let root = file.parse.syntax_node();
    let tok = root
        .token_at_offset(offset)
        .left_biased()
        .or_else(|| root.token_at_offset(offset).right_biased())?;
    let func = tok
        .parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::FUNC_DEF)?;
    let name = ast::FuncDef::cast(func.clone())?.name_text();
    let class = func
        .ancestors()
        .skip(1)
        .find(|n| matches!(n.kind(), SyntaxKind::CLASS_DEF | SyntaxKind::FUNC_DEF))?;
    if class.kind() != SyntaxKind::CLASS_DEF {
        return None;
    }
    let cname = ast::ClassDef::cast(class.clone())?.name_text();
    let _ = db;
    Some((
        ClassRef {
            file: file.id,
            range: class.text_range(),
            name: cname.into(),
        },
        name,
    ))
}

/// Resolve a method reference before considering the enclosing declaration.
/// A local variable inside a method must not silently navigate as that method.
fn referenced_method(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<(ClassRef, String)> {
    let token = features::ident_at(file, offset)?;
    let parent = token.parent()?;
    if parent.kind() == SyntaxKind::FUNC_DEF {
        return method_at(db, file, offset);
    }
    if !matches!(
        parent.kind(),
        SyntaxKind::NAME_EXPR | SyntaxKind::ATTRIBUTE_EXPR
    ) {
        return None;
    }
    let mut methods = Vec::new();
    for location in features::definition_at(db, file, offset) {
        let Some(target) = db.file_for_path(&location.path) else {
            continue;
        };
        let Some(method) = method_at(db, &target, location.range.start()) else {
            continue;
        };
        // A same-spelled local or parameter inside this method is not its
        // declaration. Require the physical FuncDef's actual name token.
        let physical_name = features::ident_at(&target, location.range.start())
            .filter(|name| name.text_range() == location.range)
            .and_then(|name| {
                let function = name.parent().and_then(ast::FuncDef::cast)?;
                function
                    .name()
                    .filter(|declaration| declaration.text_range() == name.text_range())
            });
        if physical_name.is_some() && !methods.contains(&method) {
            methods.push(method);
        }
    }
    if methods.len() == 1 {
        methods.pop()
    } else {
        None
    }
}

fn member_location(db: &Analysis, class: &ClassRef, name: &str) -> Option<Location> {
    let inf = Inferer::new(db);
    let info = inf.class_info(class)?;
    let m = info.members.get(name)?;
    if !matches!(
        m.kind,
        crate::classes::MemberKind::Method
            | crate::classes::MemberKind::StaticMethod
            | crate::classes::MemberKind::ClassMethod
            | crate::classes::MemberKind::Property
    ) {
        return None;
    }
    let f = db.file(m.file)?;
    Some(Location {
        path: f.path.clone(),
        range: m.range,
        is_stub: f.is_stub,
    })
}

/// Ctrl+U: the same-named methods in base classes (nearest first).
pub fn super_methods(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    let offset = crate::clamp_offset(file, offset);
    let Some((class, name)) =
        referenced_method(db, file, offset).or_else(|| method_at(db, file, offset))
    else {
        return Vec::new();
    };
    let inf = Inferer::new(db);
    let mro = inf.mro(&class);
    let mut out = Vec::new();
    for base in mro.iter().skip(1) {
        if let Some(loc) = member_location(db, base, &name) {
            out.push(loc);
        }
    }
    out
}

/// Ctrl+Alt+B: subclasses (for a class) or overriding methods (for a method).
pub fn implementations(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Vec<Location> {
    let offset = crate::clamp_offset(file, offset);
    let files = load_files(db, project_files);
    if let Some((class, name)) = referenced_method(db, file, offset) {
        let mut out = Vec::new();
        let mut stack = vec![class];
        let mut visited = rustc_hash::FxHashSet::default();
        while let Some(c) = stack.pop() {
            if !visited.insert(c.clone()) {
                continue;
            }
            if visited.len() > 500 {
                break;
            }
            for sub in direct_subclasses(db, &c, &files) {
                if declares_method(db, &sub, &name) {
                    if let Some(loc) = member_location(db, &sub, &name) {
                        if !out.contains(&loc) {
                            out.push(loc);
                        }
                    }
                }
                stack.push(sub);
            }
        }
        return out;
    }
    let Some(class) = class_at(db, file, offset) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![class];
    let mut visited = rustc_hash::FxHashSet::default();
    while let Some(c) = stack.pop() {
        if !visited.insert(c.clone()) {
            continue;
        }
        for sub in direct_subclasses(db, &c, &files) {
            if let Some(f) = db.file(sub.file) {
                let loc = Location {
                    path: f.path.clone(),
                    range: class_name_range(&f, &sub),
                    is_stub: f.is_stub,
                };
                if !out.contains(&loc) {
                    out.push(loc);
                }
            }
            stack.push(sub);
        }
    }
    out
}

/// Method hierarchy: the method at `offset` with its overriding methods in subclasses.
pub fn method_hierarchy(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Option<HierarchyNode> {
    let offset = crate::clamp_offset(file, offset);
    let (class, name) =
        referenced_method(db, file, offset).or_else(|| method_at(db, file, offset))?;
    let files = load_files(db, project_files);
    fn build(
        db: &Analysis,
        c: &ClassRef,
        name: &str,
        files: &[Arc<FileInfo>],
        depth: usize,
    ) -> Option<HierarchyNode> {
        let f = db.file(c.file)?;
        let loc = member_location(db, c, name);
        let (range, detail) = match loc.filter(|l| l.path == f.path) {
            Some(l) => (l.range, format!("{}.{}", c.name, name)),
            None => (
                class_name_range(&f, c),
                format!("{} (inherits {})", c.name, name),
            ),
        };
        let mut node = HierarchyNode {
            name: c.name.to_string(),
            path: f.path.clone(),
            range,
            detail,
            children: Vec::new(),
        };
        if depth > 12 {
            return Some(node);
        }
        for sub in direct_subclasses(db, c, files) {
            if let Some(child) = build(db, &sub, name, files, depth + 1) {
                node.children.push(child);
            }
        }
        Some(node)
    }
    build(db, &class, &name, &files, 0)
}

fn enclosing_function_name(file: &FileInfo, range: TextRange) -> String {
    let root = file.parse.syntax_node();
    let Some(tok) = root.token_at_offset(range.start()).right_biased() else {
        return "<module>".into();
    };
    let mut parts = Vec::new();
    for n in tok.parent_ancestors() {
        match n.kind() {
            SyntaxKind::FUNC_DEF => parts.push(
                ast::FuncDef::cast(n.clone())
                    .map(|f| f.name_text())
                    .unwrap_or_default(),
            ),
            SyntaxKind::CLASS_DEF => parts.push(
                ast::ClassDef::cast(n.clone())
                    .map(|c| c.name_text())
                    .unwrap_or_default(),
            ),
            _ => {}
        }
    }
    if parts.is_empty() {
        "<module>".into()
    } else {
        parts.reverse();
        parts.join(".")
    }
}

/// Classify a non-definition usage in one file as an incoming call.
pub fn caller_in_file(f: &FileInfo, u: &usages::Usage) -> Option<HierarchyNode> {
    if u.is_definition {
        return None;
    }
    let root = f.parse.syntax_node();
    let tok = root.token_at_offset(u.range.start()).right_biased()?;
    let is_call = tok.parent_ancestors().take(3).any(|n| {
        n.kind() == SyntaxKind::CALL_EXPR
            && ast::CallExpr::cast(n.clone())
                .and_then(|c| c.callee())
                .is_some_and(|c| c.syntax().text_range().contains_range(u.range))
    });
    if !is_call {
        return None;
    }
    let caller = enclosing_function_name(f, u.range);
    let range = tok
        .parent_ancestors()
        .find_map(ast::FuncDef::cast)
        .and_then(|function| function.name())
        .map(|name| name.text_range())
        .unwrap_or(u.range);
    let detail = format!(
        "{}:{}  {}",
        u.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        u.line + 1,
        u.line_text.trim()
    );
    Some(HierarchyNode {
        name: caller,
        path: u.path.clone(),
        range,
        detail,
        children: Vec::new(),
    })
}

/// Callees of the function at `offset`: resolved call targets inside its body.
/// Physical callable at a declaration, reference, or inside its body.
/// Resolution of a reference precedes falling back to its containing function.
pub fn callable_root(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<HierarchyNode> {
    let offset = crate::clamp_offset(file, offset);
    let mut target = None;
    for location in features::definition_at(db, file, offset) {
        let Some(owner) = db.file_for_path(&location.path) else {
            continue;
        };
        let root = owner.parse.syntax_node();
        let Some(token) = root.token_at_offset(location.range.start()).right_biased() else {
            continue;
        };
        let Some(function) = token.parent().and_then(ast::FuncDef::cast) else {
            continue;
        };
        let Some(name) = function.name() else {
            continue;
        };
        if name.text_range() != location.range {
            continue;
        }
        target = Some(HierarchyNode {
            name: function.name_text().to_string(),
            path: owner.path.clone(),
            range: name.text_range(),
            detail: String::new(),
            children: Vec::new(),
        });
    }
    if target.is_some() {
        return target;
    }
    let root = file.parse.syntax_node();
    let token = root
        .token_at_offset(offset)
        .left_biased()
        .or_else(|| root.token_at_offset(offset).right_biased())?;
    let function = token.parent_ancestors().find_map(ast::FuncDef::cast)?;
    Some(HierarchyNode {
        name: function.name_text().to_string(),
        path: file.path.clone(),
        range: function.name()?.text_range(),
        detail: String::new(),
        children: Vec::new(),
    })
}

pub fn callees(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<HierarchyNode> {
    let Some(target) = callable_root(db, file, offset) else {
        return Vec::new();
    };
    let Some(file) = db.file_for_path(&target.path) else {
        return Vec::new();
    };
    let root = file.parse.syntax_node();
    let Some(func) = root
        .token_at_offset(target.range.start())
        .right_biased()
        .and_then(|token| token.parent())
        .filter(|node| node.kind() == SyntaxKind::FUNC_DEF)
    else {
        return Vec::new();
    };
    let Some(body) = ast::FuncDef::cast(func.clone()).and_then(|function| function.body()) else {
        return Vec::new();
    };
    let mut out: Vec<HierarchyNode> = Vec::new();
    for call in body
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CALL_EXPR)
        .filter(|node| {
            node.ancestors()
                .skip(1)
                .find(|parent| {
                    matches!(
                        parent.kind(),
                        SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
                    )
                })
                .as_ref()
                == Some(&func)
        })
    {
        let Some(c) = ast::CallExpr::cast(call.clone()) else {
            continue;
        };
        let Some(callee) = c.callee() else { continue };
        let name_range = match &callee {
            ast::Expr::Name(n) => n.syntax().text_range(),
            ast::Expr::Attribute(a) => a
                .syntax()
                .last_token()
                .map(|t| t.text_range())
                .unwrap_or(a.syntax().text_range()),
            _ => continue,
        };
        let name =
            file.text[usize::from(name_range.start())..usize::from(name_range.end())].to_string();
        let locs = features::definition_at(db, &file, name_range.start());
        let Some(loc) = locs.first() else { continue };
        if out
            .iter()
            .any(|n| n.path == loc.path && n.range == loc.range)
        {
            continue;
        }
        let detail = loc
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(HierarchyNode {
            name,
            path: loc.path.clone(),
            range: loc.range,
            detail,
            children: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
mod index_tests {
    use super::*;

    #[test]
    fn only_files_whose_base_names_can_match_are_resolved() {
        let db = Analysis::for_project(std::path::Path::new("/hierarchy-candidates"));
        let ids = [
            ("a.py", "class A: pass\n"),
            ("b.py", "from a import A\nclass B(A): pass\n"),
            ("c.py", "class C(object): pass\nclass D(Exception): pass\n"),
        ]
        .map(|(name, text)| {
            db.set_file_text(
                &std::path::Path::new("/hierarchy-candidates").join(name),
                text,
            )
        });
        let files: Vec<_> = ids.iter().map(|id| db.file(*id).unwrap()).collect();
        let class = |at: usize| files[at].class_declarations()[0].clone();
        assert!(direct_subclasses(&db, &class(1), &files).is_empty());
        let index = hierarchy_index(&db, &files);
        assert!(index.edges.lock().is_empty(), "no base is named B");
        assert_eq!(
            direct_subclasses(&db, &class(0), &files)
                .iter()
                .map(|class| class.name.as_str())
                .collect::<Vec<_>>(),
            ["B"]
        );
        assert_eq!(
            index.edges.lock().keys().copied().collect::<Vec<_>>(),
            [1],
            "only b.py names A as a base"
        );
    }

    #[test]
    fn resolved_subclass_index_retires_edges_after_source_edits() {
        let db = Analysis::for_project(std::path::Path::new("/hierarchy-index"));
        let base = db.set_file_text(
            std::path::Path::new("/hierarchy-index/base.py"),
            "class Base: pass\n",
        );
        let child = db.set_file_text(
            std::path::Path::new("/hierarchy-index/child.py"),
            "from base import Base as Parent\nclass Child(Parent): pass\n",
        );
        let base_file = db.file(base).unwrap();
        let class = base_file.class_declarations()[0].clone();
        let files = vec![base_file.clone(), db.file(child).unwrap()];
        assert_eq!(
            direct_subclasses(&db, &class, &files)
                .iter()
                .map(|class| class.name.as_str())
                .collect::<Vec<_>>(),
            ["Child"]
        );
        db.set_file_text(
            std::path::Path::new("/hierarchy-index/child.py"),
            "class Child: pass\n",
        );
        let files = vec![base_file, db.file(child).unwrap()];
        assert!(direct_subclasses(&db, &class, &files).is_empty());
    }
}

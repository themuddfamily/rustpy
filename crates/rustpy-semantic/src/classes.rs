//! Class model: bases, MRO (C3), type parameters, declared and instance
//! members (roadmap §2.2 "classes & instances", "MRO via C3").

use std::sync::Arc;

use indexmap::IndexMap;
use rustpy_core::{FileId, TextRange};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;
use smol_str::SmolStr;

use crate::db::FileInfo;
use crate::scope::{BindingId, BindingKind, ScopeId};
use crate::types::ClassRef;

/// A member declared in a class body or assigned on `self`.
#[derive(Clone, Debug)]
pub struct Member {
    pub name: SmolStr,
    pub kind: MemberKind,
    pub file: FileId,
    /// Name range of the declaration.
    pub range: TextRange,
    /// Binding in the class scope (for declared members).
    pub binding: Option<BindingId>,
    /// Assigned value expression (for `self.x = value` / class vars).
    pub value: Option<TextRange>,
    pub annotation: Option<TextRange>,
    /// Whether the member is defined in a `.pyi` stub.
    pub is_stub: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    StaticMethod,
    ClassMethod,
    Property,
    ClassVar,
    InstanceVar,
    NestedClass,
}

/// Structural facts about a class definition.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub class_ref: ClassRef,
    pub scope: ScopeId,
    /// Base class expressions as written (ranges), in order.
    pub base_exprs: Vec<TextRange>,
    pub keywords: Vec<(SmolStr, TextRange)>,
    pub type_params: Vec<SmolStr>,
    pub decorators: Vec<SmolStr>,
    pub docstring: Option<Arc<str>>,
    /// Declared members in body order plus `self.x = …` instance attributes.
    pub members: IndexMap<SmolStr, Member>,
    pub is_protocol: bool,
    pub is_enum_like: bool,
    pub is_dataclass: bool,
}

impl ClassInfo {
    /// Builds the class info from its definition node.
    pub fn new(file: &FileInfo, class: &ast::ClassDef) -> Option<ClassInfo> {
        let name = class.name()?;
        let class_ref = ClassRef {
            file: file.id,
            range: class.text_range(),
            name: SmolStr::new(name.text()),
        };
        let scope = file.scopes.scope_for_node(class.text_range())?;
        let base_exprs = class.bases().iter().map(|b| b.text_range()).collect();
        let keywords = class
            .arguments()
            .map(|a| {
                a.keywords()
                    .filter_map(|k| Some((SmolStr::new(k.name()?.text()), k.value()?.text_range())))
                    .collect()
            })
            .unwrap_or_default();
        let mut type_params: Vec<SmolStr> = class
            .type_params()
            .map(|tp| tp.names().map(|t| SmolStr::new(t.text())).collect())
            .unwrap_or_default();
        if type_params.is_empty() {
            // Legacy generics: TypeVars from Generic[...] / Protocol[...] or any subscripted base.
            let mut generic_args: Vec<SmolStr> = Vec::new();
            let mut other_args: Vec<SmolStr> = Vec::new();
            for b in class.bases() {
                if let ast::Expr::Subscript(s) = &b {
                    let base_name = s
                        .object()
                        .map(|o| o.syntax().text().to_string())
                        .unwrap_or_default();
                    let args: Vec<SmolStr> = s
                        .index()
                        .into_iter()
                        .flat_map(|idx| {
                            idx.descendants_with_tokens()
                                .filter_map(|e| e.into_token())
                                .filter(|t| t.kind() == SyntaxKind::IDENT)
                                .map(|t| SmolStr::new(t.text()))
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    let is_generic =
                        base_name.ends_with("Generic") || base_name.ends_with("Protocol");
                    for a in args {
                        let looks_like_typevar = a.starts_with('_')
                            && a.chars().nth(1).is_some_and(|c| c.is_uppercase())
                            || a.len() <= 2 && a.chars().all(|c| c.is_uppercase())
                            || a.ends_with("_co")
                            || a.ends_with("_contra")
                            || a == "T"
                            || a.starts_with("T_")
                            || a.starts_with("_T");
                        if is_generic {
                            if !generic_args.contains(&a) {
                                generic_args.push(a);
                            }
                        } else if looks_like_typevar && !other_args.contains(&a) {
                            other_args.push(a);
                        }
                    }
                }
            }
            type_params = if generic_args.is_empty() {
                other_args
            } else {
                generic_args
            };
        }
        let decorators: Vec<SmolStr> = class
            .decorators()
            .filter_map(|d| d.name_text())
            .map(SmolStr::new)
            .collect();
        let base_texts: Vec<String> = class
            .bases()
            .iter()
            .map(|b| b.syntax().text().to_string())
            .collect();
        let is_protocol = base_texts
            .iter()
            .any(|b| b == "Protocol" || b.ends_with(".Protocol") || b.starts_with("Protocol["));
        let is_enum_like = base_texts.iter().any(|b| {
            matches!(
                b.rsplit('.').next().unwrap_or(""),
                "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag" | "ReprEnum"
            )
        });
        let is_dataclass = decorators.iter().any(|d| {
            d == "dataclass"
                || d.ends_with(".dataclass")
                || d == "dataclasses.dataclass"
                || d == "attrs.define"
                || d == "define"
                || d == "attr.s"
        });
        let docstring = class.docstring().map(|d| Arc::from(d.value().as_str()));
        let mut info = ClassInfo {
            class_ref,
            scope,
            base_exprs,
            keywords,
            type_params,
            decorators,
            docstring,
            members: IndexMap::new(),
            is_protocol,
            is_enum_like,
            is_dataclass,
        };
        info.collect_members(file, class);
        Some(info)
    }

    fn collect_members(&mut self, file: &FileInfo, class: &ast::ClassDef) {
        let Some(body) = class.body() else { return };
        // Flatten `if sys.version_info ...:` / `else:` blocks (typeshed convention);
        // in stubs only the branches live for the target interpreter count.
        let stmts = crate::static_cond::flatten_live(&body, file.static_env.as_ref());
        let cscope = file.scopes.scope(self.scope);
        for stmt in stmts {
            match &stmt {
                ast::Stmt::FuncDef(f) => {
                    let Some(name_tok) = f.name() else { continue };
                    let decos: Vec<String> = f.decorators().filter_map(|d| d.name_text()).collect();
                    let kind = if decos.iter().any(|d| d == "staticmethod") {
                        MemberKind::StaticMethod
                    } else if decos.iter().any(|d| d == "classmethod") {
                        MemberKind::ClassMethod
                    } else if decos.iter().any(|d| {
                        d == "property"
                            || d.ends_with(".setter")
                            || d.ends_with(".getter")
                            || d.ends_with(".deleter")
                            || d == "cached_property"
                            || d == "functools.cached_property"
                    }) {
                        MemberKind::Property
                    } else {
                        MemberKind::Method
                    };
                    let binding = cscope.bindings.get(name_tok.text()).and_then(|ids| {
                        ids.iter()
                            .find(|id| file.scopes.binding(**id).range == name_tok.text_range())
                            .copied()
                    });
                    let name = SmolStr::new(name_tok.text());
                    // Keep the first declaration (property getter / first overload) but
                    // don't let a setter override the getter kind.
                    if !self.members.contains_key(&name) || matches!(kind, MemberKind::Method) {
                        self.members.entry(name.clone()).or_insert(Member {
                            name,
                            kind,
                            file: file.id,
                            range: name_tok.text_range(),
                            binding,
                            value: None,
                            annotation: f.return_annotation().map(|r| r.text_range()),
                            is_stub: file.is_stub,
                        });
                    }
                    // Instance attributes assigned on self inside the method.
                    let canonical_class_receiver = kind == MemberKind::ClassMethod
                        && decos.len() == 1
                        && decos[0] == "classmethod"
                        && file.scopes.resolve(self.scope, "classmethod").is_none();
                    if matches!(kind, MemberKind::Method | MemberKind::Property)
                        || canonical_class_receiver
                    {
                        let self_name = f
                            .params()
                            .and_then(|p| p.params().next())
                            .and_then(|p| p.name())
                            .map(|t| t.text().to_string());
                        if let (Some(self_name), Some(fbody)) = (self_name, f.body()) {
                            self.collect_receiver_attrs(
                                file,
                                &fbody,
                                &self_name,
                                if kind == MemberKind::ClassMethod {
                                    MemberKind::ClassVar
                                } else {
                                    MemberKind::InstanceVar
                                },
                            );
                        }
                    }
                }
                ast::Stmt::ClassDef(c) => {
                    if let Some(n) = c.name() {
                        let binding = cscope
                            .bindings
                            .get(n.text())
                            .and_then(|ids| ids.first().copied());
                        self.members
                            .entry(SmolStr::new(n.text()))
                            .or_insert(Member {
                                name: SmolStr::new(n.text()),
                                kind: MemberKind::NestedClass,
                                file: file.id,
                                range: n.text_range(),
                                binding,
                                value: None,
                                annotation: None,
                                is_stub: file.is_stub,
                            });
                    }
                }
                ast::Stmt::Assign(a) => {
                    let value = a.value().map(|v| v.text_range());
                    for t in a.targets() {
                        self.class_var_target(file, &t, value, None, cscope);
                    }
                    // `__slots__ = ('a', 'b')` declares the instance attributes.
                    if a.targets()
                        .iter()
                        .any(|t| matches!(t, ast::Expr::Name(n) if n.name_text() == "__slots__"))
                    {
                        for s in a.value().into_iter().flat_map(|v| {
                            v.syntax()
                                .descendants()
                                .filter_map(ast::StringExpr::cast)
                                .collect::<Vec<_>>()
                        }) {
                            let name = SmolStr::new(s.value());
                            if !name.is_empty()
                                && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                            {
                                self.members.entry(name.clone()).or_insert(Member {
                                    name,
                                    kind: MemberKind::InstanceVar,
                                    file: file.id,
                                    range: s.text_range(),
                                    binding: None,
                                    value: None,
                                    annotation: None,
                                    is_stub: file.is_stub,
                                });
                            }
                        }
                    }
                }
                ast::Stmt::AnnAssign(a) => {
                    if let Some(t) = a.target() {
                        let is_classvar = a
                            .annotation()
                            .map(|x| x.syntax().text().to_string().contains("ClassVar"))
                            .unwrap_or(false);
                        let value = a.value().map(|v| v.text_range());
                        let ann = a.annotation().map(|x| x.text_range());
                        // In dataclasses and stubs, annotated names are instance attributes.
                        let kind = if is_classvar {
                            MemberKind::ClassVar
                        } else if a.value().is_none() || self.is_dataclass || file.is_stub {
                            MemberKind::InstanceVar
                        } else {
                            MemberKind::ClassVar
                        };
                        if let ast::Expr::Name(n) = t {
                            if let Some(tok) = n.name_token() {
                                let binding = cscope
                                    .bindings
                                    .get(tok.text())
                                    .and_then(|ids| ids.first().copied());
                                let name = SmolStr::new(tok.text());
                                let m = Member {
                                    name: name.clone(),
                                    kind,
                                    file: file.id,
                                    range: tok.text_range(),
                                    binding,
                                    value,
                                    annotation: ann,
                                    is_stub: file.is_stub,
                                };
                                self.members.insert(name, m);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn class_var_target(
        &mut self,
        file: &FileInfo,
        t: &ast::Expr,
        value: Option<TextRange>,
        ann: Option<TextRange>,
        cscope: &crate::scope::Scope,
    ) {
        match t {
            ast::Expr::Name(n) => {
                if let Some(tok) = n.name_token() {
                    let binding = cscope
                        .bindings
                        .get(tok.text())
                        .and_then(|ids| ids.first().copied());
                    let name = SmolStr::new(tok.text());
                    self.members.entry(name.clone()).or_insert(Member {
                        name,
                        kind: MemberKind::ClassVar,
                        file: file.id,
                        range: tok.text_range(),
                        binding,
                        value,
                        annotation: ann,
                        is_stub: file.is_stub,
                    });
                }
            }
            ast::Expr::Tuple(tu) => {
                for e in tu.elements() {
                    self.class_var_target(file, &e, None, None, cscope);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn collect_self_attrs(
        &mut self,
        file: &FileInfo,
        body: &ast::Block,
        self_name: &str,
    ) {
        self.collect_receiver_attrs(file, body, self_name, MemberKind::InstanceVar);
    }

    fn collect_receiver_attrs(
        &mut self,
        file: &FileInfo,
        body: &ast::Block,
        self_name: &str,
        kind: MemberKind,
    ) {
        let mut nodes = body.syntax().preorder();
        while let Some(event) = nodes.next() {
            let rustpy_syntax::rowan::WalkEvent::Enter(node) = event else {
                continue;
            };
            // Don't descend into nested defs/classes.
            if matches!(
                node.kind(),
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
            ) {
                nodes.skip_subtree();
                continue;
            }
            let (targets, value, ann): (Vec<ast::Expr>, Option<TextRange>, Option<TextRange>) =
                match node.kind() {
                    SyntaxKind::ASSIGN_STMT => {
                        let a = ast::AssignStmt::cast(node.clone()).unwrap();
                        (a.targets(), a.value().map(|v| v.text_range()), None)
                    }
                    SyntaxKind::ANN_ASSIGN_STMT => {
                        let a = ast::AnnAssignStmt::cast(node.clone()).unwrap();
                        (
                            a.target().into_iter().collect(),
                            a.value().map(|v| v.text_range()),
                            a.annotation().map(|x| x.text_range()),
                        )
                    }
                    SyntaxKind::AUG_ASSIGN_STMT => {
                        let a = ast::AugAssignStmt::cast(node.clone()).unwrap();
                        (a.target().into_iter().collect(), None, None)
                    }
                    _ => continue,
                };
            for t in targets {
                self.self_attr_target(file, &t, self_name, value, ann, kind);
            }
        }
    }

    fn self_attr_target(
        &mut self,
        file: &FileInfo,
        t: &ast::Expr,
        self_name: &str,
        value: Option<TextRange>,
        ann: Option<TextRange>,
        kind: MemberKind,
    ) {
        match t {
            ast::Expr::Attribute(a) => {
                if let (Some(ast::Expr::Name(obj)), Some(attr)) = (a.object(), a.attr()) {
                    if obj.name_text() == self_name {
                        let name = SmolStr::new(attr.text());
                        let existing = self.members.get(&name);
                        // Declared annotations in the class body win; otherwise first assignment wins
                        // unless the later one carries an annotation or a value where the first had none.
                        let replace = match existing {
                            None => true,
                            Some(m) => {
                                m.kind == kind
                                    && m.annotation.is_none()
                                    && (ann.is_some() || (m.value.is_none() && value.is_some()))
                            }
                        };
                        if replace {
                            self.members.insert(
                                name.clone(),
                                Member {
                                    name,
                                    kind,
                                    file: file.id,
                                    range: attr.text_range(),
                                    binding: None,
                                    value,
                                    annotation: ann,
                                    is_stub: file.is_stub,
                                },
                            );
                        }
                    }
                }
            }
            ast::Expr::Tuple(tu) => {
                for e in tu.elements() {
                    self.self_attr_target(file, &e, self_name, None, None, kind);
                }
            }
            ast::Expr::Paren(p) => {
                if let Some(i) = p.inner() {
                    self.self_attr_target(file, &i, self_name, value, ann, kind);
                }
            }
            _ => {}
        }
    }
}

/// C3 linearization. `bases_mro` are the MROs of the direct bases in order.
pub fn c3_merge(class: ClassRef, bases_mro: Vec<Vec<ClassRef>>) -> Vec<ClassRef> {
    let mut result = vec![class];
    let mut seqs: Vec<Vec<ClassRef>> = bases_mro.into_iter().filter(|s| !s.is_empty()).collect();
    let direct: Vec<ClassRef> = seqs.iter().filter_map(|s| s.first().cloned()).collect();
    seqs.push(direct);
    loop {
        seqs.retain(|s| !s.is_empty());
        if seqs.is_empty() {
            return result;
        }
        let mut candidate = None;
        for s in &seqs {
            let head = &s[0];
            let in_tail = seqs.iter().any(|other| other[1..].contains(head));
            if !in_tail {
                candidate = Some(head.clone());
                break;
            }
        }
        match candidate {
            Some(c) => {
                for s in &mut seqs {
                    if s.first() == Some(&c) {
                        s.remove(0);
                    }
                }
                if !result.contains(&c) {
                    result.push(c);
                }
            }
            None => {
                // Inconsistent hierarchy: fall back to a DFS order.
                for s in &seqs {
                    for c in s {
                        if !result.contains(c) {
                            result.push(c.clone());
                        }
                    }
                }
                return result;
            }
        }
    }
}

/// Kind used for the `is_definition` check on a binding.
pub fn binding_is_class(kind: &BindingKind) -> bool {
    matches!(kind, BindingKind::Class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Analysis;
    use std::path::Path;

    #[test]
    fn collects_members_and_type_params() {
        let db = Analysis::for_project(Path::new("/x"));
        let src = "from typing import Generic, TypeVar\nT = TypeVar('T')\n\nclass Box(Generic[T]):\n    count: int = 0\n    label: str\n\n    def __init__(self, item: T):\n        self.item = item\n        self._cache = None\n\n    @property\n    def size(self) -> int:\n        return 1\n\n    @staticmethod\n    def make(): ...\n\n    @classmethod\n    def of(cls, x): ...\n\n    class Inner: ...\n";
        let id = db.set_file_text(Path::new("/x/m.py"), src);
        let file = db.file(id).unwrap();
        let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
        let class = module
            .statements()
            .find_map(|s| {
                if let ast::Stmt::ClassDef(c) = s {
                    Some(c)
                } else {
                    None
                }
            })
            .unwrap();
        let info = ClassInfo::new(&file, &class).unwrap();
        assert_eq!(info.type_params, vec![SmolStr::new("T")]);
        let kinds: Vec<(String, MemberKind)> = info
            .members
            .values()
            .map(|m| (m.name.to_string(), m.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("count".into(), MemberKind::ClassVar),
                ("label".into(), MemberKind::InstanceVar),
                ("__init__".into(), MemberKind::Method),
                ("item".into(), MemberKind::InstanceVar),
                ("_cache".into(), MemberKind::InstanceVar),
                ("size".into(), MemberKind::Property),
                ("make".into(), MemberKind::StaticMethod),
                ("of".into(), MemberKind::ClassMethod),
                ("Inner".into(), MemberKind::NestedClass)
            ]
        );
    }

    #[test]
    fn c3_linearization() {
        let c = |n: &str| ClassRef {
            file: FileId(0),
            range: TextRange::empty(rustpy_core::TextSize::new(n.len() as u32)),
            name: n.into(),
        };
        // class A(O); class B(O); class C(A, B)
        let o = c("O");
        let a_mro = vec![c("A"), o.clone()];
        let b_mro = vec![c("B"), o.clone()];
        let mro = c3_merge(c("C"), vec![a_mro, b_mro]);
        assert_eq!(
            mro.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["C", "A", "B", "O"]
        );
    }
}

//! Typed AST layer over the lossless syntax tree (roadmap §2.1).
//!
//! Every wrapper is a thin newtype around a [`SyntaxNode`]; accessors walk
//! children lazily, so the AST is always consistent with the CST.

use rowan::NodeOrToken;
use rustpy_core::TextRange;

use crate::syntax_kind::SyntaxKind::{self, *};
use crate::syntax_kind::{SyntaxNode, SyntaxToken};

/// Trait implemented by all typed nodes.
pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;

    fn text_range(&self) -> TextRange {
        self.syntax().text_range()
    }
    fn text(&self) -> String {
        self.syntax().text().to_string()
    }
}

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);
        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if node.kind() == $kind {
                    Some(Self(node))
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

macro_rules! ast_enum {
    ($name:ident { $($variant:ident($ty:ident)),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant($ty)),* }
        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                $( $ty::can_cast(kind) )||*
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                $( if let Some(n) = $ty::cast(node.clone()) { return Some($name::$variant(n)); } )*
                None
            }
            fn syntax(&self) -> &SyntaxNode {
                match self { $( $name::$variant(n) => n.syntax() ),* }
            }
        }
    };
}

// ----- generic helpers ------------------------------------------------------

pub fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

pub fn children<N: AstNode>(parent: &SyntaxNode) -> impl Iterator<Item = N> {
    parent.children().filter_map(N::cast)
}

pub fn token(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|t| t.kind() == kind)
}

fn nth_child<N: AstNode>(parent: &SyntaxNode, n: usize) -> Option<N> {
    children::<N>(parent).nth(n)
}

// ----- module & blocks ------------------------------------------------------

ast_node!(Module, MODULE);
ast_node!(Block, BLOCK);

impl Module {
    pub fn statements(&self) -> impl Iterator<Item = Stmt> {
        children::<Stmt>(&self.0)
    }
    /// The module docstring, if the first statement is a string literal.
    pub fn docstring(&self) -> Option<StringLike> {
        docstring_of(self.statements())
    }
}

impl Block {
    pub fn statements(&self) -> impl Iterator<Item = Stmt> {
        children::<Stmt>(&self.0)
    }
    pub fn docstring(&self) -> Option<StringLike> {
        docstring_of(self.statements())
    }
}

fn docstring_of(mut stmts: impl Iterator<Item = Stmt>) -> Option<StringLike> {
    match stmts.next()? {
        Stmt::Expr(e) => match e.expr()? {
            Expr::String(s) => Some(StringLike::Single(s)),
            Expr::ConcatString(s) => Some(StringLike::Concat(s)),
            _ => None,
        },
        _ => None,
    }
}

// ----- statements ----------------------------------------------------------

ast_node!(ExprStmt, EXPR_STMT);
ast_node!(AssignStmt, ASSIGN_STMT);
ast_node!(AugAssignStmt, AUG_ASSIGN_STMT);
ast_node!(AnnAssignStmt, ANN_ASSIGN_STMT);
ast_node!(ReturnStmt, RETURN_STMT);
ast_node!(PassStmt, PASS_STMT);
ast_node!(BreakStmt, BREAK_STMT);
ast_node!(ContinueStmt, CONTINUE_STMT);
ast_node!(DelStmt, DEL_STMT);
ast_node!(AssertStmt, ASSERT_STMT);
ast_node!(GlobalStmt, GLOBAL_STMT);
ast_node!(NonlocalStmt, NONLOCAL_STMT);
ast_node!(RaiseStmt, RAISE_STMT);
ast_node!(ImportStmt, IMPORT_STMT);
ast_node!(ImportFromStmt, IMPORT_FROM_STMT);
ast_node!(IfStmt, IF_STMT);
ast_node!(WhileStmt, WHILE_STMT);
ast_node!(ForStmt, FOR_STMT);
ast_node!(TryStmt, TRY_STMT);
ast_node!(WithStmt, WITH_STMT);
ast_node!(FuncDef, FUNC_DEF);
ast_node!(ClassDef, CLASS_DEF);
ast_node!(TypeAliasStmt, TYPE_ALIAS_STMT);
ast_node!(MatchStmt, MATCH_STMT);

ast_enum!(Stmt {
    Expr(ExprStmt),
    Assign(AssignStmt),
    AugAssign(AugAssignStmt),
    AnnAssign(AnnAssignStmt),
    Return(ReturnStmt),
    Pass(PassStmt),
    Break(BreakStmt),
    Continue(ContinueStmt),
    Del(DelStmt),
    Assert(AssertStmt),
    Global(GlobalStmt),
    Nonlocal(NonlocalStmt),
    Raise(RaiseStmt),
    Import(ImportStmt),
    ImportFrom(ImportFromStmt),
    If(IfStmt),
    While(WhileStmt),
    For(ForStmt),
    Try(TryStmt),
    With(WithStmt),
    FuncDef(FuncDef),
    ClassDef(ClassDef),
    TypeAlias(TypeAliasStmt),
    Match(MatchStmt),
});

ast_node!(ElifClause, ELIF_CLAUSE);
ast_node!(ElseClause, ELSE_CLAUSE);
ast_node!(ExceptClause, EXCEPT_CLAUSE);
ast_node!(FinallyClause, FINALLY_CLAUSE);
ast_node!(WithItem, WITH_ITEM);
ast_node!(ImportAlias, IMPORT_ALIAS);
ast_node!(DottedName, DOTTED_NAME);
ast_node!(RelativeLevel, RELATIVE_LEVEL);
ast_node!(Decorators, DECORATORS);
ast_node!(Decorator, DECORATOR);
ast_node!(ParamList, PARAM_LIST);
ast_node!(Param, PARAM);
ast_node!(StarParam, STAR_PARAM);
ast_node!(StarStarParam, STAR_STAR_PARAM);
ast_node!(SlashSeparator, SLASH_SEPARATOR);
ast_node!(StarSeparator, STAR_SEPARATOR);
ast_node!(Annotation, ANNOTATION);
ast_node!(DefaultValue, DEFAULT_VALUE);
ast_node!(ReturnAnnotation, RETURN_ANNOTATION);
ast_node!(TypeParamList, TYPE_PARAM_LIST);
ast_node!(TypeParam, TYPE_PARAM);
ast_node!(TypeVarTupleParam, TYPE_VAR_TUPLE_PARAM);
ast_node!(ParamSpecParam, PARAM_SPEC_PARAM);
ast_node!(CaseClause, CASE_CLAUSE);
ast_node!(Guard, GUARD);

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl AssignStmt {
    /// All targets (everything before the last `=`).
    pub fn targets(&self) -> Vec<Expr> {
        let mut v: Vec<Expr> = children(&self.0).collect();
        v.pop();
        v
    }
    pub fn value(&self) -> Option<Expr> {
        children::<Expr>(&self.0).last()
    }
}

impl AugAssignStmt {
    pub fn target(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .find(|t| t.kind().is_augmented_assign_op())
    }
    pub fn value(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl AnnAssignStmt {
    pub fn target(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn annotation(&self) -> Option<Expr> {
        child::<Annotation>(&self.0).and_then(|a| a.expr())
    }
    pub fn value(&self) -> Option<Expr> {
        // The value follows the ANNOTATION node.
        let mut seen_ann = false;
        for c in self.0.children() {
            if c.kind() == ANNOTATION {
                seen_ann = true;
            } else if seen_ann {
                return Expr::cast(c);
            }
        }
        None
    }
}

impl Annotation {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl RaiseStmt {
    pub fn exception(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn cause(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl AssertStmt {
    pub fn test(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn message(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl DelStmt {
    pub fn targets(&self) -> Vec<Expr> {
        match child::<Expr>(&self.0) {
            Some(Expr::Tuple(t)) if token(t.syntax(), L_PAREN).is_none() => t.elements().collect(),
            Some(e) => vec![e],
            None => Vec::new(),
        }
    }
}

impl GlobalStmt {
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|t| t.kind() == IDENT)
    }
}

impl NonlocalStmt {
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|t| t.kind() == IDENT)
    }
}

impl ImportStmt {
    pub fn aliases(&self) -> impl Iterator<Item = ImportAlias> {
        children(&self.0)
    }
}

impl ImportFromStmt {
    /// Number of leading dots (relative import level).
    pub fn level(&self) -> usize {
        child::<RelativeLevel>(&self.0)
            .map(|l| {
                l.syntax()
                    .text()
                    .to_string()
                    .chars()
                    .filter(|c| *c == '.')
                    .count()
            })
            .unwrap_or(0)
    }
    pub fn module(&self) -> Option<DottedName> {
        child(&self.0)
    }
    pub fn aliases(&self) -> impl Iterator<Item = ImportAlias> {
        children(&self.0)
    }
    pub fn is_star(&self) -> bool {
        token(&self.0, STAR).is_some()
    }
    /// Module path as written, e.g. `..pkg.mod`.
    pub fn module_path(&self) -> String {
        let dots = ".".repeat(self.level());
        format!(
            "{dots}{}",
            self.module().map(|m| m.path()).unwrap_or_default()
        )
    }
}

impl ImportAlias {
    /// Dotted module name for `import a.b`, or the imported name for `from x import name`.
    pub fn name(&self) -> String {
        match child::<DottedName>(&self.0) {
            Some(d) => d.path(),
            None => token(&self.0, IDENT)
                .map(|t| t.text().to_string())
                .unwrap_or_default(),
        }
    }
    pub fn name_token(&self) -> Option<SyntaxToken> {
        match child::<DottedName>(&self.0) {
            Some(d) => d.first_token(),
            None => token(&self.0, IDENT),
        }
    }
    pub fn alias(&self) -> Option<SyntaxToken> {
        let mut after_as = false;
        for el in self.0.children_with_tokens() {
            if let NodeOrToken::Token(t) = el {
                if t.kind() == AS_KW {
                    after_as = true;
                } else if after_as && t.kind() == IDENT {
                    return Some(t);
                }
            }
        }
        None
    }
    /// Name bound in the importing scope (`b` for `import a as b`, `a` for `import a.c`).
    pub fn bound_name(&self) -> Option<String> {
        if let Some(a) = self.alias() {
            return Some(a.text().to_string());
        }
        let name = self.name();
        name.split('.').next().map(str::to_owned)
    }
}

impl DottedName {
    pub fn path(&self) -> String {
        self.0
            .text()
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }
    pub fn first_token(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|t| t.kind() == IDENT)
    }
}

impl IfStmt {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn elif_clauses(&self) -> impl Iterator<Item = ElifClause> {
        children(&self.0)
    }
    pub fn else_clause(&self) -> Option<ElseClause> {
        child(&self.0)
    }
}

impl ElifClause {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl ElseClause {
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn else_clause(&self) -> Option<ElseClause> {
        child(&self.0)
    }
}

impl ForStmt {
    pub fn is_async(&self) -> bool {
        token(&self.0, ASYNC_KW).is_some()
    }
    pub fn target(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn iter(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn else_clause(&self) -> Option<ElseClause> {
        child(&self.0)
    }
}

impl TryStmt {
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn except_clauses(&self) -> impl Iterator<Item = ExceptClause> {
        children(&self.0)
    }
    pub fn else_clause(&self) -> Option<ElseClause> {
        child(&self.0)
    }
    pub fn finally_clause(&self) -> Option<FinallyClause> {
        child(&self.0)
    }
}

impl ExceptClause {
    pub fn is_star(&self) -> bool {
        token(&self.0, STAR).is_some()
    }
    pub fn exception_type(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl FinallyClause {
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl WithStmt {
    pub fn is_async(&self) -> bool {
        token(&self.0, ASYNC_KW).is_some()
    }
    pub fn items(&self) -> impl Iterator<Item = WithItem> {
        children(&self.0)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
}

impl WithItem {
    pub fn context_expr(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn target(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl FuncDef {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn name_text(&self) -> String {
        self.name()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
    pub fn is_async(&self) -> bool {
        token(&self.0, ASYNC_KW).is_some()
    }
    pub fn decorators(&self) -> impl Iterator<Item = Decorator> {
        child::<Decorators>(&self.0)
            .into_iter()
            .flat_map(|d| children::<Decorator>(d.syntax()).collect::<Vec<_>>())
    }
    pub fn params(&self) -> Option<ParamList> {
        child(&self.0)
    }
    pub fn type_params(&self) -> Option<TypeParamList> {
        child(&self.0)
    }
    pub fn return_annotation(&self) -> Option<Expr> {
        child::<ReturnAnnotation>(&self.0).and_then(|r| child(r.syntax()))
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn docstring(&self) -> Option<StringLike> {
        self.body().and_then(|b| b.docstring())
    }
    /// True for `def` nested directly in a class body.
    pub fn is_method(&self) -> bool {
        self.0
            .parent()
            .and_then(|b| b.parent())
            .is_some_and(|p| p.kind() == CLASS_DEF)
    }
    pub fn has_decorator(&self, name: &str) -> bool {
        self.decorators()
            .any(|d| d.name_text().as_deref() == Some(name))
    }
    pub fn def_token(&self) -> Option<SyntaxToken> {
        token(&self.0, DEF_KW)
    }
}

impl Decorator {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
    /// Dotted name of the decorator without call arguments (`a.b` for `@a.b(1)`).
    pub fn name_text(&self) -> Option<String> {
        let mut e = self.expr()?;
        if let Expr::Call(c) = &e {
            e = c.callee()?;
        }
        match e {
            Expr::Name(n) => Some(n.name_text()),
            Expr::Attribute(a) => Some(
                a.syntax()
                    .text()
                    .to_string()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect(),
            ),
            _ => None,
        }
    }
}

impl ClassDef {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn name_text(&self) -> String {
        self.name()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
    pub fn decorators(&self) -> impl Iterator<Item = Decorator> {
        child::<Decorators>(&self.0)
            .into_iter()
            .flat_map(|d| children::<Decorator>(d.syntax()).collect::<Vec<_>>())
    }
    pub fn type_params(&self) -> Option<TypeParamList> {
        child(&self.0)
    }
    pub fn arguments(&self) -> Option<ArgList> {
        child(&self.0)
    }
    pub fn bases(&self) -> Vec<Expr> {
        self.arguments()
            .map(|a| a.positional().collect())
            .unwrap_or_default()
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    pub fn docstring(&self) -> Option<StringLike> {
        self.body().and_then(|b| b.docstring())
    }
    pub fn methods(&self) -> impl Iterator<Item = FuncDef> {
        self.body().into_iter().flat_map(|b| {
            b.statements()
                .filter_map(|s| {
                    if let Stmt::FuncDef(f) = s {
                        Some(f)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        })
    }
    pub fn class_token(&self) -> Option<SyntaxToken> {
        token(&self.0, CLASS_KW)
    }
}

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = AnyParam> {
        children(&self.0)
    }
    /// Plain parameters only (`a`, `b=1`), excluding `*args`/`**kwargs`.
    pub fn plain_params(&self) -> impl Iterator<Item = Param> {
        children(&self.0)
    }
}

ast_enum!(AnyParam {
    Plain(Param),
    Star(StarParam),
    StarStar(StarStarParam),
    Slash(SlashSeparator),
    StarSep(StarSeparator),
});

impl AnyParam {
    pub fn name(&self) -> Option<SyntaxToken> {
        match self {
            AnyParam::Plain(p) => p.name(),
            AnyParam::Star(p) => p.name(),
            AnyParam::StarStar(p) => p.name(),
            _ => None,
        }
    }
    pub fn annotation(&self) -> Option<Expr> {
        match self {
            AnyParam::Plain(p) => p.annotation(),
            AnyParam::Star(p) => child::<Annotation>(p.syntax()).and_then(|a| a.expr()),
            AnyParam::StarStar(p) => child::<Annotation>(p.syntax()).and_then(|a| a.expr()),
            _ => None,
        }
    }
    pub fn default(&self) -> Option<Expr> {
        match self {
            AnyParam::Plain(p) => p.default(),
            _ => None,
        }
    }
}

impl Param {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn name_text(&self) -> String {
        self.name()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
    pub fn annotation(&self) -> Option<Expr> {
        child::<Annotation>(&self.0).and_then(|a| a.expr())
    }
    pub fn default(&self) -> Option<Expr> {
        child::<DefaultValue>(&self.0).and_then(|d| child(d.syntax()))
    }
}

impl StarParam {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

impl StarStarParam {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

impl TypeParamList {
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        self.0.children().filter_map(|c| token(&c, IDENT))
    }
}

impl TypeAliasStmt {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl MatchStmt {
    pub fn subject(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn cases(&self) -> impl Iterator<Item = CaseClause> {
        children(&self.0)
    }
}

impl CaseClause {
    pub fn guard(&self) -> Option<Expr> {
        child::<Guard>(&self.0).and_then(|g| child(g.syntax()))
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.0)
    }
    /// The pattern node (any `MATCH_*_PATTERN` kind).
    pub fn pattern(&self) -> Option<SyntaxNode> {
        self.0.children().find(|c| c.kind().is_pattern())
    }
}

// ----- expressions ---------------------------------------------------------

ast_node!(NameExpr, NAME_EXPR);
ast_node!(LiteralExpr, LITERAL_EXPR);
ast_node!(StringExpr, STRING_EXPR);
ast_node!(FStringExpr, FSTRING_EXPR);
ast_node!(ConcatStringExpr, CONCAT_STRING_EXPR);
ast_node!(ParenExpr, PAREN_EXPR);
ast_node!(TupleExpr, TUPLE_EXPR);
ast_node!(ListExpr, LIST_EXPR);
ast_node!(SetExpr, SET_EXPR);
ast_node!(DictExpr, DICT_EXPR);
ast_node!(ListComp, LIST_COMP);
ast_node!(SetComp, SET_COMP);
ast_node!(DictComp, DICT_COMP);
ast_node!(GeneratorExpr, GENERATOR_EXPR);
ast_node!(AttributeExpr, ATTRIBUTE_EXPR);
ast_node!(SubscriptExpr, SUBSCRIPT_EXPR);
ast_node!(SliceExpr, SLICE_EXPR);
ast_node!(SliceList, SLICE_LIST);
ast_node!(CallExpr, CALL_EXPR);
ast_node!(UnaryExpr, UNARY_EXPR);
ast_node!(BinaryExpr, BINARY_EXPR);
ast_node!(BoolOpExpr, BOOL_OP_EXPR);
ast_node!(CompareExpr, COMPARE_EXPR);
ast_node!(NotExpr, NOT_EXPR);
ast_node!(LambdaExpr, LAMBDA_EXPR);
ast_node!(ConditionalExpr, CONDITIONAL_EXPR);
ast_node!(NamedExpr, NAMED_EXPR);
ast_node!(StarExpr, STAR_EXPR);
ast_node!(AwaitExpr, AWAIT_EXPR);
ast_node!(YieldExpr, YIELD_EXPR);
ast_node!(YieldFromExpr, YIELD_FROM_EXPR);
ast_node!(DictItem, DICT_ITEM);
ast_node!(DictUnpack, DICT_UNPACK);
ast_node!(CompFor, COMP_FOR);
ast_node!(CompIf, COMP_IF);
ast_node!(CompareOp, COMPARE_OP);
ast_node!(ArgList, ARG_LIST);
ast_node!(Arg, ARG);
ast_node!(KeywordArg, KEYWORD_ARG);
ast_node!(StarArg, STAR_ARG);
ast_node!(StarStarArg, STAR_STAR_ARG);
ast_node!(FStringReplacement, FSTRING_REPLACEMENT);

ast_enum!(Expr {
    Name(NameExpr),
    Literal(LiteralExpr),
    String(StringExpr),
    FString(FStringExpr),
    ConcatString(ConcatStringExpr),
    Paren(ParenExpr),
    Tuple(TupleExpr),
    List(ListExpr),
    Set(SetExpr),
    Dict(DictExpr),
    ListComp(ListComp),
    SetComp(SetComp),
    DictComp(DictComp),
    Generator(GeneratorExpr),
    Attribute(AttributeExpr),
    Subscript(SubscriptExpr),
    Call(CallExpr),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    BoolOp(BoolOpExpr),
    Compare(CompareExpr),
    Not(NotExpr),
    Lambda(LambdaExpr),
    Conditional(ConditionalExpr),
    Named(NamedExpr),
    Star(StarExpr),
    Await(AwaitExpr),
    Yield(YieldExpr),
    YieldFrom(YieldFromExpr),
    Slice(SliceExpr),
});

/// A string literal or an implicit concatenation of them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StringLike {
    Single(StringExpr),
    Concat(ConcatStringExpr),
}

impl StringLike {
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            StringLike::Single(s) => s.syntax(),
            StringLike::Concat(s) => s.syntax(),
        }
    }
    /// Decoded value (escapes processed, prefixes/quotes removed).
    pub fn value(&self) -> String {
        match self {
            StringLike::Single(s) => s.value(),
            StringLike::Concat(c) => c
                .parts()
                .map(|p| match p {
                    Expr::String(s) => s.value(),
                    other => other.syntax().text().to_string(),
                })
                .collect(),
        }
    }
}

impl Expr {
    /// Strips redundant parentheses.
    pub fn unparen(self) -> Expr {
        let mut e = self;
        while let Expr::Paren(p) = &e {
            match p.inner() {
                Some(inner) => e = inner,
                None => break,
            }
        }
        e
    }
}

impl NameExpr {
    pub fn name_token(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn name_text(&self) -> String {
        self.name_token()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
}

impl LiteralExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0.first_token()
    }
    pub fn kind(&self) -> Option<SyntaxKind> {
        self.token().map(|t| t.kind())
    }
}

impl StringExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        token(&self.0, STRING)
    }
    pub fn raw_text(&self) -> String {
        self.0.text().to_string()
    }
    pub fn prefix(&self) -> String {
        let t = self.raw_text();
        t.chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_lowercase()
    }
    pub fn is_bytes(&self) -> bool {
        self.prefix().contains('b')
    }
    pub fn is_raw(&self) -> bool {
        self.prefix().contains('r')
    }
    /// Decoded string value.
    pub fn value(&self) -> String {
        decode_string_literal(&self.raw_text())
    }
    /// Range of the content between the quotes.
    pub fn content_range(&self) -> TextRange {
        let text = self.raw_text();
        let prefix_len = text.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        let body = &text[prefix_len..];
        let quote_len = if body.starts_with("\"\"\"") || body.starts_with("'''") {
            3
        } else {
            1
        };
        let start =
            self.text_range().start() + rustpy_core::TextSize::new((prefix_len + quote_len) as u32);
        let end_quote = if body.len() >= 2 * quote_len
            && (body.ends_with("\"\"\"")
                || body.ends_with("'''")
                || (quote_len == 1 && (body.ends_with('"') || body.ends_with('\''))))
        {
            quote_len
        } else {
            0
        };
        let end = self.text_range().end() - rustpy_core::TextSize::new(end_quote as u32);
        TextRange::new(start, end.max(start))
    }
}

/// Decodes a Python string literal (prefix + quotes + escapes) into its value.
pub fn decode_string_literal(raw: &str) -> String {
    let prefix: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let is_raw = prefix.to_ascii_lowercase().contains('r');
    let body = &raw[prefix.len()..];
    let quote_len = if body.starts_with("\"\"\"") || body.starts_with("'''") {
        3
    } else {
        1
    };
    let mut inner = &body[quote_len.min(body.len())..];
    if inner.len() >= quote_len
        && (inner.ends_with("\"\"\"")
            || inner.ends_with("'''")
            || (quote_len == 1 && (inner.ends_with('"') || inner.ends_with('\''))))
    {
        inner = &inner[..inner.len() - quote_len];
    }
    if is_raw || !inner.contains('\\') {
        return inner.to_string();
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('\n') => {}
            Some('x') => push_hex(&mut chars, 2, &mut out),
            Some('u') => push_hex(&mut chars, 4, &mut out),
            Some('U') => push_hex(&mut chars, 8, &mut out),
            Some('N') => {
                // Named escapes need the unicode database; keep as-is.
                out.push_str("\\N");
            }
            Some(d) if ('0'..='7').contains(&d) => {
                let mut v = d.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.peek() {
                        Some(n) if ('0'..='7').contains(n) => {
                            v = v * 8 + n.to_digit(8).unwrap();
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn push_hex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, n: usize, out: &mut String) {
    let mut v = 0u32;
    let mut taken = 0;
    while taken < n {
        match chars.peek().and_then(|c| c.to_digit(16)) {
            Some(d) => {
                v = v * 16 + d;
                chars.next();
                taken += 1;
            }
            None => break,
        }
    }
    out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
}

impl ConcatStringExpr {
    pub fn parts(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
}

impl FStringExpr {
    pub fn replacements(&self) -> impl Iterator<Item = FStringReplacement> {
        children(&self.0)
    }
    pub fn is_template(&self) -> bool {
        self.0
            .first_token()
            .is_some_and(|t| t.kind() == TSTRING_START)
    }
}

impl FStringReplacement {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl TupleExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
}

impl ListExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
}

impl SetExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
}

impl DictExpr {
    pub fn items(&self) -> impl Iterator<Item = DictItem> {
        children(&self.0)
    }
    pub fn unpacks(&self) -> impl Iterator<Item = DictUnpack> {
        children(&self.0)
    }
}

impl DictItem {
    pub fn key(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn value(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl DictUnpack {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ListComp {
    pub fn element(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn clauses(&self) -> impl Iterator<Item = CompFor> {
        children(&self.0)
    }
}

impl SetComp {
    pub fn element(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn clauses(&self) -> impl Iterator<Item = CompFor> {
        children(&self.0)
    }
}

impl GeneratorExpr {
    pub fn element(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn clauses(&self) -> impl Iterator<Item = CompFor> {
        children(&self.0)
    }
}

impl DictComp {
    pub fn item(&self) -> Option<DictItem> {
        child(&self.0)
    }
    pub fn clauses(&self) -> impl Iterator<Item = CompFor> {
        children(&self.0)
    }
}

impl CompFor {
    pub fn is_async(&self) -> bool {
        token(&self.0, ASYNC_KW).is_some()
    }
    pub fn target(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn iter(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
    pub fn conditions(&self) -> impl Iterator<Item = Expr> {
        let mut conds = Vec::new();
        let mut sib = self.0.next_sibling();
        while let Some(n) = sib {
            if n.kind() != COMP_IF {
                break;
            }
            if let Some(e) = child::<Expr>(&n) {
                conds.push(e);
            }
            sib = n.next_sibling();
        }
        conds.into_iter()
    }
}

impl AttributeExpr {
    pub fn object(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn attr(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn attr_text(&self) -> String {
        self.attr()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
}

impl SubscriptExpr {
    pub fn object(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    /// The index expression (an `Expr`, or the SLICE_LIST node for `a[1, 2]`).
    pub fn index(&self) -> Option<SyntaxNode> {
        self.0.children().nth(1)
    }
    pub fn index_expr(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        child(&self.0)
    }
    pub fn arg_list(&self) -> Option<ArgList> {
        child(&self.0)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = AnyArg> {
        children(&self.0)
    }
    pub fn positional(&self) -> impl Iterator<Item = Expr> {
        children::<Arg>(&self.0).filter_map(|a| a.expr())
    }
    pub fn keywords(&self) -> impl Iterator<Item = KeywordArg> {
        children(&self.0)
    }
}

ast_enum!(AnyArg {
    Positional(Arg),
    Keyword(KeywordArg),
    Star(StarArg),
    StarStar(StarStarArg),
    Generator(GeneratorExpr),
});

impl AnyArg {
    pub fn value(&self) -> Option<Expr> {
        match self {
            AnyArg::Positional(a) => a.expr(),
            AnyArg::Keyword(k) => k.value(),
            AnyArg::Star(s) => child(s.syntax()),
            AnyArg::StarStar(s) => child(s.syntax()),
            AnyArg::Generator(g) => Some(Expr::Generator(g.clone())),
        }
    }
}

impl Arg {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl KeywordArg {
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
    pub fn name_text(&self) -> String {
        self.name()
            .map(|t| t.text().to_string())
            .unwrap_or_default()
    }
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl UnaryExpr {
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0.first_token()
    }
    pub fn operand(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn rhs(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
    }
}

impl BoolOpExpr {
    pub fn operands(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
    pub fn is_or(&self) -> bool {
        token(&self.0, OR_KW).is_some()
    }
}

impl CompareExpr {
    pub fn operands(&self) -> impl Iterator<Item = Expr> {
        children(&self.0)
    }
    pub fn ops(&self) -> impl Iterator<Item = CompareOp> {
        children(&self.0)
    }
}

impl CompareOp {
    pub fn text(&self) -> String {
        self.0
            .text()
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl NotExpr {
    pub fn operand(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LambdaExpr {
    pub fn params(&self) -> Option<ParamList> {
        child(&self.0)
    }
    pub fn body(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ConditionalExpr {
    pub fn true_expr(&self) -> Option<Expr> {
        nth_child(&self.0, 0)
    }
    pub fn condition(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
    pub fn false_expr(&self) -> Option<Expr> {
        nth_child(&self.0, 2)
    }
}

impl NamedExpr {
    pub fn target(&self) -> Option<NameExpr> {
        child(&self.0)
    }
    pub fn value(&self) -> Option<Expr> {
        nth_child(&self.0, 1)
    }
}

impl StarExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl AwaitExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl YieldExpr {
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl YieldFromExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl SliceExpr {
    pub fn parts(&self) -> (Option<Expr>, Option<Expr>, Option<Expr>) {
        // Positions are determined by colons.
        let mut slot = 0;
        let mut out: [Option<Expr>; 3] = [None, None, None];
        for el in self.0.children_with_tokens() {
            match el {
                NodeOrToken::Token(t) if t.kind() == COLON => slot += 1,
                NodeOrToken::Node(n) if slot < 3 => out[slot] = Expr::cast(n),
                _ => {}
            }
        }
        let [a, b, c] = out;
        (a, b, c)
    }
}

// ----- navigation helpers --------------------------------------------------

/// Finds the innermost node of type `N` containing `offset`.
pub fn node_at_offset<N: AstNode>(root: &SyntaxNode, offset: rustpy_core::TextSize) -> Option<N> {
    let tok = root.token_at_offset(offset).right_biased()?;
    tok.parent_ancestors().find_map(N::cast)
}

/// Ancestors of `node` that are typed `N`, innermost first.
pub fn ancestors_of_type<N: AstNode>(node: &SyntaxNode) -> impl Iterator<Item = N> {
    node.ancestors().filter_map(N::cast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn walks_definitions() {
        let src = "import os.path as osp\nfrom ..a.b import c as d, e\n\n@deco(1)\nclass C(Base, kw=1):\n    '''Doc.'''\n    def m(self, x: int = 1, *args, **kw) -> str:\n        return f'{x}'\n";
        let p = parse(src);
        assert!(p.ok(), "{:?}", p.errors());
        let module = Module::cast(p.syntax_node()).unwrap();
        let stmts: Vec<Stmt> = module.statements().collect();
        assert_eq!(stmts.len(), 3);
        let Stmt::Import(i) = &stmts[0] else { panic!() };
        let a = i.aliases().next().unwrap();
        assert_eq!(a.name(), "os.path");
        assert_eq!(a.alias().unwrap().text(), "osp");
        assert_eq!(a.bound_name().as_deref(), Some("osp"));
        let Stmt::ImportFrom(f) = &stmts[1] else {
            panic!()
        };
        assert_eq!(f.level(), 2);
        assert_eq!(f.module_path(), "..a.b");
        assert_eq!(
            f.aliases()
                .map(|a| a.bound_name().unwrap())
                .collect::<Vec<_>>(),
            vec!["d", "e"]
        );
        let Stmt::ClassDef(c) = &stmts[2] else {
            panic!()
        };
        assert_eq!(c.name_text(), "C");
        assert_eq!(c.docstring().unwrap().value(), "Doc.");
        assert_eq!(
            c.decorators().next().unwrap().name_text().as_deref(),
            Some("deco")
        );
        assert_eq!(c.bases().len(), 1);
        let m = c.methods().next().unwrap();
        assert_eq!(m.name_text(), "m");
        assert!(m.is_method());
        let params: Vec<_> = m.params().unwrap().params().collect();
        assert_eq!(params.len(), 4);
        assert_eq!(params[1].name().unwrap().text(), "x");
        assert!(params[1].annotation().is_some());
        assert!(params[1].default().is_some());
        assert!(m.return_annotation().is_some());
    }

    #[test]
    fn string_decoding() {
        assert_eq!(decode_string_literal(r#""a\n\x41\u00e9\101""#), "a\nAéA");
        assert_eq!(decode_string_literal(r#"r'\n'"#), "\\n");
        assert_eq!(decode_string_literal("'''x'''"), "x");
        assert_eq!(decode_string_literal("b'\\t'"), "\t");
    }

    #[test]
    fn expression_accessors() {
        let p = parse("x = a.b(1, k=2)[0] if c else -d\n");
        let module = Module::cast(p.syntax_node()).unwrap();
        let Stmt::Assign(a) = module.statements().next().unwrap() else {
            panic!()
        };
        assert_eq!(a.targets().len(), 1);
        let Expr::Conditional(cond) = a.value().unwrap() else {
            panic!()
        };
        let Expr::Subscript(sub) = cond.true_expr().unwrap() else {
            panic!()
        };
        let Expr::Call(call) = sub.object().unwrap() else {
            panic!()
        };
        let Expr::Attribute(attr) = call.callee().unwrap() else {
            panic!()
        };
        assert_eq!(attr.attr_text(), "b");
        let kws: Vec<_> = call.arg_list().unwrap().keywords().collect();
        assert_eq!(kws[0].name_text(), "k");
        let Expr::Unary(u) = cond.false_expr().unwrap() else {
            panic!()
        };
        assert_eq!(u.op().unwrap().text(), "-");
        let name =
            node_at_offset::<NameExpr>(&p.syntax_node(), rustpy_core::TextSize::new(4)).unwrap();
        assert_eq!(name.name_text(), "a");
    }
}

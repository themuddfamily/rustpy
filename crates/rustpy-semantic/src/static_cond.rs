//! Static evaluation of `if` conditions (roadmap §2.2 "Stubs"): typeshed
//! guards declarations with `sys.version_info` comparisons, `sys.platform`
//! checks and `TYPE_CHECKING`; only the branch matching the configured
//! interpreter is part of the stub's API.

use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

/// The interpreter a stub is evaluated for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StaticEnv {
    /// `(major, minor)`.
    pub python_version: (u32, u32),
    /// `sys.platform`: `linux`, `win32`, `darwin`, …
    pub platform: String,
    /// Whether `TYPE_CHECKING` is true (stubs, as type checkers read them).
    /// In user code both of its branches run somewhere, so it stays undecided.
    pub type_checking: bool,
}

/// `platform` value that leaves `sys.platform` checks undecided.
const ANY_PLATFORM: &str = "*";

impl StaticEnv {
    pub fn new(python_version: (u32, u32), platform: &str) -> Self {
        let platform = if platform.is_empty() {
            host_platform().to_string()
        } else {
            platform.to_string()
        };
        Self {
            python_version,
            platform,
            type_checking: true,
        }
    }

    /// The same Python version on every platform (`sys.platform` checks stay undecided).
    pub fn any_platform(&self) -> Self {
        Self {
            platform: ANY_PLATFORM.to_string(),
            ..self.clone()
        }
    }

    /// For checking user code: `if TYPE_CHECKING:` / `else:` are both live.
    pub fn for_user_code(&self) -> Self {
        Self {
            type_checking: false,
            ..self.clone()
        }
    }

    fn platform_known(&self) -> bool {
        self.platform != ANY_PLATFORM
    }
}

/// `sys.platform` of the machine running the IDE.
pub fn host_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        "freebsd" => "freebsd",
        other => other,
    }
}

/// Evaluates an `if` condition: `Some(value)` when `env` decides it, `None`
/// for anything that is not a recognised static check.
pub fn eval(cond: &ast::Expr, env: &StaticEnv) -> Option<bool> {
    match cond {
        ast::Expr::Paren(p) => eval(&p.inner()?, env),
        ast::Expr::Not(n) => eval(&n.operand()?, env).map(|v| !v),
        ast::Expr::BoolOp(b) => {
            let values: Vec<Option<bool>> = b.operands().map(|o| eval(&o, env)).collect();
            if b.is_or() {
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(|v| *v == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            } else if values.contains(&Some(false)) {
                Some(false)
            } else if values.iter().all(|v| *v == Some(true)) {
                Some(true)
            } else {
                None
            }
        }
        ast::Expr::Name(n) if n.name_text() == "TYPE_CHECKING" => env.type_checking.then_some(true),
        ast::Expr::Attribute(a) if a.attr_text() == "TYPE_CHECKING" => {
            env.type_checking.then_some(true)
        }
        ast::Expr::Compare(c) => compare(c, env),
        ast::Expr::Call(call) => {
            // `sys.platform.startswith("linux")` / `.startswith(("linux", "darwin"))`.
            let ast::Expr::Attribute(m) = call.callee()? else {
                return None;
            };
            if m.attr_text() != "startswith"
                || !is_sys_attr(&m.object()?, "platform")
                || !env.platform_known()
            {
                return None;
            }
            let args: Vec<ast::Expr> = call.arg_list()?.positional().collect();
            let [arg] = args.as_slice() else { return None };
            let prefixes = string_values(arg)?;
            Some(
                prefixes
                    .iter()
                    .any(|p| env.platform.starts_with(p.as_str())),
            )
        }
        _ => None,
    }
}

/// `sys.<attr>` (also through `import sys as _sys`-style names ending in `sys`).
fn is_sys_attr(e: &ast::Expr, attr: &str) -> bool {
    matches!(e, ast::Expr::Attribute(a) if a.attr_text() == attr && a.object().is_some_and(|o| matches!(o, ast::Expr::Name(n) if n.name_text() == "sys")))
}

fn string_values(e: &ast::Expr) -> Option<Vec<String>> {
    match e {
        ast::Expr::String(s) => Some(vec![s.value()]),
        ast::Expr::Tuple(t) => t
            .elements()
            .map(|x| match x {
                ast::Expr::String(s) => Some(s.value()),
                _ => None,
            })
            .collect(),
        ast::Expr::Paren(p) => string_values(&p.inner()?),
        _ => None,
    }
}

fn int_value(e: &ast::Expr) -> Option<i64> {
    match e {
        ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::INT_NUMBER) => {
            l.text().replace('_', "").parse().ok()
        }
        _ => None,
    }
}

fn int_tuple(e: &ast::Expr) -> Option<Vec<i64>> {
    match e {
        ast::Expr::Tuple(t) => t.elements().map(|x| int_value(&x)).collect(),
        ast::Expr::Paren(p) => int_tuple(&p.inner()?),
        _ => None,
    }
}

/// What a comparison's left-hand side reads from `sys`.
enum Subject {
    /// `sys.version_info`: the full tuple (longer than any 2-tuple literal).
    VersionInfo,
    /// `sys.version_info[:n]`.
    VersionPrefix(usize),
    /// `sys.version_info[i]`.
    VersionPart(usize),
    Platform,
}

fn subject(e: &ast::Expr) -> Option<Subject> {
    if is_sys_attr(e, "version_info") {
        return Some(Subject::VersionInfo);
    }
    if is_sys_attr(e, "platform") {
        return Some(Subject::Platform);
    }
    let ast::Expr::Subscript(s) = e else {
        return None;
    };
    if !is_sys_attr(&s.object()?, "version_info") {
        return None;
    }
    match s.index_expr()? {
        ast::Expr::Slice(sl) => match sl.parts() {
            (None, Some(end), None) => Some(Subject::VersionPrefix(int_value(&end)? as usize)),
            _ => None,
        },
        idx => Some(Subject::VersionPart(int_value(&idx)? as usize)),
    }
}

fn apply(op: &str, ord: std::cmp::Ordering) -> Option<bool> {
    use std::cmp::Ordering::*;
    Some(match op {
        "<" => ord == Less,
        "<=" => ord != Greater,
        ">" => ord == Greater,
        ">=" => ord != Less,
        "==" => ord == Equal,
        "!=" => ord != Equal,
        _ => return None,
    })
}

fn compare(c: &ast::CompareExpr, env: &StaticEnv) -> Option<bool> {
    let operands: Vec<ast::Expr> = c.operands().collect();
    let ops: Vec<String> = c.ops().map(|o| o.text()).collect();
    let ([lhs, rhs], [op]) = (operands.as_slice(), ops.as_slice()) else {
        return None;
    };
    let (major, minor) = (env.python_version.0 as i64, env.python_version.1 as i64);
    match subject(lhs)? {
        Subject::Platform => {
            let ast::Expr::String(s) = rhs else {
                return None;
            };
            if !env.platform_known() {
                return None;
            }
            match op.as_str() {
                "==" => Some(env.platform == s.value()),
                "!=" => Some(env.platform != s.value()),
                _ => None,
            }
        }
        Subject::VersionPart(i) => {
            let value = match i {
                0 => major,
                1 => minor,
                _ => return None,
            };
            apply(op, value.cmp(&int_value(rhs)?))
        }
        Subject::VersionPrefix(n) => {
            if n > 2 {
                return None;
            }
            let ours: Vec<i64> = [major, minor].into_iter().take(n).collect();
            apply(op, ours.cmp(&int_tuple(rhs)?))
        }
        Subject::VersionInfo => {
            let theirs = int_tuple(rhs)?;
            let ours = [major, minor];
            let k = theirs.len().min(2);
            let ord = ours[..k].cmp(&theirs[..k]);
            if ord != std::cmp::Ordering::Equal {
                return apply(op, ord);
            }
            if theirs.len() > 2 {
                // `(3, 12, 1)`: depends on the micro version, which is not tracked.
                return None;
            }
            // The real tuple continues (micro, release level, …): it is longer, hence greater.
            apply(op, std::cmp::Ordering::Greater)
        }
    }
}

/// The blocks of an `if`/`elif`/`else` chain that can run under `env` (all of
/// them without an environment or when a condition is undecided), in order.
pub fn live_branches(stmt: &ast::IfStmt, env: Option<&StaticEnv>) -> Vec<ast::Block> {
    let mut out = Vec::new();
    let arms = std::iter::once((stmt.condition(), stmt.body()))
        .chain(stmt.elif_clauses().map(|e| (e.condition(), e.body())));
    for (cond, body) in arms {
        match cond.as_ref().and_then(|c| eval(c, env?)) {
            Some(true) => {
                out.extend(body);
                return out;
            }
            Some(false) => {}
            None => out.extend(body),
        }
    }
    out.extend(stmt.else_clause().and_then(|e| e.body()));
    out
}

/// Whether `node` sits in an `if`/`elif`/`else` branch that cannot run under
/// `env` (e.g. `if sys.version_info < (3, 8):` on 3.12): such code is not
/// checked against the target interpreter's API.
pub fn in_dead_branch(node: &rustpy_syntax::SyntaxNode, env: &StaticEnv) -> bool {
    node.ancestors()
        .any(|ancestor| block_is_dead(&ancestor, env))
}

fn block_is_dead(block: &rustpy_syntax::SyntaxNode, env: &StaticEnv) -> bool {
    if block.kind() != SyntaxKind::BLOCK {
        return false;
    }
    let Some(parent) = block.parent() else {
        return false;
    };
    let if_stmt = match parent.kind() {
        SyntaxKind::IF_STMT => Some(parent),
        SyntaxKind::ELIF_CLAUSE | SyntaxKind::ELSE_CLAUSE => {
            parent.parent().filter(|p| p.kind() == SyntaxKind::IF_STMT)
        }
        _ => None,
    };
    let Some(stmt) = if_stmt.and_then(ast::IfStmt::cast) else {
        return false;
    };
    !live_branches(&stmt, Some(env))
        .iter()
        .any(|branch| branch.syntax() == block)
}

/// Reuses ancestry results during one immutable inspection pass under one
/// environment. In preorder, each node only needs its already visited parent's
/// result, even for deeply nested expressions without any conditional block.
/// Out-of-order queries fill the missing ancestor path iteratively as well.
pub struct DeadBranchCache<'env> {
    env: &'env StaticEnv,
    dead: rustc_hash::FxHashMap<rustpy_syntax::SyntaxNode, bool>,
}

impl<'env> DeadBranchCache<'env> {
    pub fn new(env: &'env StaticEnv) -> Self {
        Self {
            env,
            dead: rustc_hash::FxHashMap::default(),
        }
    }

    pub fn in_dead_branch(&mut self, node: &rustpy_syntax::SyntaxNode) -> bool {
        let mut missing = Vec::new();
        let mut inherited = false;
        for ancestor in node.ancestors() {
            if let Some(&dead) = self.dead.get(&ancestor) {
                inherited = dead;
                break;
            }
            missing.push(ancestor);
        }
        for ancestor in missing.into_iter().rev() {
            inherited = inherited || block_is_dead(&ancestor, self.env);
            self.dead.insert(ancestor, inherited);
        }
        inherited
    }
}

/// Statements of `block` with the live branches of nested `if` statements
/// flattened in (typeshed declares version-dependent members that way).
pub fn flatten_live(block: &ast::Block, env: Option<&StaticEnv>) -> Vec<ast::Stmt> {
    let mut out = Vec::new();
    for s in block.statements() {
        match &s {
            ast::Stmt::If(i) => {
                for b in live_branches(i, env) {
                    out.extend(flatten_live(&b, env));
                }
            }
            _ => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(src: &str, version: (u32, u32), platform: &str) -> Option<bool> {
        let p = rustpy_syntax::parse_expression(src);
        let e = p
            .syntax_node()
            .children()
            .find_map(ast::Expr::cast)
            .unwrap();
        eval(&e, &StaticEnv::new(version, platform))
    }

    #[test]
    fn version_checks() {
        assert_eq!(
            cond("sys.version_info >= (3, 11)", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info >= (3, 12)", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info >= (3, 13)", (3, 12), "linux"),
            Some(false)
        );
        assert_eq!(
            cond("sys.version_info < (3, 12)", (3, 12), "linux"),
            Some(false)
        );
        assert_eq!(
            cond("sys.version_info < (3, 12)", (3, 11), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info > (3, 12)", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info == (3, 12)", (3, 12), "linux"),
            Some(false)
        );
        assert_eq!(
            cond("sys.version_info >= (3,)", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info >= (3, 12, 1)", (3, 12), "linux"),
            None
        );
        assert_eq!(
            cond("sys.version_info >= (3, 12, 1)", (3, 13), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info[:2] == (3, 12)", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info[:2] >= (3, 13)", (3, 12), "linux"),
            Some(false)
        );
        assert_eq!(
            cond("sys.version_info[0] >= 3", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.version_info[1] < 10", (3, 12), "linux"),
            Some(false)
        );
    }

    #[test]
    fn platform_checks_and_logic() {
        assert_eq!(
            cond("sys.platform == 'win32'", (3, 12), "linux"),
            Some(false)
        );
        assert_eq!(
            cond("sys.platform != \"win32\"", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("sys.platform == 'win32'", (3, 12), "win32"),
            Some(true)
        );
        assert_eq!(
            cond("sys.platform.startswith('linux')", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond(
                "sys.platform.startswith(('freebsd', 'darwin'))",
                (3, 12),
                "darwin"
            ),
            Some(true)
        );
        assert_eq!(
            cond(
                "sys.platform != 'win32' and sys.version_info >= (3, 11)",
                (3, 12),
                "linux"
            ),
            Some(true)
        );
        assert_eq!(
            cond(
                "sys.platform == 'win32' or sys.version_info >= (3, 13)",
                (3, 12),
                "linux"
            ),
            Some(false)
        );
        assert_eq!(
            cond("sys.platform == 'win32' or FLAG", (3, 12), "linux"),
            None
        );
        assert_eq!(
            cond("sys.platform == 'linux' or FLAG", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(
            cond("not (sys.version_info >= (3, 13))", (3, 12), "linux"),
            Some(true)
        );
        assert_eq!(cond("TYPE_CHECKING", (3, 12), "linux"), Some(true));
        assert_eq!(cond("typing.TYPE_CHECKING", (3, 12), "linux"), Some(true));
        // In user code both branches of `if TYPE_CHECKING:` are checked.
        let user = StaticEnv::new((3, 12), "linux").for_user_code();
        for src in ["TYPE_CHECKING", "not TYPE_CHECKING", "typing.TYPE_CHECKING"] {
            let p = rustpy_syntax::parse_expression(src);
            let e = p
                .syntax_node()
                .children()
                .find_map(ast::Expr::cast)
                .unwrap();
            assert_eq!(eval(&e, &user), None, "{src}");
        }
        assert_eq!(cond("x > 1", (3, 12), "linux"), None);
        assert_eq!(cond("os.name == 'nt'", (3, 12), "linux"), None);
    }

    #[test]
    fn any_platform_leaves_platform_checks_open() {
        let env = StaticEnv::new((3, 12), "linux").any_platform();
        let p = rustpy_syntax::parse_expression("sys.platform == 'win32'");
        let e = p
            .syntax_node()
            .children()
            .find_map(ast::Expr::cast)
            .unwrap();
        assert_eq!(eval(&e, &env), None);
        let p = rustpy_syntax::parse_expression("sys.version_info >= (3, 13)");
        let e = p
            .syntax_node()
            .children()
            .find_map(ast::Expr::cast)
            .unwrap();
        assert_eq!(eval(&e, &env), Some(false));
    }

    #[test]
    fn dead_branches_of_user_code() {
        let src = "if sys.version_info < (3, 8):
    old()
else:
    new()
if sys.platform == 'win32':
    win()
if flag:
    other()
";
        let p = rustpy_syntax::parse(src);
        let env = StaticEnv::new((3, 12), "linux");
        let dead = |needle: &str| {
            let off = rustpy_core::TextSize::new(src.find(needle).unwrap() as u32);
            let tok = p.syntax_node().token_at_offset(off).right_biased().unwrap();
            in_dead_branch(&tok.parent().unwrap(), &env)
        };
        assert!(dead("old()"));
        assert!(!dead("new()"));
        assert!(dead("win()"));
        assert!(!dead("other()"));
        assert!(!dead("flag"));
    }

    #[test]
    fn cached_dead_branches_preserve_nested_guards_and_environment() {
        let src = "if sys.version_info < (3, 12):\n    old()\nelif sys.platform == 'linux':\n    if TYPE_CHECKING:\n        typed()\n    else:\n        runtime()\nelse:\n    fallback()\nif FLAG:\n    unknown()\nelse:\n    unknown_else()\n";
        let parsed = rustpy_syntax::parse(src);
        assert!(parsed.errors().is_empty());
        for (env, dead_calls) in [
            (
                StaticEnv::new((3, 11), "linux"),
                vec!["typed", "runtime", "fallback"],
            ),
            (
                StaticEnv::new((3, 12), "linux"),
                vec!["old", "runtime", "fallback"],
            ),
            (
                StaticEnv::new((3, 12), "linux").for_user_code(),
                vec!["old", "fallback"],
            ),
            (
                StaticEnv::new((3, 12), "win32").for_user_code(),
                vec!["old", "typed", "runtime"],
            ),
            (
                StaticEnv::new((3, 12), "linux").any_platform(),
                vec!["old", "runtime"],
            ),
        ] {
            let nodes: Vec<_> = parsed.syntax_node().descendants().collect();
            let mut cached = DeadBranchCache::new(&env);
            // A leaf queried first must inherit all enclosing branch decisions.
            for node in nodes.iter().rev().chain(nodes.iter()) {
                assert_eq!(cached.in_dead_branch(node), in_dead_branch(node, &env));
                if let Some(ast::Expr::Call(call)) = ast::Expr::cast(node.clone()) {
                    let name = call.callee().unwrap().syntax().text().to_string();
                    assert_eq!(
                        cached.in_dead_branch(node),
                        dead_calls.contains(&name.as_str())
                    );
                }
            }
        }
    }

    #[test]
    fn cached_dead_branches_visit_every_node_of_a_deep_binary_expression() {
        let source = format!("value = 0{}\n", " + 1".repeat(9_500));
        let parsed = rustpy_syntax::parse(&source);
        assert!(parsed.errors().is_empty());
        let env = StaticEnv::new((3, 12), "linux").for_user_code();
        let mut cached = DeadBranchCache::new(&env);
        let mut binary_count = 0;
        for node in parsed.syntax_node().descendants() {
            assert!(!cached.in_dead_branch(&node));
            binary_count += usize::from(node.kind() == SyntaxKind::BINARY_EXPR);
        }
        assert_eq!(binary_count, 9_500);
    }

    #[test]
    fn live_branches_follow_the_first_decided_arm() {
        let src = "if sys.version_info >= (3, 13):\n    a = 1\nelif sys.version_info >= (3, 11):\n    b = 1\nelif FLAG:\n    c = 1\nelse:\n    d = 1\n";
        let p = rustpy_syntax::parse(src);
        let module = ast::Module::cast(p.syntax_node()).unwrap();
        let ast::Stmt::If(i) = module.statements().next().unwrap() else {
            panic!()
        };
        let names = |env: Option<&StaticEnv>| -> Vec<String> {
            live_branches(&i, env)
                .iter()
                .map(|b| {
                    b.syntax()
                        .text()
                        .to_string()
                        .trim()
                        .chars()
                        .next()
                        .unwrap()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(names(Some(&StaticEnv::new((3, 12), "linux"))), vec!["b"]);
        assert_eq!(names(Some(&StaticEnv::new((3, 13), "linux"))), vec!["a"]);
        assert_eq!(
            names(Some(&StaticEnv::new((3, 10), "linux"))),
            vec!["c", "d"]
        );
        assert_eq!(names(None), vec!["a", "b", "c", "d"]);
    }
}

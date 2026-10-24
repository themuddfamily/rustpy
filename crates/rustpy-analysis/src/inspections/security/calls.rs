use super::{authority, InspectionOptions, Sink};
use crate::inspections::QuickFix;
use rustpy_core::Severity;
use rustpy_semantic::{db::FileInfo, infer::Inferer};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K,
};
use std::sync::Arc;

pub(super) fn text(e: &ast::Expr) -> Option<String> {
    if e.syntax().text().len() > rustpy_core::TextSize::new(65536) {
        return None;
    }
    match e.clone().unparen() {
        ast::Expr::String(s) if !s.is_bytes() => Some(s.value()),
        ast::Expr::ConcatString(s) => {
            let parts: Option<Vec<_>> = s
                .parts()
                .take(129)
                .map(|p| match p {
                    ast::Expr::String(s) if !s.is_bytes() => Some(s.value()),
                    _ => None,
                })
                .collect();
            parts.filter(|p| p.len() <= 128).map(|p| p.concat())
        }
        _ => None,
    }
}
fn boolean(e: &ast::Expr, value: bool) -> bool {
    e.syntax().text() == if value { "True" } else { "False" }
}
fn argument(args: &[ast::AnyArg], index: usize, name: &str) -> Option<ast::Expr> {
    let positional = args
        .iter()
        .filter_map(|a| match a {
            ast::AnyArg::Positional(a) => a.expr(),
            _ => None,
        })
        .nth(index);
    let keyword = args.iter().find_map(|a| match a {
        ast::AnyArg::Keyword(k) if k.name_text() == name => k.value(),
        _ => None,
    });
    if positional.is_some() && keyword.is_some() {
        None
    } else {
        keyword.or(positional)
    }
}
fn kw(args: &[ast::AnyArg], name: &str) -> Option<ast::Expr> {
    args.iter().find_map(|a| match a {
        ast::AnyArg::Keyword(k) if k.name_text() == name => k.value(),
        _ => None,
    })
}
pub(super) fn option(options: &InspectionOptions, code: &str, name: &str, default: bool) -> bool {
    options
        .policy
        .as_ref()
        .and_then(|p| p.rule(code))
        .and_then(|r| r.options.get(name))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn report(
    node: &rustpy_syntax::SyntaxNode,
    code: &str,
    message: impl Into<String>,
    fixes: Vec<QuickFix>,
    sink: &mut Sink,
) {
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        code,
        message,
        fixes,
    );
}
fn replace(e: &ast::Expr, title: &str, value: &str) -> Vec<QuickFix> {
    vec![QuickFix {
        title: title.into(),
        edits: vec![(e.text_range(), value.into())],
    }]
}
fn security_context(call: &ast::CallExpr) -> bool {
    call.syntax().ancestors().take(8).any(|a| {
        let name = if let Some(assign) = ast::AssignStmt::cast(a.clone()) {
            assign
                .targets()
                .into_iter()
                .filter_map(|t| match t {
                    ast::Expr::Name(n) => Some(n.name_text()),
                    ast::Expr::Attribute(n) => Some(n.attr_text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("_")
        } else if let Some(assign) = ast::AnnAssignStmt::cast(a.clone()) {
            assign
                .target()
                .map(|t| t.syntax().text().to_string())
                .unwrap_or_default()
        } else if let Some(function) = ast::FuncDef::cast(a) {
            function
                .name()
                .map(|n| n.text().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let name = name.to_lowercase();
        [
            "password", "token", "secret", "nonce", "salt", "auth", "session", "crypt",
        ]
        .iter()
        .any(|w| name.contains(w))
    })
}
fn safe_autoescape(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr) -> bool {
    if boolean(expr, true) {
        return true;
    }
    if let ast::Expr::Call(call) = expr.clone().unparen() {
        return call
            .callee()
            .and_then(|c| authority::identity(inf, file, &c))
            .as_deref()
            == Some("jinja2.select_autoescape");
    }
    false
}
pub(super) fn check(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    call: &ast::CallExpr,
    options: &InspectionOptions,
    sink: &mut Sink,
) {
    let Some(callee) = call.callee() else {
        return;
    };
    let Some(list) = call.arg_list() else {
        return;
    };
    let args: Vec<_> = list.args().take(129).collect();
    if args.len() > 128 {
        return;
    }
    let id = authority::identity(inf, file, &callee);
    let name = id.as_deref().unwrap_or("");
    let first = argument(&args, 0, "object");
    match name {
        "builtins.eval" | "builtins.exec" if first.as_ref().is_some_and(|e| text(e).is_none()) => {
            report(
                callee.syntax(),
                "PySecurityEval",
                "Executing non-literal Python source is a security risk",
                vec![],
                sink,
            )
        }
        "builtins.input" if options.python_version < (3, 0) => report(
            callee.syntax(),
            "PySecurityInput",
            "Python 2 input evaluates user input; use raw_input",
            replace(&callee, "Use raw_input", "raw_input"),
            sink,
        ),
        "subprocess.run"
        | "subprocess.call"
        | "subprocess.check_call"
        | "subprocess.check_output"
        | "subprocess.Popen" => {
            if let Some(shell) = kw(&args, "shell").filter(|e| boolean(e, true)) {
                report(
                    shell.syntax(),
                    "PySecurityShellInjection",
                    "shell=True executes commands through a shell; pass an argument list and disable the shell",
                    vec![],
                    sink,
                );
            }
        }
        "pickle.load" | "pickle.loads" | "cPickle.load" | "cPickle.loads" | "marshal.load"
        | "marshal.loads" | "dill.load" | "dill.loads" | "shelve.open" => report(
            callee.syntax(),
            "PySecurityUnsafeDeserialization",
            "Deserialization can execute code; accept only trusted data",
            vec![],
            sink,
        ),
        "yaml.load" | "yaml.load_all" | "yaml.unsafe_load" | "yaml.unsafe_load_all" => {
            let loader = argument(&args, 1, "Loader");
            let safe = loader
                .as_ref()
                .and_then(|e| authority::identity(inf, file, e))
                .is_some_and(|n| matches!(n.as_str(), "yaml.SafeLoader" | "yaml.CSafeLoader"));
            if !safe {
                let fixes = if loader.is_none() && !name.contains("unsafe") {
                    replace(
                        &callee,
                        "Use safe YAML loader",
                        if name.ends_with("load_all") {
                            "yaml.safe_load_all"
                        } else {
                            "yaml.safe_load"
                        },
                    )
                } else {
                    vec![]
                };
                // Only qualified yaml spelling admits a qualified replacement; aliases retain the warning.
                let fixes = if callee.syntax().text().to_string().starts_with("yaml.") {
                    fixes
                } else {
                    vec![]
                };
                report(
                    callee.syntax(),
                    "PySecurityUnsafeDeserialization",
                    "YAML loading without a canonical SafeLoader may construct arbitrary objects",
                    fixes,
                    sink,
                );
            }
        }
        "tempfile.mktemp" => report(
            callee.syntax(),
            "PySecurityTempfile",
            "mktemp has a race condition; use mkstemp or NamedTemporaryFile and manage the returned resource",
            vec![],
            sink,
        ),
        "os.system" | "os.popen" => report(
            callee.syntax(),
            "PySecurityOsSystem",
            "Shell execution can permit injection; prefer subprocess with an argument list",
            vec![],
            sink,
        ),
        "hashlib.md5" | "hashlib.sha1" | "hashlib.new" => {
            let weak = name != "hashlib.new"
                || argument(&args, 0, "name")
                    .and_then(|e| text(&e))
                    .is_some_and(|v| matches!(v.to_lowercase().as_str(), "md5" | "sha1"));
            if weak && !kw(&args, "usedforsecurity").is_some_and(|e| boolean(&e, false)) {
                report(
                    callee.syntax(),
                    "PySecurityWeakHash",
                    "MD5 and SHA-1 are unsuitable for security-sensitive hashing",
                    vec![],
                    sink,
                );
            }
        }
        "ssl._create_unverified_context" => report(
            callee.syntax(),
            "PySecurityTls",
            "This SSL context disables certificate verification",
            vec![],
            sink,
        ),
        "ssl.wrap_socket" => {
            let required = kw(&args, "cert_reqs")
                .and_then(|e| authority::identity(inf, file, &e))
                .is_some_and(|n| n == "ssl.CERT_REQUIRED");
            if !required {
                report(
                    callee.syntax(),
                    "PySecurityTls",
                    "ssl.wrap_socket does not require trusted certificates by default; use a verified SSLContext",
                    vec![],
                    sink,
                );
            }
        }
        "jinja2.Environment"
            if !kw(&args, "autoescape").is_some_and(|e| safe_autoescape(inf, file, &e)) => {
            report(
                callee.syntax(),
                "PySecurityAutoescape",
                "Enable Jinja2 autoescaping for untrusted HTML values",
                vec![],
                sink,
            );
        }
        _ => {}
    }
    if name.starts_with("random.")
        && !name.contains(".SystemRandom.")
        && matches!(
            name.rsplit('.').next(),
            Some(
                "random"
                    | "randint"
                    | "randrange"
                    | "choice"
                    | "choices"
                    | "getrandbits"
                    | "randbytes"
                    | "uniform"
            )
        )
        && (security_context(call) || option(options, "PySecurityRandom", "warn_all_uses", false))
    {
        report(
            callee.syntax(),
            "PySecurityRandom",
            "The random module is not cryptographically secure; use secrets or SystemRandom",
            vec![],
            sink,
        );
    }
    // These identities also admit captured aliases of bound, nominal library methods.
    let sql = [
        "sqlite3.",
        "psycopg.",
        "psycopg2.",
        "pymysql.",
        "MySQLdb.",
        "mysql.connector.",
    ]
    .iter()
    .any(|m| name.starts_with(*m));
    if sql
        && matches!(
            name.rsplit('.').next(),
            Some("execute" | "executemany" | "executescript")
        )
    {
        if let Some(query) = argument(&args, 0, "sql").or_else(|| argument(&args, 0, "query")) {
            if formatted(&query) {
                report(
                    query.syntax(),
                    "PySecuritySqlInjection",
                    "Formatted SQL may permit injection; use query parameters",
                    vec![],
                    sink,
                );
            }
        }
    }
    if name.starts_with("tarfile.") && name.ends_with(".extractall") {
        let safe = kw(&args, "filter").is_some_and(|e| {
            text(&e).as_deref() == Some("data")
                || authority::identity(inf, file, &e).as_deref() == Some("tarfile.data_filter")
        });
        if !safe && (options.python_version < (3, 14) || kw(&args, "filter").is_some()) {
            report(
                callee.syntax(),
                "PySecurityTarExtraction",
                "Tar extraction may escape its destination; use the data filter",
                vec![],
                sink,
            );
        }
    }
    if name.starts_with("socket.")
        && name.ends_with(".bind")
        && argument(&args, 0, "address").is_some_and(|e| {
            e.syntax()
                .descendants()
                .filter_map(ast::StringExpr::cast)
                .any(|s| matches!(s.value().as_str(), "0.0.0.0" | "::"))
        })
    {
        report(
            callee.syntax(),
            "PySecurityBindAll",
            "Binding all interfaces exposes this service to the network",
            vec![],
            sink,
        );
    }
    let requests = name.starts_with("requests.") || name.starts_with("httpx.");
    if requests && kw(&args, "verify").is_some_and(|e| boolean(&e, false)) {
        let value = kw(&args, "verify").unwrap();
        report(
            value.syntax(),
            "PySecurityTls",
            "Certificate verification is disabled",
            replace(&value, "Enable certificate verification", "True"),
            sink,
        );
    }
    if name == "ssl.SSLContext" {
        if let Some(protocol) =
            argument(&args, 0, "protocol").and_then(|e| authority::identity(inf, file, &e))
        {
            if matches!(
                protocol.as_str(),
                "ssl.PROTOCOL_SSLv2"
                    | "ssl.PROTOCOL_SSLv3"
                    | "ssl.PROTOCOL_TLSv1"
                    | "ssl.PROTOCOL_TLSv1_1"
            ) {
                report(
                    callee.syntax(),
                    "PySecurityTls",
                    "This SSL/TLS protocol version is obsolete",
                    vec![],
                    sink,
                );
            }
        }
    }
    // Flask-style app.run host is an explicit public-listener configuration.
    if (name == "http.server.HTTPServer" || name == "socket.create_server")
        && argument(&args, 0, "address").is_some_and(|e| {
            e.syntax()
                .descendants()
                .filter_map(ast::StringExpr::cast)
                .any(|s| s.value() == "0.0.0.0")
        })
    {
        report(
            callee.syntax(),
            "PySecurityBindAll",
            "Binding all interfaces exposes this service to the network",
            vec![],
            sink,
        );
    }
}
fn formatted(expr: &ast::Expr) -> bool {
    match expr.clone().unparen() {
        ast::Expr::FString(_) => true,
        ast::Expr::Binary(b) => b.op().is_some_and(|o| {
            o.kind() == K::PERCENT
                || (o.kind() == K::PLUS
                    && !(b.lhs().is_some_and(|e| text(&e).is_some())
                        && b.rhs().is_some_and(|e| text(&e).is_some())))
        }),
        ast::Expr::Call(c) => {
            matches!(c.callee(),Some(ast::Expr::Attribute(a)) if a.attr_text()=="format" && a.object().is_some_and(|e|text(&e).is_some()))
        }
        _ => false,
    }
}

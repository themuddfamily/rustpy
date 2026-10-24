//! Security weak warnings (`PySecurity*`): dangerous calls and hardcoded secrets.

use std::sync::Arc;

use rustpy_core::Severity;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use super::{InspectionOptions, Sink};
use rustpy_semantic::infer::Inferer;
pub(super) mod authority;
mod calls;
use rustpy_semantic::db::FileInfo;

const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "access_key",
    "auth_key",
    "client_secret",
];
const NOT_SECRET_SUFFIXES: &[&str] = &[
    "_name",
    "_header",
    "_field",
    "_param",
    "_file",
    "_path",
    "_url",
    "_env",
    "_prefix",
    "_type",
    "_label",
    "_key_name",
    "_var",
    "_option",
    "_arg",
    "_help",
    "_prompt",
    "_title",
    "_message",
    "_msg",
    "_text",
    "_format",
    "_pattern",
    "_regex",
    "_re",
];

pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    options: &InspectionOptions,
    sink: &mut Sink,
) {
    if file.text.len() > 2 * 1024 * 1024 {
        return;
    }
    let django = django_context(file);
    let environment = inf.db.resolver.static_env().for_user_code();
    let mut calls_remaining = 512usize;
    let mut dead_branches = rustpy_semantic::static_cond::DeadBranchCache::new(&environment);
    for node in file.parse.syntax_node().descendants().take(100_000) {
        if dead_branches.in_dead_branch(&node) {
            continue;
        }
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            if calls_remaining > 0 {
                calls_remaining -= 1;
                calls::check(inf, file, &call, options, sink);
            }
            if let Some(args) = call.arg_list() {
                for argument in args.args().take(128) {
                    if let ast::AnyArg::Keyword(k) = argument {
                        if let Some(value) = k.value() {
                            check_secret(&k.name_text(), &value, sink);
                        }
                    }
                }
            }
        }
        if let Some(param) = ast::Param::cast(node.clone()) {
            if let (Some(name), Some(value)) = (param.name(), param.default()) {
                check_secret(name.text(), &value, sink);
            }
        }
        if let Some(dict) = ast::DictExpr::cast(node.clone()) {
            for element in dict.items().take(128) {
                if let (Some(ast::Expr::String(key)), Some(value)) =
                    (element.key(), element.value())
                {
                    check_secret(&key.value(), &value, sink);
                }
            }
        }
        if let Some(a) = ast::AssignStmt::cast(node.clone()) {
            if let Some(value) = a.value() {
                for target in a.targets() {
                    assignment(inf, file, &target, &value, django, sink);
                }
            }
        }
        if let Some(a) = ast::AnnAssignStmt::cast(node.clone()) {
            if let (Some(target), Some(value)) = (a.target(), a.value()) {
                assignment(inf, file, &target, &value, django, sink);
            }
        }
        if node.kind() == SyntaxKind::ASSERT_STMT
            && !test_file(file)
            && calls::option(options, "PySecurityAssert", "warn_assertions", true)
        {
            sink.push(node.text_range(), Severity::WeakWarning, "PySecurityAssert", "Assertions are removed by optimized Python; do not use them for input or security validation", Vec::new());
        }
    }
}
fn test_file(file: &FileInfo) -> bool {
    file.path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("test_") || n.ends_with("_test.py"))
        || file.path.components().any(|c| c.as_os_str() == "tests")
}
fn assignment(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    target: &ast::Expr,
    value: &ast::Expr,
    django: bool,
    sink: &mut Sink,
) {
    let name = match target {
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Attribute(a) => a.attr_text(),
        _ => return,
    };
    check_secret(&name, value, sink);
    if let ast::Expr::Attribute(attribute) = target {
        if let Some(object) = attribute.object() {
            let receiver = authority::receiver_module(inf, file, &object);
            if receiver.as_deref() == Some("ssl")
                && ((name == "check_hostname" && value.syntax().text() == "False")
                    || (name == "verify_mode"
                        && authority::identity(inf, file, value).as_deref()
                            == Some("ssl.CERT_NONE")))
            {
                sink.push(
                    value.text_range(),
                    Severity::WeakWarning,
                    "PySecurityTls",
                    "SSL certificate or hostname verification is disabled",
                    Vec::new(),
                );
            }
        }
    }
    if django
        && ((name == "DEBUG" && value.syntax().text() == "True")
            || (name == "ALLOWED_HOSTS"
                && value
                    .syntax()
                    .descendants()
                    .filter_map(ast::StringExpr::cast)
                    .any(|s| s.value() == "*")))
    {
        sink.push(
            value.text_range(),
            Severity::WeakWarning,
            "PySecurityDjango",
            format!("Unsafe Django {name} setting for production"),
            Vec::new(),
        );
    }
}

fn django_context(file: &FileInfo) -> bool {
    file.path
        .file_stem()
        .and_then(|v| v.to_str())
        .is_some_and(|n| n == "settings" || n.starts_with("settings_"))
        || file
            .parse
            .syntax_node()
            .descendants()
            .take(10000)
            .filter_map(ast::ImportFromStmt::cast)
            .any(|i| {
                i.level() == 0
                    && (i.module_path() == "django" || i.module_path().starts_with("django."))
            })
}

fn check_secret(name: &str, value: &ast::Expr, sink: &mut Sink) {
    let lower = name.to_lowercase();
    if !SECRET_WORDS.iter().any(|w| lower.contains(w))
        || NOT_SECRET_SUFFIXES.iter().any(|s| lower.ends_with(s))
    {
        return;
    }
    let Some(v) = calls::text(value) else {
        return;
    };
    let placeholder = v.trim().is_empty()
        || v.starts_with('<')
        || v.starts_with('{')
        || v.starts_with('$')
        || v.chars()
            .all(|c| c == '*' || c == 'x' || c == 'X' || c == '.')
        || v.eq_ignore_ascii_case("changeme")
        || v.eq_ignore_ascii_case("none");
    if placeholder {
        return;
    }
    sink.push(
        value.text_range(),
        Severity::WeakWarning,
        "PySecurityHardcodedSecret",
        format!("Hardcoded secret '{name}' in source code"),
        Vec::new(),
    );
}

pub(super) fn calls_text(expr: &ast::Expr) -> Option<String> {
    calls::text(expr)
}

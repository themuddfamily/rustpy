//! Debugger data model and the pure parts of the DAP mapping: breakpoints,
//! exception breakpoints, launch options and the debugpy command line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustpy_project::{Interpreter, RunConfig};
use rustpy_run::PythonCommand;
use serde_json::{json, Value};

/// A line breakpoint as the debugger sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    /// 1-based line.
    pub line: usize,
    /// Python expression; the breakpoint only fires when it is true.
    pub condition: Option<String>,
    /// Logged on every hit; `{expr}` parts are evaluated (DAP logpoint syntax).
    pub log_message: Option<String>,
    /// Hit count condition such as `5`, `>= 3` or `% 2`.
    pub hit_condition: Option<String>,
    pub enabled: bool,
    /// Suspend the program when hit. A breakpoint that does not suspend only logs.
    pub suspend: bool,
    /// Remove the breakpoint once it has suspended the program ("Remove once hit").
    pub temporary: bool,
    /// False once the debugger rejected it (no code on the line, a library file
    /// it ignores, …); true until then.
    pub verified: bool,
}

impl Breakpoint {
    /// An enabled, suspending, unconditional breakpoint at `line` (1-based).
    pub fn new(line: usize) -> Breakpoint {
        Breakpoint {
            line,
            condition: None,
            log_message: None,
            hit_condition: None,
            enabled: true,
            suspend: true,
            temporary: false,
            verified: true,
        }
    }

    /// The log message when it is not blank.
    pub fn log_text(&self) -> Option<&str> {
        self.log_message.as_deref().filter(|m| !m.trim().is_empty())
    }
}

fn non_blank(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// The `setBreakpoints` entries for `list` and the line each entry was sent for.
///
/// * disabled breakpoints are left out, and all of them while `muted`;
/// * a breakpoint that does not suspend becomes a DAP logpoint (debugpy never
///   stops at logpoints); without a message it has nothing to do and is left out;
/// * a suspending breakpoint with a message is sent as a plain breakpoint: the
///   session logs the message itself when the program stops there;
/// * `run_to` (Run to Cursor) adds an unconditional breakpoint at that line,
///   replacing a conditional one there, even while muted.
pub fn dap_breakpoints(
    list: &[Breakpoint],
    muted: bool,
    run_to: Option<usize>,
) -> (Vec<Value>, Vec<usize>) {
    let mut out: Vec<(usize, Value)> = Vec::new();
    if !muted {
        for b in list.iter().filter(|b| b.enabled) {
            let log = b.log_text();
            if !b.suspend && log.is_none() {
                continue;
            }
            if out.iter().any(|(l, _)| *l == b.line) {
                continue;
            }
            let mut v = json!({ "line": b.line });
            if let Some(c) = non_blank(&b.condition) {
                v["condition"] = json!(c);
            }
            if let Some(h) = non_blank(&b.hit_condition) {
                v["hitCondition"] = json!(h);
            }
            if let (false, Some(m)) = (b.suspend, log) {
                v["logMessage"] = json!(m);
            }
            out.push((b.line, v));
        }
    }
    if let Some(line) = run_to {
        out.retain(|(l, _)| *l != line);
        out.push((line, json!({ "line": line })));
    }
    let lines = out.iter().map(|(l, _)| *l).collect();
    (out.into_iter().map(|(_, v)| v).collect(), lines)
}

/// One "Python Exception Breakpoint" for an exception class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExceptionClassBreakpoint {
    /// Class name, e.g. `ValueError` or `requests.HTTPError`.
    pub name: String,
    pub enabled: bool,
    /// Break when it is raised, even if it is handled later.
    pub on_raise: bool,
    /// Break when it terminates the program (not handled).
    pub on_terminate: bool,
}

/// Exception breakpoints of a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExceptionBreakpoints {
    /// Any exception, when raised (DAP filter `raised`).
    pub on_raise: bool,
    /// Any exception that is not handled (`uncaught`).
    pub on_terminate: bool,
    /// Exceptions that user code does not handle (`userUnhandled`).
    pub user_unhandled: bool,
    pub classes: Vec<ExceptionClassBreakpoint>,
}

impl Default for ExceptionBreakpoints {
    fn default() -> Self {
        ExceptionBreakpoints {
            on_raise: false,
            on_terminate: true,
            user_unhandled: false,
            classes: Vec::new(),
        }
    }
}

impl ExceptionBreakpoints {
    /// `setExceptionBreakpoints` arguments. debugpy ignores `filters` as soon as
    /// `exceptionOptions` is not empty, so with class breakpoints the settings for
    /// all exceptions are sent as options on the `Python Exceptions` root as well.
    pub fn to_dap(&self, muted: bool) -> Value {
        if muted {
            return json!({ "filters": [] });
        }
        let modes = [
            (self.on_raise, "raised", "always"),
            (self.on_terminate, "uncaught", "unhandled"),
            (self.user_unhandled, "userUnhandled", "userUnhandled"),
        ];
        let filters: Vec<&str> = modes.iter().filter(|m| m.0).map(|m| m.1).collect();
        let classes: Vec<&ExceptionClassBreakpoint> = self
            .classes
            .iter()
            .filter(|c| c.enabled && !c.name.trim().is_empty() && (c.on_raise || c.on_terminate))
            .collect();
        if classes.is_empty() {
            return json!({ "filters": filters });
        }
        let root = json!({ "names": ["Python Exceptions"] });
        let mut options: Vec<Value> = modes
            .iter()
            .filter(|m| m.0)
            .map(|m| json!({ "path": [root.clone()], "breakMode": m.2 }))
            .collect();
        for c in classes {
            let path = json!([root.clone(), { "names": [c.name.trim()] }]);
            if c.on_raise {
                options.push(json!({ "path": path.clone(), "breakMode": "always" }));
            }
            if c.on_terminate {
                options.push(json!({ "path": path, "breakMode": "unhandled" }));
            }
        }
        json!({ "filters": filters, "exceptionOptions": options })
    }
}

/// Debugger options sent with `attach` (Settings › Build, Execution, Deployment › Python Debugger).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugOptions {
    pub backend: rustpy_settings::debugger_workspace::DebuggerWorkspaceSettings,
    /// Do not step into library scripts (debugpy `justMyCode`). debugpy then also
    /// ignores exceptions raised in library files; files with breakpoints are
    /// counted as the user's code (see [`DebugOptions::attach_args`]).
    pub just_my_code: bool,
    /// Show the return values of functions after stepping out of them.
    pub show_return_value: bool,
    /// gevent compatible debugging (`GEVENT_SUPPORT=True`).
    pub gevent: bool,
}

impl Default for DebugOptions {
    fn default() -> Self {
        DebugOptions {
            backend: Default::default(),
            just_my_code: true,
            show_return_value: true,
            gevent: false,
        }
    }
}

impl DebugOptions {
    /// `attach` request arguments. With `justMyCode` debugpy ignores breakpoints
    /// in library files, so `breakpoint_files` (the files with breakpoints) are
    /// counted as the user's code: their breakpoints keep working with "Do not
    /// step into library scripts".
    pub fn attach_args(&self, breakpoint_files: &[PathBuf]) -> Value {
        let mut args = json!({ "justMyCode": self.just_my_code, "redirectOutput": false, "showReturnValue": self.show_return_value, "subProcess": self.backend.sub_process, "django":self.backend.django, "jinja":self.backend.jinja, "stopAllThreadsOnSuspend":self.backend.suspend_all_threads, "qt":self.backend.qt_support, "variablePresentation":{"special":if self.backend.show_special{"group"}else{"hide"},"protected":if self.backend.show_protected{"group"}else{"hide"}} });
        if self.just_my_code && !breakpoint_files.is_empty() {
            args["rules"] = breakpoint_files
                .iter()
                .map(|p| json!({ "path": p.to_string_lossy(), "include": true }))
                .collect();
        }
        let mut rules = args["rules"].as_array().cloned().unwrap_or_default();
        rules.extend(
            self.backend
                .step_filters
                .iter()
                .map(|path| json!({"path":path,"include":false})),
        );
        if !rules.is_empty() {
            args["rules"] = json!(rules);
        }
        args
    }
}

/// Why the debugger rejected a breakpoint (a `setBreakpoints` result or the
/// breakpoint of a `breakpoint` event); None when it was accepted or still
/// waits for its module to be loaded.
pub fn breakpoint_rejection(bp: &Value) -> Option<String> {
    if bp["verified"].as_bool().unwrap_or(true) {
        return None;
    }
    let message = bp["message"].as_str().unwrap_or("").trim();
    // pydevd verifies breakpoints in modules that are not imported yet lazily.
    if message.starts_with("Waiting for code to be loaded") {
        return None;
    }
    Some(
        message
            .lines()
            .next()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or("Breakpoint not accepted by the debugger")
            .to_string(),
    )
}

/// A process to spawn: program, arguments, working directory and environment.
pub type CommandLine = (PathBuf, Vec<String>, PathBuf, BTreeMap<String, String>);

/// The debuggee command: `python [interpreter options] -Xfrozen_modules=off -m debugpy
/// --listen 127.0.0.1:PORT --wait-for-client <script or -m module> [args…]`.
///
/// Interpreter options (`-X dev`, `-O`, `-u`, `-W error`) must come before
/// `-m debugpy`: debugpy's own command line rejects switches it does not know.
/// Shell script configurations cannot be debugged.
pub fn debug_command(
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
    port: u16,
    options: &DebugOptions,
) -> anyhow::Result<CommandLine> {
    let cmd = PythonCommand::for_config(cfg, interpreter, project_root, "Debugging")?;
    let mut args = cmd.interpreter_options.clone();
    args.extend([
        "-Xfrozen_modules=off".to_string(),
        "-m".into(),
        "debugpy".into(),
        "--listen".into(),
        format!("127.0.0.1:{port}"),
        "--wait-for-client".into(),
    ]);
    args.extend([
        "--configure-subProcess".into(),
        if options.backend.sub_process {
            "true".into()
        } else {
            "false".into()
        },
    ]);
    args.extend(cmd.target.iter().cloned());
    let mut env = cmd.env.clone();
    if options.gevent {
        env.insert("GEVENT_SUPPORT".into(), "True".into());
    }
    Ok((cmd.python, args, cmd.cwd, env))
}

/// A Python expression that renders a DAP log message: literal text with
/// `{expression}` parts (nested braces allowed), e.g. `x is {x}` becomes
/// `''.join(['x is ', str(x)])`.
pub fn log_expression(template: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            literal.push(c);
            continue;
        }
        let mut depth = 1;
        let mut expr = String::new();
        for d in chars.by_ref() {
            match d {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            expr.push(d);
        }
        if depth != 0 {
            return py_str(&format!("Unbalanced braces in: {template}"));
        }
        if !literal.is_empty() {
            parts.push(py_str(&std::mem::take(&mut literal)));
        }
        if !expr.trim().is_empty() {
            parts.push(format!("str({})", expr.trim()));
        }
    }
    if !literal.is_empty() {
        parts.push(py_str(&literal));
    }
    match parts.len() {
        0 => "''".into(),
        _ => format!("''.join([{}])", parts.join(", ")),
    }
}

/// A Python string literal for `s`.
fn py_str(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A completion offered in the debug console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// DAP item type (`function`, `field`, `keyword`, …).
    pub kind: String,
}

/// The completions answered for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completions {
    pub id: u64,
    pub text: String,
    /// Byte offset of the cursor in `text`.
    pub cursor: usize,
    pub items: Vec<CompletionItem>,
}

/// The largest char boundary of `text` at or before byte offset `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Units of a one-based DAP column. The protocol specifies UTF-16, but the
/// debugpy/pydevd adapter uses Python string (Unicode scalar) indices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnEncoding {
    #[default]
    Utf16,
    /// Compatibility with debugpy's scalar completion and stack-frame columns.
    DebugpyScalar,
}

impl ColumnEncoding {
    fn units(self) -> rustpy_text::ColumnUnits {
        match self {
            Self::Utf16 => rustpy_text::ColumnUnits::Utf16,
            Self::DebugpyScalar => rustpy_text::ColumnUnits::Scalars,
        }
    }

    /// Convert a byte cursor on a source line to a one-based column. Invalid
    /// byte offsets round down; oversized offsets stop at the line ending.
    pub fn column(self, line: &str, cursor: usize) -> usize {
        rustpy_text::line_column(line, cursor, self.units()) + 1
    }

    /// Convert a one-based column to a byte cursor on a source line. Zero maps
    /// to the start, UTF-16 surrogate interiors round down, and oversized
    /// columns stop before the line ending. Work is bounded by the line length.
    pub fn byte_offset(self, line: &str, column: usize) -> usize {
        rustpy_text::line_offset(line, column.saturating_sub(1), self.units())
    }

    /// Convert a byte cursor in multi-line input to a one-based line and column.
    pub fn position(self, text: &str, cursor: usize) -> (usize, usize) {
        let position = rustpy_text::text_position(text, cursor, self.units());
        (position.line + 1, position.column + 1)
    }
}

/// debugpy completion position for a UTF-8 byte cursor. Kept for existing
/// callers; new protocol boundaries should select their encoding explicitly.
pub fn completion_position(text: &str, cursor: usize) -> (usize, usize) {
    ColumnEncoding::DebugpyScalar.position(text, cursor)
}

/// Start (byte offset) of the identifier that ends at `cursor`.
pub fn identifier_start(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor);
    text[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(cursor)
}

/// Inserts `label` for the identifier before `cursor`; returns the new text and cursor.
pub fn apply_completion(text: &str, cursor: usize, label: &str) -> (String, usize) {
    let cursor = floor_boundary(text, cursor);
    let start = identifier_start(text, cursor);
    let mut out = String::with_capacity(text.len() + label.len());
    out.push_str(&text[..start]);
    out.push_str(label);
    let new_cursor = out.len();
    out.push_str(&text[cursor..]);
    (out, new_cursor)
}

impl Completions {
    /// The items still offered for `text` with the cursor at byte offset
    /// `cursor`: the answer stays valid while the user types more of the name
    /// (debugpy filters by the typed prefix, so filtering again matches a new
    /// request). None when the text changed otherwise or nothing matches.
    pub fn visible_for(&self, text: &str, cursor: usize) -> Option<Vec<CompletionItem>> {
        let k = self.cursor;
        if cursor < k || !text.is_char_boundary(cursor) || !self.text.is_char_boundary(k) {
            return None;
        }
        if text.get(..k)? != &self.text[..k] || text.get(cursor..)? != &self.text[k..] {
            return None;
        }
        if !text[k..cursor]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
        {
            return None;
        }
        let prefix = text[identifier_start(text, cursor)..cursor].to_lowercase();
        let items: Vec<CompletionItem> = self
            .items
            .iter()
            .filter(|i| i.label.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();
        (!items.is_empty()).then_some(items)
    }
}

/// A call that Smart Step Into can enter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepInTarget {
    pub id: i64,
    /// Source of the call, e.g. `double(inc(items[0]))`, possibly with ` (call 2)`.
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_map_to_dap_entries() {
        let mut cond = Breakpoint::new(3);
        cond.condition = Some(" i == 3 ".into());
        cond.hit_condition = Some(">= 2".into());
        let mut disabled = Breakpoint::new(4);
        disabled.enabled = false;
        let mut logpoint = Breakpoint::new(5);
        logpoint.suspend = false;
        logpoint.log_message = Some("i is {i}".into());
        let mut silent = Breakpoint::new(6);
        silent.suspend = false;
        let mut log_and_stop = Breakpoint::new(7);
        log_and_stop.log_message = Some("here".into());
        let list = vec![cond, disabled, logpoint, silent, log_and_stop];
        let (bps, lines) = dap_breakpoints(&list, false, None);
        assert_eq!(lines, vec![3, 5, 7]);
        assert_eq!(
            bps[0],
            json!({ "line": 3, "condition": "i == 3", "hitCondition": ">= 2" })
        );
        assert_eq!(bps[1], json!({ "line": 5, "logMessage": "i is {i}" }));
        assert_eq!(
            bps[2],
            json!({ "line": 7 }),
            "a suspending breakpoint is not sent as a logpoint (debugpy would not stop)"
        );
        let (bps, lines) = dap_breakpoints(&list, true, None);
        assert!(bps.is_empty() && lines.is_empty(), "muted");
        // Run to Cursor: an unconditional breakpoint, even while muted, replacing a conditional one.
        let (bps, lines) = dap_breakpoints(&list, false, Some(3));
        assert_eq!(lines, vec![5, 7, 3]);
        assert_eq!(bps[2], json!({ "line": 3 }));
        let (bps, lines) = dap_breakpoints(&list, true, Some(10));
        assert_eq!((bps, lines), (vec![json!({ "line": 10 })], vec![10]));
    }

    #[test]
    fn exception_breakpoints_map_to_filters_and_options() {
        let ex = ExceptionBreakpoints::default();
        assert_eq!(ex.to_dap(false), json!({ "filters": ["uncaught"] }));
        assert_eq!(ex.to_dap(true), json!({ "filters": [] }));
        let mut ex = ExceptionBreakpoints {
            on_raise: false,
            on_terminate: true,
            user_unhandled: true,
            classes: vec![],
        };
        ex.classes.push(ExceptionClassBreakpoint {
            name: "ValueError".into(),
            enabled: true,
            on_raise: true,
            on_terminate: false,
        });
        ex.classes.push(ExceptionClassBreakpoint {
            name: "KeyError".into(),
            enabled: false,
            on_raise: true,
            on_terminate: true,
        });
        ex.classes.push(ExceptionClassBreakpoint {
            name: "OSError".into(),
            enabled: true,
            on_raise: true,
            on_terminate: true,
        });
        let v = ex.to_dap(false);
        assert_eq!(v["filters"], json!(["uncaught", "userUnhandled"]));
        let opts = v["exceptionOptions"].as_array().unwrap();
        let root = json!({ "names": ["Python Exceptions"] });
        assert_eq!(
            opts[0],
            json!({ "path": [root.clone()], "breakMode": "unhandled" })
        );
        assert_eq!(
            opts[1],
            json!({ "path": [root.clone()], "breakMode": "userUnhandled" })
        );
        assert_eq!(
            opts[2],
            json!({ "path": [root.clone(), { "names": ["ValueError"] }], "breakMode": "always" })
        );
        assert_eq!(
            opts[3],
            json!({ "path": [root.clone(), { "names": ["OSError"] }], "breakMode": "always" })
        );
        assert_eq!(
            opts[4],
            json!({ "path": [root, { "names": ["OSError"] }], "breakMode": "unhandled" })
        );
        assert_eq!(opts.len(), 5, "disabled classes are left out");
    }

    #[test]
    fn interpreter_options_come_before_debugpy() {
        let dir = std::env::temp_dir();
        let mut cfg = RunConfig::for_script(&dir.join("prog.py"));
        cfg.interpreter = Some(PathBuf::from("/usr/bin/python3"));
        cfg.interpreter_options = "-X dev -W error".into();
        cfg.parameters = "--flag value".into();
        let (program, args, _, env) =
            debug_command(&cfg, None, &dir, 5678, &DebugOptions::default()).unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/python3"));
        let script = dir.join("prog.py").to_string_lossy().into_owned();
        let expected: Vec<String> = [
            "-X",
            "dev",
            "-W",
            "error",
            "-Xfrozen_modules=off",
            "-m",
            "debugpy",
            "--listen",
            "127.0.0.1:5678",
            "--wait-for-client",
            "--configure-subProcess",
            "true",
            &script,
            "--flag",
            "value",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(args, expected);
        assert!(!env.contains_key("GEVENT_SUPPORT"));
        // Modules keep `-m module` after debugpy's switches.
        cfg.kind = rustpy_project::RunConfigKind::PythonModule {
            module: "pkg.main".into(),
        };
        let (_, args, _, env) = debug_command(
            &cfg,
            None,
            &dir,
            1,
            &DebugOptions {
                backend: Default::default(),
                gevent: true,
                ..Default::default()
            },
        )
        .unwrap();
        let tail: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .skip_while(|a| *a != "--wait-for-client")
            .collect();
        assert_eq!(
            tail,
            vec![
                "--wait-for-client",
                "--configure-subProcess",
                "true",
                "-m",
                "pkg.main",
                "--flag",
                "value"
            ]
        );
        assert_eq!(env.get("GEVENT_SUPPORT").map(String::as_str), Some("True"));
        cfg.kind = rustpy_project::RunConfigKind::ShellScript {
            script: dir.join("run.sh"),
        };
        let err = debug_command(&cfg, None, &dir, 1, &DebugOptions::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Debugging is only available for Python run configurations"),
            "{err}"
        );
    }

    #[test]
    fn attach_arguments_follow_the_options() {
        let v = DebugOptions::default().attach_args(&[]);
        assert_eq!(v["justMyCode"], json!(true));
        assert_eq!(v["showReturnValue"], json!(true));
        assert!(v.get("rules").is_none());
        let lib = PathBuf::from("/venv/lib/python3.12/site-packages/pkg/mod.py");
        let v = DebugOptions::default().attach_args(std::slice::from_ref(&lib));
        assert_eq!(
            v["rules"],
            json!([{ "path": lib.to_string_lossy(), "include": true }]),
            "files with breakpoints count as the user's code"
        );
        let v = DebugOptions {
            backend: Default::default(),
            just_my_code: false,
            show_return_value: false,
            gevent: false,
        }
        .attach_args(&[lib]);
        assert_eq!(
            (v["justMyCode"].clone(), v["showReturnValue"].clone()),
            (json!(false), json!(false))
        );
        assert!(
            v.get("rules").is_none(),
            "everything is traced without justMyCode"
        );
    }

    #[test]
    fn rejected_breakpoints() {
        assert_eq!(
            breakpoint_rejection(&json!({ "verified": true, "line": 3 })),
            None
        );
        assert_eq!(
            breakpoint_rejection(
                &json!({ "verified": false, "message": "Waiting for code to be loaded to verify breakpoint." })
            ),
            None,
            "lazily verified"
        );
        assert_eq!(
            breakpoint_rejection(
                &json!({ "verified": false, "message": "Breakpoint added to invalid line." })
            )
            .as_deref(),
            Some("Breakpoint added to invalid line.")
        );
        assert_eq!(breakpoint_rejection(&json!({ "verified": false, "message": "Breakpoint in file excluded by filters.\nNote: may be excluded because of \"justMyCode\"" })).as_deref(), Some("Breakpoint in file excluded by filters."));
        assert_eq!(
            breakpoint_rejection(&json!({ "verified": false })).as_deref(),
            Some("Breakpoint not accepted by the debugger")
        );
    }

    #[test]
    fn log_templates_become_python_expressions() {
        assert_eq!(log_expression("x is {x}"), "''.join(['x is ', str(x)])");
        assert_eq!(
            log_expression("{a}+{b['k']}!"),
            "''.join([str(a), '+', str(b['k']), '!'])"
        );
        assert_eq!(
            log_expression("set {{1: 2}[1]} it's"),
            "''.join(['set ', str({1: 2}[1]), ' it\\'s'])"
        );
        assert_eq!(log_expression("plain 100%"), "''.join(['plain 100%'])");
        assert_eq!(log_expression("bad {x"), "'Unbalanced braces in: bad {x'");
        assert_eq!(log_expression(""), "''");
    }

    #[test]
    fn completions_replace_the_identifier_before_the_cursor() {
        assert_eq!(
            apply_completion("items.app", 9, "append"),
            ("items.append".to_string(), 12)
        );
        assert_eq!(
            apply_completion("x = it + 1", 6, "items"),
            ("x = items + 1".to_string(), 9)
        );
        assert_eq!(
            apply_completion("len(", 4, "items"),
            ("len(items".to_string(), 9)
        );
        assert_eq!(identifier_start("é_x", "é_x".len()), 0);
        // Offsets inside a character are rounded down instead of panicking.
        assert_eq!(apply_completion("é", 1, "x"), ("xé".to_string(), 1));
    }

    #[test]
    fn completions_follow_further_typing() {
        let item = |l: &str| CompletionItem {
            label: l.into(),
            kind: "function".into(),
        };
        let c = Completions {
            id: 1,
            text: "items.".into(),
            cursor: 6,
            items: vec![item("append"), item("clear"), item("Count")],
        };
        let labels = |v: Option<Vec<CompletionItem>>| {
            v.map(|v| v.into_iter().map(|i| i.label).collect::<Vec<_>>())
        };
        assert_eq!(
            labels(c.visible_for("items.", 6)),
            Some(vec!["append".to_string(), "clear".into(), "Count".into()])
        );
        assert_eq!(
            labels(c.visible_for("items.c", 7)),
            Some(vec!["clear".to_string(), "Count".into()]),
            "case-insensitive prefix"
        );
        assert_eq!(
            labels(c.visible_for("items.cl", 8)),
            Some(vec!["clear".to_string()])
        );
        assert_eq!(c.visible_for("items.x", 7), None, "nothing matches");
        assert_eq!(c.visible_for("items.c(", 8), None, "not a name any more");
        assert_eq!(
            c.visible_for("items", 5),
            None,
            "deleted before the request"
        );
        assert_eq!(c.visible_for("other.c", 7), None);
        // Text after the cursor must stay as it was.
        let c = Completions {
            id: 2,
            text: "f(it)".into(),
            cursor: 4,
            items: vec![item("items")],
        };
        assert!(c.visible_for("f(ite)", 5).is_some());
        assert_eq!(c.visible_for("f(ite]", 5), None);
    }

    #[test]
    fn columns_convert_utf16_and_debugpy_scalars_at_boundaries() {
        let line = "a😀e\u{301}z\r\nnext";
        let content = "a😀e\u{301}z";
        for encoding in [ColumnEncoding::Utf16, ColumnEncoding::DebugpyScalar] {
            for byte in content
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([content.len()])
            {
                assert_eq!(
                    encoding.byte_offset(line, encoding.column(line, byte)),
                    byte
                );
            }
            assert_eq!(
                encoding.column(line, 3),
                2,
                "inside astral UTF-8 bytes rounds down"
            );
            assert_eq!(encoding.byte_offset(line, 0), 0);
            assert_eq!(encoding.byte_offset(line, usize::MAX), content.len());
            assert_eq!(
                encoding.column(line, usize::MAX),
                encoding.column(content, content.len())
            );
            assert_eq!(encoding.position("😀\nabc", usize::MAX), (2, 4));
            assert_eq!(
                encoding.position("😀\r\nabc", "😀\r".len()),
                (1, encoding.column("😀", 4))
            );
        }
        assert_eq!(ColumnEncoding::default(), ColumnEncoding::Utf16);
        assert_eq!(
            ColumnEncoding::Utf16.byte_offset(line, 3),
            1,
            "surrogate interior rounds down"
        );
        assert_eq!(ColumnEncoding::Utf16.column(line, 5), 4);
        assert_eq!(ColumnEncoding::DebugpyScalar.column(line, 5), 3);
        assert_eq!(
            ColumnEncoding::Utf16.column("e\u{301}", 3),
            3,
            "combining mark is a separate code unit"
        );
        assert_eq!(ColumnEncoding::Utf16.position("", usize::MAX), (1, 1));
    }

    #[test]
    fn physical_newlines_preserve_column_units_and_cursor_boundaries() {
        let content = "😀e\u{301}";
        for encoding in [ColumnEncoding::Utf16, ColumnEncoding::DebugpyScalar] {
            let end_column = encoding.column(content, content.len());
            for separator in ["\r", "\n", "\r\n"] {
                let text = format!("{content}{separator}z");
                assert_eq!(encoding.position(&text, content.len()), (1, end_column));
                if separator == "\r\n" {
                    assert_eq!(encoding.position(&text, content.len() + 1), (1, end_column));
                }
                assert_eq!(
                    encoding.position(&text, content.len() + separator.len()),
                    (2, 1)
                );
                assert_eq!(encoding.position(&text, usize::MAX), (2, 2));
                assert_eq!(encoding.column(&text, usize::MAX), end_column);
                assert_eq!(encoding.byte_offset(&text, usize::MAX), content.len());
                assert_eq!(encoding.position(&text, 3), (1, 1));
                assert_eq!(
                    encoding.position(&text, 6),
                    (1, encoding.column(content, 5))
                );
                for byte in content
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain([content.len()])
                {
                    assert_eq!(
                        encoding.byte_offset(&text, encoding.column(&text, byte)),
                        byte
                    );
                }
                assert_eq!(
                    encoding.position(&format!("{content}{separator}"), usize::MAX),
                    (2, 1)
                );
            }
            assert_eq!(encoding.position("a\rb\r\nc\nd", usize::MAX), (4, 2));
            assert_eq!(encoding.position("\r\n\r\n", 3), (2, 1));
        }
    }

    #[test]
    fn shared_positions_keep_dap_one_based_and_adapter_scalar_policy() {
        for separator in ["\r", "\n", "\r\n"] {
            let text = format!("😀{separator}e\u{301}😀.app");
            let start = "😀".len() + separator.len();
            let cursor = text.len();
            assert_eq!(
                ColumnEncoding::DebugpyScalar.position(&text, cursor),
                (2, 8)
            );
            assert_eq!(ColumnEncoding::Utf16.position(&text, cursor), (2, 9));
            assert_eq!(completion_position(&text, cursor), (2, 8));
            let line = &text[start..];
            assert_eq!(ColumnEncoding::DebugpyScalar.byte_offset(line, 4), 7);
            assert_eq!(
                ColumnEncoding::Utf16.byte_offset(line, 4),
                3,
                "surrogate interior floors"
            );
            assert_eq!(ColumnEncoding::Utf16.byte_offset(line, 5), 7);
            assert_eq!(ColumnEncoding::DebugpyScalar.column(line, 7), 4);
            assert_eq!(ColumnEncoding::Utf16.column(line, 7), 5);
            for encoding in [ColumnEncoding::DebugpyScalar, ColumnEncoding::Utf16] {
                assert_eq!(encoding.byte_offset(line, 0), 0);
                assert_eq!(encoding.position(&text, start), (2, 1));
                assert_eq!(
                    encoding.position(&text, start + 5),
                    (2, 3),
                    "astral UTF-8 interior floors"
                );
            }
        }
    }

    #[test]
    fn completion_positions_are_per_line() {
        assert_eq!(completion_position("items.app", 9), (1, 10));
        // Multi-line console input: the line of the cursor and the column within it.
        let text = "for v in items:\n    print(v.re";
        assert_eq!(completion_position(text, text.len()), (2, 15));
        assert_eq!(completion_position(text, 3), (1, 4));
        assert_eq!(completion_position("é\nab", "é\nab".len()), (2, 3));
        assert_eq!(completion_position("é", 1), (1, 1));
    }
}

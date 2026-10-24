//! Live console tests; required-provider mode binds an explicitly admitted Python.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustpy_console::{ConsoleEvent, ConsoleSession};

fn python() -> Option<PathBuf> {
    if std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").is_ok_and(|value| value == "1") {
        let path = PathBuf::from(
            std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                .expect("required-provider mode needs RUSTPY_PROVIDER_PYTHON"),
        );
        assert!(
            path.is_absolute() && path.is_file(),
            "RUSTPY_PROVIDER_PYTHON must name an installed absolute executable: {}",
            path.display()
        );
        Some(path)
    } else {
        which::which("python3").ok()
    }
}

fn required_python() -> PathBuf {
    assert!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").is_ok_and(|value| value == "1"),
        "configured acceptance requires RUSTPY_REQUIRE_PROVIDER_TESTS=1"
    );
    python().expect("required configured Python")
}

/// Polls until the result for `id` arrives, collecting stdout/stderr on the way.
fn wait_result(
    s: &mut ConsoleSession,
    id: u64,
) -> (Option<String>, Option<String>, bool, String, String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut out, mut err) = (String::new(), String::new());
    while Instant::now() < deadline {
        for ev in s.poll() {
            match ev {
                ConsoleEvent::Stdout(t) => out.push_str(&t),
                ConsoleEvent::Stderr(t) => err.push_str(&t),
                ConsoleEvent::Result {
                    id: rid,
                    value,
                    error,
                    more,
                } if rid == id => return (value, error, more, out, err),
                ConsoleEvent::Failed(e) => panic!("console failed: {e}"),
                ConsoleEvent::Exited(c) => {
                    panic!("console exited early with {c}; stdout={out:?} stderr={err:?}")
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for result {id}; stdout={out:?} stderr={err:?}");
}

fn wait_event<F: FnMut(&ConsoleEvent) -> bool>(
    s: &mut ConsoleSession,
    mut pred: F,
) -> ConsoleEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        for ev in s.poll() {
            if let ConsoleEvent::Failed(e) = &ev {
                panic!("console failed: {e}");
            }
            if pred(&ev) {
                return ev;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for event");
}

#[test]
fn exec_complete_vars_inspect_roundtrip() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut s = ConsoleSession::start(
        &py,
        dir.path(),
        &[("RUSTPY_TEST_VAR".into(), "1".into())],
        "import os\nSTARTUP_OK = os.environ.get('RUSTPY_TEST_VAR')",
    )
    .unwrap();
    assert!(s.is_running());
    wait_event(&mut s, |e| matches!(e, ConsoleEvent::Ready { .. }));

    // Statements execute silently; a trailing expression yields its repr.
    let id = s.exec("x = 1");
    let (value, error, more, out, _) = wait_result(&mut s, id);
    assert_eq!((value, error, more), (None, None, false));
    assert!(out.is_empty());

    let id = s.exec("x + 1");
    let (value, error, more, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("2"));
    assert!(error.is_none() && !more);

    // Startup code ran with the environment applied.
    let id = s.exec("STARTUP_OK");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("'1'"));

    // print() output is streamed on stdout.
    let id = s.exec("print('hello', x)");
    let (value, _, _, out, _) = wait_result(&mut s, id);
    assert!(value.is_none());
    assert_eq!(out, "hello 1\n");

    // Exceptions go to stderr (traceback) and the result's error field; the session survives.
    let id = s.exec("1/0");
    let (value, error, more, _, err) = wait_result(&mut s, id);
    assert!(value.is_none() && !more);
    assert!(err.contains("ZeroDivisionError"), "stderr was {err:?}");
    assert!(error.unwrap_or_default().contains("ZeroDivisionError"));

    // Incomplete input asks for more.
    let id = s.exec("def f():");
    let (_, error, more, _, _) = wait_result(&mut s, id);
    assert!(more, "expected more for an open block");
    assert!(error.is_none());
    let id = s.exec("def f():\n    return x * 10\n");
    let (_, _, more, _, _) = wait_result(&mut s, id);
    assert!(!more);
    let id = s.exec("f()");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("10"));

    // Pasted multi-statement blocks run in one go.
    let id = s.exec("a = [1, 2]\nb = {'k': a, 'n': 3}\nlen(b)");
    let (value, error, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("2"), "error: {error:?}");

    // Syntax errors are reported without killing the session.
    let id = s.exec("x = = 1");
    let (_, error, more, _, err) = wait_result(&mut s, id);
    assert!(!more);
    assert!(error.is_some() && err.contains("SyntaxError"));

    // Completions.
    let id = s.complete("str.jo", 6);
    let ev = wait_event(
        &mut s,
        |e| matches!(e, ConsoleEvent::Completions { id: rid, .. } if *rid == id),
    );
    let ConsoleEvent::Completions { items, prefix, .. } = ev else {
        unreachable!()
    };
    assert_eq!(prefix, "jo");
    assert!(
        items.iter().any(|c| c.name == "join" && c.kind == "method"),
        "items: {items:?}"
    );
    let id = s.complete("pri", 3);
    let ev = wait_event(
        &mut s,
        |e| matches!(e, ConsoleEvent::Completions { id: rid, .. } if *rid == id),
    );
    let ConsoleEvent::Completions { items, .. } = ev else {
        unreachable!()
    };
    assert!(items
        .iter()
        .any(|c| c.name == "print" && c.kind == "function"));

    // Variables snapshot (modules such as `os` are hidden).
    let id = s.request_vars();
    let ev = wait_event(
        &mut s,
        |e| matches!(e, ConsoleEvent::Vars { id: rid, .. } if *rid == id),
    );
    let ConsoleEvent::Vars { items, .. } = ev else {
        unreachable!()
    };
    let x = items.iter().find(|v| v.name == "x").expect("x in vars");
    assert_eq!(x.type_name, "int");
    assert_eq!(x.value, "1");
    assert!(!items.iter().any(|v| v.name == "os"));
    let b = items.iter().find(|v| v.name == "b").unwrap();
    assert!(b.has_children && b.type_name == "dict");
    assert!(items.iter().any(|v| v.name == "f" && v.is_callable));

    // Children use opaque stable handles, including keys without evaluable reprs.
    let id = s.inspect("b");
    let ev = wait_event(
        &mut s,
        |e| matches!(e, ConsoleEvent::Children { id: rid, .. } if *rid == id),
    );
    let ConsoleEvent::Children {
        name, items, error, ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(name, "b");
    assert!(error.is_none());
    let k = items.iter().find(|v| v.name == "'k'").expect("key k");
    assert_eq!(k.type_name, "list");
    assert!(k.expr.starts_with('@'));
    assert!(k.has_children);
    let id = s.inspect(&k.expr);
    let ev = wait_event(
        &mut s,
        |e| matches!(e, ConsoleEvent::Children { id: rid, .. } if *rid == id),
    );
    let ConsoleEvent::Children { items, .. } = ev else {
        unreachable!()
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].value, "2");

    // Raw writes to the file descriptor are forwarded as plain stdout.
    let id = s.exec("import os\nos.write(1, b'raw line\\n')");
    let (_, _, _, out, _) = wait_result(&mut s, id);
    assert!(out.contains("raw line"), "out: {out:?}");

    s.stop();
    assert!(!s.is_running());
}

#[test]
fn ended_session_removes_its_helper_dir_and_writes_no_bytecode_there() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let cwd = tempfile::tempdir().unwrap();
    let mut s = ConsoleSession::start(&py, cwd.path(), &[], "").unwrap();
    let dir = s.helper_dir().unwrap().to_path_buf();
    assert!(dir.starts_with(std::env::temp_dir()), "{}", dir.display());
    let id = s.exec("import sys; sys.dont_write_bytecode");
    let (value, error, ..) = wait_result(&mut s, id);
    assert_eq!(error, None);
    // The helper's own imports do not change the setting for user code.
    assert_eq!(value.as_deref(), Some("False"));
    assert!(!dir.join("__pycache__").exists());
    // Bytecode written there anyway (user imports of the helper modules) goes too.
    std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
    std::fs::write(dir.join("__pycache__/console_values.pyc"), b"").unwrap();
    drop(s);
    assert!(!dir.exists(), "{} was left behind", dir.display());
}

#[test]
fn interrupt_and_restart() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut s = ConsoleSession::start(&py, dir.path(), &[], "").unwrap();
    wait_event(&mut s, |e| matches!(e, ConsoleEvent::Ready { .. }));
    let id = s.exec("import time\ntime.sleep(30)");
    std::thread::sleep(Duration::from_millis(400));
    s.interrupt();
    let (_, error, _, _, err) = wait_result(&mut s, id);
    assert!(
        error.as_deref() == Some("KeyboardInterrupt"),
        "error: {error:?} stderr: {err:?}"
    );
    assert!(s.is_running());

    let id = s.exec("y = 5");
    wait_result(&mut s, id);
    s.restart();
    wait_event(&mut s, |e| matches!(e, ConsoleEvent::Ready { .. }));
    let id = s.exec("'y' in dir()");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("False"));
    s.stop();
}

#[test]
fn bad_interpreter_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    assert!(ConsoleSession::start(
        &PathBuf::from("/nonexistent/python-xyz"),
        dir.path(),
        &[],
        ""
    )
    .is_err());
}

#[test]
fn editor_fragments_run_as_a_whole() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut s = ConsoleSession::start(&py, dir.path(), &[], "").unwrap();
    wait_event(&mut s, |e| matches!(e, ConsoleEvent::Ready { .. }));

    // Typed input keeps the REPL rule: an open block waits for more...
    let id = s.exec("def f():\n  return 1");
    let (_, error, more, _, _) = wait_result(&mut s, id);
    assert!(
        more && error.is_none(),
        "typed block should wait for more input"
    );
    // ...while the same code sent from the editor runs at once.
    let id = s.exec_fragment("def f():\n  return 1");
    let (value, error, more, _, _) = wait_result(&mut s, id);
    assert_eq!((value, error, more), (None, None, false));
    let id = s.exec("f()");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("1"));

    // A fragment ending with a compound statement, then one with a trailing expression.
    let id = s.exec_fragment("total = 0\nfor i in range(4):\n    total += i\n");
    let (value, error, more, _, _) = wait_result(&mut s, id);
    assert_eq!((value, error, more), (None, None, false));
    let id = s.exec_fragment("for i in range(2):\n    print('row', i)\ntotal * 10");
    let (value, error, _, out, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("60"), "error: {error:?}");
    assert_eq!(out, "row 0\nrow 1\n");

    // Incomplete editor code is an error, not a `...` prompt; it is reported like the REPL does.
    let id = s.exec_fragment("def g():");
    let (_, error, more, _, err) = wait_result(&mut s, id);
    assert!(!more);
    let error = error.expect("incomplete fragment is an error");
    assert!(
        error.contains("IndentationError") || error.contains("SyntaxError"),
        "{error}"
    );
    assert!(
        !err.contains("Traceback") && !err.contains("ast.py") && !err.contains("codeop"),
        "compiler frames leaked: {err:?}"
    );

    // A SyntaxError explicitly raised by user code must retain user frames.
    let id = s.exec_fragment("raise SyntaxError('user-raised syntax error')");
    let (_, error, more, _, err) = wait_result(&mut s, id);
    assert!(!more);
    assert!(error
        .unwrap_or_default()
        .contains("user-raised syntax error"));
    assert!(
        err.contains("Traceback") && err.contains("<console>"),
        "{err}"
    );

    // Pasted at the prompt: a compound statement followed by more code runs (exec-mode retry).
    let id = s.exec("for i in range(2):\n    pass\nz = 3");
    let (_, error, more, _, err) = wait_result(&mut s, id);
    assert!(!more && error.is_none(), "error: {error:?} stderr: {err:?}");
    let id = s.exec("z");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("3"));
    // A bad single line at the prompt is still a plain syntax error.
    let id = s.exec("x = = 1");
    let (_, error, _, _, err) = wait_result(&mut s, id);
    assert!(
        error.is_some() && err.starts_with("  File \"<console>\""),
        "stderr: {err:?}"
    );

    // Runtime tracebacks keep the user frames but none of the helper's.
    let id = s.exec_fragment("def boom():\n    raise ValueError('bad')\nboom()");
    let (_, error, _, _, err) = wait_result(&mut s, id);
    assert!(error.unwrap_or_default().contains("ValueError: bad"));
    assert!(
        err.contains("in boom") && !err.contains("rustpy_console_helper"),
        "stderr: {err:?}"
    );
    s.stop();
}

#[test]
fn runfile_runs_scripts_in_the_console_namespace() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("pkg");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("helper_mod.py"), "VALUE = 41\n").unwrap();
    let script = sub.join("job.py");
    std::fs::write(
        &script,
        "import os, sys, pickle\nfrom dataclasses import dataclass\nimport helper_mod\n\n@dataclass\nclass Point:\n    x: int\n\nresult = helper_mod.VALUE + 1\npoint = Point(3)\nroundtrip = pickle.loads(pickle.dumps(point)) == point\nprint('argv', sys.argv[1:], os.path.basename(__file__), os.path.basename(os.getcwd()), os.environ.get('JOB_ENV'))\nif __name__ == '__main__':\n    print('main block')\n",
    )
    .unwrap();
    std::fs::write(
        sub.join("broken.py"),
        "def inner():\n    raise KeyError('missing')\n\ninner()\n",
    )
    .unwrap();
    let mut s = ConsoleSession::start(&py, dir.path(), &[], "").unwrap();
    wait_event(&mut s, |e| matches!(e, ConsoleEvent::Ready { .. }));

    let id = s.run_file(
        &script,
        &["one".into(), "two words".into()],
        Some(&sub),
        &[("JOB_ENV".into(), "set".into())],
    );
    let (value, error, more, out, err) = wait_result(&mut s, id);
    assert_eq!((value, error, more), (None, None, false), "stderr: {err:?}");
    assert_eq!(
        out,
        "argv ['one', 'two words'] job.py pkg set\nmain block\n"
    );
    // The script's variables stay in the console (pickle finds its classes in
    // __main__); __file__ and sys.argv are restored afterwards.
    let id = s.exec("(result, roundtrip, '__file__' in dir(), __import__('sys').argv)");
    let (value, error, _, _, _) = wait_result(&mut s, id);
    assert_eq!(
        value.as_deref(),
        Some("(42, True, False, ['<console>'])"),
        "error: {error:?}"
    );

    // A failing script reports a traceback with its own frames only.
    let id = s.run_file(&sub.join("broken.py"), &[], None, &[]);
    let (_, error, _, _, err) = wait_result(&mut s, id);
    assert!(error.unwrap_or_default().contains("KeyError: 'missing'"));
    assert!(
        err.contains("broken.py\", line 4") && err.contains("in inner"),
        "stderr: {err:?}"
    );
    assert!(
        !err.contains("rustpy_console_helper"),
        "helper frames leaked: {err:?}"
    );

    // `runfile()` is also available at the prompt (the command the IDE echoes);
    // the working directory is still `pkg` from the first run.
    let id = s.exec("result = 0\nrunfile('job.py', args='x')");
    let (_, error, _, out, err) = wait_result(&mut s, id);
    assert!(error.is_none(), "stderr: {err:?}");
    assert!(out.starts_with("argv ['x'] job.py"), "{out:?}");
    let id = s.exec("result");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("42"));

    // A missing file is an ordinary error and the session keeps working.
    let id = s.run_file(&sub.join("nope.py"), &[], None, &[]);
    let (_, error, _, _, _) = wait_result(&mut s, id);
    assert!(error.unwrap_or_default().contains("FileNotFoundError"));
    let id = s.exec("1 + 1");
    let (value, _, _, _, _) = wait_result(&mut s, id);
    assert_eq!(value.as_deref(), Some("2"));
    s.stop();
}

#[test]
fn lazy_pages_edit_handles_and_rectangular_data_share_namespace() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut session = ConsoleSession::start(&py, dir.path(), &[], "").unwrap();
    let id = session.exec_fragment("items = list(range(1000))\nclass Guard:\n    @property\n    def dangerous(self):\n        raise AssertionError('inspection evaluated property')\nguard = Guard()");
    assert!(wait_result(&mut session, id).1.is_none());
    let id = session.inspect_page("items", 400, 3);
    let event = wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Page { id: found, .. } if *found == id),
    );
    let handle = match event {
        ConsoleEvent::Page {
            start,
            total,
            items,
            error,
            ..
        } => {
            assert_eq!((start, total, items.len()), (400, 1000, 3));
            assert!(error.is_none());
            assert_eq!(items[0].value, "400");
            items[0].expr.clone()
        }
        _ => unreachable!(),
    };
    let id = session.set_value(&handle, "'changed'");
    assert!(matches!(
        wait_event(
            &mut session,
            |event| matches!(event, ConsoleEvent::ValueSet { id: found, .. } if *found == id)
        ),
        ConsoleEvent::ValueSet { error: None, .. }
    ));
    let id = session.exec_fragment("items[400]");
    assert_eq!(
        wait_result(&mut session, id).0.as_deref(),
        Some("'changed'")
    );
    let id = session.inspect_page(&handle, 0, 10);
    assert!(matches!(
        wait_event(
            &mut session,
            |event| matches!(event, ConsoleEvent::Page { id: found, .. } if *found == id)
        ),
        ConsoleEvent::Page { error: Some(_), .. }
    ));
    let id = session.inspect_page("guard", 0, 200);
    match wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Page { id: found, .. } if *found == id),
    ) {
        ConsoleEvent::Page { items, error, .. } => {
            assert!(error.is_none());
            assert!(items
                .iter()
                .any(|item| item.name == "dangerous" && item.value.contains("descriptor")));
        }
        _ => unreachable!(),
    }
    // Rendering owns the helper installed at session startup, not whichever
    // module a user import path (or runfile) later resolves by the same name.
    let id = session.exec_fragment("import sys\nsys.modules.pop('rustpy_scientific_table', None)\nassert 'scientific_table' not in globals()\nassert '_scientific_table' not in globals()");
    assert!(wait_result(&mut session, id).1.is_none());
    let id = session.request_data("items", 399, 3, 0, 1);
    match wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Data { id: found, .. } if *found == id),
    ) {
        ConsoleEvent::Data {
            table: Some(table),
            error: None,
            ..
        } => {
            assert_eq!(table.total_rows, 1000);
            // Variable/REPL values above retain repr quotes. Scientific cells
            // use the shared table adapter's bounded str(value) display text.
            assert_eq!(table.rows, vec![vec!["399"], vec!["changed"], vec!["401"]]);
            assert_eq!(
                (table.row_start, table.col_start, table.total_cols),
                (399, 0, 1)
            );
            assert_eq!(table.index, ["399", "400", "401"]);
            assert_eq!(table.columns, ["0"]);
            assert!(table.truncated);
        }
        event => panic!("unexpected data response {event:?}"),
    }
}

#[test]
fn explicit_console_launch_options_and_failed_runfile_preserve_environment_boundaries() {
    let Some(python) = python() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let options = vec!["-O".to_string()];
    let mut session = ConsoleSession::start_with_launch_options(
        &python,
        directory.path(),
        &[("CONSOLE_ONLY".into(), "present".into())],
        "import os, sys",
        false,
        &options,
    )
    .unwrap();
    wait_event(&mut session, |event| {
        matches!(event, ConsoleEvent::Ready { .. })
    });
    assert!(session.launch_matches(&python, &options, false));
    assert!(!session.launch_matches(&python, &[], false));
    let request = session
        .exec("(sys.flags.optimize, os.environ.get('PATH'), os.environ.get('CONSOLE_ONLY'))");
    assert_eq!(
        wait_result(&mut session, request).0.as_deref(),
        Some("(1, None, 'present')")
    );
    let script = directory.path().join("failed.py");
    std::fs::write(&script,"import os\nif 'CONSOLE_ONLY' in os.environ: raise RuntimeError('leaked console environment')\nif os.environ.get('SCRIPT_ONLY') != 'yes': raise RuntimeError('missing explicit environment')\nraise RuntimeError('intentional failure after environment checks')\n").unwrap();
    let request = session.run_file_with_environment(
        &script,
        &[],
        Some(directory.path()),
        &[("SCRIPT_ONLY".into(), "yes".into())],
        false,
    );
    let result = wait_result(&mut session, request);
    assert!(result
        .1
        .unwrap_or_default()
        .contains("intentional failure after environment checks"));
    let request = session.exec("(os.environ.get('CONSOLE_ONLY'), os.environ.get('SCRIPT_ONLY'))");
    assert_eq!(
        wait_result(&mut session, request).0.as_deref(),
        Some("('present', None)")
    );
    session.restart();
    wait_event(&mut session, |event| {
        matches!(event, ConsoleEvent::Ready { .. })
    });
    let request = session
        .exec("(sys.flags.optimize, os.environ.get('PATH'), os.environ.get('CONSOLE_ONLY'))");
    assert_eq!(
        wait_result(&mut session, request).0.as_deref(),
        Some("(1, None, 'present')")
    );
    session.stop();
}

#[test]
#[ignore = "requires explicitly admitted RUSTPY_PROVIDER_PYTHON with real IPython"]
fn configured_ipython_magic_help_and_completion_share_live_namespace() {
    let python = required_python();
    let directory = tempfile::tempdir().unwrap();
    let mut session = ConsoleSession::start(&python, directory.path(), &[], "").unwrap();
    wait_event(&mut session, |event| {
        matches!(event, ConsoleEvent::Ready { .. })
    });
    let request = session.use_ipython(true);
    let configured = wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Configured { id, .. } if *id == request),
    );
    assert!(
        matches!(
            configured,
            ConsoleEvent::Configured {
                ipython: true,
                error: None,
                ..
            }
        ),
        "{configured:?}"
    );
    let request = session.exec("%pwd");
    let (value, error, more, _, _) = wait_result(&mut session, request);
    assert!(error.is_none() && !more, "{error:?}");
    assert!(value.unwrap_or_default().contains(
        &directory
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string()
    ));
    let request = session.exec_fragment(
        "def owned_help():\n    \"\"\"ACTUAL_IPYTHON_HELP_MARKER\"\"\"\n    return 42",
    );
    assert!(wait_result(&mut session, request).1.is_none());
    let request = session.exec("owned_help?");
    let (_, error, more, stdout, stderr) = wait_result(&mut session, request);
    assert!(error.is_none() && !more, "{error:?}: {stderr}");
    assert!(
        stdout.contains("ACTUAL_IPYTHON_HELP_MARKER"),
        "real IPython help missing: {stdout:?} {stderr:?}"
    );
    let request = session.complete("owned_h", 7);
    assert!(
        matches!(wait_event(&mut session, |event| matches!(event, ConsoleEvent::Completions { id, .. } if *id == request)),
        ConsoleEvent::Completions { items, .. } if items.iter().any(|item| item.name.trim_end_matches('(') == "owned_help"))
    );
    let request = session.exec_fragment("π_owned = 'hello'");
    assert!(wait_result(&mut session, request).1.is_none());
    for (code, cursor) in [
        ("π_owned.up", "π_owned.up".len()),
        ("π_owned.up + 'trailing'", "π_owned.up".len()),
        (
            "π_header = 1\nπ_owned.up + 'trailing'",
            "π_header = 1\nπ_owned.up".len(),
        ),
    ] {
        let request = session.complete(code, cursor);
        assert!(
            matches!(wait_event(&mut session, |event| matches!(event, ConsoleEvent::Completions { id, .. } if *id == request)),
            ConsoleEvent::Completions { items, prefix, .. } if prefix == "up" && items.iter().any(|item| item.name == "upper")),
            "IPython member completion must honor byte cursor {cursor} in {code:?}"
        );
    }
    // A normal rlcompleter result cannot prove IPython's magic-name provider.
    let request = session.complete("%matplo", 7);
    assert!(
        matches!(wait_event(&mut session, |event| matches!(event, ConsoleEvent::Completions { id, .. } if *id == request)),
        ConsoleEvent::Completions { items, prefix, .. } if items.iter().any(|item|
            (prefix == "%matplo" && item.name == "%matplotlib") || (prefix == "matplo" && item.name == "matplotlib"))),
        "selected IPython must supply its real magic completion"
    );
    let request = session.use_ipython(false);
    let configured = wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Configured { id, .. } if *id == request),
    );
    assert!(
        matches!(
            configured,
            ConsoleEvent::Configured {
                ipython: false,
                error: None,
                ..
            }
        ),
        "{configured:?}"
    );
    let code = "π_owned.up + 'trailing'";
    let request = session.complete(code, "π_owned.up".len());
    assert!(
        matches!(wait_event(&mut session, |event| matches!(event, ConsoleEvent::Completions { id, .. } if *id == request)),
        ConsoleEvent::Completions { items, prefix, .. } if prefix == "up" && items.iter().any(|item| item.name == "upper")),
        "CPython fallback also honors the documented byte cursor before trailing text"
    );
    session.stop();
    assert!(!session.is_running());
}

#[test]
#[ignore = "requires explicitly admitted Python with real NumPy/pandas/matplotlib"]
fn configured_scientific_handles_rectangles_and_real_matplotlib_plot_are_published() {
    let python = required_python();
    let directory = tempfile::tempdir().unwrap();
    let mut session = ConsoleSession::start(
        &python,
        directory.path(),
        &[("MPLBACKEND".into(), "Agg".into())],
        "",
    )
    .unwrap();
    wait_event(&mut session, |event| {
        matches!(event, ConsoleEvent::Ready { .. })
    });
    let request = session.exec_fragment("import numpy as np\nimport pandas as pd\narray = np.arange(24).reshape(6, 4)\nframe = pd.DataFrame({'alpha': [10, 11, 12], 'beta': [20, 21, 22]}, index=['r0', 'r1', 'r2'])");
    let result = wait_result(&mut session, request);
    assert!(
        result.1.is_none(),
        "real scientific imports failed: {result:?}"
    );
    let request = session.request_vars();
    let ConsoleEvent::Vars { items, .. } = wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Vars { id, .. } if *id == request),
    ) else {
        unreachable!()
    };
    let array = items
        .iter()
        .find(|item| item.name == "array")
        .expect("real array variable");
    let frame = items
        .iter()
        .find(|item| item.name == "frame")
        .expect("real dataframe variable");
    assert!(
        array.value.contains("6") && array.value.contains("4"),
        "{array:?}"
    );
    assert!(frame.value.contains("DataFrame"), "{frame:?}");
    for (handle, row, column, expected, rows, columns) in [
        (
            array.expr.as_str(),
            2,
            1,
            vec![vec!["9", "10"], vec!["13", "14"]],
            6,
            4,
        ),
        (
            frame.expr.as_str(),
            1,
            0,
            vec![vec!["11", "21"], vec!["12", "22"]],
            3,
            2,
        ),
    ] {
        assert!(!handle.is_empty());
        let request = session.request_data(handle, row, 2, column, 2);
        let event = wait_event(
            &mut session,
            |event| matches!(event, ConsoleEvent::Data { id, .. } if *id == request),
        );
        let ConsoleEvent::Data {
            table: Some(table),
            error: None,
            ..
        } = event
        else {
            panic!("real scientific rectangle failed: {event:?}")
        };
        assert_eq!(
            (
                table.row_start,
                table.col_start,
                table.total_rows,
                table.total_cols
            ),
            (row, column, rows, columns)
        );
        assert_eq!(table.rows, expected);
        if handle == frame.expr {
            assert_eq!(table.columns, ["alpha", "beta"]);
            assert_eq!(table.index, ["r1", "r2"]);
        }
    }
    let request = session.set_plot_capture(true);
    let configured = wait_event(
        &mut session,
        |event| matches!(event, ConsoleEvent::Configured { id, .. } if *id == request),
    );
    assert!(
        matches!(
            configured,
            ConsoleEvent::Configured {
                plots: true,
                error: None,
                ..
            }
        ),
        "{configured:?}"
    );
    let request = session.exec_fragment("import matplotlib.pyplot as plt\nplt.plot([0, 1], [1, 3])\nplt.title('ACTUAL_CONSOLE_PLOT')\nplt.show()");
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut plot, mut result) = (false, false);
    while Instant::now() < deadline && !(plot && result) {
        for event in session.poll() {
            match event {
                ConsoleEvent::Plot {
                    id,
                    mime,
                    data_base64,
                } if id == request => {
                    assert_eq!(mime, "image/png");
                    assert!(
                        data_base64.starts_with("iVBORw0KGgo") && data_base64.len() > 1000,
                        "matplotlib must publish a real encoded PNG"
                    );
                    plot = true;
                }
                ConsoleEvent::Result {
                    id, error, more, ..
                } if id == request => {
                    assert!(error.is_none() && !more, "{error:?}");
                    result = true;
                }
                ConsoleEvent::Failed(error) => panic!("scientific console failed: {error}"),
                ConsoleEvent::Exited(code) => panic!("scientific console exited early: {code}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        plot && result,
        "real plot and completed execution must both arrive"
    );
    session.stop();
    assert!(!session.is_running());
}

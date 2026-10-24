//! Real selected-debugpy acceptance for the shared console execution engine.
//! Explicit invocation requires RUSTPY_REQUIRE_PROVIDER_TESTS=1 and an absolute
//! RUSTPY_PROVIDER_PYTHON; run this ignored target serially with --nocapture.
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use rustpy_console::debugger::evaluate_in_console;
use rustpy_debug::{Breakpoint, DebugSession, DebugState, LaunchSetup, ReplEntry, Variable};

fn admitted_python() -> PathBuf {
    assert_eq!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
        Ok("1"),
        "real debugger-console acceptance requires explicit provider admission"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("set RUSTPY_PROVIDER_PYTHON to the installed selected SDK"),
    );
    assert!(
        python.is_absolute() && python.is_file(),
        "admit an absolute installed SDK"
    );
    let result = Command::new(&python)
        .args(["-I", "-c", "import debugpy; print(debugpy.__version__)"])
        .output()
        .expect("selected SDK must launch");
    assert!(
        result.status.success(),
        "selected SDK must provide real debugpy: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    eprintln!(
        "REQUIRED_DEBUGGER_CONSOLE python={} debugpy={}",
        python.display(),
        String::from_utf8_lossy(&result.stdout).trim()
    );
    python
}

struct OwnedSession(DebugSession);
impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.stop();
    }
}

fn wait(session: &mut DebugSession, description: &str, ready: impl Fn(&DebugSession) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        session.poll();
        if ready(session) {
            return;
        }
        if Instant::now() >= deadline {
            let errors = session.take_errors();
            panic!(
                "{description}: state={:?}, errors={errors:?}, output={:?}, repl={:?}",
                session.state,
                output(session),
                session.repl
            );
        }
        assert!(
            !matches!(session.state, DebugState::Failed(_)),
            "{description}: {:?}: {}",
            session.state,
            output(session)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn output(session: &DebugSession) -> String {
    session
        .output
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect()
}

fn evaluate(session: &mut DebugSession, source: &str) -> Variable {
    let start = session.repl.len();
    let epoch = session.stop_epoch;
    evaluate_in_console(session, source);
    wait(
        session,
        "real suspended-frame console evaluation",
        |session| {
            session.repl[start..]
                .iter()
                .any(|item| matches!(item.entry, ReplEntry::Value(_) | ReplEntry::Error(_)))
        },
    );
    let entries = &session.repl[start..];
    assert!(
        entries
            .iter()
            .any(|item| item.epoch == epoch && item.entry == ReplEntry::Input(source.into())),
        "console retains user input rather than the generated helper expression: {entries:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|item| matches!(item.entry, ReplEntry::Error(_))),
        "{entries:?}"
    );
    entries
        .iter()
        .find_map(|item| match &item.entry {
            ReplEntry::Value(value) if item.epoch == epoch => Some(value.clone()),
            _ => None,
        })
        .expect("actual debugpy result at selected stop epoch")
}

const DEBUGGEE: &str = r#"import os
from pathlib import Path
Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))

def outer():
    counter = 100
    def inner():
        counter = 7
        bag = {'visits': 1}
        pause_here = 0
        print('DEBUGGEE_RESUMED', counter, bag['visits'], flush=True)
        return counter
    result = inner()
    print('DEBUGGEE_OUTER', counter, result, flush=True)
outer()
print('DEBUGGEE_FINISHED', flush=True)
"#;

#[test]
#[ignore = "requires explicitly admitted selected Python SDK with real debugpy and local DAP"]
fn configured_debugpy_suspended_frame_uses_console_engine_and_retires() {
    let python = admitted_python();
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("frame_console.py");
    std::fs::write(&script, DEBUGGEE).unwrap();
    let line = DEBUGGEE
        .lines()
        .position(|line| line.contains("pause_here ="))
        .unwrap()
        + 1;
    // Deserialization uses the public RunConfig accepted by launch, without
    // adding a duplicate project-model dependency to this integration target.
    let configuration = serde_json::from_value(serde_json::json!({
        "name": "required debugger console frame",
        "kind": {"type": "python", "script": script},
        "working_dir": root.path(),
        "interpreter": python,
        "env": {"PYTHONUNBUFFERED": "1"}
    }))
    .unwrap();
    let setup = LaunchSetup {
        breakpoints: std::collections::HashMap::from([(
            script.clone(),
            vec![Breakpoint::new(line)],
        )]),
        ..Default::default()
    };
    let mut owner = OwnedSession(
        DebugSession::launch(74100, &configuration, None, root.path(), setup)
            .expect("launch real selected debugpy debuggee"),
    );
    let session = &mut owner.0;
    wait(session, "actual breakpoint and stack", |session| {
        session.is_stopped()
            && session.frames.iter().any(|frame| frame.name == "inner")
            && session.frames.iter().any(|frame| frame.name == "outer")
    });
    assert_eq!(session.is_verified(&script, line), Some(true));
    let inner = session
        .frames
        .iter()
        .position(|frame| frame.name == "inner")
        .unwrap();
    let outer = session
        .frames
        .iter()
        .position(|frame| frame.name == "outer")
        .unwrap();
    session.select_frame(outer);
    assert_eq!(evaluate(session, "counter").value, "100");
    session.select_frame(inner);
    assert_eq!(
        session.current_frame().unwrap().path.as_deref(),
        Some(script.as_path())
    );
    assert_eq!(session.current_frame().unwrap().line, line);
    assert_eq!(evaluate(session, "counter").value, "7");
    wait(session, "selected frame scopes", |session| {
        !session.scopes.is_empty()
    });
    let references: Vec<_> = session
        .scopes
        .iter()
        .filter(|scope| !scope.expensive)
        .map(|scope| scope.variables_reference)
        .collect();
    assert!(!references.is_empty());
    for reference in references {
        session.request_variables(reference);
    }
    wait(session, "actual frame local variables", |session| {
        session
            .variables
            .values()
            .flatten()
            .any(|variable| variable.name == "counter" && variable.value == "7")
    });
    let changed = evaluate(session,
        "counter += 5\nbag['visits'] += 2\nprint('FRAME_CONSOLE_MUTATED', counter, bag['visits'], flush=True)\n(counter, bag['visits'])");
    assert_eq!(changed.value, "(12, 3)");
    assert_eq!(
        evaluate(session, "(counter, bag['visits'])").value,
        "(12, 3)",
        "a separate DAP evaluation sees the mutated frame"
    );
    assert_eq!(evaluate(session, "sorted(set(globals()) & {'execute', 'safe_repr', 'ast', 'inspect', '_rp', '_rg', '_rl'})").value, "[]",
        "bundled implementation must not pollute debuggee globals");
    wait(
        session,
        "console print forwarded through actual debugpy output",
        |session| output(session).contains("FRAME_CONSOLE_MUTATED 12 3"),
    );
    let pid: u32 = std::fs::read_to_string(script.with_extension("pid"))
        .unwrap()
        .parse()
        .unwrap();
    session.continue_();
    wait(session, "normal debuggee/session termination", |session| {
        matches!(session.state, DebugState::Terminated(Some(0)))
            && output(session).contains("DEBUGGEE_FINISHED")
    });
    let text = output(session);
    assert!(
        text.contains("DEBUGGEE_RESUMED 12 3"),
        "resumed Python observes frame mutation: {text}"
    );
    assert!(
        text.contains("DEBUGGEE_OUTER 100 12"),
        "only selected inner frame was mutated: {text}"
    );
    assert!(!session.is_active());
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let proc_entry = PathBuf::from(format!("/proc/{pid}"));
        while proc_entry.exists() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_entry.exists(),
            "actual debuggee PID {pid} must physically retire"
        );
    }
    session.stop();
    wait(
        session,
        "DAP disconnect ownership released",
        DebugSession::disconnect_completed,
    );
    assert_eq!(std::fs::read_to_string(&script).unwrap(), DEBUGGEE);
    eprintln!("REQUIRED_DEBUGGER_CONSOLE actual_selected_frame=inner debuggee_pid={pid} normal_exit=0 source_unchanged=true");
    // The PID assertion covers the debuggee on Linux; it does not claim that
    // the public DAP API exposes every detached adapter descendant's identity.
}

//! "Collect run-time types information": runs a Python configuration under
//! `typecollect.py`, which records argument/return types of project functions
//! into `<project>/.rustpy/runtime_types.json`.

use std::path::{Path, PathBuf};

use crate::process::RunSession;
use rustpy_project::config::{RunConfig, RunConfigKind};
use rustpy_project::interpreter::Interpreter;

pub const COLLECTOR_SOURCE: &str = include_str!("typecollect.py");

pub fn output_path(project_root: &Path) -> PathBuf {
    project_root.join(".rustpy").join("runtime_types.json")
}

/// Whether a configuration can run under the collector (scripts and modules).
/// Test configurations are collected by the Test Runner instead
/// (`rustpy_test::TestRunOptions::collect_runtime_types`), which wraps its
/// reporter or runner module with [`wrap_args`].
pub fn supports(cfg: &RunConfig) -> bool {
    matches!(
        cfg.kind,
        RunConfigKind::Python { .. } | RunConfigKind::PythonModule { .. }
    )
}

/// Inserts `collector OUT ROOT` after the interpreter options of a Python command line.
pub fn wrap_args(
    args: &[String],
    interpreter_option_count: usize,
    collector: &Path,
    out: &Path,
    root: &Path,
) -> Vec<String> {
    let at = interpreter_option_count.min(args.len());
    let mut v = args[..at].to_vec();
    v.push(collector.to_string_lossy().into_owned());
    v.push(out.to_string_lossy().into_owned());
    v.push(root.to_string_lossy().into_owned());
    v.extend_from_slice(&args[at..]);
    v
}

/// Spawns `cfg` under the collector (falls back to a plain run for other kinds).
pub fn spawn_collecting(
    id: u64,
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
) -> RunSession {
    if !supports(cfg) {
        return RunSession::spawn(id, cfg, interpreter, project_root);
    }
    let (program, args, cwd, env) = RunSession::command_for(cfg, interpreter, project_root);
    let collector =
        std::env::temp_dir().join(format!("rustpy-typecollect-{}.py", std::process::id()));
    if std::fs::write(&collector, COLLECTOR_SOURCE).is_err() {
        return RunSession::spawn(id, cfg, interpreter, project_root);
    }
    let n = RunSession::interpreter_option_count(cfg);
    let args = wrap_args(
        &args,
        n,
        &collector,
        &output_path(project_root),
        project_root,
    );
    let mut session = if cfg.emulate_terminal {
        RunSession::spawn_pty_command_with_input(
            id,
            &cfg.name,
            &program,
            &args,
            &cwd,
            &env,
            crate::process::PtyInput {
                inherit_environment: cfg.workflow.inherit_environment,
                path: cfg.input_file(project_root).as_deref(),
            },
        )
    } else {
        RunSession::spawn_command_with_environment(
            id,
            &cfg.name,
            &program,
            &args,
            &cwd,
            &env,
            crate::process::CommandInput {
                stdin: crate::process::config_stdin(cfg, project_root),
                inherit_environment: cfg.workflow.inherit_environment,
            },
        )
    };
    session.config = Some(cfg.clone());
    if let Some(kib) = cfg.console_buffer_kib {
        session.set_capture_limit(kib.saturating_mul(1024));
    }
    session.close_redirected_stdin(cfg, project_root);
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_after_interpreter_options() {
        let args: Vec<String> = ["-X", "dev", "main.py", "--fast"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let w = wrap_args(
            &args,
            2,
            Path::new("/tmp/c.py"),
            Path::new("/p/.rustpy/rt.json"),
            Path::new("/p"),
        );
        assert_eq!(
            w,
            vec![
                "-X",
                "dev",
                "/tmp/c.py",
                "/p/.rustpy/rt.json",
                "/p",
                "main.py",
                "--fast"
            ]
        );
    }

    #[test]
    fn collected_process_gets_eof_when_redirect_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("input.py");
        std::fs::write(
            &script,
            "import sys\nprint('redirect EOF:' + repr(sys.stdin.read()))\n",
        )
        .unwrap();
        let input = dir.path().join("null");
        std::fs::write(&input, "owned nonempty regular input\n").unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(which::which("python3").expect("Python fixture interpreter"));
        cfg.redirect_input = Some(input.clone());
        assert!(cfg.input_problem(dir.path()).is_none());
        std::fs::remove_file(input).unwrap();
        let mut session = spawn_collecting(8, &cfg, None, dir.path());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while session.is_running() && std::time::Instant::now() < deadline {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        session.poll();
        assert_eq!(
            session.status,
            crate::process::SessionStatus::Exited(0),
            "{}",
            session.text()
        );
        let stdout: String = session
            .output
            .iter()
            .filter(|chunk| chunk.kind == crate::process::OutputKind::Stdout)
            .map(|chunk| chunk.text.as_str())
            .collect();
        assert_eq!(stdout, "redirect EOF:''\n");
    }

    #[test]
    fn collects_types_end_to_end() {
        let python = if cfg!(windows) { "python" } else { "python3" };
        if std::process::Command::new(python)
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("rustpy-typecollect-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.py"), "class Shape:\n    def scale(self, k):\n        return k * 2\n\n\ndef area(w, h):\n    return w * h\n\n\nprint(area(2, 3.5), Shape().scale(3))\n").unwrap();
        let collector = root.join("collector.py");
        std::fs::write(&collector, COLLECTOR_SOURCE).unwrap();
        let out = output_path(&root);
        let status = std::process::Command::new(python)
            .arg(&collector)
            .arg(&out)
            .arg(&root)
            .arg(root.join("app.py"))
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let funcs = json["functions"].as_object().unwrap();
        let area = funcs
            .iter()
            .find(|(k, _)| k.ends_with("::area"))
            .map(|(_, v)| v)
            .expect("area recorded");
        assert_eq!(area["params"]["w"], serde_json::json!(["int"]));
        assert_eq!(area["params"]["h"], serde_json::json!(["float"]));
        assert_eq!(area["return"], serde_json::json!(["float"]));
        assert!(
            funcs.keys().all(|k| !k.contains("<frozen")),
            "frozen stdlib modules are not project code: {:?}",
            funcs.keys().collect::<Vec<_>>()
        );
        let scale = funcs
            .iter()
            .find(|(k, _)| k.ends_with("::Shape.scale"))
            .map(|(_, v)| v)
            .expect("method recorded with qualname");
        assert!(scale["params"].get("self").is_none());
        assert_eq!(scale["return"], serde_json::json!(["int"]));
        let _ = std::fs::remove_dir_all(&root);
    }
}

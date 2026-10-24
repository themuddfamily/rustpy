use rustpy_core::cancel::CancellationToken;
use rustpy_format::{
    candidate::{format_candidate, Formatter},
    FormatOptions,
};
use rustpy_syntax::{TextRange, TextSize};

#[test]
fn builtin_candidate_preserves_primary_selection_and_input() {
    let original = "first=1\nsecond=2\n";
    let range = TextRange::new(0.into(), 7.into());
    let options = FormatOptions::default();
    let expected = rustpy_format::format_range(original, range, &options).unwrap();
    let result = format_candidate(
        original,
        Formatter::Builtin {
            range,
            options: &options,
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(result.edits, expected);
    assert_eq!(result.text, "first = 1\nsecond=2\n");
    assert_eq!(original, "first=1\nsecond=2\n");
}
#[test]
fn builtin_failure_or_precancel_produces_no_candidate() {
    let options = FormatOptions::default();
    let text = "def broken(:\n";
    let formatter = || Formatter::Builtin {
        range: TextRange::new(0.into(), TextSize::of(text)),
        options: &options,
    };
    assert!(format_candidate(text, formatter(), &CancellationToken::new()).is_err());
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        format_candidate("x=1\n", formatter(), &token),
        Err(rustpy_format::candidate::CandidateError::Cancelled)
    ));
}

// macOS runners fail these with EPERM at process-group operations; see the
// known-issues list until that is understood on a Mac.
#[cfg(target_os = "linux")]
mod process {
    use super::*;
    use rustpy_format::candidate::{CandidateError, ExternalFormatter, FormatterLimits};
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex, MutexGuard,
        },
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // External invocations share a process-wide cap of four concurrent
    // formatters (excess calls return `Busy`). Serialise the tests in this
    // binary that launch formatters so parallel test threads cannot exhaust it.
    static EXTERNAL: Mutex<()> = Mutex::new(());
    fn serial() -> MutexGuard<'static, ()> {
        EXTERNAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(module: &str, script: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rustpy-formatter-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join(format!("{module}.py")), script).unwrap();
            Self(dir)
        }
        fn config<'a>(
            &'a self,
            module: &'a str,
            env: &'a HashMap<String, String>,
        ) -> ExternalFormatter<'a> {
            ExternalFormatter {
                python: Path::new("python3"),
                formatter: module,
                extra_args: "",
                cwd: Some(&self.0),
                env,
                limits: FormatterLimits {
                    timeout: Duration::from_secs(10),
                    ..FormatterLimits::default()
                },
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn external_preserves_arguments_environment_cwd_and_normalizes_output() {
        let _serial = serial();
        for (module, expected) in [
            ("black", "['-q', '-', '--line-length', '80']"),
            ("ruff", "['format', '-', '--line-length', '80']"),
            ("autopep8", "['-', '--line-length', '80']"),
            ("yapf", "['--line-length', '80']"),
        ] {
            let script = format!(
                "import sys, os\nassert repr(sys.argv[1:]) == {expected:?}\nassert os.environ['RUSTPY_TEST_ENV'] == 'chosen'\nassert os.path.exists('{module}.py')\nsys.stdout.buffer.write(sys.stdin.buffer.read().replace(b'x=1', b'x = 1').replace(b'\\n', b'\\r\\n'))\n"
            );
            let fixture = Fixture::new(module, &script);
            let env = HashMap::from([("RUSTPY_TEST_ENV".into(), "chosen".into())]);
            let mut config = fixture.config(module, &env);
            config.extra_args = " --line-length  80 ";
            let result = format_candidate(
                "x=1\n",
                Formatter::External(config),
                &CancellationToken::new(),
            )
            .unwrap();
            assert_eq!(result.text, "x = 1\n");
            assert_eq!(
                result.edits,
                vec![(TextRange::new(0.into(), 4.into()), "x = 1\n".into())]
            );
        }
    }
    #[test]
    fn failed_invalid_utf8_and_empty_outputs_are_errors() {
        let _serial = serial();
        let env = HashMap::new();
        for (script, kind) in [
            (
                "import sys\nsys.stderr.write('bad formatter')\nsys.exit(7)\n",
                0,
            ),
            ("import os\nos.write(1, b'\\xff')\n", 1),
            ("import sys\nsys.stdin.buffer.read()\n", 2),
        ] {
            let f = Fixture::new("fixture", script);
            let error = format_candidate(
                "x=1\n",
                Formatter::External(f.config("fixture", &env)),
                &CancellationToken::new(),
            )
            .unwrap_err();
            match (kind, error) {
                (0, CandidateError::Failed { status, stderr }) => {
                    assert_eq!(status.code(), Some(7));
                    assert_eq!(stderr, "bad formatter");
                }
                (1, CandidateError::InvalidUtf8(_)) | (2, CandidateError::EmptyOutput) => {}
                (_, error) => panic!("unexpected error: {error}"),
            }
        }
    }
    #[test]
    fn both_output_pipes_are_drained_and_capped() {
        let _serial = serial();
        let env = HashMap::new();
        for stream in ["stdout", "stderr"] {
            let f = Fixture::new(
                "fixture",
                &format!(
                    "import sys\nsys.stdin.buffer.read()\nsys.{stream}.buffer.write(b'x' * 2000000)\nsys.{stream}.flush()\n"
                ),
            );
            let mut config = f.config("fixture", &env);
            config.limits.max_stdout_bytes = 1024;
            config.limits.max_stderr_bytes = 1024;
            let start = Instant::now();
            assert!(
                matches!(format_candidate("x=1\n", Formatter::External(config), &CancellationToken::new()), Err(CandidateError::OutputTooLarge(s)) if s == stream)
            );
            assert!(start.elapsed() < Duration::from_secs(8));
        }
        let f = Fixture::new(
            "fixture",
            "import sys\nsys.stderr.buffer.write(b'e'*200000)\nsys.stderr.flush()\nsys.stdout.buffer.write(sys.stdin.buffer.read())\n",
        );
        let input = "x".repeat(300000);
        let result = format_candidate(
            &input,
            Formatter::External(f.config("fixture", &env)),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(result.text, input);
        assert!(result.edits.is_empty());
    }
    #[test]
    fn timeout_and_cancel_stop_blocked_input_and_inherited_output() {
        let _serial = serial();
        let env = HashMap::new();
        for cancel in [false, true] {
            let f = Fixture::new(
                "fixture",
                "import subprocess, sys, time\np=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\nopen('descendant.pid', 'w').write(str(p.pid))\ntime.sleep(60)\n",
            );
            let mut config = f.config("fixture", &env);
            // The timeout must leave room for the formatter to spawn its
            // descendant under load, and must never win the cancel race.
            config.limits.timeout = Duration::from_secs(if cancel { 30 } else { 3 });
            let token = CancellationToken::new();
            let pid_file = f.0.join("descendant.pid");
            let cancellation = cancel.then(|| {
                let (t, pid_file) = (token.clone(), pid_file.clone());
                std::thread::spawn(move || {
                    // Cancel only once the descendant exists, so cleanup of
                    // the whole process group is what this case exercises.
                    let give_up = Instant::now() + Duration::from_secs(20);
                    while Instant::now() < give_up
                        && std::fs::read_to_string(&pid_file)
                            .ok()
                            .and_then(|pid| pid.parse::<u32>().ok())
                            .is_none()
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    t.cancel();
                })
            });
            let start = Instant::now();
            let result = format_candidate(&"x".repeat(300000), Formatter::External(config), &token);
            assert!(
                matches!(
                    (cancel, &result),
                    (true, Err(CandidateError::Cancelled)) | (false, Err(CandidateError::TimedOut))
                ),
                "cancel={cancel}: unexpected result {:?}",
                result.map(|c| c.text)
            );
            // Far below the 60 s the blocked formatter and descendant sleep.
            assert!(start.elapsed() < Duration::from_secs(25));
            if let Some(thread) = cancellation {
                thread.join().unwrap();
            }
            #[cfg(target_os = "linux")]
            {
                let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                assert!(
                    stat.is_empty() || stat.split_once(") ").unwrap().1.starts_with('Z'),
                    "descendant is still running: {stat}"
                );
            }
        }
        // A successful leader exit must not bypass the output-drain deadline.
        let f = Fixture::new(
            "fixture",
            "import subprocess,sys\nsubprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])\nsys.stdin.buffer.read()\nprint('x = 1')\n",
        );
        let mut config = f.config("fixture", &env);
        config.limits.timeout = Duration::from_millis(300);
        assert!(matches!(
            format_candidate(
                "x=1\n",
                Formatter::External(config),
                &CancellationToken::new()
            ),
            Err(CandidateError::TimedOut)
        ));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn successful_leader_retires_descendant_even_after_its_pipes_close() {
        let _serial = serial();
        let env = HashMap::new();
        let f = Fixture::new(
            "fixture",
            "import subprocess,sys\np=subprocess.Popen([sys.executable,'-c','import os,time;os.close(0);os.close(1);os.close(2);time.sleep(60)'])\nwith open('descendant.pid','w') as out: out.write(str(p.pid))\nsys.stdin.buffer.read()\nprint('x = 1')\n",
        );
        let result = format_candidate(
            "x=1\n",
            Formatter::External(f.config("fixture", &env)),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(result.text, "x = 1\n");
        let pid: u32 = std::fs::read_to_string(f.0.join("descendant.pid"))
            .unwrap()
            .parse()
            .unwrap();
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            assert!(
                stat.is_empty() || stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
                "descendant is still running after its leader succeeded: {stat}"
            );
        }
    }
    #[test]
    fn input_limit_refuses_before_launch() {
        let env = HashMap::new();
        let f = Fixture::new("fixture", "raise Exception('must not launch')\n");
        let mut config = f.config("fixture", &env);
        config.python = Path::new("/no-such-python");
        config.limits.max_input_bytes = 3;
        assert!(matches!(
            format_candidate(
                "four",
                Formatter::External(config),
                &CancellationToken::new()
            ),
            Err(CandidateError::InputTooLarge)
        ));
    }
}

#[test]
fn five_stdin_protocols_and_quoted_arguments_never_use_a_shell() {
    use rustpy_format::candidate::{formatter_arguments, parse_arguments};
    use std::path::Path;
    for (tool, expected) in [
        ("black", vec!["-q", "-"]),
        ("ruff", vec!["format", "-"]),
        ("isort", vec!["--stdout", "-"]),
        ("autopep8", vec!["-"]),
        ("yapf", vec![]),
    ] {
        assert_eq!(formatter_arguments(tool, None).unwrap(), expected);
    }
    assert_eq!(
        formatter_arguments("isort", Some(Path::new("folder with spaces/m.py"))).unwrap(),
        ["--stdout", "-", "--filename", "folder with spaces/m.py"]
    );
    assert_eq!(
        parse_arguments(r#"--config "C:\folder with spaces\style.toml" --line-length 88 ''"#)
            .unwrap(),
        [
            "--config",
            r"C:\folder with spaces\style.toml",
            "--line-length",
            "88",
            ""
        ]
    );
    assert!(parse_arguments("--config 'unfinished").is_err());
    assert!(parse_arguments(&"a".repeat(65537)).is_err());
}

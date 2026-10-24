//! What `rustpy [path …]` does about an IDE that already runs for the same
//! configuration directory (see [`crate::ipc`]): forward the request to it, or
//! start the IDE here and serve later requests.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::args::{GlobalOptions, OpenArgs, OpenTarget};
use crate::ipc::{self, ForwardError, Forwarded, Probe, Server};

/// How long a launcher tries to reach an IDE that holds the lock.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `--wait` waits for an IDE it started to listen.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);

/// The outcome of [`launch`].
#[derive(Debug)]
pub enum Launch {
    /// The running IDE took the request: exit with this status.
    Done(i32),
    /// Start the IDE in this process; it serves later requests when a server is given.
    Start(Option<Server>),
}

/// Whether this launch takes part in single-instance handling. Smoke tests
/// (`--screenshot`, `--input-script`) do not, unless `RUSTPY_SINGLE_INSTANCE`
/// is set; `--new-window` and `RUSTPY_NO_SINGLE_INSTANCE` turn it off.
pub fn single_instance_enabled(args: &OpenArgs) -> bool {
    if args.new_window || std::env::var_os("RUSTPY_NO_SINGLE_INSTANCE").is_some() {
        return false;
    }
    !args.is_smoke_test() || std::env::var_os("RUSTPY_SINGLE_INSTANCE").is_some()
}

fn finish(fw: Forwarded, wait: bool) -> i32 {
    log::info!("forwarded to the running IDE: {} opened", fw.opened.len());
    if !wait {
        return 0;
    }
    match fw.wait_closed() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rustpy: {e}");
            1
        }
    }
}

/// One asynchronous Quit phase for an already authenticated window. This is
/// transport only: discovery uncertainty, UI guards, durable checkpoint proof
/// and retry/abort policy belong to the caller's coordinator. In particular,
/// no empty live-window list is interpreted here as permission to restore.
pub fn request_quit_phase(
    window: &crate::window_registry::WindowInfo,
    transaction_id: &str,
    expected_state: ipc::QuitStateStamp,
    phase: ipc::QuitControl,
    timeout: Duration,
) -> Result<ipc::ControlResponse, ipc::ForwardError> {
    let timeout_ms = u64::try_from(timeout.as_millis())
        .ok()
        .filter(|millis| (1..=30_000).contains(millis))
        .ok_or_else(|| {
            ipc::ForwardError::Rejected("control timeout must be 1..30000 milliseconds".into())
        })?;
    let request = ipc::ControlRequest {
        protocol: ipc::PROTOCOL,
        token: window.endpoint.token.clone(),
        timeout_ms,
        expected_window: crate::window_registry::ExpectedWindow {
            id: window.id.clone(),
            project: window.project.clone(),
        },
        transaction_id: transaction_id.into(),
        expected_state,
        quit_control: phase,
    };
    ipc::control_window(window, &request, timeout)
}

/// Decides how to open `args`: forwards to the IDE running for `config_dir`,
/// or returns the server this process should run the IDE with.
pub fn launch(args: &OpenArgs, global: &GlobalOptions, config_dir: &Path) -> Launch {
    launch_inner(args, global, config_dir, &mut None)
}
/// The GUI launcher holds admission before endpoint creation. Forwarded --wait
/// commands release it after OPEN acceptance, before waiting for tab closure;
/// retaining it through --wait would prevent guarded Quit from obtaining exclusivity.
pub fn launch_admitted(
    args: &OpenArgs,
    global: &GlobalOptions,
    config_dir: &Path,
    admission: crate::window_admission::AdmissionLease,
) -> (Launch, Option<crate::window_admission::AdmissionLease>) {
    if admission.validate_identity().is_err()
        || dunce::canonicalize(config_dir).ok().as_deref() != Some(admission.configuration())
    {
        eprintln!("rustpy: startup admission does not match this configuration");
        return (Launch::Done(1), None);
    }
    let mut admission = Some(admission);
    let launch = launch_inner(args, global, config_dir, &mut admission);
    if matches!(launch, Launch::Done(_)) {
        admission.take();
    }
    (launch, admission)
}
fn finish_admitted(
    forwarded: Forwarded,
    wait: bool,
    admission: &mut Option<crate::window_admission::AdmissionLease>,
) -> i32 {
    admission.take();
    finish(forwarded, wait)
}
fn launch_inner(
    args: &OpenArgs,
    global: &GlobalOptions,
    config_dir: &Path,
    admission: &mut Option<crate::window_admission::AdmissionLease>,
) -> Launch {
    #[cfg(target_os = "macos")]
    if args.new_window
        && !args.is_smoke_test()
        && std::env::var_os("RUSTPY_NO_SINGLE_INSTANCE").is_none()
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        return match ipc::probe(config_dir) {
            Ok(Probe::Running) => match ipc::forward_native_window(config_dir, args, &cwd) {
                Ok(forwarded) => Launch::Done(finish_admitted(forwarded, args.wait, admission)),
                Err(error) => {
                    eprintln!("rustpy: {error}");
                    Launch::Done(1)
                }
            },
            Ok(Probe::Free(lock)) if args.wait => {
                drop(lock);
                admission.take();
                Launch::Done(spawn_and_wait(
                    global,
                    args,
                    &ipc::absolute_targets(&args.targets, &cwd),
                    &cwd,
                    config_dir,
                ))
            }
            Ok(Probe::Free(lock)) => match Server::start(lock) {
                Ok(server) => Launch::Start(Some(server)),
                Err(error) => {
                    eprintln!("rustpy: {error}");
                    Launch::Done(1)
                }
            },
            Err(error) => {
                eprintln!("rustpy: {error}");
                Launch::Done(1)
            }
        };
    }
    if args.new_window {
        return match Server::start_secondary(config_dir) {
            Ok(server) => Launch::Start(Some(server)),
            Err(error) => {
                eprintln!("rustpy: cannot register new window: {error}");
                Launch::Done(1)
            }
        };
    }
    if !single_instance_enabled(args) {
        if args.wait && !args.new_window {
            eprintln!("rustpy: --wait has no effect in a test run");
        }
        return Launch::Start(None);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let targets = ipc::absolute_targets(&args.targets, &cwd);
    for target in &targets {
        match crate::window_registry::find_project(config_dir, &target.path) {
            Ok(Some(window)) => {
                return match ipc::forward_window(&window, &targets, args.wait, Some(&cwd)) {
                    Ok(forwarded) => Launch::Done(finish_admitted(forwarded, args.wait, admission)),
                    Err(error) => {
                        eprintln!("rustpy: {error}");
                        Launch::Done(1)
                    }
                }
            }
            Ok(None) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("rustpy: window discovery failed: {error}");
                return Launch::Done(1);
            }
        }
    }
    match ipc::probe(config_dir) {
        Err(e) => {
            log::warn!(
                "single instance: cannot lock {}: {e}; starting a separate window",
                config_dir.display()
            );
            Launch::Start(None)
        }
        Ok(Probe::Running) => {
            match ipc::forward(config_dir, &targets, args.wait, Some(&cwd), FORWARD_TIMEOUT) {
                Ok(fw) => Launch::Done(finish_admitted(fw, args.wait, admission)),
                Err(e) => {
                    eprintln!("rustpy: {e}; opening a separate window");
                    log::warn!("single instance: {e}");
                    Launch::Start(None)
                }
            }
        }
        Ok(Probe::Free(lock)) if args.wait => {
            // The IDE must outlive this command: start it on its own, then wait
            // for the files like any other launcher.
            drop(lock);
            admission.take();
            Launch::Done(spawn_and_wait(global, args, &targets, &cwd, config_dir))
        }
        Ok(Probe::Free(lock)) => match Server::start(lock) {
            Ok(server) => Launch::Start(Some(server)),
            Err(e) => {
                log::warn!("single instance: cannot listen: {e}");
                Launch::Start(None)
            }
        },
    }
}

/// The arguments that open `targets` in a new IDE process.
pub fn ide_arguments(
    global: &GlobalOptions,
    args: &OpenArgs,
    targets: &[OpenTarget],
) -> Vec<String> {
    let mut out = Vec::new();
    if args.new_window {
        out.push("--new-window".into());
    }
    if global.verbose {
        out.push("--verbose".to_string());
    }
    if args.dont_reopen_projects {
        out.push("dontReopenProjects".to_string());
    }
    for t in targets {
        if let Some(line) = t.line {
            out.push("--line".into());
            out.push(line.to_string());
            if let Some(col) = t.column {
                out.push("--column".into());
                out.push(col.to_string());
            }
        }
        let path = t.path.to_string_lossy().into_owned();
        // A relative path must not look like an option.
        out.push(if path.starts_with('-') {
            format!(".{}{path}", std::path::MAIN_SEPARATOR)
        } else {
            path
        });
    }
    out
}

/// Starts the IDE detached from this terminal (its own process group, no
/// standard streams), so it keeps running after the launcher exits.
pub fn spawn_detached(
    program: &Path,
    args: &[String],
    cwd: &Path,
) -> std::io::Result<std::process::Child> {
    spawn_detached_with_log(program, args, cwd, &rustpy_settings::Settings::log_dir())
}

fn spawn_detached_with_log(
    program: &Path,
    args: &[String],
    cwd: &Path,
    log_dir: &Path,
) -> std::io::Result<std::process::Child> {
    // A real anonymous pipe is closed immediately after spawning. Output goes
    // to an owned append log, so a detached child can outlive its launcher.
    std::fs::create_dir_all(log_dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let log = options.open(log_dir.join("detached-launcher.log"))?;
    if !log.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "detached output log must be a regular file",
        ));
    }
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd.spawn()?;
    drop(child.stdin.take());
    Ok(child)
}

fn spawn_and_wait(
    global: &GlobalOptions,
    args: &OpenArgs,
    targets: &[OpenTarget],
    cwd: &Path,
    config_dir: &Path,
) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("rustpy: cannot find the RustPy executable: {e}");
            return 1;
        }
    };
    let mut child = match spawn_detached(&exe, &ide_arguments(global, args, targets), cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rustpy: cannot start RustPy: {e}");
            return 1;
        }
    };
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut exited_ok = false;
    loop {
        match ipc::forward(
            config_dir,
            targets,
            true,
            Some(cwd),
            Duration::from_millis(500),
        ) {
            Ok(fw) => return finish(fw, true),
            Err(ForwardError::Rejected(m)) => {
                eprintln!("rustpy: the IDE refused the request: {m}");
                return 1;
            }
            Err(ForwardError::Unreachable(_)) => {}
        }
        if !exited_ok {
            match child.try_wait() {
                // It handed the files to another IDE that started meanwhile: wait for that one.
                Ok(Some(status)) if status.success() => exited_ok = true,
                Ok(Some(status)) => {
                    eprintln!(
                        "rustpy: RustPy exited before opening the files ({status}); see {}",
                        rustpy_settings::Settings::log_dir()
                            .join("rustpy.log")
                            .display()
                    );
                    return 1;
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            eprintln!(
                "rustpy: RustPy did not start within {} seconds",
                STARTUP_TIMEOUT.as_secs()
            );
            return 1;
        }
    }
}

#[cfg(test)]
mod quit_control_tests {
    use super::*;
    #[test]
    fn transport_hook_preserves_full_welcome_expectation_and_requires_finite_budget() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_secondary(dir.path()).unwrap();
        let window = crate::window_registry::live_windows(dir.path())
            .unwrap()
            .pop()
            .unwrap();
        let state = ipc::QuitStateStamp {
            owner: "logical-owner".into(),
            generation: 0,
            owner_revision: 0,
        };
        for timeout in [Duration::ZERO, Duration::from_secs(31)] {
            assert!(matches!(
                request_quit_phase(
                    &window,
                    "tx",
                    state.clone(),
                    ipc::QuitControl::Prepare,
                    timeout
                ),
                Err(ipc::ForwardError::Rejected(_))
            ));
            assert!(server.try_recv_control().is_none());
        }
        let client = std::thread::spawn(move || {
            request_quit_phase(
                &window,
                "tx",
                state,
                ipc::QuitControl::Prepare,
                Duration::from_secs(2),
            )
            .unwrap()
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(incoming.request.expected_window.id, server.window_id());
        assert_eq!(incoming.request.expected_window.project, None);
        assert!((1..=2000).contains(&incoming.request.timeout_ms));
        incoming.respond(ipc::ControlStatus::Pending).unwrap();
        assert_eq!(client.join().unwrap().status, ipc::ControlStatus::Pending);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn forwarded_wait_releases_admission_before_tab_closure() {
        use crate::window_admission::WindowAdmission;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_path_buf();
        let server = match ipc::probe(&config).unwrap() {
            Probe::Free(lock) => Server::start(lock).unwrap(),
            _ => panic!("fresh configuration must be free"),
        };
        let admission = WindowAdmission::new(&config)
            .unwrap()
            .acquire_shared(Instant::now() + Duration::from_secs(2))
            .unwrap();
        let args = OpenArgs {
            wait: true,
            ..Default::default()
        };
        let client = std::thread::spawn(move || {
            launch_admitted(&args, &GlobalOptions::default(), &config, admission)
        });
        let incoming = server.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(incoming.wait);
        let waiter = incoming.accept(Vec::new()).unwrap();
        let exclusive = WindowAdmission::new(dir.path())
            .unwrap()
            .acquire_quit(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert!(!client.is_finished(), "--wait returned before tab closure");
        drop(exclusive);
        waiter.finish();
        let (result, remaining) = client.join().unwrap();
        assert!(matches!(result, Launch::Done(0)));
        assert!(remaining.is_none());
    }

    use super::*;

    #[test]
    fn smoke_tests_and_new_windows_skip_single_instance() {
        let plain = OpenArgs::default();
        let smoke = OpenArgs {
            screenshot: Some("/tmp/s.png".into()),
            ..Default::default()
        };
        let new_window = OpenArgs {
            new_window: true,
            ..Default::default()
        };
        if std::env::var_os("RUSTPY_NO_SINGLE_INSTANCE").is_none() {
            assert!(single_instance_enabled(&plain));
        }
        if std::env::var_os("RUSTPY_SINGLE_INSTANCE").is_none() {
            assert!(!single_instance_enabled(&smoke));
        }
        assert!(!single_instance_enabled(&new_window));
    }

    #[cfg(unix)]
    #[test]
    fn detached_spawn_uses_closed_pipe_and_owned_append_log() {
        let directory = tempfile::tempdir().unwrap();
        let arguments = vec![
            "-c".into(),
            "if read line; then exit 9; fi; printf eof; printf stderr >&2".into(),
        ];
        for _ in 0..2 {
            let mut child = spawn_detached_with_log(
                Path::new("/bin/sh"),
                &arguments,
                directory.path(),
                directory.path(),
            )
            .unwrap();
            assert!(child.stdin.is_none());
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(
            std::fs::read(directory.path().join("detached-launcher.log")).unwrap(),
            b"eofstderreofstderr"
        );
    }

    #[cfg(unix)]
    #[test]
    // The enclosing test reaps this launcher, then releases the bounded child.
    // Waiting here would invalidate the child-outlives-launcher regression.
    #[allow(clippy::zombie_processes)]
    fn detached_parent_helper() {
        let Some(directory) = std::env::var_os("RUSTPY_DETACHED_PARENT_DIRECTORY") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        let script = r#"n=0; while [ ! -f "$1/release" ]; do n=$((n+1)); [ "$n" -lt 500 ] || exit 8; sleep 0.01; done; printf after-parent; printf stderr-after >&2; printf done > "$1/done""#;
        let arguments = vec![
            "-c".into(),
            script.into(),
            "child".into(),
            directory.to_string_lossy().into_owned(),
        ];
        let child =
            spawn_detached_with_log(Path::new("/bin/sh"), &arguments, &directory, &directory)
                .unwrap();
        std::fs::write(directory.join("spawned"), child.id().to_string()).unwrap();
        // Intentionally exit without waiting: the child owns its output handles.
    }
    #[cfg(unix)]
    #[test]
    fn detached_output_survives_actual_launcher_process_exit() {
        let directory = tempfile::tempdir().unwrap();
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::detached_parent_helper",
                "--nocapture",
            ])
            .env("RUSTPY_DETACHED_PARENT_DIRECTORY", directory.path())
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(launcher.stdin.take());
        let output = launcher.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(directory.path().join("spawned").exists());
        assert!(!directory.path().join("done").exists());
        std::fs::write(directory.path().join("release"), b"parent exited").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("done").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read(directory.path().join("detached-launcher.log")).unwrap(),
            b"after-parentstderr-after"
        );
    }

    #[test]
    fn arguments_for_a_new_ide() {
        let targets = vec![
            OpenTarget {
                path: "/p/a.py".into(),
                line: Some(3),
                column: Some(4),
            },
            OpenTarget::new("/p/-odd"),
            OpenTarget {
                path: "/p/b.py".into(),
                line: Some(9),
                column: None,
            },
        ];
        let global = GlobalOptions {
            verbose: true,
            ..Default::default()
        };
        let args = OpenArgs {
            dont_reopen_projects: true,
            ..Default::default()
        };
        let a = ide_arguments(&global, &args, &targets);
        assert_eq!(
            a,
            [
                "--verbose",
                "dontReopenProjects",
                "--line",
                "3",
                "--column",
                "4",
                "/p/a.py",
                "/p/-odd",
                "--line",
                "9",
                "/p/b.py"
            ]
        );
        let relative = ide_arguments(
            &GlobalOptions::default(),
            &OpenArgs::default(),
            &[OpenTarget::new("-x.py")],
        );
        assert_eq!(relative, [format!(".{}-x.py", std::path::MAIN_SEPARATOR)]);
        // Parsing them back gives the same targets.
        let cli = crate::args::parse(a).unwrap();
        let crate::args::Command::Open(o) = cli.command else {
            panic!()
        };
        assert_eq!(o.targets, targets);
        assert!(o.dont_reopen_projects && cli.global.verbose);
    }

    #[test]
    fn forwards_to_a_running_ide() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = ipc::probe(dir.path()).unwrap() else {
            panic!()
        };
        let server = Server::start(lock).unwrap();
        let ide = std::thread::spawn(move || {
            let incoming = server.recv_timeout(Duration::from_secs(10)).unwrap();
            let opened: Vec<_> = incoming.targets.iter().map(|t| t.path.clone()).collect();
            incoming.accept(opened);
            server
        });
        let args = OpenArgs {
            targets: vec![OpenTarget::new(dir.path().join("x.py"))],
            ..Default::default()
        };
        if single_instance_enabled(&args) {
            assert!(matches!(
                launch(&args, &GlobalOptions::default(), dir.path()),
                Launch::Done(0)
            ));
        } else {
            // RUSTPY_NO_SINGLE_INSTANCE is set: answer the IDE thread ourselves.
            ipc::forward(
                dir.path(),
                &args.targets,
                false,
                None,
                Duration::from_secs(5),
            )
            .unwrap();
        }
        drop(ide.join().unwrap());
        // With the IDE gone, the next launch becomes the IDE.
        if single_instance_enabled(&args) {
            match launch(&args, &GlobalOptions::default(), dir.path()) {
                Launch::Start(Some(server)) => assert!(server.port() > 0),
                other => panic!("{other:?}"),
            }
        }
    }
}

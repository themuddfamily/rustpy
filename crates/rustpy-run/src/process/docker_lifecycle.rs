//! Daemon ownership is separate from the local Docker CLI process tree.
use super::*;
use std::ffi::OsString;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LABEL: &str = "rustpy.run.owner";
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Default)]
pub(super) struct Local {
    exit: Mutex<Option<SessionStatus>>,
    diagnostics: Mutex<String>,
}
impl Local {
    pub(super) fn finish(&self, status: SessionStatus) {
        if let Ok(mut exit) = self.exit.lock() {
            *exit = Some(status);
        }
    }
    fn refused(&self) -> bool {
        self.exit
            .lock()
            .ok()
            .and_then(|exit| {
                exit.as_ref().map(|status| {
                    self.diagnostics
                        .lock()
                        .map(|text| definite_refusal(status, &text))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }
}
pub(super) struct DiagnosticReader<R>(pub R, pub Arc<Local>);
impl<R: Read> Read for DiagnosticReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.0.read(buffer)?;
        if let Ok(mut text) = self.1.diagnostics.lock() {
            let remaining = 16384usize.saturating_sub(text.len());
            text.extend(
                String::from_utf8_lossy(&buffer[..count])
                    .chars()
                    .take(remaining),
            );
        }
        Ok(count)
    }
}
pub(super) struct Prepared {
    program: PathBuf,
    prefix: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    directory: PathBuf,
    cid: PathBuf,
    token: String,
}
pub(super) struct Owner {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) retired: Arc<AtomicBool>,
    pub(super) detail: Arc<Mutex<String>>,
    pub(super) unresolved: Arc<AtomicBool>,
}
impl Owner {
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub(super) fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
    pub(super) fn stamp(&self) -> String {
        self.detail
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "Docker ownership state poisoned".into())
    }
    pub(super) fn unresolved(&self) -> bool {
        self.unresolved.load(Ordering::Acquire)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) fn prepare(
    cfg: &RunConfig,
    program: &Path,
    args: &mut Vec<String>,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<Prepared>, String> {
    if !cfg.type_options.as_ref().is_some_and(|spec| {
        spec.kind == rustpy_project::configuration_types::ConfigurationType::Docker
    }) {
        return Ok(None);
    }
    let run = run_position(args)?;
    if args[run + 1..].iter().any(|arg| {
        arg == "--cidfile"
            || arg.starts_with("--cidfile=")
            || arg.starts_with(&format!("{LABEL}="))
            || arg.starts_with(&format!("--label={LABEL}="))
            || arg.starts_with(&format!("-l{LABEL}="))
    }) {
        return Err(
            "Typed Docker launches reserve their CID file and rustpy.run.owner label".into(),
        );
    }
    let program = resolve_program(program);
    if !program.is_absolute() {
        return Err("Cannot resolve the typed Docker CLI".into());
    }
    let token = format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let directory = std::env::temp_dir().join(format!("rustpy-docker-owner-{token}"));
    std::fs::create_dir(&directory)
        .map_err(|e| format!("Cannot create Docker ownership directory: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        {
            let _ = std::fs::remove_dir(&directory);
            return Err(error.to_string());
        }
    }
    let cid = directory.join("container.cid");
    let prefix = args[..run].to_vec();
    // The lifecycle owner, rather than docker run --rm, removes the container.
    // This preserves both stopped containers and their CID until reconciled.
    if args.get(run + 1).is_some_and(|arg| arg == "--rm") {
        args.remove(run + 1);
    }
    args.splice(
        run + 1..run + 1,
        [
            "--cidfile".into(),
            cid.to_string_lossy().into_owned(),
            "--label".into(),
            format!("{LABEL}={token}"),
        ],
    );
    let mut environment = if cfg.workflow.inherit_environment {
        std::env::vars_os().collect()
    } else {
        BTreeMap::new()
    };
    environment.extend(
        env.iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    Ok(Some(Prepared {
        program,
        prefix,
        cwd: cwd.to_path_buf(),
        environment,
        directory,
        cid,
        token,
    }))
}
fn run_position(args: &[String]) -> Result<usize, String> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "run" => return Ok(index),
            "--host" | "-H" | "--context" | "--config" | "--tlscacert" | "--tlscert"
            | "--tlskey" | "--log-level" => {
                if args.get(index + 1).is_none() {
                    return Err("Incomplete Docker connection option".into());
                }
                index += 2;
            }
            "--tls" | "--tlsverify" | "--debug" | "-D" => index += 1,
            _ if [
                "--host=",
                "--context=",
                "--config=",
                "--tlscacert=",
                "--tlscert=",
                "--tlskey=",
                "--log-level=",
            ]
            .iter()
            .any(|prefix| arg.starts_with(prefix)) =>
            {
                index += 1
            }
            _ => {
                return Err("Typed Docker ownership requires a captured docker run command".into());
            }
        }
    }
    Err("Typed Docker run command is missing".into())
}
fn full_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn definite_refusal(status: &SessionStatus, diagnostics: &str) -> bool {
    if *status != SessionStatus::Exited(125) {
        return false;
    }
    // These CLI/daemon refusals occur before create. Transport loss, signals,
    // and arbitrary nonzero application exits are deliberately not evidence.
    diagnostics.lines().map(str::trim).any(|line| {
        line.starts_with("unknown flag:")
            || line.starts_with("unknown shorthand flag:")
            || line.starts_with("docker: Error response from daemon: No such image:")
    })
}
impl Prepared {
    fn command(&self, args: &[String]) -> Result<String, String> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix)
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null());
        let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
            &mut command,
            &rustpy_core::CancellationToken::new(),
            Duration::from_secs(5),
            262144,
        )
        .map_err(|e| format!("Docker retirement transport: {e:#}"))?;
        if !status.success() {
            return Err(format!(
                "Docker retirement {status}: {}",
                String::from_utf8_lossy(&err)
                    .chars()
                    .take(2048)
                    .collect::<String>()
            ));
        }
        String::from_utf8(out).map_err(|_| "Docker retirement returned non-UTF8 output".into())
    }
    fn listed(&self, filter: String) -> Result<Vec<String>, String> {
        let output = self.command(&[
            "ps".into(),
            "-aq".into(),
            "--no-trunc".into(),
            "--filter".into(),
            filter,
        ])?;
        output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let id = line.trim();
                if full_id(id) {
                    Ok(id.to_owned())
                } else {
                    Err("Docker returned an invalid owned container ID".into())
                }
            })
            .collect()
    }
    fn remove_owned(&self, id: &str) -> Result<(), String> {
        // An ID from a CID file alone is not authority: verify the reserved
        // label on the exact full ID before any daemon mutation.
        let format = format!("{{{{.Id}}}} {{{{index .Config.Labels \"{LABEL}\"}}}}");
        match self.command(&["inspect".into(), "--format".into(), format, id.into()]) {
            Ok(value) => {
                let expected = format!("{id} {}", self.token);
                if value.trim() != expected {
                    return Err("Docker container ownership label changed; removal refused".into());
                }
                self.command(&["rm".into(), "-f".into(), id.into()])?;
            }
            Err(error) => {
                if !self.listed(format!("id={id}"))?.is_empty() {
                    return Err(error);
                }
            }
        }
        if !self.listed(format!("id={id}"))?.is_empty() {
            return Err("Owned Docker container has not physically retired".into());
        }
        Ok(())
    }
    pub(super) fn start(
        self,
        local_done: Arc<AtomicBool>,
        local: Arc<Local>,
        launched: bool,
        tree: Option<Arc<ProcessTree>>,
    ) -> Owner {
        let stop = Arc::new(AtomicBool::new(false));
        let retired = Arc::new(AtomicBool::new(false));
        let detail = Arc::new(Mutex::new(format!(
            "Docker owner {} awaiting CLI retirement",
            self.token
        )));
        let unresolved = Arc::new(AtomicBool::new(false));
        let owner = Owner {
            stop: stop.clone(),
            retired: retired.clone(),
            detail: detail.clone(),
            unresolved: unresolved.clone(),
        };
        rustpy_core::TaskManager::global().task(format!("Docker container ownership · {}", self.token)).cancellable(false).spawn(move |progress| {
            let mut known = std::collections::BTreeSet::new();
            let mut failures = 0u32;
            let mut uncertain_since = None;
            loop {
                if !launched {
                    retired.store(true, Ordering::Release); break;
                }
                // Observe the CID while the CLI is still alive, including a
                // process which creates and finishes before the UI next polls.
                if let Ok(value) = std::fs::read_to_string(&self.cid) {
                    if full_id(value.trim()) { known.insert(value.trim().to_owned()); }
                }
                if !stop.load(Ordering::Acquire) && !local_done.load(Ordering::Acquire) { std::thread::sleep(Duration::from_millis(25)); continue; }
                let attempt = (|| -> Result<bool, String> {
                    match std::fs::read_to_string(&self.cid) {
                        Ok(value) if full_id(value.trim()) => { known.insert(value.trim().to_owned()); }
                        Ok(value) if !value.trim().is_empty() => return Err("Invalid Docker CID file; ownership unresolved".into()),
                        _ => {},
                    }
                    known.extend(self.listed(format!("label={LABEL}={}", self.token))?);
                    if known.is_empty() {
                        // Keep the CLI alive on Stop: daemon creation may still
                        // be in flight. An empty list is not retirement proof.
                        let refused = local_done.load(Ordering::Acquire) && local.refused();
                        if refused {
                            // A definite pre-create refusal plus this reachable
                            // daemon's successful owner query proves no create.
                            return Ok(true);
                        }
                        return Err("Docker creation/CID publication unresolved; cleanup authority retained".into());
                    }
                    for id in &known { self.remove_owned(id)?; }
                    Ok(self.listed(format!("label={LABEL}={}", self.token))?.is_empty())
                })();
                match attempt {
                    Ok(true) => {
                        if let Some(tree) = &tree { tree.request_force(); }
                        retired.store(true, Ordering::Release);
                        break;
                    }
                    Ok(false) => progress.set_text("Waiting for owned Docker container removal"),
                    Err(error) => {
                        failures = failures.saturating_add(1);
                        if uncertain_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(15) { unresolved.store(true, Ordering::Release); }
                        progress.set_text(&error);
                        if let Ok(mut state) = detail.lock() { *state = error; }
                    }
                }
                // Keep unresolved authority without a permanent tight poll.
                std::thread::sleep(Duration::from_secs(u64::from(failures.clamp(1, 30))));
            }
            let _ = std::fs::remove_file(&self.cid);
            let _ = std::fs::remove_dir(&self.directory);
            if let Ok(mut state) = detail.lock() { *state = "Docker ownership reconciled; no owned daemon container remains".into(); }
            Ok(())
        });
        owner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_failed_status_does_not_release_unresolved_daemon_or_local_ownership() {
        let mut session =
            RunSession::failed_preparation(1, "Docker", "unresolved transport".into());
        let retired = Arc::new(AtomicBool::new(false));
        session.docker_owner = Some(Owner {
            stop: Arc::new(AtomicBool::new(false)),
            retired: retired.clone(),
            detail: Arc::new(Mutex::new("unresolved".into())),
            unresolved: Arc::new(AtomicBool::new(true)),
        });
        assert!(!session.is_running());
        assert!(session.owns_process());
        session.local_retired.store(false, Ordering::Release);
        retired.store(true, Ordering::Release);
        assert!(
            session.owns_process(),
            "local CLI retirement is an independent barrier"
        );
        session.local_retired.store(true, Ordering::Release);
        session.readers.store(1, Ordering::Release);
        assert!(session.owns_process(), "owned pipe drain must also finish");
        session.readers.store(0, Ordering::Release);
        assert!(!session.owns_process());
        assert!(!session.is_running());
    }
    #[test]
    fn only_definite_pre_create_refusals_can_finish_without_a_container_identity() {
        assert!(definite_refusal(
            &SessionStatus::Exited(125),
            "docker: Error response from daemon: No such image: private:missing\n"
        ));
        assert!(definite_refusal(
            &SessionStatus::Exited(125),
            "unknown flag: --invalid\nSee 'docker run --help'."
        ));
        assert!(!definite_refusal(
            &SessionStatus::Exited(125),
            "docker: error during connect: EOF"
        ));
        assert!(!definite_refusal(
            &SessionStatus::Killed,
            "unknown flag: --invalid"
        ));
        assert!(!definite_refusal(
            &SessionStatus::Exited(1),
            "unknown flag: --invalid"
        ));
    }
    #[test]
    fn ordinary_programs_keep_their_arguments_and_do_not_acquire_daemon_ownership() {
        let cfg = RunConfig::default();
        let mut args = vec!["run".into(), "--cidfile=user-file".into()];
        let original = args.clone();
        assert!(prepare(
            &cfg,
            Path::new("ordinary-program"),
            &mut args,
            Path::new("."),
            &BTreeMap::new()
        )
        .unwrap()
        .is_none());
        assert_eq!(args, original);
    }
    #[test]
    fn typed_docker_cannot_replace_the_private_cid_or_owner_label() {
        let cfg = RunConfig {
            type_options: Some(rustpy_project::configuration_types::ConfigurationSpec::new(
                rustpy_project::configuration_types::ConfigurationType::Docker,
            )),
            ..RunConfig::default()
        };
        for option in [
            vec!["--cidfile", "foreign"],
            vec!["--cidfile=foreign"],
            vec!["--label", "rustpy.run.owner=foreign"],
            vec!["--label=rustpy.run.owner=foreign"],
            vec!["-lrustpy.run.owner=foreign"],
        ] {
            let mut args: Vec<String> = std::iter::once("run")
                .chain(option)
                .chain(std::iter::once("image"))
                .map(str::to_owned)
                .collect();
            let original = args.clone();
            assert!(prepare(
                &cfg,
                Path::new("docker"),
                &mut args,
                Path::new("."),
                &BTreeMap::new()
            )
            .is_err());
            assert_eq!(args, original);
        }
    }
    #[test]
    fn connection_option_values_do_not_become_the_run_subcommand() {
        assert_eq!(
            run_position(&[
                "--context".into(),
                "run".into(),
                "run".into(),
                "image".into()
            ])
            .unwrap(),
            2
        );
        assert!(run_position(&["--context".into()]).is_err());
        assert!(!full_id("user-container-name"));
        assert!(!full_id(&"a".repeat(12)));
        assert!(full_id(&"a".repeat(64)));
    }
}

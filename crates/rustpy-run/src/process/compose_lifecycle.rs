//! Explicit isolated Compose sessions. Creation completes before start/Stop can
//! retire the project. Startup completes before read-only log following.
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
const PROJECT: &str = "com.docker.compose.project";
struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn path(&self) -> &Path {
        &self.0
    }
    fn new(project: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("rustpy-compose-owner-{project}"));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            {
                let _ = std::fs::remove_dir(&path);
                return Err(error.to_string());
            }
        }
        Ok(Self(path))
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("effective-compose.json"));
        let _ = std::fs::remove_dir(&self.0);
    }
}
struct Plan {
    program: PathBuf,
    cwd: PathBuf,
    project_dir: PathBuf,
    environment: BTreeMap<String, String>,
    project: String,
    host: String,
    daemon: String,
    named_volumes: std::collections::BTreeSet<String>,
    anonymous_volumes: Mutex<std::collections::BTreeSet<String>>,
    file: PathBuf,
    service: String,
    services: Vec<String>,
    options: UpOptions,
    directory: PrivateDirectory,
}
#[derive(Clone, Default)]
struct UpOptions {
    global: Vec<String>,
    create: Vec<String>,
    exit_from: Option<String>,
    abort_exit: bool,
    abort_failure: bool,
    no_build: bool,
}
fn validate(cfg: &RunConfig, args: &[String]) -> Result<UpOptions, String> {
    let spec = cfg
        .type_options
        .as_ref()
        .ok_or("Missing Compose configuration")?;
    if cfg.emulate_terminal || cfg.run_with_console || cfg.redirect_input.is_some() {
        return Err("Isolated Compose currently supports streamed output without redirected input or terminal emulation".into());
    }
    if spec.service.starts_with('-') || !cfg.parameters.trim().is_empty() {
        return Err("Put isolated Compose options in structured arguments".into());
    }
    let mut expected = vec![
        "compose".into(),
        "-f".into(),
        spec.target.clone(),
        "up".into(),
    ];
    expected.extend(spec.arguments.clone());
    if !spec.service.is_empty() {
        expected.push(spec.service.clone());
    }
    if args != expected {
        return Err("Isolated Compose command changed after materialization".into());
    }
    let mut options = UpOptions::default();
    let mut index = 0;
    while index < spec.arguments.len() {
        let flag = &spec.arguments[index];
        match flag.as_str() {
            "--no-build" => {
                options.no_build = true;
                options.create.push(flag.clone());
                index += 1;
            }
            "--force-recreate" | "--no-recreate" | "--remove-orphans" | "--quiet-pull" => {
                options.create.push(flag.clone());
                index += 1;
            }
            "--abort-on-container-exit" => {
                options.abort_exit = true;
                index += 1;
            }
            "--abort-on-container-failure" => {
                options.abort_failure = true;
                index += 1;
            }
            "--pull" | "--scale" | "--exit-code-from" | "--profile" | "--env-file" => {
                let value = spec
                    .arguments
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| format!("Missing {flag} value"))?;
                match flag.as_str() {
                    "--pull" if !["always", "missing", "never"].contains(&value.as_str()) => {
                        return Err("Choose pull always, missing or never".into());
                    }
                    "--scale" => {
                        let (name, count) = value
                            .split_once('=')
                            .ok_or("Scale requires SERVICE=COUNT")?;
                        if name.is_empty() || count.parse::<u32>().is_err() {
                            return Err("Invalid Compose scale".into());
                        }
                    }
                    "--exit-code-from" => {
                        options.exit_from = Some(value.clone());
                        options.abort_exit = true;
                    }
                    _ => {}
                }
                if flag == "--profile" || flag == "--env-file" {
                    options.global.extend([flag.clone(), value.clone()]);
                } else if flag != "--exit-code-from" {
                    options.create.extend([flag.clone(), value.clone()]);
                }
                index += 2;
            }
            "--build" => {
                return Err(
                    "Isolated Compose image builds require a separately capped build configuration"
                        .into(),
                );
            }
            other => return Err(format!("Unsupported isolated Compose up option: {other}")),
        }
    }
    if options.create.iter().any(|flag| flag == "--force-recreate")
        && options.create.iter().any(|flag| flag == "--no-recreate")
    {
        return Err("Choose force-recreate or no-recreate".into());
    }
    if options.abort_exit && options.abort_failure {
        return Err("Choose one Compose abort policy".into());
    }
    Ok(options)
}
impl Plan {
    fn spec(&self, args: &[String], compose: bool) -> Command {
        let mut command = Command::new(&self.program);
        command.args(["--host", &self.host]);
        if compose {
            command
                .arg("compose")
                .args(&self.options.global)
                .arg("--project-directory")
                .arg(&self.project_dir)
                .args(["--project-name", &self.project, "--file"])
                .arg(&self.file);
        }
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null());
        command
    }
    fn verify_daemon(&self) -> Result<(), String> {
        if !self.daemon.is_empty() {
            let identity =
                self.raw_query(&["info".into(), "--format".into(), "{{.ID}}".into()], false)?;
            if identity.trim() != self.daemon {
                return Err(
                    "Captured Docker daemon identity changed; retirement authority retained".into(),
                );
            }
        }
        Ok(())
    }
    fn query(&self, args: &[String], compose: bool) -> Result<String, String> {
        self.verify_daemon()?;
        self.raw_query(args, compose)
    }
    fn raw_query(&self, args: &[String], compose: bool) -> Result<String, String> {
        let mut command = self.spec(args, compose);
        let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
            &mut command,
            &rustpy_core::CancellationToken::new(),
            Duration::from_secs(20),
            2 * 1024 * 1024,
        )
        .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!(
                "Compose ownership transport {status}: {}",
                String::from_utf8_lossy(&err)
                    .chars()
                    .take(2048)
                    .collect::<String>()
            ));
        }
        String::from_utf8(out).map_err(|_| "Non-UTF8 Compose response".into())
    }
    fn inventory(&self, kind: &str) -> Result<Vec<String>, String> {
        let args = match kind {
            "container" => vec!["ps", "-aq", "--no-trunc", "--filter"],
            "network" => vec!["network", "ls", "-q", "--no-trunc", "--filter"],
            _ => vec!["volume", "ls", "-q", "--filter"],
        };
        let args = args
            .into_iter()
            .map(str::to_owned)
            .chain(std::iter::once(format!("label={PROJECT}={}", self.project)))
            .collect::<Vec<_>>();
        self.query(&args, false)?
            .lines()
            .filter(|v| !v.is_empty())
            .map(|id| {
                if id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                {
                    Ok(id.to_owned())
                } else {
                    Err("Invalid owned resource identity".into())
                }
            })
            .collect()
    }
    fn anonymous_present(&self, name: &str) -> Result<bool, String> {
        Ok(self
            .query(
                &[
                    "volume".into(),
                    "ls".into(),
                    "-q".into(),
                    "--filter".into(),
                    format!("name={name}"),
                ],
                false,
            )?
            .lines()
            .any(|line| line == name))
    }
    fn absent(&self) -> Result<bool, String> {
        for kind in ["container", "network", "volume"] {
            if !self.inventory(kind)?.is_empty() {
                return Ok(false);
            }
        }
        for name in self
            .anonymous_volumes
            .lock()
            .map_err(|_| "Anonymous ownership poisoned")?
            .iter()
        {
            if self.anonymous_present(name)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn cleanup(&self) -> Result<(), String> {
        for kind in ["container", "network", "volume"] {
            for id in self.inventory(kind)? {
                let template = if kind == "container" {
                    format!("{{{{index .Config.Labels \"{PROJECT}\"}}}}")
                } else {
                    format!("{{{{index .Labels \"{PROJECT}\"}}}}")
                };
                let label = self.query(
                    &[
                        kind.into(),
                        "inspect".into(),
                        "--format".into(),
                        template,
                        id.clone(),
                    ],
                    false,
                )?;
                if label.trim() != self.project {
                    return Err("Compose ownership label changed; removal refused".into());
                }
                if kind == "container" {
                    let mounts = self.query(
                        &[
                            "container".into(),
                            "inspect".into(),
                            "--format".into(),
                            "{{json .Mounts}}".into(),
                            id.clone(),
                        ],
                        false,
                    )?;
                    let mounts: Vec<serde_json::Value> =
                        serde_json::from_str(&mounts).map_err(|e| e.to_string())?;
                    for mount in mounts {
                        if mount["Type"] == "volume" {
                            let name = mount["Name"].as_str().ok_or("Volume mount name missing")?;
                            if !self.named_volumes.contains(name) {
                                if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit())
                                {
                                    return Err(
                                        "Unrecognized anonymous volume identity; removal refused"
                                            .into(),
                                    );
                                }
                                self.anonymous_volumes
                                    .lock()
                                    .map_err(|_| "Anonymous ownership poisoned")?
                                    .insert(name.into());
                            }
                        }
                    }
                }
                let mut args = vec![kind.into(), "rm".into()];
                if kind == "container" {
                    args.push("-f".into());
                }
                args.push(id);
                self.query(&args, false)?;
            }
        }
        for name in self
            .anonymous_volumes
            .lock()
            .map_err(|_| "Anonymous ownership poisoned")?
            .iter()
        {
            if self.anonymous_present(name)? {
                self.query(&["volume".into(), "rm".into(), name.clone()], false)?;
            }
        }
        if !self.absent()? {
            return Err("Owned Compose resources have not physically retired".into());
        }
        Ok(())
    }
    fn capture(&mut self) -> Result<(), String> {
        if !self.absent()? {
            return Err("Isolated Compose project already has resources; ownership refused".into());
        }
        let text = self.query(&["config".into(), "--format".into(), "json".into()], true)?;
        let mut config: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let services = config["services"]
            .as_object()
            .ok_or("Compose services missing")?;
        let mut active_profiles = std::collections::BTreeSet::new();
        if let Some(profiles) = self.environment.get("COMPOSE_PROFILES") {
            active_profiles.extend(profiles.split(',').map(str::to_owned));
        }
        for pair in self.options.global.as_chunks::<2>().0 {
            if pair[0] == "--profile" {
                active_profiles.insert(pair[1].clone());
            }
        }
        if let Some(selected) = services.get(&self.service) {
            if let Some(profiles) = selected["profiles"].as_array() {
                active_profiles.extend(
                    profiles
                        .iter()
                        .filter_map(|profile| profile.as_str().map(str::to_owned)),
                );
            }
        }
        let enabled = |service: &serde_json::Value| {
            service["profiles"].as_array().is_none_or(|profiles| {
                profiles.is_empty()
                    || profiles.iter().any(|profile| {
                        profile.as_str().is_some_and(|profile| {
                            active_profiles.contains(profile) || active_profiles.contains("*")
                        })
                    })
            })
        };
        let mut wanted = std::collections::BTreeSet::new();
        let mut pending = if self.service.is_empty() {
            services
                .iter()
                .filter(|(_, service)| enabled(service))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        } else {
            vec![self.service.clone()]
        };
        while let Some(name) = pending.pop() {
            if !wanted.insert(name.clone()) {
                continue;
            }
            let service = services
                .get(&name)
                .ok_or_else(|| format!("Compose service {name} missing"))?;
            for (dependency, required) in service_dependencies(service)? {
                if services.get(&dependency).is_some_and(enabled) {
                    pending.push(dependency);
                } else if required {
                    return Err(format!(
                        "Required Compose dependency {dependency} missing or profile inactive"
                    ));
                }
            }
        }
        if wanted.is_empty() {
            return Err("No active Compose services".into());
        }
        if self
            .options
            .exit_from
            .as_ref()
            .is_some_and(|name| !wanted.contains(name))
        {
            return Err("Exit-code service is outside the selected Compose workload".into());
        }
        for name in &wanted {
            let service = &services[name];
            if service.get("build").is_some() && !self.options.no_build {
                return Err("Compose builds need a separately capped build configuration; use no-build for existing images".into());
            }
            let image = service["image"]
                .as_str()
                .filter(|image| !image.is_empty())
                .ok_or("Select an image for each Compose service")?;
            if self
                .options
                .create
                .windows(2)
                .any(|pair| pair == ["--pull", "never"])
                || (self.options.create.iter().all(|arg| arg != "--pull")
                    && service["pull_policy"] == "never")
            {
                self.query(&["image".into(), "inspect".into(), image.into()], false)?;
            }
            if let Some(name) = service["container_name"].as_str() {
                let listed = self.query(
                    &[
                        "ps".into(),
                        "-aq".into(),
                        "--filter".into(),
                        format!("name=^/{}$", regex::escape(name)),
                    ],
                    false,
                )?;
                if !listed.trim().is_empty() {
                    return Err(
                        "Explicit Compose container name already exists; ownership refused".into(),
                    );
                }
            }
        }
        if let Some(volumes) = config["volumes"].as_object() {
            for entry in volumes.values() {
                if let Some(name) = entry["name"].as_str() {
                    self.named_volumes.insert(name.into());
                }
            }
        }
        let retained = services
            .iter()
            .filter(|(name, _)| wanted.contains(*name))
            .map(|(name, service)| (name.clone(), service.clone()))
            .collect();
        self.services = wanted.into_iter().collect();
        config["services"] = serde_json::Value::Object(retained);
        // Environment/profile files have already been resolved. Replaying them
        // would reintroduce mutable authority into the captured manifest.
        self.options.global.clear();
        // `compose config` already emits escaped dollar values suitable for
        // another Compose parse. Escaping again changes the container value.
        // Preserve that captured representation when selecting its services.
        let file = self.directory.path().join("effective-compose.json");
        std::fs::write(
            &file,
            serde_json::to_vec(&config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.file = file;
        Ok(())
    }
    fn states(&self) -> Result<Vec<(String, String, serde_json::Value)>, String> {
        self.inventory("container")?.into_iter().map(|id|{
            let value=self.query(&["container".into(),"inspect".into(),"--format".into(),"[{{json (index .Config.Labels \"com.docker.compose.service\")}},{{json .State}}]".into(),id.clone()],false)?;
            let value:serde_json::Value=serde_json::from_str(&value).map_err(|e|e.to_string())?;
            Ok((id,value[0].as_str().ok_or("Compose service label missing")?.into(),value[1].clone()))
        }).collect()
    }
    fn service_exit(&self) -> Result<Option<i32>, String> {
        let mut states = self.states()?;
        let mut finished = states
            .iter()
            .filter(|(_, _, state)| matches!(state["Status"].as_str(), Some("exited" | "dead")))
            .map(|(id, service, state)| {
                let code = state["ExitCode"]
                    .as_i64()
                    .and_then(|code| i32::try_from(code).ok())
                    .ok_or("Invalid Compose service exit code")?;
                Ok((
                    finished_key(
                        state["FinishedAt"]
                            .as_str()
                            .ok_or("Compose finish time missing")?,
                    )?,
                    id.clone(),
                    service.clone(),
                    code,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        finished.sort();
        let trigger = finished
            .iter()
            .find(|(_, _, _, code)| {
                self.options.abort_exit || (self.options.abort_failure && *code != 0)
            })
            .map(|(_, _, _, code)| *code);
        if trigger.is_some() {
            let running = states
                .iter()
                .filter(|(_, _, state)| {
                    !matches!(state["Status"].as_str(), Some("exited" | "dead"))
                })
                .map(|(id, _, _)| id.clone())
                .collect::<Vec<_>>();
            if !running.is_empty() {
                let mut args = vec![
                    "container".into(),
                    "stop".into(),
                    "--time".into(),
                    "10".into(),
                ];
                args.extend(running);
                self.query(&args, false)?;
                states = self.states()?;
            }
        }
        if states.iter().any(|(_, _, state)| {
            matches!(
                state["Status"].as_str(),
                Some("running" | "restarting" | "paused")
            )
        }) {
            return Ok(None);
        }
        if states
            .iter()
            .any(|(_, _, state)| !matches!(state["Status"].as_str(), Some("exited" | "dead")))
        {
            return Err("Compose startup ended with an unstarted service".into());
        }
        if let Some(service) = &self.options.exit_from {
            let mut selected = states
                .iter()
                .filter(|(_, name, _)| name == service)
                .map(|(id, _, state)| {
                    Ok((
                        finished_key(
                            state["FinishedAt"]
                                .as_str()
                                .ok_or("Compose finish time missing")?,
                        )?,
                        id.clone(),
                        state["ExitCode"]
                            .as_i64()
                            .and_then(|code| i32::try_from(code).ok())
                            .ok_or("Invalid Compose exit code")?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            selected.sort();
            return selected
                .first()
                .map(|(_, _, code)| Some(*code))
                .ok_or("Exit-code service has no created container".into());
        }
        Ok(Some(trigger.unwrap_or(0)))
    }
    fn follow_logs(&self, output: &Sender<OutputChunk>, stop: &AtomicBool) -> SessionStatus {
        let states = match self.states() {
            Ok(states) if !states.is_empty() => states,
            Ok(_) => return SessionStatus::Failed("Compose created no owned containers".into()),
            Err(error) => return SessionStatus::Failed(error),
        };
        let mut children = Vec::new();
        let mut failure = None;
        for (id, _, _) in states {
            if let Err(error) = self.verify_daemon() {
                failure = Some(SessionStatus::Failed(error));
                break;
            }
            // Unlike `compose logs --follow`, the Engine container log stream
            // reaches EOF after this exact container exits, including its final
            // historical frames. Each container is followed once: no replay or
            // prefix/offset guessing can duplicate or omit published output.
            children.push(RunSession::spawn_command_with_environment(
                0,
                "Compose owned container logs",
                &self.program,
                &[
                    "--host".into(),
                    self.host.clone(),
                    "container".into(),
                    "logs".into(),
                    "--follow".into(),
                    id,
                ],
                &self.cwd,
                &self.environment,
                CommandInput {
                    stdin: None,
                    inherit_environment: false,
                },
            ));
        }
        let mut observation = Instant::now();
        while children.iter().any(RunSession::owns_process) {
            for child in &mut children {
                child.poll();
                for chunk in child.output.drain(..) {
                    let _ = output.send(chunk);
                }
                child.clear_output();
                if failure.is_none()
                    && !child.owns_process()
                    && child.status != SessionStatus::Exited(0)
                {
                    failure = Some(child.status.clone());
                }
                if stop.load(Ordering::Acquire) || failure.is_some() {
                    child.stop();
                }
            }
            if !stop.load(Ordering::Acquire)
                && failure.is_none()
                && observation.elapsed() >= Duration::from_millis(100)
            {
                observation = Instant::now();
                // Applies --abort-on-* policy to the captured services. Normal
                // exit does not kill the readers: their EOF fences final logs.
                if let Err(error) = self.service_exit() {
                    failure = Some(SessionStatus::Failed(error));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for child in &mut children {
            child.poll();
            for chunk in child.output.drain(..) {
                let _ = output.send(chunk);
            }
            child.clear_output();
            if failure.is_none() && child.status != SessionStatus::Exited(0) {
                failure = Some(child.status.clone());
            }
        }
        if stop.load(Ordering::Acquire) {
            return SessionStatus::Killed;
        }
        if let Some(failure) = failure {
            return failure;
        }
        match self.service_exit() {
            Ok(Some(code)) => SessionStatus::Exited(code),
            Ok(None) => SessionStatus::Failed(
                "Container logs ended while Compose service remains running".into(),
            ),
            Err(error) => SessionStatus::Failed(error),
        }
    }
    fn stage(
        &self,
        args: &[String],
        output: &Sender<OutputChunk>,
        stop: Option<&AtomicBool>,
    ) -> (SessionStatus, bool) {
        if let Err(error) = self.verify_daemon() {
            return (SessionStatus::Failed(error), false);
        }
        let mut command_args = vec![
            "--host".into(),
            self.host.clone(),
            "compose".into(),
            "--project-directory".into(),
            self.project_dir.to_string_lossy().into_owned(),
            "--project-name".into(),
            self.project.clone(),
            "--file".into(),
            self.file.to_string_lossy().into_owned(),
        ];
        command_args.splice(3..3, self.options.global.clone());
        command_args.extend_from_slice(args);
        let mut child = RunSession::spawn_command_with_environment(
            0,
            "Compose owned stage",
            &self.program,
            &command_args,
            &self.cwd,
            &self.environment,
            CommandInput {
                stdin: None,
                inherit_environment: false,
            },
        );
        while child.owns_process() {
            child.poll();
            for chunk in child.output.drain(..) {
                let _ = output.send(chunk);
            }
            child.clear_output();
            if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
                child.stop();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.poll();
        for chunk in child.output.drain(..) {
            let _ = output.send(chunk);
        }
        child.clear_output();
        (child.status.clone(), child.launched)
    }
}
pub(super) fn spawn(
    id: u64,
    cfg: &RunConfig,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> RunSession {
    let options = match validate(cfg, args) {
        Ok(options) => options,
        Err(error) => return RunSession::failed_preparation(id, &cfg.name, error),
    };
    let mut session = RunSession::failed_preparation(id, &cfg.name, String::new());
    session.clear_output();
    session.status = SessionStatus::Running;
    session.finished_at = None;
    session.config = Some(cfg.clone());
    session.command_line = std::iter::once(program.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let (output, rx) = unbounded();
    let (status, status_rx) = unbounded();
    session.rx = rx;
    session.status_rx = status_rx;
    session.local_retired.store(false, Ordering::Release);
    let stop = Arc::new(AtomicBool::new(false));
    let retired = Arc::new(AtomicBool::new(false));
    let detail = Arc::new(Mutex::new("Compose preflight pending".into()));
    let unresolved = Arc::new(AtomicBool::new(false));
    session.docker_owner = Some(docker_lifecycle::Owner {
        stop: stop.clone(),
        retired: retired.clone(),
        detail: detail.clone(),
        unresolved: unresolved.clone(),
    });
    let mut environment = if cfg.workflow.inherit_environment {
        std::env::vars().collect()
    } else {
        BTreeMap::new()
    };
    environment.extend(env.clone());
    let captured = (
        resolve_program(program),
        cwd.to_path_buf(),
        cfg.type_options.as_ref().unwrap().target.clone(),
        cfg.type_options.as_ref().unwrap().service.clone(),
    );
    let local_done = session.local_retired.clone();
    rustpy_core::TaskManager::global()
        .task("Owned Compose lifecycle")
        .cancellable(false)
        .spawn(move |progress| {
            let preflight = (|| -> Result<Plan, String> {
                let project = format!(
                    "rustpyowned{}{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_nanos()
                );
                let directory = PrivateDirectory::new(&project)?;
                let source = captured.1.join(&captured.2).canonicalize().map_err(|e|e.to_string())?;
                let project_dir = source.parent().ok_or("Compose file has no parent")?.to_path_buf();
                if environment.get("DOCKER_CONTEXT").is_some_and(|value: &String|!value.is_empty()) {return Err("Isolated Compose requires explicit endpoint authority without DOCKER_CONTEXT override".into());}
                let host=environment.get("DOCKER_HOST").filter(|value|!value.is_empty()).ok_or("Isolated Compose requires a captured explicit DOCKER_HOST")?.clone();
                if !["unix://","npipe://","tcp://"].iter().any(|prefix|host.starts_with(prefix)){return Err("Isolated Compose requires a direct Unix, named-pipe or TCP endpoint".into());}
                let mut plan = Plan {
                    program: captured.0,
                    cwd: captured.1.clone(),
                    project_dir,
                    environment,
                    project,
                    host,
                    daemon:String::new(),
                    named_volumes:Default::default(),
                    anonymous_volumes:Mutex::new(Default::default()),
                    file: source,
                    service: captured.3,
                    services:Vec::new(),
                    options,
                    directory,
                };
                plan.daemon=plan.raw_query(&["info".into(),"--format".into(),"{{.ID}}".into()],false)?.trim().to_owned();
                if plan.daemon.is_empty(){return Err("Docker daemon identity missing".into());}
                plan.capture()?;
                Ok(plan)
            })();
            let mut final_status = SessionStatus::Killed;
            match preflight {
                Err(error) => final_status = SessionStatus::Failed(error),
                Ok(plan) => {
                    if let Ok(mut state) = detail.lock() {
                        *state = format!("Compose owned project {}", plan.project);
                    }
                    let _ = output.send(OutputChunk {
                        kind: OutputKind::System,
                        text: format!("RUSTPY_COMPOSE_OWNED_PROJECT={}\n", plan.project),
                    });
                    let mut creation_fenced = true;
                    if !stop.load(Ordering::Acquire) {
                        if let Ok(mut state) = detail.lock() { *state = format!("Compose owned project {} awaiting creation fence", plan.project); }
                        // Never interrupt create: its completion is the creation fence.
                        let mut create=vec!["create".into()];create.extend(plan.options.create.clone());create.extend(plan.services.clone());
                        let (created, launched) = plan.stage(&create,&output,None);
                        creation_fenced = creation_is_fenced(&created, launched);
                        if created == SessionStatus::Exited(0) && !stop.load(Ordering::Acquire) {
                            // `compose start` has no --attach flag. Await all
                            // startup requests, then follow logs read-only.
                            let mut start=vec!["start".into()];start.extend(plan.services.clone());
                            let (started, launched) = plan.stage(&start,&output,None);
                            creation_fenced = creation_is_fenced(&started,launched);
                            if started==SessionStatus::Exited(0) && !stop.load(Ordering::Acquire) {
                                final_status=plan.follow_logs(&output,&stop);
                            } else if started!=SessionStatus::Exited(0) { final_status=started; }
                        } else if created != SessionStatus::Exited(0) {
                            final_status = created;
                        }
                    }
                    // Only a successful create (or a child never launched) is
                    // a creation fence. A failed HTTP exchange can outlive its CLI.
                    loop {
                        match plan.cleanup() {
                            Ok(()) if creation_fenced => break,
                            Ok(()) => {
                                unresolved.store(true, Ordering::Release);
                                let error = "Compose create transport outcome unresolved; empty inventory is not a creation fence";
                                if let Ok(mut state) = detail.lock() { *state = error.into(); }
                                progress.set_text(error);
                                std::thread::sleep(Duration::from_secs(2));
                            },
                            Err(error) => {
                                unresolved.store(true, Ordering::Release);
                                if let Ok(mut state) = detail.lock() {
                                    *state = error.clone();
                                }
                                progress.set_text(error);
                                std::thread::sleep(Duration::from_secs(2));
                            }
                        }
                    }
                }
            }
            retired.store(true, Ordering::Release);
            local_done.store(true, Ordering::Release);
            let _ = status.send(final_status);
            Ok(())
        });
    session
}

/// Runtime dependency edges documented by the Compose service specification.
/// `extends` and build contexts are resolved before this effective model arrives.
fn service_dependencies(service: &serde_json::Value) -> Result<Vec<(String, bool)>, String> {
    let mut dependencies = std::collections::BTreeMap::new();
    if let Some(entries) = service["depends_on"].as_object() {
        for (name, options) in entries {
            dependencies.insert(name.clone(), options["required"].as_bool() != Some(false));
        }
    }
    if let Some(entries) = service["depends_on"].as_array() {
        for entry in entries {
            dependencies.insert(
                entry.as_str().ok_or("Invalid Compose dependency")?.into(),
                true,
            );
        }
    }
    for field in ["network_mode", "ipc", "pid"] {
        if let Some(name) = service[field]
            .as_str()
            .and_then(|value| value.strip_prefix("service:"))
        {
            if name.is_empty() {
                return Err(format!("Missing {field} service reference"));
            }
            dependencies.insert(name.into(), true);
        }
    }
    for field in ["links", "volumes_from"] {
        if let Some(entries) = service[field].as_array() {
            for entry in entries {
                let reference = entry
                    .as_str()
                    .ok_or_else(|| format!("Invalid Compose {field} reference"))?;
                if field == "volumes_from" && reference.starts_with("container:") {
                    return Err("Isolated Compose cannot establish ownership of inherited external-container anonymous volumes".into());
                }
                let reference = if field == "volumes_from" {
                    reference.strip_prefix("service:").unwrap_or(reference)
                } else {
                    reference
                };
                let name = reference
                    .split(':')
                    .next()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| format!("Missing Compose {field} service"))?;
                dependencies.insert(name.into(), true);
            }
        }
    }
    Ok(dependencies.into_iter().collect())
}
fn finished_key(value: &str) -> Result<String, String> {
    let value = value
        .strip_suffix('Z')
        .ok_or("Compose finish timestamp must be UTC")?;
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    if seconds.len() != 19
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Invalid Compose finish timestamp".into());
    }
    Ok(format!("{seconds}.{fraction:0<9}"))
}
fn creation_is_fenced(status: &SessionStatus, launched: bool) -> bool {
    !launched || *status == SessionStatus::Exited(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effective_dependencies_include_sharing_links_and_inherited_volumes() {
        let service = serde_json::json!({"depends_on":{"optional":{"required":false},"required":{}},"network_mode":"service:network","ipc":"service:ipc","pid":"service:pid","links":["linked:alias"],"volumes_from":["storage:ro","service:normalized:rw"]});
        let edges = service_dependencies(&service)
            .unwrap()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        for name in ["network", "ipc", "pid", "linked", "storage", "required"] {
            assert_eq!(edges.get(name), Some(&true));
        }
        assert_eq!(edges.get("normalized"), Some(&true));
        assert_eq!(edges.get("optional"), Some(&false));
        assert!(service_dependencies(
            &serde_json::json!({"volumes_from":["container:external:ro"]})
        )
        .is_err());
        assert!(service_dependencies(&serde_json::json!({"pid":"service:"})).is_err());
        assert!(
            service_dependencies(&serde_json::json!({"network_mode":"host","ipc":"private"}))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn service_exit_order_normalizes_fractional_utc_precision() {
        assert!(
            finished_key("2026-10-16T10:20:30.1Z").unwrap()
                < finished_key("2026-10-16T10:20:30.12Z").unwrap()
        );
        assert_eq!(
            finished_key("2026-10-16T10:20:30Z").unwrap(),
            finished_key("2026-10-16T10:20:30.000000000Z").unwrap()
        );
        assert!(finished_key("unrecognized time").is_err());
    }
    #[test]
    fn multi_service_options_keep_scale_pull_and_captured_project_inputs() {
        let mut spec = rustpy_project::configuration_types::ConfigurationSpec::new(
            rustpy_project::configuration_types::ConfigurationType::DockerCompose,
        );
        spec.compose_owned_project = true;
        spec.arguments = [
            "--scale",
            "web=2",
            "--pull",
            "missing",
            "--profile",
            "test",
            "--env-file",
            "selected.env",
            "--exit-code-from",
            "worker",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let cfg = RunConfig {
            type_options: Some(spec.clone()),
            ..Default::default()
        };
        let mut args = vec!["compose".into(), "-f".into(), spec.target, "up".into()];
        args.extend(spec.arguments);
        let options = validate(&cfg, &args).unwrap();
        assert_eq!(options.create, ["--scale", "web=2", "--pull", "missing"]);
        assert_eq!(
            options.global,
            ["--profile", "test", "--env-file", "selected.env"]
        );
        assert_eq!(options.exit_from.as_deref(), Some("worker"));
        assert!(options.abort_exit);
    }
    #[test]
    fn ambiguous_create_failure_keeps_authority_despite_empty_inventory() {
        assert!(creation_is_fenced(&SessionStatus::Exited(0), true));
        assert!(creation_is_fenced(
            &SessionStatus::Failed("spawn refused".into()),
            false
        ));
        assert!(!creation_is_fenced(&SessionStatus::Exited(1), true));
        assert!(!creation_is_fenced(
            &SessionStatus::Failed("transport EOF".into()),
            true
        ));
        assert!(!creation_is_fenced(&SessionStatus::Killed, true));
    }
    #[test]
    fn unsupported_flags_are_refused_before_any_provider_mutation() {
        let mut spec = rustpy_project::configuration_types::ConfigurationSpec::new(
            rustpy_project::configuration_types::ConfigurationType::DockerCompose,
        );
        spec.compose_owned_project = true;
        spec.service = "python".into();
        for flag in [
            "--project-name",
            "--scale",
            "--build",
            "--detach",
            "--unknown",
        ] {
            spec.arguments = vec![flag.into()];
            let cfg = RunConfig {
                type_options: Some(spec.clone()),
                ..Default::default()
            };
            let mut args = vec![
                "compose".into(),
                "-f".into(),
                spec.target.clone(),
                "up".into(),
            ];
            args.extend(spec.arguments.clone());
            args.push(spec.service.clone());
            assert!(validate(&cfg, &args).is_err());
        }
    }
}

//! Real Node + Microsoft js-debug TCP server through the production Web Debug route.
use super::*;
use rustpy_debug::DebugState;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, serde::Deserialize)]
struct Identity {
    pid: u32,
    start: String,
}
fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(Identity {
        pid,
        start: stat.rsplit(") ").next()?.split_whitespace().nth(19)?.into(),
    })
}
fn retired(owner: &Identity) -> bool {
    identity(owner.pid).is_none_or(|actual| actual.start != owner.start)
}
struct Owned {
    app: IdeApp,
    context: egui::Context,
    pid_file: PathBuf,
    adapter: Option<Identity>,
    debuggee: Option<Identity>,
    _directory: tempfile::TempDir,
}
impl Owned {
    fn tick(&mut self) {
        self.app.poll_run_workflows(&self.context);
        self.app.run_panel.poll();
        self.app.poll_debug(&self.context);
        if self.adapter.is_none() {
            self.adapter = self
                .app
                .run_panel
                .sessions
                .iter()
                .find(|session| {
                    session
                        .config
                        .as_ref()
                        .is_some_and(|config| config.name.ends_with(" adapter"))
                })
                .and_then(|session| session.process_id())
                .and_then(identity);
        }
        if self.debuggee.is_none() {
            self.debuggee = std::fs::read(&self.pid_file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Identity>(&bytes).ok());
        }
    }
    fn until(&mut self, description: &str, ready: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.tick();
            if ready(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "real JS DAP did not reach {description}; status={:?}; debug={:?}; adapter={:?}",
                self.app.status,
                self.app.debug.as_ref().map(|debug| &debug.state),
                self.app
                    .run_panel
                    .sessions
                    .iter()
                    .map(|session| (&session.status, &session.output))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop_and_retire(&mut self) {
        self.app.cancel_run_workflows();
        if let Some(debug) = &mut self.app.debug {
            debug.stop();
        }
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        // Keep the complete App, socket client and source directory until the
        // public RunSession owner acknowledges retirement; never signal a PID
        // merely because it came from a file. Exact identities are observations.
        loop {
            self.tick();
            if !self.app.run_workflows_pending()
                && self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .all(|session| !session.owns_process())
                && self.adapter.as_ref().is_none_or(retired)
                && self.debuggee.as_ref().is_none_or(retired)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.stop_and_retire();
    }
}

#[test]
#[ignore = "requires RUSTPY_REQUIRE_JS_DAP=1, real absolute Node and genuine installed Microsoft js-debug TCP server entry/argv; run alone"]
fn configured_web_debug_launch_hits_real_node_breakpoint_variables_continue_and_retirement() {
    assert_eq!(std::env::var("RUSTPY_REQUIRE_JS_DAP").as_deref(), Ok("1"));
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "fixture admits only its selected tools"
    );
    let config = PathBuf::from(
        std::env::var_os("RUSTPY_CONFIG_DIR").expect("explicit private profile required"),
    );
    let canonical_config =
        dunce::canonicalize(&config).expect("private profile must already exist");
    // The profile must be a throwaway one: under the system temp directory or
    // under an explicitly named test profile root, never a real user profile.
    let allowed_roots: Vec<PathBuf> = std::iter::once(std::env::temp_dir())
        .chain(std::env::var_os("RUSTPY_TEST_PROFILE_ROOT").map(PathBuf::from))
        .filter(|root| root.is_absolute())
        .filter_map(|root| dunce::canonicalize(&root).ok())
        .collect();
    assert!(
        config.is_absolute()
            && config.is_dir()
            && allowed_roots
                .iter()
                .any(|root| canonical_config != *root && canonical_config.starts_with(root)),
        "keep App settings/publication inside an explicitly owned private RustPy profile \
         (under the temp directory or RUSTPY_TEST_PROFILE_ROOT)"
    );
    let absolute = |name: &str| {
        let path = PathBuf::from(std::env::var_os(name).expect(name));
        assert!(
            path.is_absolute() && path.is_file(),
            "{name} must be an explicitly admitted installed file"
        );
        path
    };
    for name in ["NODE_OPTIONS", "VSCODE_INSPECTOR_OPTIONS"] {
        assert!(
            std::env::var_os(name).is_none(),
            "remove ambient {name} before admitting this isolated case"
        );
    }
    let node = absolute("RUSTPY_JS_DAP_NODE");
    let entry = absolute("RUSTPY_JS_DAP_SERVER_ENTRY");
    assert_eq!(
        entry.file_name().and_then(|name| name.to_str()),
        Some("dapDebugServer.js"),
        "use the genuine standalone TCP server, not the stdio adapter entry"
    );
    let arguments: Vec<String> = serde_json::from_str(&std::env::var("RUSTPY_JS_DAP_ARGUMENTS_JSON").expect("explicit argument array from the installed server's primary documentation/source required")).unwrap();
    assert_eq!(arguments.first().map(String::as_str), entry.to_str());
    assert!(
        arguments.iter().any(|argument| argument.contains("$Port$")),
        "the production typed route connects to its captured TCP port"
    );
    let directory = tempfile::Builder::new()
        .prefix("real-js-dap-λ-")
        .tempdir()
        .unwrap();
    let path = directory.path().join("owned π.cjs");
    let pid_file = directory.path().join("debuggee-identity.json");
    let completed = directory.path().join("continued.txt");
    let exited = directory.path().join("actual-exit.json");
    let source = format!(
        "const fs = require('node:fs'); process.on('exit', code => fs.writeFileSync({}, JSON.stringify({{pid:process.pid,code}})));\nconst identityPath = {};\nfs.writeFileSync(identityPath, JSON.stringify({{pid:process.pid,start:fs.readFileSync('/proc/self/stat','utf8').split(') ').pop().trim().split(/\\s+/)[19]}}));\nconst ownedValue42 = 42;\nconst ownedMessage = 'ACTUAL_NODE_DAP_CONTINUED_7182';\nconsole.log(ownedMessage, ownedValue42);\nfs.writeFileSync({}, 'continued-after-real-breakpoint');\n",
        serde_json::to_string(&exited).unwrap(), serde_json::to_string(&pid_file).unwrap(), serde_json::to_string(&completed).unwrap());
    std::fs::write(&path, &source).unwrap();
    let mut settings = Settings::default();
    settings.web_development.node = node.to_str().unwrap().into();
    settings.web_development.javascript_adapter = node.to_str().unwrap().into();
    settings.web_development.adapter_arguments = arguments.clone();
    settings.web_development.automatic = false;
    // Untrusted construction forbids unrelated automatic Python discovery.
    let mut app = IdeApp::new(settings, Some(directory.path().into()));
    app.settings.trust_project(directory.path());
    app.safe_mode = false;
    assert!(!app.project_safe_mode());
    app.open_file(&path);
    assert!(
        app.breakpoints.toggle(&path, 6),
        "actual one-based line breakpoint"
    );
    let mut owned = Owned {
        app,
        context: egui::Context::default(),
        pid_file,
        adapter: None,
        debuggee: None,
        _directory: directory,
    };
    // This is the actual Web Development Debug action implementation. It must
    // build JavaScriptDebug, pass launch_config(Debug), and own its adapter in Run.
    owned.app.web_launch(1, None);
    owned.until(
        "typed JavaScript session and real line-6 breakpoint",
        |fixture| {
            fixture.app.debug.as_ref().is_some_and(|debug| {
                debug.is_stopped()
                    && debug
                        .current_frame()
                        .is_some_and(|frame| frame.path.as_ref() == Some(&path) && frame.line == 6)
            })
        },
    );
    assert!(
        owned.adapter.is_some() && owned.debuggee.is_some(),
        "capture actual adapter and debuggee PID/start identities"
    );
    let debug = owned.app.debug.as_ref().unwrap();
    let spec = debug
        .config
        .type_options
        .as_ref()
        .expect("typed config must survive launch");
    assert_eq!(
        spec.kind,
        rustpy_project::configuration_types::ConfigurationType::JavaScriptDebug
    );
    assert_eq!(spec.executable, node);
    assert_eq!(spec.adapter_arguments, arguments);
    assert_eq!(
        spec.debug_options["program"],
        serde_json::to_value(&path).unwrap()
    );
    assert_eq!(
        spec.debug_options["runtimeExecutable"],
        node.to_str().unwrap()
    );
    assert!(matches!(&debug.state, DebugState::Stopped { reason, .. } if reason == "breakpoint"));
    assert_eq!(debug.is_verified(&path, 6), Some(true));
    assert!(
        !completed.exists() && !exited.exists(),
        "continue and actual exit oracles must not be pre-seeded before suspension"
    );
    let mut requested = std::collections::HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        owned.tick();
        let debug = owned.app.debug.as_mut().unwrap();
        let references: Vec<_> = debug
            .scopes
            .iter()
            .filter(|scope| !scope.expensive && !scope.name.to_ascii_lowercase().contains("global"))
            .take(4)
            .map(|scope| scope.variables_reference)
            .collect();
        for reference in references {
            if reference > 0 && requested.insert(reference) {
                debug.request_variables(reference);
            }
        }
        if debug
            .variables
            .values()
            .flatten()
            .any(|variable| variable.name == "ownedValue42" && variable.value == "42")
            && debug.variables.values().flatten().any(|variable| {
                variable.name == "ownedMessage"
                    && variable.value.contains("ACTUAL_NODE_DAP_CONTINUED_7182")
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual JS scope variables missing: {:?}",
            debug.variables
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    owned.app.debug.as_mut().unwrap().continue_();
    owned.until(
        // js-debug 1.140.0 Binder._markTargetAsTerminated emits terminated
        // for the target. Its root launcher forwards only restart metadata,
        // not IStopMetadata.code, so a DAP exited event is not guaranteed.
        // Obtain the actual zero exit status independently from Node's exit
        // callback; never manufacture a code in DebugSession.
        "real continued program completion, actual exit receipt and DAP termination",
        |fixture| {
            completed.is_file()
                && exited.is_file()
                && fixture
                    .app
                    .debug
                    .as_ref()
                    .is_some_and(|debug| matches!(debug.state, DebugState::Terminated(_)))
        },
    );
    let exit: serde_json::Value = serde_json::from_slice(&std::fs::read(&exited).unwrap()).unwrap();
    assert_eq!(
        exit["code"].as_i64(),
        Some(0),
        "the actual Node exit callback must report success"
    );
    assert_eq!(
        exit["pid"].as_u64(),
        Some(u64::from(
            owned
                .debuggee
                .as_ref()
                .expect("captured actual debuggee identity")
                .pid
        )),
        "exit receipt must come from the same owned program that hit the breakpoint"
    );
    if let DebugState::Terminated(Some(code)) = owned.app.debug.as_ref().unwrap().state {
        assert_eq!(
            code, 0,
            "any adapter-provided exit status must agree with the real program"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&completed).unwrap(),
        "continued-after-real-breakpoint"
    );
    assert!(
        owned
            .app
            .debug
            .as_ref()
            .unwrap()
            .output
            .iter()
            .any(|chunk| chunk.text.contains("ACTUAL_NODE_DAP_CONTINUED_7182")),
        "real debuggee output must reach the shared Debug console"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    owned.stop_and_retire();
    assert!(owned
        .app
        .run_panel
        .sessions
        .iter()
        .all(|session| !session.owns_process()));
    assert!(
        retired(owned.adapter.as_ref().unwrap()) && retired(owned.debuggee.as_ref().unwrap()),
        "physical PID/start retirement, not just DAP terminated status"
    );
}

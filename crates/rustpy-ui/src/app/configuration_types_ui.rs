//! Genuine launch dispatch for typed configuration intents that are not a
//! single ordinary subprocess: compound graphs, HTTP and DAP attachments.
use super::*;
use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
impl IdeApp {
    /// Returns true after consuming a special intent. Ordinary factories are
    /// lowered by launch_now and retain the existing run/test/debug pipelines.
    pub(super) fn launch_configuration_type(
        &mut self,
        cfg: &RunConfig,
        mode: crate::run_context::RunMode,
    ) -> bool {
        if self.project_safe_mode() {
            self.set_status("Trust the project before starting configurations");
            return true;
        }
        let Some(spec) = cfg.type_options.as_ref() else {
            return false;
        };
        let builtin = match spec.kind {
            ConfigurationType::Docker
            | ConfigurationType::DockerCompose
            | ConfigurationType::DockerImage => Some("docker"),
            ConfigurationType::Jupyter => Some("jupyter"),
            _ => None,
        };
        if builtin.is_some_and(|id| !self.builtin_plugin_enabled(id)) {
            self.set_status("This configuration's bundled capability is disabled in Plugins");
            return true;
        }
        if let Err(error) = spec.validate() {
            self.set_status(error);
            return true;
        }
        match spec.kind {
            ConfigurationType::Compound => {
                let configs = self
                    .project
                    .as_ref()
                    .map(|project| project.run_configs.configs.clone())
                    .unwrap_or_default();
                match rustpy_project::configuration_types::compound_members(&cfg.name, &configs) {
                    Ok(members) => {
                        let members = members.into_iter().cloned().collect::<Vec<_>>();
                        for member in members {
                            self.launch_config(member, mode);
                        }
                    }
                    Err(error) => self.set_status(error),
                }
                true
            }
            ConfigurationType::HttpRequest => {
                let cfg = self.launch_cfg(cfg);
                let root = self
                    .project
                    .as_ref()
                    .map(|p| p.root.clone())
                    .unwrap_or_default();
                let base = cfg
                    .working_dir
                    .as_ref()
                    .map(|path| {
                        if path.is_absolute() {
                            path.clone()
                        } else {
                            root.join(path)
                        }
                    })
                    .unwrap_or_else(|| root.clone());
                let target = cfg
                    .type_options
                    .as_ref()
                    .map(|spec| spec.target.clone())
                    .unwrap_or_default();
                let path = if Path::new(&target).is_absolute() {
                    PathBuf::from(&target)
                } else {
                    base.join(&target)
                };
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        if let Err(error) = self.run_http_configuration(
                            &cfg,
                            &path,
                            &text,
                            spec.request_line,
                            &root,
                        ) {
                            self.set_status(error);
                        }
                    }
                    Err(error) => self.set_status(format!("Cannot open HTTP request: {error}")),
                }
                true
            }
            ConfigurationType::AttachProcess
            | ConfigurationType::PythonDebugServer
            | ConfigurationType::JavaScriptDebug => {
                self.start_configuration_adapter(cfg.clone());
                true
            }
            _ => false,
        }
    }
    pub(super) fn attach_run_process(&mut self, pid: u32, interpreter: Option<PathBuf>) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Trust the project before attaching a debugger");
            return;
        }
        let mut spec = ConfigurationSpec::new(ConfigurationType::AttachProcess);
        spec.process_id = pid;
        let cfg = RunConfig {
            name: format!("Attach to process {pid}"),
            type_options: Some(spec),
            interpreter,
            ..Default::default()
        };
        self.start_configuration_adapter(cfg);
    }
    fn start_configuration_adapter(&mut self, mut cfg: RunConfig) {
        let Some(spec) = cfg.type_options.clone() else {
            return;
        };
        if let Err(error) = spec.validate() {
            self.set_status(error);
            return;
        }
        if !self.save_all() {
            return;
        }
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        if spec.debug_options["subProcessId"].as_u64().is_some() {
            let owner = self
                .debug
                .iter()
                .chain(self.debug_sessions.iter().map(|(session, _)| session))
                .find(|session| {
                    session.is_active()
                        && session
                            .protocol
                            .endpoint
                            .as_ref()
                            .is_some_and(|(host, port, _)| host == &spec.host && *port == spec.port)
                });
            let attached = owner.map(|owner| {
                owner.attach_subprocess(&serde_json::json!({
                    "connect":{"host":spec.host,"port":spec.port},
                    "subProcessId":spec.debug_options["subProcessId"], "name":cfg.name
                }))
            });
            if let Some(result) = attached {
                match result {
                    Ok(mut session) => {
                        session.id = self.next_session_id;
                        self.next_session_id += 1;
                        session.config = cfg;
                        self.retain_debug_session();
                        self.debug = Some(session);
                        self.bottom = Some(BottomTool::Debug);
                    }
                    Err(error) => self.set_status(format!("Cannot reconnect subprocess: {error}")),
                }
                return;
            }
        }
        let setup = self.debug_launch_setup();
        let direct_pydevd = spec.kind == ConfigurationType::PythonDebugServer
            && spec.listen
            && (spec.pydevd || spec.pydevd_legacy);
        let local_adapter =
            !direct_pydevd && (spec.kind != ConfigurationType::PythonDebugServer || spec.listen);
        let (host, port) = if local_adapter {
            match std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|listener| listener.local_addr())
            {
                Ok(address) => ("127.0.0.1".to_string(), address.port()),
                Err(error) => {
                    self.set_status(format!("Cannot reserve adapter port: {error}"));
                    return;
                }
            }
        } else {
            (spec.host.clone(), spec.port)
        };
        if local_adapter {
            let mut adapter = cfg.clone();
            adapter.type_options = None;
            adapter.name = format!("{} adapter", cfg.name);
            adapter.parameters.clear();
            adapter.run_with_console = false;
            adapter.kind = if spec.kind == ConfigurationType::JavaScriptDebug {
                RunConfigKind::Program {
                    program: spec.executable.clone(),
                    args: spec
                        .adapter_arguments
                        .iter()
                        .map(|arg| {
                            arg.replace("$Port$", &port.to_string())
                                .replace("$Host$", &host)
                        })
                        .collect(),
                }
            } else {
                adapter.parameters = crate::run_anything::quote_args(&[
                    "--host".into(),
                    host.clone(),
                    "--port".into(),
                    port.to_string(),
                ]);
                RunConfigKind::PythonModule {
                    module: "debugpy.adapter".into(),
                }
            };
            let interpreter = self
                .project
                .as_ref()
                .and_then(|project| project.interpreter.as_ref());
            let id = self.next_session_id;
            self.next_session_id += 1;
            let session = RunSession::spawn(id, &adapter, interpreter, &root);
            if let rustpy_run::SessionStatus::Failed(error) = &session.status {
                self.set_status(format!("Cannot start debug adapter: {error}"));
                return;
            }
            self.run_panel.add_session(session);
        }
        if cfg.working_dir.is_none() {
            cfg.working_dir = Some(root);
        }
        let adapter_id = if spec.kind == ConfigurationType::JavaScriptDebug {
            "javascript"
        } else {
            "debugpy"
        };
        self.retain_debug_session();
        let attached = if spec.pydevd_legacy {
            rustpy_debug::DebugSession::listen_pydevd_legacy(
                &host,
                port,
                setup.options,
                setup.breakpoints,
            )
        } else if direct_pydevd {
            rustpy_debug::DebugSession::listen_pydevd(&host, port, setup.options, setup.breakpoints)
        } else {
            rustpy_debug::DebugSession::attach_adapter(
                &host,
                port,
                setup.options,
                setup.breakpoints,
                adapter_id,
            )
        };
        match attached {
            Ok(mut session) => {
                session.id = self.next_session_id;
                self.next_session_id += 1;
                session.name = cfg.name.clone();
                session.config = cfg;
                session.exceptions = setup.exceptions;
                session.muted = setup.muted;
                self.debug = Some(session);
                self.debug_panel = DebugPanel::default();
                self.bottom = Some(BottomTool::Debug);
            }
            Err(error) => self.set_status(format!("Cannot connect debug adapter: {error}")),
        }
    }
}

//! Deployment events reuse the existing Run, DAP, interpreter and terminal owners.
use super::*;
impl IdeApp {
    pub(super) fn stop_deployment(&mut self, id: u64, stamp: &str) {
        if self
            .deploy
            .pending_transfers()
            .iter()
            .any(|(current, _, state)| *current == id && state == stamp)
        {
            self.deploy.stop_transfer(id);
        } else {
            self.set_status("Deployment operation changed; open Stop again");
        }
    }
    pub(super) fn attach_deployment_docker(
        &mut self,
        command: rustpy_deploy::CommandSpec,
        host: String,
        port: u16,
        remote: String,
        root: PathBuf,
    ) {
        if !self.builtin_plugin_enabled("docker") {
            return;
        }

        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode || port == 0 || !self.save_all() {
            return;
        }
        self.spawn_command_session(
            "Docker debugpy injection",
            crate::run_anything::CommandSpec {
                program: command.program.into(),
                args: command.args,
                cwd: root.clone(),
                env: command.env.into_iter().collect(),
            },
        );
        let mut specification = rustpy_project::configuration_types::ConfigurationSpec::new(
            rustpy_project::configuration_types::ConfigurationType::PythonDebugServer,
        );
        specification.host = host;
        specification.port = port;
        specification.listen = false;
        if !remote.is_empty() {
            specification
                .path_mappings
                .insert(root.to_string_lossy().into_owned(), remote);
        }
        self.launch_config(
            RunConfig {
                name: "Attach Docker process".into(),
                kind: RunConfigKind::PythonModule {
                    module: "debugpy".into(),
                },
                type_options: Some(specification),
                temporary: true,
                working_dir: Some(root),
                ..Default::default()
            },
            crate::run_context::RunMode::Debug,
        );
    }
}
impl IdeApp {
    pub(super) fn open_deployment_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        let Some(kind) = tab
            .state
            .path
            .as_deref()
            .and_then(rustpy_deploy::targets::document_kind)
        else {
            return false;
        };
        let text = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let (start, items) = rustpy_deploy::targets::document_completions(
            &text,
            usize::from(caret),
            kind == "Dockerfile",
        );
        let items = items
            .into_iter()
            .map(|insert| CompletionItem {
                label: insert.trim().into(),
                kind: CompletionKind::Keyword,
                detail: format!("{kind} instruction"),
                insert,
                caret_offset: None,
                priority: 0,
            })
            .collect::<Vec<_>>();
        let version = tab.state.doc.version();
        self.cancel_basic_completion();
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: 0,
            prefix_start: TextSize::new(start as u32),
            doc_version: version,
            caret,
            explicit,
            rect: None,
        };
        true
    }
    pub(super) fn deployment_document_run(
        &mut self,
        path: &Path,
        mode: crate::run_context::RunMode,
    ) -> bool {
        if rustpy_deploy::targets::document_kind(path).is_none() {
            return false;
        }
        if mode != crate::run_context::RunMode::Run {
            self.set_status("Use a container process attachment to debug Docker; coverage and profiling run inside its interpreter");
            return true;
        }
        let root = self.project_root();
        match rustpy_deploy::targets::document_command(&root, path) {
            Ok(Some(command)) => {
                let config = RunConfig {
                    name: format!(
                        "{} {}",
                        rustpy_deploy::targets::document_kind(path).unwrap(),
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    kind: RunConfigKind::Program {
                        program: command.program.into(),
                        args: command.args,
                    },
                    env: command.env,
                    working_dir: Some(root),
                    temporary: true,
                    ..Default::default()
                };
                self.launch_context(config, mode);
            }
            Err(error) => self.set_status(error.to_string()),
            _ => {}
        }
        true
    }
}

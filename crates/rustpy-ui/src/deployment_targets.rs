//! Dedicated Docker/Compose/Vagrant controls; existing Services keep their resource identities.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_deploy::targets::{Docker, DockerArguments, Registry, Tools};
pub(in crate::deploy_ui) struct State {
    pub expected: Option<u64>,
    confirmation: Option<(String, rustpy_deploy::CommandSpec)>,
    pub root: PathBuf,
    pub config: Tools,
    pub selected: usize,
    pub arguments: DockerArguments,
    pub output: String,
    pub rows: Vec<serde_json::Value>,
    pub operation: String,
    pub machine: String,
    pub box_name: String,
    pub registry: usize,
    pub password: String,
    pub debug_pid: u32,
    pub debug_port: u16,
    pub debug_host: String,
}
impl State {
    fn new(root: &Path) -> Self {
        let config = Tools::load(root);
        let selected = config
            .docker
            .iter()
            .position(|connection| connection.name == config.default_docker)
            .unwrap_or(0);
        Self {
            expected: None,
            confirmation: None,
            root: root.into(),
            config,
            selected,
            arguments: Default::default(),
            output: String::new(),
            rows: Vec::new(),
            operation: String::new(),
            machine: String::new(),
            box_name: String::new(),
            registry: 0,
            password: String::new(),
            debug_pid: 0,
            debug_port: 5678,
            debug_host: "127.0.0.1".into(),
        }
    }
}
impl DeployUi {
    pub(in crate::deploy_ui) fn open_target_tools(&mut self, root: &Path) {
        self.capture_root(root);
        self.extra.tools = Some(State::new(root));
    }
    pub(in crate::deploy_ui) fn targets_window(
        &mut self,
        ctx: &egui::Context,
        events: &mut Vec<DeployEvent>,
    ) {
        let Some(mut state) = self.extra.tools.take() else {
            return;
        };
        let mut open = true;
        let mut action = None;
        let mut vagrant = None;
        let mut login = false;
        let mut interpreter = false;
        let mut image_interpreter = None;
        let mut debug = false;
        let mut kubernetes = false;
        egui::Window::new("Deployment Targets — Docker / Compose / Vagrant").open(&mut open).default_size([900.0,700.0]).show_modeless(ctx,|ui|{egui::ScrollArea::vertical().show(ui,|ui|{
            if ui.button("Kubernetes / Helm / Kustomize…").clicked(){kubernetes=true;}
            ui.heading("Docker connections");ui.horizontal(|ui|{for(index,connection)in state.config.docker.iter().enumerate(){ui.selectable_value(&mut state.selected,index,&connection.name);}if ui.button("Add connection").clicked(){state.config.docker.push(Docker::default());state.selected=state.config.docker.len()-1;}});
            if let Some(connection)=state.config.docker.get_mut(state.selected){for(label,value)in[("Name",&mut connection.name),("Executable (docker or podman)",&mut connection.executable),("Daemon (unix:///…, tcp://…, ssh://…)",&mut connection.endpoint),("Context (e.g. colima)",&mut connection.context)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}ui.checkbox(&mut connection.tls,"Verify daemon TLS");for(label,value)in[("CA certificate",&mut connection.ca),("Client certificate",&mut connection.certificate),("Client private key",&mut connection.key)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}}
            if ui.button("Save targets").clicked(){state.config.default_docker=state.config.docker.get(state.selected).map(|connection|connection.name.clone()).unwrap_or_default();if let Err(error)=state.config.save(&state.root){state.output=error.to_string();}}
            ui.separator();let a=&mut state.arguments;
            for(label,value)in[("Selected resource / name",&mut a.target),("Image / tag",&mut a.image),("Container name",&mut a.name),("File / container path / Dockerfile",&mut a.path),("Local destination / container destination / build context",&mut a.destination),("Compose file",&mut a.compose_file),("Compose service",&mut a.service),("Compose env file",&mut a.env_file)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
            for(label,values)in[("Command arguments",&mut a.command),("Ports (host:container)",&mut a.ports),("Volumes (local:container)",&mut a.volumes),("Environment (NAME=value)",&mut a.environment),("Compose profiles",&mut a.profiles)]{let mut text=crate::run_anything::quote_args(values);ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));if ui.text_edit_singleline(&mut text).changed(){*values=rustpy_project::RunConfig::split_args(&text);}});}
            ui.horizontal(|ui|{ui.label("Replicas");ui.add(egui::DragValue::new(&mut a.scale).range(1..=1000));ui.checkbox(&mut a.build,"Build Compose images");});
            ui.horizontal_wrapped(|ui|{for operation in ["Test connection","Images","Pull image","Push image","Delete image","Build Dockerfile","Create container","Run image","Containers","Start","Stop","Restart","Delete container","Logs","Attach","Exec terminal","Inspect","Files","Download container file","Upload container file","Ports","Environment","Properties","Volumes","Inspect volume","Create volume","Delete volume","Networks","Inspect network","Create network","Delete network","Compose services","Compose up","Compose down","Compose restart","Compose scale","Compose logs","Validate Compose"]{if ui.button(crate::i18n::tr_static(operation)).clicked(){action=Some(operation.to_string());}}});
            ui.horizontal(|ui|{if ui.button("Use Docker interpreter").clicked(){interpreter=true;}
if ui.button("Use Docker image interpreter").clicked(){image_interpreter=Some(false);}
if ui.button("Use Compose interpreter").clicked(){image_interpreter=Some(true);}ui.label("Python executable is first Command argument (default python3)");});
            ui.horizontal(|ui|{ui.label("Attach debugger to container PID");ui.add(egui::DragValue::new(&mut state.debug_pid).range(1..=u32::MAX));ui.label("Published host");ui.text_edit_singleline(&mut state.debug_host);ui.label("Port");ui.add(egui::DragValue::new(&mut state.debug_port).range(1..=65535));if ui.button("Inject debugpy and attach").clicked(){debug=true;}});ui.label("The chosen debug port must be published by the container. Remote root is File / container path.");
            ui.collapsing("Docker registries",|ui|{for(index,registry)in state.config.registries.iter_mut().enumerate(){ui.horizontal(|ui|{ui.selectable_value(&mut state.registry,index,&registry.name);ui.text_edit_singleline(&mut registry.name);ui.text_edit_singleline(&mut registry.host);ui.text_edit_singleline(&mut registry.username);ui.checkbox(&mut registry.save_password,"Save password");});}if ui.button("Add registry").clicked(){state.config.registries.push(Registry::default());}ui.add(egui::TextEdit::singleline(&mut state.password).password(true).hint_text("Password safe when empty"));if ui.button("Log in selected registry").clicked(){login=true;}});
            ui.separator();ui.heading("Vagrant (project Vagrantfile)");ui.horizontal(|ui|{ui.label("Executable");ui.text_edit_singleline(&mut state.config.vagrant_executable);ui.label("Machine");ui.text_edit_singleline(&mut state.machine);ui.label("Box");ui.text_edit_singleline(&mut state.box_name);});ui.horizontal_wrapped(|ui|{for operation in ["Status","Up","Halt","Destroy","Reload","SSH","SSH config","Boxes","Add box","Remove box","Update boxes"]{if ui.button(crate::i18n::tr_static(operation)).clicked(){vagrant=Some(operation.to_string());}}if ui.button("Use Vagrant interpreter").clicked(){vagrant=Some("Interpreter".into());}});
            ui.separator();ui.label(format!("{} · captured root {}",state.operation,state.root.display()));for row in &state.rows{let label=row.get("Names").or_else(||row.get("Repository")).or_else(||row.get("Name")).and_then(|v|v.as_str()).unwrap_or("Resource");if ui.selectable_label(false,label).clicked(){state.arguments.target=row.get("ID").or_else(||row.get("Name")).and_then(|v|v.as_str()).unwrap_or(label).into();if let Some(repository)=row.get("Repository").and_then(|v|v.as_str()){let tag=row.get("Tag").and_then(|v|v.as_str()).unwrap_or("latest");state.arguments.image=format!("{repository}:{tag}");}}}
            if state.operation=="Files"{for entry in state.output.lines().skip(1).filter(|entry|*entry!="./"&&*entry!="../"){if ui.link(entry).clicked(){state.arguments.path=format!("{}/{}",state.arguments.path.trim_end_matches('/'),entry.trim_end_matches('/'));}}}
            ui.add(egui::TextEdit::multiline(&mut state.output).font(egui::TextStyle::Monospace).desired_rows(12).interactive(false));
        });});
        let root = state.root.clone();
        let connection = state.config.docker.get(state.selected).cloned();
        let args = state.arguments.clone();
        if let Some(operation) = action {
            if let Some(connection) = connection.clone() {
                match rustpy_deploy::targets::docker_args(&operation, &args, &root)
                    .and_then(|args| connection.command_at(args, &root))
                {
                    Ok(command) => {
                        state.operation = operation.clone();
                        if matches!(operation.as_str(), "Attach" | "Exec terminal") {
                            events.push(DeployEvent::Terminal(
                                operation,
                                command.program,
                                command.args,
                                root.clone(),
                            ));
                        } else if operation.starts_with("Delete ") {
                            state.confirmation = Some((operation, command));
                        } else {
                            state.expected = Some(self.spawn_target_command(operation, command));
                        }
                    }
                    Err(error) => state.output = error.to_string(),
                }
            }
        }
        if let Some(operation) = vagrant {
            let program = if state.config.vagrant_executable.is_empty() {
                "vagrant".to_string()
            } else {
                state.config.vagrant_executable.clone()
            };
            if operation == "Interpreter" {
                self.prepare_container_interpreter(
                    &root,
                    None,
                    state.machine.clone(),
                    args.command
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "python3".into()),
                    Some(program),
                );
            } else {
                match rustpy_deploy::targets::vagrant_args(
                    &operation,
                    &state.machine,
                    &state.box_name,
                ) {
                    Ok(args) => {
                        let command = rustpy_deploy::CommandSpec::new(program).args(args);
                        state.operation = format!("Vagrant {operation}");
                        if operation == "SSH" {
                            events.push(DeployEvent::Terminal(
                                "Vagrant SSH".into(),
                                command.program,
                                command.args,
                                root.clone(),
                            ));
                        } else if matches!(operation.as_str(), "Destroy" | "Remove box") {
                            state.confirmation = Some((state.operation.clone(), command));
                        } else {
                            state.expected =
                                Some(self.spawn_target_command(state.operation.clone(), command));
                        }
                    }
                    Err(error) => state.output = error.to_string(),
                }
            }
        }
        if login {
            if let (Some(connection), Some(registry)) = (
                connection.clone(),
                state.config.registries.get(state.registry).cloned(),
            ) {
                let password = std::mem::take(&mut state.password);
                self.registry_login(connection, registry, password);
            }
        }
        if interpreter {
            if let Some(connection) = connection.clone() {
                self.prepare_container_interpreter(
                    &root,
                    Some(connection),
                    args.target.clone(),
                    args.command
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "python3".into()),
                    None,
                );
            }
        }
        if let Some(compose) = image_interpreter {
            if let Some(connection) = connection.clone() {
                self.prepare_image_interpreter(&root, connection, args.clone(), compose);
            }
        }
        if debug {
            if let Some(connection) = connection {
                if state.debug_pid > 0 {
                    let python = args
                        .command
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "python3".into());
                    let inject = connection.command(vec![
                        "exec".into(),
                        args.target.clone(),
                        python,
                        "-m".into(),
                        "debugpy".into(),
                        "--listen".into(),
                        format!("0.0.0.0:{}", state.debug_port),
                        "--pid".into(),
                        state.debug_pid.to_string(),
                    ]);
                    match inject {
                        Ok(command) => events.push(DeployEvent::DockerDebug(
                            command,
                            state.debug_host.clone(),
                            state.debug_port,
                            args.path.clone(),
                            root.clone(),
                        )),
                        Err(error) => state.output = error.to_string(),
                    }
                }
            }
        }
        if let Some((label, command)) = state.confirmation.clone() {
            let mut answer = None;
            egui::Window::new("Confirm resource deletion")
                .collapsible(false)
                .show_modal(ctx, |ui| {
                    ui.label(format!("{label} in {}", root.display()));
                    ui.monospace(command.display());
                    if ui.button("Delete").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            if let Some(proceed) = answer {
                state.confirmation = None;
                if proceed {
                    state.expected = Some(self.spawn_target_command(label, command));
                }
            }
        }
        if kubernetes {
            self.open_kubernetes(&state.root);
        }
        if open {
            self.extra.tools = Some(state);
        }
    }
    fn spawn_target_command(&mut self, label: String, command: rustpy_deploy::CommandSpec) -> u64 {
        self.spawn(
            label.clone(),
            move || match rustpy_deploy::exec::run_checked(
                &command,
                std::time::Duration::from_secs(600),
                |_| None,
            ) {
                Ok(output) => JobResult::TargetOutput(
                    rustpy_deploy::transfer::current()
                        .owner
                        .map_or(0, |owner| owner.id),
                    label,
                    format!(
                        "{}\n{}{}",
                        command.display(),
                        output.stdout_text(),
                        output.stderr
                    ),
                ),
                Err(error) => JobResult::Message(format!("{label} failed: {error}"), false),
            },
        );
        self.jobs.last().map_or(0, |job| job.owner.id)
    }
    fn registry_login(&mut self, connection: Docker, registry: Registry, mut password: String) {
        self.spawn(format!("Docker login {}", registry.host), move || {
            let key =
                rustpy_secrets::deployment_key("docker", &registry.username, &registry.host, 443);
            let result = (|| -> anyhow::Result<()> {
                if password.is_empty() {
                    password = with_unlock(|| rustpy_secrets::get(&key))?
                        .ok_or_else(|| anyhow::anyhow!("Enter a registry password"))?;
                }
                let command = rustpy_deploy::targets::registry_login(
                    &connection,
                    &registry,
                    password.clone(),
                )?;
                rustpy_deploy::exec::run_checked(
                    &command,
                    std::time::Duration::from_secs(60),
                    |_| None,
                )?;
                if registry.save_password {
                    with_unlock(|| rustpy_secrets::set(&key, &password))?;
                } else {
                    rustpy_secrets::set_for_session(&key, &password)?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => JobResult::Message("Registry login succeeded".into(), false),
                Err(error) => JobResult::Message(format!("Registry login failed: {error}"), false),
            }
        });
    }
}
impl DeployUi {
    fn prepare_container_interpreter(
        &mut self,
        root: &Path,
        connection: Option<Docker>,
        machine: String,
        python: String,
        vagrant: Option<String>,
    ) {
        let root = root.to_path_buf();
        self.spawn(
            format!(
                "Prepare {} interpreter",
                if connection.is_some() {
                    "Docker"
                } else {
                    "Vagrant"
                }
            ),
            move || {
                let result = (|| -> anyhow::Result<rustpy_project::Interpreter> {
                    let path = rustpy_deploy::targets::install_python_adapter(
                        connection.as_ref(),
                        &machine,
                        &python,
                        vagrant.as_deref(),
                        &root,
                    )?;
                    let command = rustpy_deploy::targets::python_command(
                        connection.as_ref(),
                        &machine,
                        &python,
                        &[
                            "-I".into(),
                            "-c".into(),
                            rustpy_project::interpreter::PROBE.into(),
                        ],
                        vagrant.as_deref(),
                    )?;
                    let output = rustpy_deploy::exec::run_checked(
                        &command,
                        std::time::Duration::from_secs(30),
                        |_| None,
                    )?;
                    let mut interpreter = rustpy_project::interpreter::decode_metadata(
                        &path,
                        if connection.is_some() {
                            "Docker"
                        } else {
                            "Vagrant"
                        },
                        &output.stdout,
                    )
                    .map_err(anyhow::Error::msg)?
                    .interpreter;
                    interpreter.sys_path = vec![root.clone()];
                    Ok(interpreter)
                })();
                match result {
                    Ok(interpreter) => JobResult::InterpreterReady(interpreter, root),
                    Err(error) => {
                        JobResult::Message(format!("Target interpreter failed: {error}"), false)
                    }
                }
            },
        );
    }
}
impl DeployUi {
    fn prepare_image_interpreter(
        &mut self,
        root: &Path,
        connection: Docker,
        args: DockerArguments,
        compose: bool,
    ) {
        let root = root.to_path_buf();
        self.spawn(
            if compose {
                "Prepare Compose interpreter"
            } else {
                "Prepare Docker image interpreter"
            },
            move || {
                let result = (|| -> anyhow::Result<rustpy_project::Interpreter> {
                    let python = args
                        .command
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "python3".into());
                    let command = if compose {
                        rustpy_deploy::targets::compose_python_command(
                            &connection,
                            &args.service,
                            &python,
                            &root,
                            &args.compose_file,
                            &args.path,
                            &[],
                        )?
                    } else {
                        rustpy_deploy::targets::image_python_command(
                            &connection,
                            &args.image,
                            &python,
                            &root,
                            &args.path,
                            &[],
                        )?
                    };
                    let path = rustpy_deploy::targets::install_argv_adapter(
                        &command,
                        &root,
                        if compose { "Compose" } else { "Docker image" },
                        match (compose, args.path.as_str()) {
                            (_, path) if !path.is_empty() => Some(path),
                            (false, _) => Some("/work"),
                            (true, _) => None,
                        },
                    )?;
                    let mut probe = command;
                    probe.args.extend([
                        "-I".into(),
                        "-c".into(),
                        rustpy_project::interpreter::PROBE.into(),
                    ]);
                    let output = rustpy_deploy::exec::run_checked(
                        &probe,
                        std::time::Duration::from_secs(60),
                        |_| None,
                    )?;
                    let mut interpreter = rustpy_project::interpreter::decode_metadata(
                        &path,
                        if compose {
                            "Docker Compose"
                        } else {
                            "Docker image"
                        },
                        &output.stdout,
                    )
                    .map_err(anyhow::Error::msg)?
                    .interpreter;
                    interpreter.sys_path = vec![root.clone()];
                    Ok(interpreter)
                })();
                match result {
                    Ok(interpreter) => JobResult::InterpreterReady(interpreter, root),
                    Err(error) => {
                        JobResult::Message(format!("Container interpreter failed: {error}"), false)
                    }
                }
            },
        );
    }
}

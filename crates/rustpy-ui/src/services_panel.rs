//! Services tool window: run configurations grouped by type, running
//! processes, debug session, test runs and terminals.

use crate::a11y::AccessibleCombo;
use std::time::Duration;

use egui::{Sense, Vec2};
use rustpy_project::{RunConfig, RunConfigKind};
use rustpy_run::SessionStatus;

use crate::theme::UiTheme;
mod docker_state;
mod tabs;
pub use docker_state::DockerState;

pub enum ServicesEvent {
    Select(ServiceId),
    Add(AddService),
    RerunSession(u64),
    StopService(ServiceId),
    RestartService(ServiceId),
    ShowService(ServiceId),
    Run(RunConfig),
    Debug(RunConfig),
    Edit(RunConfig),
    Delete(String),
    StopSession(u64),
    ShowSession(u64),
    StopDebug,
    ShowDebug,
    StopTests,
    ShowTests,
    ShowTerminal,
    /// Run `docker <args>` in the background (lifecycle commands).
    Docker(Vec<String>),
    /// Run `docker <args>` in a terminal tab (logs, exec, compose up).
    DockerTerminal(String, Vec<String>),
}

pub struct ProcessRow {
    pub id: u64,
    pub name: String,
    pub status: SessionStatus,
    /// A failed presentation status can still retain a daemon/local owner.
    pub owns_process: bool,
    pub elapsed: Duration,
    pub group: String,
}

impl ProcessRow {
    fn into_resource(self, show_finished: bool) -> Option<ResourceRow> {
        if !self.owns_process && !show_finished {
            return None;
        }
        let status = match self.status {
            SessionStatus::Running => "running".into(),
            SessionStatus::Exited(code) => format!("exited ({code})"),
            SessionStatus::Killed => "stopped".into(),
            SessionStatus::Failed(error) => format!("failed: {error}"),
        };
        Some(ResourceRow {
            id: ServiceId::Process(self.id),
            name: self.name,
            group: self.group,
            status: format!("{status} · {:.1}s", self.elapsed.as_secs_f32()),
            running: self.owns_process,
            restartable: true,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServiceId {
    Configuration(String),
    Process(u64),
    Measured(u64),
    Debug(u64),
    Tests(u64),
    Database(String),
    DatabaseSession(u64),
    SettingsSync(u64, String),
    WebLanguages(Vec<String>),
    Collaboration(String),
    AiWorkers(Vec<u64>),
    RemoteDevelopment(u64, String),
    PluginJob(u64),
    WindowOperation(u64),
    DockerSnapshot(u64, String),
    Notebook(u64),
    Console(u64),
    Terminal(u64),
    Http(u64),
    WebPreview(Vec<String>),
    Deployment(u64, String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddService {
    Configuration,
    Database,
    Jupyter,
    Django,
    PythonConsole,
    Docker,
}
#[derive(Clone, Debug)]
pub struct ResourceRow {
    pub id: ServiceId,
    pub name: String,
    pub group: String,
    pub status: String,
    pub running: bool,
    pub restartable: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    #[default]
    Type,
    Status,
    Flat,
}

pub struct ServicesInput<'a> {
    pub configs: &'a [RunConfig],
    pub processes: Vec<ProcessRow>,
    pub resources: Vec<ResourceRow>,
}

#[derive(Default)]
pub struct ServicesPanel {
    tabs: Vec<(ServiceId, crate::tool_tabs::Appearance)>,
    accessibility_blocked: bool,
    menu_owner: Option<(egui::Id, bool, String)>,
    pub selected_config: Option<String>,
    pub docker: DockerState,
    pub docker_enabled: bool,
    pub selected: Option<ServiceId>,
    grouping: Grouping,
    filter: String,
    show_finished: bool,
    pub debug_input: String,
    pub debug_inputs: std::collections::HashMap<u64, String>,
}

impl ServicesPanel {
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
        if !available {
            self.menu_owner = None;
        }
    }
    pub(crate) fn claims_context_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        if self.accessibility_blocked
            || key != egui::Key::F10
            || mods != egui::Modifiers::SHIFT
            || egui::Popup::is_any_open(ctx)
        {
            return false;
        }
        let Some((focus, image, id)) = &self.menu_owner else {
            return false;
        };
        let Some(Ok(snapshot)) = &self.docker.snapshot else {
            return false;
        };
        ctx.memory(|memory| memory.has_focus(*focus))
            && if *image {
                snapshot.images.iter().any(|item| &item.id == id)
            } else {
                snapshot.containers.iter().any(|item| &item.id == id)
            }
    }

    fn docker_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme, events: &mut Vec<ServicesEvent>) {
        use rustpy_docker::{compose_args, container_args, ContainerAction};
        let now = ui.input(|i| i.time);
        let mut refresh_requested = false;
        if self.docker.pending().is_some() {
            ui.horizontal(|ui| {
                ui.label("Docker refresh is running or draining");
                if ui.small_button("Cancel refresh").clicked() {
                    self.docker.cancel();
                }
            });
        }
        if self.docker.paused() {
            ui.horizontal(|ui| {
                ui.label("Docker refresh paused");
                if ui.small_button("Refresh Docker").clicked() {
                    refresh_requested = true;
                }
            });
        }
        let open = egui::CollapsingHeader::new(egui::RichText::new("Docker").strong())
            .id_salt("svc_docker")
            .default_open(true)
            .show(ui, |ui| match &self.docker.snapshot {
                None => {
                    ui.label(
                        egui::RichText::new(if self.docker.paused() {
                            "Refresh cancelled"
                        } else {
                            "Waiting for Docker snapshot…"
                        })
                        .color(theme.text_dim),
                    );
                }
                Some(Err(e)) => {
                    ui.label(
                        egui::RichText::new(format!("Docker is not available: {e}"))
                            .color(theme.text_dim),
                    );
                    if ui.small_button("Retry").clicked() {
                        refresh_requested = true;
                    }
                }
                Some(Ok(snap)) => {
                    for warning in &snap.warnings {
                        ui.colored_label(theme.warning, warning);
                    }
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Engine {}", snap.server_version))
                                .color(theme.text_dim)
                                .small(),
                        );
                        if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                            refresh_requested = true;
                        }
                    });
                    egui::CollapsingHeader::new(format!("Containers ({})", snap.containers.len()))
                        .id_salt("svc_docker_containers")
                        .default_open(true)
                        .show(ui, |ui| {
                            for c in &snap.containers {
                                ui.push_id(("docker-container", &c.id), |ui| {
                                    ui.horizontal(|ui| {
                                        let color = if c.is_running() {
                                            theme.success
                                        } else {
                                            theme.text_dim
                                        };
                                        ui.painter().circle_filled(
                                            ui.cursor().left_top() + Vec2::new(4.0, 10.0),
                                            4.0,
                                            color,
                                        );
                                        ui.add_space(12.0);
                                        let resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&c.names).color(theme.text),
                                            )
                                            .sense(Sense::click()),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} · {}{}",
                                                c.image,
                                                c.status,
                                                if c.ports.is_empty() {
                                                    String::new()
                                                } else {
                                                    format!(" · {}", c.ports)
                                                }
                                            ))
                                            .color(theme.text_dim)
                                            .small(),
                                        );
                                        let id = c.id.chars().take(12).collect::<String>();
                                        if c.is_running() {
                                            if ui.small_button("Stop").clicked() {
                                                events.push(ServicesEvent::Docker(container_args(
                                                    ContainerAction::Stop,
                                                    &id,
                                                )));
                                            }
                                        } else if ui.small_button("Start").clicked() {
                                            events.push(ServicesEvent::Docker(container_args(
                                                ContainerAction::Start,
                                                &id,
                                            )));
                                        }
                                        if resp.has_focus() {
                                            self.menu_owner = Some((resp.id, false, c.id.clone()));
                                        }
                                        crate::a11y::keyboard_context_menu(
                                            &resp,
                                            !self.accessibility_blocked,
                                        )
                                        .show(|ui| {
                                            if ui.button("Show Log").clicked() {
                                                events.push(ServicesEvent::DockerTerminal(
                                                    format!("log: {}", c.names),
                                                    vec![
                                                        "logs".into(),
                                                        "-f".into(),
                                                        "--tail".into(),
                                                        "500".into(),
                                                        id.clone(),
                                                    ],
                                                ));
                                                ui.close();
                                            }
                                            if ui
                                                .add_enabled(
                                                    c.is_running(),
                                                    egui::Button::new("Open Shell (exec sh)"),
                                                )
                                                .clicked()
                                            {
                                                events.push(ServicesEvent::DockerTerminal(
                                                    format!("exec: {}", c.names),
                                                    vec![
                                                        "exec".into(),
                                                        "-it".into(),
                                                        id.clone(),
                                                        "sh".into(),
                                                    ],
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Restart").clicked() {
                                                events.push(ServicesEvent::Docker(container_args(
                                                    ContainerAction::Restart,
                                                    &id,
                                                )));
                                                ui.close();
                                            }
                                            if ui.button("Inspect").clicked() {
                                                events.push(ServicesEvent::DockerTerminal(
                                                    format!("inspect: {}", c.names),
                                                    vec!["inspect".into(), id.clone()],
                                                ));
                                                ui.close();
                                            }
                                            ui.separator();
                                            if ui.button("Delete").clicked() {
                                                events.push(ServicesEvent::Docker(container_args(
                                                    ContainerAction::Remove,
                                                    &id,
                                                )));
                                                ui.close();
                                            }
                                        });
                                        if resp.double_clicked() {
                                            events.push(ServicesEvent::DockerTerminal(
                                                format!("log: {}", c.names),
                                                vec![
                                                    "logs".into(),
                                                    "-f".into(),
                                                    "--tail".into(),
                                                    "500".into(),
                                                    id.clone(),
                                                ],
                                            ));
                                        }
                                    })
                                });
                            }
                        });
                    egui::CollapsingHeader::new(format!("Images ({})", snap.images.len()))
                        .id_salt("svc_docker_images")
                        .show(ui, |ui| {
                            for im in &snap.images {
                                ui.push_id(("docker-image", &im.id), |ui| {
                                    ui.horizontal(|ui| {
                                        let r = im.reference();
                                        let resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&r).color(theme.text),
                                            )
                                            .sense(Sense::click()),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} · {}",
                                                im.size, im.created_since
                                            ))
                                            .color(theme.text_dim)
                                            .small(),
                                        );
                                        if resp.has_focus() {
                                            self.menu_owner = Some((resp.id, true, im.id.clone()));
                                        }
                                        crate::a11y::keyboard_context_menu(
                                            &resp,
                                            !self.accessibility_blocked,
                                        )
                                        .show(|ui| {
                                            if ui.button("Create and Run Container").clicked() {
                                                events.push(ServicesEvent::DockerTerminal(
                                                    format!("run: {r}"),
                                                    vec![
                                                        "run".into(),
                                                        "--rm".into(),
                                                        "-it".into(),
                                                        r.clone(),
                                                    ],
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Pull").clicked() {
                                                events.push(ServicesEvent::DockerTerminal(
                                                    format!("pull: {r}"),
                                                    vec!["pull".into(), r.clone()],
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Delete").clicked() {
                                                events.push(ServicesEvent::Docker(vec![
                                                    "rmi".into(),
                                                    r.clone(),
                                                ]));
                                                ui.close();
                                            }
                                        });
                                    })
                                });
                            }
                        });
                    egui::CollapsingHeader::new(format!("Compose ({})", snap.compose.len()))
                        .id_salt("svc_docker_compose")
                        .default_open(true)
                        .show(ui, |ui| {
                            for p in &snap.compose {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&p.name).color(theme.text));
                                    ui.label(
                                        egui::RichText::new(&p.status)
                                            .color(theme.text_dim)
                                            .small(),
                                    );
                                    if ui.small_button("Up").clicked() {
                                        events.push(ServicesEvent::DockerTerminal(
                                            format!("compose up: {}", p.name),
                                            compose_args(p, "up -d"),
                                        ));
                                    }
                                    if ui.small_button("Down").clicked() {
                                        events.push(ServicesEvent::Docker(compose_args(p, "down")));
                                    }
                                    if ui.small_button("Logs").clicked() {
                                        events.push(ServicesEvent::DockerTerminal(
                                            format!("compose logs: {}", p.name),
                                            compose_args(p, "logs -f --tail 200"),
                                        ));
                                    }
                                });
                            }
                        });
                }
            });
        if refresh_requested {
            self.docker.request_refresh();
        }
        if open.fully_open() || open.openness > 0.0 {
            self.docker.refresh_if_needed(now, ui.ctx());
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        input: ServicesInput<'_>,
    ) -> Vec<ServicesEvent> {
        let mut events = Vec::new();
        self.content_tabs(ui, &input, &mut events);
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("+ Add service", |ui| {
                for (label, service) in [
                    ("Run configuration…", AddService::Configuration),
                    ("Database…", AddService::Database),
                    ("Jupyter server…", AddService::Jupyter),
                    ("Django server…", AddService::Django),
                    ("Python console", AddService::PythonConsole),
                    ("Docker connection", AddService::Docker),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        events.push(ServicesEvent::Add(service));
                        ui.close();
                    }
                }
            });
            egui::ComboBox::from_id_salt("service_grouping")
                .selected_text(match self.grouping {
                    Grouping::Type => "Group by type",
                    Grouping::Status => "Group by status",
                    Grouping::Flat => "Flat list",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.grouping, Grouping::Type, "Group by type");
                    ui.selectable_value(&mut self.grouping, Grouping::Status, "Group by status");
                    ui.selectable_value(&mut self.grouping, Grouping::Flat, "Flat list");
                })
                .combo_named(crate::i18n::tr_static("Group by"));
            ui.checkbox(&mut self.show_finished, "Show finished");
        });
        let speed = crate::tool_tabs::type_to_filter(
            ui.ctx(),
            self.menu_owner.as_ref().map(|(id, _, _)| *id),
            &mut self.filter,
        );
        let search =
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter services…"));
        if speed {
            search.request_focus();
        }
        egui::ScrollArea::vertical()
            .id_salt("services_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let filter = self.filter.to_lowercase();
                let mut groups = std::collections::BTreeMap::<String, Vec<&RunConfig>>::new();
                for config in input.configs.iter().filter(|config| {
                    filter.is_empty() || config.name.to_lowercase().contains(&filter)
                }) {
                    let group = if self.grouping == Grouping::Flat {
                        "Run configurations".into()
                    } else {
                        configuration_group(config)
                    };
                    groups.entry(group).or_default().push(config);
                }
                for (group, configs) in groups {
                    egui::CollapsingHeader::new(format!("{group} ({})", configs.len()))
                        .id_salt(("service_configs", &group))
                        .default_open(true)
                        .show(ui, |ui| {
                            for config in configs {
                                ui.push_id((&group, &config.name), |ui| {
                                    ui.horizontal(|ui| {
                                        let id = ServiceId::Configuration(config.name.clone());
                                        let response = ui.selectable_label(
                                            self.selected.as_ref() == Some(&id),
                                            &config.name,
                                        );
                                        if response.clicked() {
                                            self.selected = Some(id.clone());
                                            self.selected_config = Some(config.name.clone());
                                            events.push(ServicesEvent::Select(id));
                                        }
                                        if response.double_clicked() {
                                            events.push(ServicesEvent::Run(config.clone()));
                                        }
                                        response.context_menu(|ui| {
                                            for (label, action) in [
                                                ("Run", 0),
                                                ("Debug", 1),
                                                ("Edit…", 2),
                                                ("Remove configuration", 3),
                                            ] {
                                                if ui
                                                    .button(crate::i18n::tr_static(label))
                                                    .clicked()
                                                {
                                                    events.push(match action {
                                                        0 => ServicesEvent::Run(config.clone()),
                                                        1 => ServicesEvent::Debug(config.clone()),
                                                        2 => ServicesEvent::Edit(config.clone()),
                                                        _ => ServicesEvent::Delete(
                                                            config.name.clone(),
                                                        ),
                                                    });
                                                    ui.close();
                                                }
                                            }
                                        });
                                        if ui.small_button("▶").on_hover_text("Run").clicked() {
                                            events.push(ServicesEvent::Run(config.clone()));
                                        }
                                        if ui.small_button("Edit").clicked() {
                                            events.push(ServicesEvent::Edit(config.clone()));
                                        }
                                    });
                                });
                            }
                        });
                }
                let mut resources = input.resources;
                for process in input.processes {
                    if let Some(resource) = process.into_resource(self.show_finished) {
                        resources.push(resource);
                    }
                }
                let mut grouped = std::collections::BTreeMap::<String, Vec<ResourceRow>>::new();
                for resource in resources.into_iter().filter(|resource| {
                    filter.is_empty()
                        || resource.name.to_lowercase().contains(&filter)
                        || resource.group.to_lowercase().contains(&filter)
                }) {
                    let group = match self.grouping {
                        Grouping::Type => resource.group.clone(),
                        Grouping::Status => {
                            if resource.running {
                                "Running".into()
                            } else {
                                "Configured / stopped".into()
                            }
                        }
                        Grouping::Flat => "Services".into(),
                    };
                    grouped.entry(group).or_default().push(resource);
                }
                for (group, resources) in grouped {
                    egui::CollapsingHeader::new(format!("{group} ({})", resources.len()))
                        .id_salt(("service_resources", &group))
                        .default_open(true)
                        .show(ui, |ui| {
                            for resource in resources {
                                ui.push_id(&resource.id, |ui| {
                                    ui.horizontal(|ui| {
                                        let response = ui.selectable_label(
                                            self.selected.as_ref() == Some(&resource.id),
                                            egui::RichText::new(&resource.name).color(
                                                if resource.running {
                                                    theme.success
                                                } else {
                                                    theme.text
                                                },
                                            ),
                                        );
                                        if response.clicked() {
                                            self.selected = Some(resource.id.clone());
                                            events.push(ServicesEvent::Select(resource.id.clone()));
                                        }
                                        if response.double_clicked() {
                                            events.push(ServicesEvent::ShowService(
                                                resource.id.clone(),
                                            ));
                                        }
                                        ui.label(
                                            egui::RichText::new(&resource.status)
                                                .small()
                                                .color(theme.text_dim),
                                        );
                                        if ui
                                            .small_button("Console")
                                            .on_hover_text("Attach this service's existing console")
                                            .clicked()
                                        {
                                            self.selected = Some(resource.id.clone());
                                            events.push(ServicesEvent::Select(resource.id.clone()));
                                        }
                                        if resource.restartable
                                            && ui
                                                .small_button("↻")
                                                .on_hover_text("Rerun / reconnect")
                                                .clicked()
                                        {
                                            events.push(ServicesEvent::RestartService(
                                                resource.id.clone(),
                                            ));
                                        }
                                        if resource.running
                                            && ui
                                                .small_button("■")
                                                .on_hover_text("Stop / disconnect")
                                                .clicked()
                                        {
                                            events.push(ServicesEvent::StopService(
                                                resource.id.clone(),
                                            ));
                                        }
                                        if ui
                                            .small_button("Show")
                                            .on_hover_text("Show in Run or its owning tool window")
                                            .clicked()
                                        {
                                            events.push(ServicesEvent::ShowService(
                                                resource.id.clone(),
                                            ));
                                        }
                                    });
                                });
                            }
                        });
                }
                if self.docker_enabled {
                    self.docker_ui(ui, theme, &mut events);
                }
            });
        events
    }
}

/// Services classify launch semantics rather than configuration display names.
pub fn configuration_group(config: &RunConfig) -> String {
    match &config.kind {
        RunConfigKind::PythonModule { module }
            if module == "jupyter" || module.starts_with("jupyter.") =>
        {
            "Jupyter servers".into()
        }
        RunConfigKind::PythonModule { module }
            if module == "django" || module.starts_with("django.") =>
        {
            "Django servers".into()
        }
        RunConfigKind::Python { script }
            if script.file_name().is_some_and(|name| name == "manage.py")
                && config
                    .parameters
                    .split_whitespace()
                    .any(|arg| arg == "runserver") =>
        {
            "Django servers".into()
        }
        RunConfigKind::Program { program, .. }
            if program.file_stem().is_some_and(|name| name == "docker") =>
        {
            "Docker".into()
        }
        _ => config.kind_label().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_names_never_determine_process_identity_or_owner_kind() {
        let ids = std::collections::HashSet::from([
            ServiceId::Process(17),
            ServiceId::Debug(17),
            ServiceId::Tests(17),
            ServiceId::Notebook(17),
            ServiceId::Console(17),
            ServiceId::Terminal(17),
            ServiceId::Configuration("17".into()),
            ServiceId::Database("17".into()),
        ]);
        assert_eq!(ids.len(), 8);
        assert_ne!(ServiceId::Process(17), ServiceId::Process(18));
    }
    #[test]
    fn servers_are_grouped_by_actual_launch_semantics() {
        let mut config = RunConfig {
            name: "not a server name".into(),
            kind: RunConfigKind::Python {
                script: "manage.py".into(),
            },
            parameters: "runserver 127.0.0.1:8001".into(),
            ..Default::default()
        };
        assert_eq!(configuration_group(&config), "Django servers");
        config.parameters = "migrate".into();
        assert_ne!(configuration_group(&config), "Django servers");
        config.name = "Django server".into();
        config.kind = RunConfigKind::PythonModule {
            module: "jupyter".into(),
        };
        assert_eq!(configuration_group(&config), "Jupyter servers");
        config.kind = RunConfigKind::Program {
            program: "/usr/bin/docker".into(),
            args: vec!["logs".into(), "container-id".into()],
        };
        assert_eq!(configuration_group(&config), "Docker");
    }
}

//! Retained, project-owned HTTP executions shared by gutter, configurations and Services.
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::theme::UiTheme;
use rustpy_http::{HistoryEntry, HttpRequest, HttpResponse};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub enum HttpEvent {
    Status(String),
    Navigate(PathBuf, usize),
    CreateRequest(String),
    OpenBrowser(String),
    NetworkSettings,
}
pub struct Execution {
    pub id: u64,
    pub configuration: String,
    pub project: PathBuf,
    pub path: Option<PathBuf>,
    pub environment: String,
    pub request: HttpRequest,
    pub response: Option<HttpResponse>,
    pub job: Option<rustpy_http::runtime::Job>,
    pub started: Instant,
    global_baseline: HashMap<String, String>,
    prior_response: Option<HttpResponse>,
}
#[derive(Clone, Debug)]
pub struct RunningRequest {
    pub id: u64,
    pub configuration: String,
    pub project: PathBuf,
    pub phase: String,
    pub elapsed: Duration,
    pub stopping: bool,
    pub received: usize,
}
#[derive(Default)]
pub struct HttpPanel {
    pub trusted: bool,
    pub env_name: String,
    pub env_names: Vec<String>,
    pub env_dir: Option<PathBuf>,
    pub last_request: Option<HttpRequest>,
    pub response: Option<HttpResponse>,
    pub history: Vec<HistoryEntry>,
    pub show_headers: bool,
    pub requests: Vec<HttpRequest>,
    pub requests_path: Option<PathBuf>,
    pub executions: Vec<Execution>,
    pub selected: Option<u64>,
    next_id: u64,
    file_text: String,
    history_root: PathBuf,
    globals: HashMap<PathBuf, HashMap<String, String>>,
    notice: String,
    previous: Option<crate::diff_view::SideBySide>,
    show_diff: bool,
    history_selected: Option<usize>,
    history_other: Option<usize>,
    history_diff: Option<crate::diff_view::SideBySide>,
    pub node_program: String,
    pub grpcurl_program: String,
    create: bool,
    template: String,
    url: String,
    import_text: String,
    auth_kind: String,
    auth_json: String,
    ws_message: String,
    image: Option<(u64, egui::TextureHandle)>,
}
impl HttpPanel {
    pub fn owns_keyboard(&self) -> bool {
        self.create
    }
    pub fn is_running(&self) -> bool {
        self.executions
            .iter()
            .any(|execution| execution.job.is_some())
    }
    pub fn pending_configuration(&self, name: &str) -> bool {
        self.executions
            .iter()
            .any(|execution| execution.configuration == name && execution.job.is_some())
    }
    pub fn running(&self) -> Vec<RunningRequest> {
        self.executions
            .iter()
            .filter_map(|execution| {
                let job = execution.job.as_ref()?;
                Some(RunningRequest {
                    id: execution.id,
                    configuration: execution.configuration.clone(),
                    project: execution.project.clone(),
                    phase: job.control.phase().into(),
                    elapsed: execution.started.elapsed(),
                    stopping: job.control.cancelled(),
                    received: job
                        .control
                        .received
                        .load(std::sync::atomic::Ordering::Relaxed),
                })
            })
            .collect()
    }
    pub fn quit_stamp(&self) -> String {
        self.running()
            .iter()
            .map(|execution| {
                format!(
                    "{}:{}:{}",
                    execution.id,
                    execution.project.display(),
                    execution.stopping
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    }
    pub fn stop(&mut self, id: u64) -> bool {
        if let Some(job) = self
            .executions
            .iter()
            .find(|execution| execution.id == id)
            .and_then(|execution| execution.job.as_ref())
        {
            job.control.cancel();
            true
        } else {
            false
        }
    }
    pub fn stop_all(&mut self) {
        for execution in &self.executions {
            if let Some(job) = &execution.job {
                job.control.cancel();
            }
        }
    }
    pub fn stop_project(&mut self, root: &Path) {
        for execution in &self.executions {
            if execution.project == root {
                if let Some(job) = &execution.job {
                    job.control.cancel();
                }
            }
        }
    }
    pub fn show_execution(&mut self, id: u64) -> bool {
        let Some(execution) = self.executions.iter().find(|execution| execution.id == id) else {
            return false;
        };
        self.selected = Some(id);
        self.last_request = Some(execution.request.clone());
        self.response = execution.response.clone();
        self.previous = execution
            .prior_response
            .as_ref()
            .zip(execution.response.as_ref())
            .map(|(before, after)| crate::diff_view::SideBySide::new(&before.body, &after.body));
        self.image = None;
        true
    }
    pub fn source_matches(&self, path: &Path, text: &str) -> bool {
        self.requests_path.as_deref() == Some(path) && self.file_text == text
    }
    pub fn set_file(&mut self, path: &Path, text: &str) {
        self.requests = rustpy_http::parse(text);
        self.file_text = text.into();
        self.requests_path = Some(path.into());
        if let Some(dir) = path.parent() {
            self.env_dir = Some(dir.into());
            let root = dir
                .ancestors()
                .find(|directory| {
                    directory.join(".git").exists() || directory.join(".rustpy").exists()
                })
                .unwrap_or(dir);
            self.env_names = rustpy_http::env::project_env_names(root, dir);
            if !self.env_name.is_empty() && !self.env_names.contains(&self.env_name) {
                self.env_name.clear();
            }
        }
    }
    pub fn run_at_line(&mut self, text: &str, line: usize, root: &Path) -> Option<String> {
        let path = self
            .requests_path
            .clone()
            .unwrap_or_else(|| root.join("request.http"));
        let name = format!("HTTP {}:{}", path.display(), line + 1);
        self.run_configuration(&path, text, line, root, &name, true)
            .err()
    }
    pub fn run_configuration(
        &mut self,
        path: &Path,
        text: &str,
        line: usize,
        root: &Path,
        name: &str,
        allow_parallel: bool,
    ) -> Result<u64, String> {
        self.run_configuration_with_environment(
            path,
            text,
            line,
            root,
            name,
            allow_parallel,
            rustpy_http::runtime::ProcessEnvironment::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn run_configuration_with_environment(
        &mut self,
        path: &Path,
        text: &str,
        line: usize,
        root: &Path,
        name: &str,
        allow_parallel: bool,
        environment: rustpy_http::runtime::ProcessEnvironment,
    ) -> Result<u64, String> {
        if !allow_parallel
            && self.executions.iter().any(|execution| {
                execution.project == root
                    && execution.configuration == name
                    && execution.job.is_some()
            })
        {
            return Err(format!("HTTP configuration '{name}' is already running"));
        }
        let requests = rustpy_http::parse(text);
        let request = requests
            .into_iter()
            .find(|request| {
                let start = request.line.saturating_sub(1);
                let end = text[..usize::from(request.range.end()).min(text.len())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                line >= start && line <= end
            })
            .ok_or("No HTTP request at the configured line")?;
        self.launch(
            request,
            text,
            root,
            Some(path.into()),
            name,
            allow_parallel,
            environment,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn launch(
        &mut self,
        request: HttpRequest,
        text: &str,
        root: &Path,
        path: Option<PathBuf>,
        name: &str,
        allow_parallel: bool,
        environment: rustpy_http::runtime::ProcessEnvironment,
    ) -> Result<u64, String> {
        if !self.trusted {
            return Err("Trust the project before executing HTTP requests or scripts".into());
        }
        if self.running().len() >= 16 {
            return Err("At most 16 HTTP requests may run concurrently".into());
        }
        if !allow_parallel
            && self.executions.iter().any(|execution| {
                execution.project == root
                    && execution.configuration == name
                    && execution.job.is_some()
            })
        {
            return Err(format!("HTTP configuration '{name}' is already running"));
        }
        let base = path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.into());
        let mut options = rustpy_http::runtime::Options::new(base.clone());
        options.project = root.into();
        self.env_names = rustpy_http::env::project_env_names(root, &base);
        options.variables = rustpy_http::env::load_project_env(root, &base, &self.env_name);
        options.variables.extend(rustpy_http::file_variables(text));
        options.variables.extend(environment.variables.clone());
        options.process_environment = environment;
        options.globals = self.globals.get(root).cloned().unwrap_or_default();
        if !self.node_program.trim().is_empty() {
            options.node = self.node_program.clone();
        }
        if !self.grpcurl_program.trim().is_empty() {
            options.grpcurl = self.grpcurl_program.clone();
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("HTTP request identifiers exhausted")?;
        let id = self.next_id;
        let global_baseline = options.globals.clone();
        let prior_response = self
            .executions
            .iter()
            .rev()
            .find(|execution| {
                execution.project == root
                    && execution.configuration == name
                    && execution.response.is_some()
            })
            .and_then(|execution| execution.response.clone());
        self.executions.push(Execution {
            id,
            configuration: name.into(),
            project: root.into(),
            path,
            environment: self.env_name.clone(),
            request: request.clone(),
            response: None,
            job: Some(rustpy_http::runtime::Job::spawn(request, options)),
            started: Instant::now(),
            global_baseline,
            prior_response,
        });
        self.selected = Some(id);
        self.response = None;
        self.image = None;
        while self.executions.len() > 64 {
            if let Some(index) = self.executions.iter().position(|execution| {
                execution.job.is_none() && Some(execution.id) != self.selected
            }) {
                self.executions.remove(index);
            } else {
                break;
            }
        }
        Ok(id)
    }
    pub fn run(&mut self, request: HttpRequest, text: &str, root: &Path) {
        let name = request
            .name
            .clone()
            .unwrap_or_else(|| format!("{} {}", request.method, request.url));
        if let Err(error) = self.launch(
            request,
            text,
            root,
            self.requests_path.clone(),
            &name,
            true,
            rustpy_http::runtime::ProcessEnvironment::default(),
        ) {
            self.notice = error;
        }
    }
    pub fn poll(&mut self, root: &Path) -> bool {
        let mut changed = false;
        for execution in &mut self.executions {
            let result = execution
                .job
                .as_ref()
                .and_then(|job| match job.try_result() {
                    Ok(Some(result)) => Some(Ok(result)),
                    Err(error) => Some(Err(error)),
                    _ => None,
                });
            if let Some(result) = result {
                let cancelled = execution
                    .job
                    .as_ref()
                    .is_some_and(|job| job.control.cancelled());
                execution.job = None;
                let mut completed = match result {
                    Ok(result) => result,
                    Err(error) => rustpy_http::runtime::Completed {
                        request: execution.request.clone(),
                        response: rustpy_http::runtime::error(error),
                        globals: HashMap::new(),
                    },
                };
                if cancelled {
                    completed.response = rustpy_http::runtime::error("Request cancelled");
                } else {
                    let globals = self.globals.entry(execution.project.clone()).or_default();
                    rustpy_http::runtime::merge_globals(
                        globals,
                        &execution.global_baseline,
                        completed.globals,
                    );
                    if let Err(error) = rustpy_http::history::append_named_history(
                        &execution.project,
                        &completed.request,
                        &completed.response,
                        Some(execution.configuration.clone()),
                        Some(execution.id),
                    ) {
                        self.notice = format!("Cannot save HTTP history: {error}");
                    }
                }
                execution.request = completed.request;
                execution.response = Some(completed.response.clone());
                if self.selected == Some(execution.id) {
                    if let Some(previous) = &execution.prior_response {
                        self.previous = Some(crate::diff_view::SideBySide::new(
                            &previous.body,
                            &completed.response.body,
                        ));
                    }
                    self.last_request = Some(execution.request.clone());
                    self.response = Some(completed.response);
                }
                changed = true;
            }
        }
        let mut retained = self
            .executions
            .iter()
            .filter_map(|execution| execution.response.as_ref())
            .map(|response| response.bytes.len() + response.body.len())
            .sum::<usize>();
        while retained > 64 * 1024 * 1024 {
            let Some(index) = self.executions.iter().position(|execution| {
                execution.job.is_none() && Some(execution.id) != self.selected
            }) else {
                break;
            };
            let removed = self.executions.remove(index);
            retained = retained.saturating_sub(
                removed
                    .response
                    .as_ref()
                    .map(|response| response.bytes.len() + response.body.len())
                    .unwrap_or(0),
            );
        }
        if self.history_root != root || changed {
            self.history_root = root.into();
            self.history = rustpy_http::load_history(root, 100);
        }
        changed
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        root: &Path,
    ) -> Vec<HttpEvent> {
        let mut events = Vec::new();
        ui.horizontal_wrapped(|ui| {
            ui.heading("HTTP Client");
            egui::ComboBox::from_id_salt("http-environment")
                .selected_text(if self.env_name.is_empty() {
                    "<none>"
                } else {
                    &self.env_name
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.env_name, String::new(), "<none>");
                    for name in &self.env_names {
                        ui.selectable_value(&mut self.env_name, name.clone(), name);
                    }
                })
                .combo_named(crate::i18n::tr_static("Environment"));
            if ui.button("Create request / examples").clicked() {
                self.create = true;
            }
            if ui.button("Proxy / SSL settings").clicked() {
                events.push(HttpEvent::NetworkSettings);
            }
            if ui.button("Show HTTP Requests History").clicked() {
                self.history = rustpy_http::load_history(root, 100);
            }
            ui.checkbox(&mut self.show_headers, "Headers");
        });
        for run in self.running() {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        self.selected == Some(run.id),
                        format!(
                            "#{} {} · {} · {:.1}s · {} bytes{}",
                            run.id,
                            run.configuration,
                            run.phase,
                            run.elapsed.as_secs_f64(),
                            run.received,
                            if run.stopping { " · stopping" } else { "" }
                        ),
                    )
                    .clicked()
                {
                    self.show_execution(run.id);
                }
                if ui.button(format!("Stop #{}", run.id)).clicked() {
                    self.stop(run.id);
                }
            });
        }
        if self.is_running() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        ui.horizontal_wrapped(|ui| {
            for execution in &self.executions {
                if execution.job.is_none()
                    && ui
                        .selectable_label(
                            self.selected == Some(execution.id),
                            format!("#{} {}", execution.id, execution.configuration),
                        )
                        .clicked()
                {
                    self.selected = Some(execution.id);
                    self.response = execution.response.clone();
                    self.last_request = Some(execution.request.clone());
                    self.previous = execution
                        .prior_response
                        .as_ref()
                        .zip(execution.response.as_ref())
                        .map(|(before, after)| {
                            crate::diff_view::SideBySide::new(&before.body, &after.body)
                        });
                    self.image = None;
                }
            }
        });
        if !self.notice.is_empty() {
            ui.colored_label(theme.warning, &self.notice);
        }
        egui::CollapsingHeader::new("Requests — Run with environment")
            .default_open(true)
            .show(ui, |ui| {
                for request in self.requests.clone() {
                    ui.horizontal(|ui| {
                        if ui.button("▶").clicked() {
                            let text = self.file_text.clone();
                            self.run(request.clone(), &text, root);
                        }
                        if ui
                            .link(
                                request.name.clone().unwrap_or_else(|| {
                                    format!("{} {}", request.method, request.url)
                                }),
                            )
                            .clicked()
                        {
                            if let Some(path) = &self.requests_path {
                                events.push(HttpEvent::Navigate(path.clone(), request.line));
                            }
                        }
                    });
                }
            });
        if let Some(execution) = self
            .selected
            .and_then(|id| self.executions.iter().find(|execution| execution.id == id))
        {
            if let Some(job) = &execution.job {
                ui.label(format!(
                    "{} bytes received",
                    job.control
                        .received
                        .load(std::sync::atomic::Ordering::Relaxed)
                ));
                if execution.request.method == "WEBSOCKET" {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.ws_message);
                        if ui.button("Send WebSocket message").clicked() {
                            job.control.send(std::mem::take(&mut self.ws_message));
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for message in job.control.messages() {
                                ui.monospace(message);
                            }
                        });
                }
            }
        }
        if let Some(response) = &self.response {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} ms · {} bytes",
                    response.status_line(),
                    response.elapsed.as_millis(),
                    response.size
                ))
                .strong(),
            );
            if let Some(error) = &response.error {
                ui.colored_label(theme.error, error);
            }
            ui.horizontal(|ui| {
                if let Some(request) = &self.last_request {
                    if ui.button("Copy as cURL").clicked() {
                        ui.ctx().copy_text(rustpy_http::workspace::to_curl(request));
                    }
                    if ui.button("Open in browser").clicked() {
                        events.push(HttpEvent::OpenBrowser(request.url.clone()));
                    }
                }
                ui.checkbox(&mut self.show_diff, "Diff with previous");
            });
            if self.show_headers {
                for (key, value) in &response.headers {
                    ui.monospace(format!("{key}: {value}"));
                }
            }
            for test in &response.tests {
                ui.colored_label(
                    if test.passed {
                        theme.success
                    } else {
                        theme.error
                    },
                    format!(
                        "{} {} {}",
                        if test.passed { "PASS" } else { "FAIL" },
                        test.name,
                        test.message
                    ),
                );
            }
            for log in &response.logs {
                ui.label(log);
            }
            if response.content_type.starts_with("image/") {
                let id = self.selected.unwrap_or(0);
                if !self.image.as_ref().is_some_and(|(owner, _)| *owner == id) {
                    let decoded = (|| {
                        let mut reader =
                            image::ImageReader::new(std::io::Cursor::new(&response.bytes))
                                .with_guessed_format()
                                .ok()?;
                        let mut limits = image::Limits::default();
                        limits.max_image_width = Some(8192);
                        limits.max_image_height = Some(8192);
                        limits.max_alloc = Some(64 * 1024 * 1024);
                        reader.limits(limits);
                        reader.decode().ok()
                    })();
                    if let Some(image) = decoded {
                        let image = image.into_rgba8();
                        let pixels = egui::ColorImage::from_rgba_unmultiplied(
                            [image.width() as usize, image.height() as usize],
                            image.as_raw(),
                        );
                        self.image = Some((
                            id,
                            ui.ctx().load_texture(
                                "http-response-image",
                                pixels,
                                egui::TextureOptions::LINEAR,
                            ),
                        ));
                    }
                }
                if let Some((_, texture)) = &self.image {
                    ui.image((texture.id(), texture.size_vec2()));
                }
            }
            if self.show_diff {
                if let Some(diff) = &self.previous {
                    diff.ui(
                        ui,
                        theme,
                        mono.clone(),
                        "Previous response",
                        "Current response",
                    );
                }
            } else {
                egui::ScrollArea::both().max_height(400.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&response.body).font(mono.clone()))
                            .selectable(true),
                    );
                });
            }
        }
        self.history_ui(ui, theme, mono.clone());
        self.create_ui(ui, &mut events);
        events
    }
    fn history_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme, mono: egui::FontId) {
        egui::CollapsingHeader::new("Services → HTTP Client history / Compare responses").show(
            ui,
            |ui| {
                for (index, entry) in self.history.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(
                                self.history_selected == Some(index),
                                format!(
                                    "{} {} {} · {} ms",
                                    entry.status, entry.method, entry.url, entry.elapsed_ms
                                ),
                            )
                            .clicked()
                        {
                            self.history_selected = Some(index);
                            if let Some(response) = &entry.response {
                                self.response = Some(response.clone());
                                self.last_request = entry.request.clone();
                                self.image = None;
                            }
                        }
                        if ui.small_button("Compare response").clicked() {
                            self.history_other = Some(index);
                            if let Some(first) = self
                                .history_selected
                                .and_then(|index| self.history.get(index))
                                .and_then(|entry| entry.response.as_ref())
                            {
                                if let Some(second) = &entry.response {
                                    self.history_diff = Some(crate::diff_view::SideBySide::new(
                                        &first.body,
                                        &second.body,
                                    ));
                                }
                            }
                        }
                    });
                }
                if let Some(diff) = &self.history_diff {
                    diff.ui(
                        ui,
                        theme,
                        mono,
                        "Selected history response",
                        "Compared response",
                    );
                }
            },
        );
    }
    fn create_ui(&mut self, ui: &mut egui::Ui, events: &mut Vec<HttpEvent>) {
        if !self.create {
            return;
        }
        let mut open = true;
        egui::Window::new("Create HTTP request / Auth helpers / Convert cURL").open(&mut open).show_modeless(ui.ctx(),|ui|{if self.template.is_empty(){self.template="gtr".into();}
if self.url.is_empty(){self.url="http://localhost:8000/".into();}egui::ComboBox::from_id_salt("http-template").selected_text(&self.template).show_ui(ui,|ui|{for name in ["gtr","ptr","mptr","GraphQL","WebSocket","gRPC"]{ui.selectable_value(&mut self.template,name.into(),crate::i18n::tr_static(name));}}).combo_named(crate::i18n::tr_static("Request template"));ui.text_edit_singleline(&mut self.url);if ui.button("Create request").clicked(){match rustpy_http::workspace::template(&self.template,&self.url){Ok(text)=>events.push(HttpEvent::CreateRequest(text)),Err(error)=>self.notice=error}}
  ui.separator();ui.label("cURL / Postman collection / OpenAPI JSON or YAML");ui.add(egui::TextEdit::multiline(&mut self.import_text).desired_rows(5));ui.horizontal(|ui|{if ui.button("Paste cURL").clicked(){if let Ok(mut clipboard)=crate::system_clipboard::open(){if let Ok(text)=clipboard.get_text(){self.import_text=text;}}}for(label,kind)in [("Convert cURL",0),("Import OpenAPI",1),("Import Postman",2)]{if ui.button(crate::i18n::tr_static(label)).clicked(){let result=match kind{0=>rustpy_http::workspace::from_curl(&self.import_text).map(|request|vec![request]),1=>rustpy_http::workspace::openapi(&self.import_text),_=>rustpy_http::workspace::postman(&self.import_text)};match result{Ok(requests)=>events.push(HttpEvent::CreateRequest(requests.iter().map(rustpy_http::workspace::to_http).collect::<Vec<_>>().join("\n"))),Err(error)=>self.notice=error}}}});
  ui.separator();let auth_label=ui.label("Auth helper (JSON fields allow {{environment_variables}})");egui::ComboBox::from_id_salt("http-auth").selected_text(&self.auth_kind).show_ui(ui,|ui|{for kind in ["basic","digest","bearer","oauth2","aws"]{if ui.selectable_label(self.auth_kind==kind,crate::i18n::tr_static(kind)).clicked(){self.auth_kind=kind.into();self.auth_json=match kind{"basic"|"digest"=>format!(r#"{{"type":"{kind}","username":"{{{{username}}}}","password":"{{{{password}}}}"}}"#),"bearer"=>r#"{"type":"bearer","token":"{{token}}"}"#.into(),"oauth2"=>r#"{"type":"oauth2","grant_type":"client_credentials","token_url":"{{token_url}}","client_id":"{{client_id}}","client_secret":"{{client_secret}}"}"#.into(),_=>r#"{"type":"aws","access_key":"{{access_key}}","secret_key":"{{secret_key}}","region":"us-east-1","service":"execute-api"}"#.into()};}}}).combo_labelled(&auth_label);ui.add(egui::TextEdit::multiline(&mut self.auth_json).desired_rows(3));if ui.button("Create authenticated request").clicked(){if serde_json::from_str::<serde_json::Value>(&self.auth_json).is_ok(){events.push(HttpEvent::CreateRequest(format!("### Authenticated request\n# @auth {}\nGET {}\n\n",self.auth_json.replace('\n'," "),self.url)));}else{self.notice="Authentication JSON is invalid".into();}}ui.label("OAuth2 grant_type also supports password, authorization_code (+ code/redirect_uri/code_verifier), and refresh_token.");ui.label("JavaScript runtime program");ui.text_edit_singleline(&mut self.node_program);ui.label("gRPC grpcurl program");ui.text_edit_singleline(&mut self.grpcurl_program);
 });
        self.create = open;
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    #[test]
    fn captured_environment_does_not_leak_and_same_name_other_root_is_independent() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.http");
        let text = "GET invalid://{{owner}}\n";
        let mut panel = HttpPanel {
            trusted: true,
            ..Default::default()
        };
        let context = rustpy_http::runtime::ProcessEnvironment {
            inherit: false,
            variables: [("owner".into(), "captured".into())].into(),
            working_dir: Some(dir.path().into()),
        };
        let captured = panel
            .run_configuration_with_environment(&path, text, 0, dir.path(), "Same", false, context)
            .unwrap();
        assert!(panel
            .run_configuration(&path, text, 0, dir.path(), "Same", false)
            .is_err());
        let ordinary = panel
            .run_configuration(
                &other.path().join("requests.http"),
                text,
                0,
                other.path(),
                "Same",
                false,
            )
            .unwrap();
        assert_ne!(captured, ordinary);
        let deadline = Instant::now() + Duration::from_secs(3);
        while panel.is_running() && Instant::now() < deadline {
            panel.poll(dir.path());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !panel.is_running(),
            "unsupported schemes must retire without network access"
        );
        let configured = panel
            .executions
            .iter()
            .find(|run| run.id == captured)
            .unwrap();
        let unconfigured = panel
            .executions
            .iter()
            .find(|run| run.id == ordinary)
            .unwrap();
        assert_eq!(configured.request.url, "invalid://captured");
        assert_eq!(unconfigured.request.url, "invalid://{{owner}}");
        assert!(unconfigured
            .response
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("unresolved"));
        assert!(panel.show_execution(captured));
        assert_eq!(panel.selected, Some(captured));
    }
}

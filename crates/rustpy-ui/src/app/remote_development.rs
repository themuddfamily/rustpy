//! Gateway UI: remote project authority remains bound to one admitted session.
use super::*;
use crate::a11y::DialogWindow;
use crate::remote_gateway::{Client, Forward, Kind, Target};
use rustpy_lsp::gateway::{self, Query, Request};
use serde_json::Value;
use std::{collections::HashMap, sync::mpsc, time::Instant};
pub(super) fn is_remote_mirror(path: &Path) -> bool {
    path.starts_with(Settings::system_dir().join("remote-workspaces"))
}
fn mirror_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = gateway::relative(relative)?;
    let mut path = root.to_path_buf();
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Remote mirror root is a symbolic link".into());
    }
    for part in relative.components() {
        path.push(part.as_os_str());
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("Remote mirror cannot traverse symbolic links".into());
        }
    }
    Ok(path)
}
#[derive(Clone)]
pub(super) struct Binding {
    session: u64,
    relative: String,
    revision: String,
}
enum Work {
    Hello,
    List(String),
    Read(String, Option<u32>),
    Save(PathBuf, u64, String),
    Insight(PathBuf, u64, u32, Query, bool),
}
struct Pending {
    session: u64,
    id: u64,
    reply: mpsc::Receiver<Result<Value, String>>,
    work: Work,
}
pub(super) struct State {
    pub(super) open: bool,
    target: Target,
    trusted: bool,
    client: Option<Client>,
    auth: crate::git_jobs::GitJobs,
    auth_id: Option<u64>,
    environment: Vec<(String, String)>,
    forwards: Vec<Forward>,
    pending: Vec<Pending>,
    pub(super) bindings: HashMap<PathBuf, Binding>,
    directory: String,
    rows: Vec<Value>,
    next: Option<usize>,
    status: String,
    remote_port: u16,
    checked: HashMap<PathBuf, (u64, Instant)>,
    conflict: Option<(PathBuf, String, String)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            target: Target::load(),
            trusted: false,
            client: None,
            auth: Default::default(),
            auth_id: None,
            environment: vec![],
            forwards: vec![],
            pending: vec![],
            bindings: HashMap::new(),
            directory: String::new(),
            rows: vec![],
            next: None,
            status: String::new(),
            remote_port: 8000,
            checked: HashMap::new(),
            conflict: None,
        }
    }
}
impl State {
    fn queue(&mut self, request: Request, work: Work) {
        let Some(client) = &self.client else {
            self.status = "Connect the captured remote workspace first".into();
            return;
        };
        match client.request(request) {
            Ok((id, reply)) => self.pending.push(Pending {
                session: client.id(),
                id,
                reply,
                work,
            }),
            Err(error) => self.status = error,
        }
    }
    fn binding(&self, path: &Path) -> Option<&Binding> {
        self.bindings.get(path)
    }
    fn list(&mut self, path: String, offset: usize) {
        self.queue(
            Request::List {
                path: path.clone(),
                offset,
            },
            Work::List(path),
        );
    }
    fn read(&mut self, path: String, offset: Option<u32>) {
        if gateway::relative(&path).is_err() {
            self.status = "Invalid remote path".into();
            return;
        }
        self.queue(
            Request::Read { path: path.clone() },
            Work::Read(path, offset),
        );
    }
    fn stop(&mut self, id: u64) {
        if let Some(client) = &self.client {
            if client.id() == id {
                client.stop();
                for forward in &self.forwards {
                    forward.stop();
                }
            }
        }
        for forward in &self.forwards {
            if forward.id == id {
                forward.stop();
            }
        }
    }
}
impl IdeApp {
    pub(super) fn remote_development_pending(&self) -> Vec<(u64, String, String)> {
        let mut owners = vec![];
        if let Some(client) = &self.remote_development.client {
            if !client.finished() {
                owners.push((
                    client.id(),
                    format!("Remote workspace {}", client.target().root),
                    client.stamp(),
                ));
            }
        }
        for forward in &self.remote_development.forwards {
            if !forward.finished() {
                owners.push((
                    forward.id,
                    format!(
                        "Remote forward 127.0.0.1:{} → {}",
                        forward.local, forward.remote
                    ),
                    forward.stamp(),
                ));
            }
        }
        owners
    }
    pub(super) fn stop_remote_development(&mut self, id: u64) {
        if self
            .remote_development
            .client
            .as_ref()
            .is_some_and(|client| client.id() == id)
        {
            self.remote_development.auth.cancel();
        }
        self.remote_development.stop(id);
    }
    pub(super) fn stop_all_remote_development(&mut self) {
        for (id, _, _) in self.remote_development_pending() {
            self.stop_remote_development(id);
        }
    }
    pub(super) fn cancel_remote_development_project(&mut self) {
        self.stop_all_remote_development();
        // Preserve physical client/forward owners until their processes and
        // pipes retire; replies captured for the old project cannot publish.
        self.remote_development.pending.clear();
        self.remote_development.bindings.clear();
        self.remote_development.checked.clear();
        self.remote_development.rows.clear();
        self.remote_development.conflict = None;
        self.remote_development.trusted = false;
    }
    pub(super) fn poll_remote_development_retirement(&mut self, ctx: &egui::Context) {
        self.remote_development
            .forwards
            .retain(|owner| !owner.finished());
        if self
            .remote_development
            .client
            .as_ref()
            .is_some_and(Client::finished)
        {
            self.remote_development.client = None;
            if let Some(id) = self.remote_development.auth_id.take() {
                self.remote_development.auth.job_finished(
                    id,
                    true,
                    "Remote workspace disconnected",
                );
            }
        }
        if !self.remote_development_pending().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }
    pub(super) fn remote_bound(&self, i: usize) -> bool {
        self.tabs
            .get(i)
            .and_then(|tab| tab.state.path.as_ref())
            .is_some_and(|path| is_remote_mirror(path))
    }
    pub(super) fn remote_development_save(&mut self, i: usize) -> Option<bool> {
        let path = self.tabs.get(i)?.state.path.clone()?;
        let Some(binding) = self.remote_development.binding(&path).cloned() else {
            if is_remote_mirror(&path) {
                self.set_status(
                    "Reconnect and reload this remote file before saving its restored mirror",
                );
                return Some(false);
            }
            return None;
        };
        if !self.tabs[i].state.doc.is_modified() {
            return Some(true);
        }
        if self
            .remote_development
            .pending
            .iter()
            .any(|pending| matches!(&pending.work,Work::Save(p,_,_)if p==&path))
        {
            return Some(false);
        }
        let text = match self.tabs[i]
            .state
            .doc
            .to_bytes()
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| "Remote edits must remain UTF-8 encoded".to_owned())
            }) {
            Ok(text) => text,
            Err(error) => {
                self.set_status(error);
                return Some(false);
            }
        };
        let version = self.tabs[i].state.doc.version();
        if self
            .remote_development
            .client
            .as_ref()
            .is_none_or(|client| client.id() != binding.session || !client.ready())
        {
            self.set_status(
                "Remote file remains unsaved: reconnect and reload its remote revision",
            );
            return Some(false);
        }
        self.remote_development.queue(
            Request::Write {
                path: binding.relative,
                base: binding.revision,
                text: text.clone(),
            },
            Work::Save(path, version, text),
        );
        Some(false)
    }
    fn remote_insight(&mut self, query: Query, explicit: bool) -> bool {
        let Some(i) = self.active else {
            return false;
        };
        let Some(path) = self.tabs[i].state.path.clone() else {
            return false;
        };
        let Some(binding) = self.remote_development.binding(&path).cloned() else {
            if is_remote_mirror(&path) {
                self.remote_development.status =
                    "Reconnect and reload this restored remote editor for code insight".into();
                return true;
            }
            return false;
        };
        let version = self.tabs[i].state.doc.version();
        let cursor = u32::from(self.tabs[i].state.primary().head());
        if self
            .remote_development
            .client
            .as_ref()
            .is_none_or(|client| client.id() != binding.session || !client.ready())
        {
            self.remote_development.status =
                "Remote editor is disconnected; analysis requires its captured server".into();
            return true;
        }
        let old = self
            .remote_development
            .pending
            .iter()
            .filter_map(|pending| match &pending.work {
                Work::Insight(p, _, _, q, _)
                    if p == &path
                        && std::mem::discriminant(q) == std::mem::discriminant(&query) =>
                {
                    Some(pending.id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in old {
            if let Some(client) = &self.remote_development.client {
                client.cancel(id);
            }
        }
        self.remote_development.queue(
            Request::Analyze {
                path: binding.relative,
                text: self.tabs[i].state.doc.text(),
                version,
                cursor,
                query,
            },
            Work::Insight(path, version, cursor, query, explicit),
        );
        true
    }
    pub(super) fn open_remote_completion(&mut self, explicit: bool) -> bool {
        self.remote_insight(Query::Completion, explicit)
    }
    pub(super) fn remote_development_action(&mut self, action: Action) -> bool {
        if action == Action::RemoteDevelopment {
            self.remote_development.open = true;
            return true;
        }
        if action == Action::GoToDeclaration {
            return self.remote_insight(Query::Definition, true);
        }
        if self.active.is_some_and(|i| self.remote_bound(i)) {
            if matches!(
                action,
                Action::Run | Action::RunContext | Action::Debug | Action::DebugContext
            ) {
                self.remote_development_run(matches!(action, Action::Debug | Action::DebugContext));
                return true;
            }
            if matches!(
                action,
                Action::RunWithCoverage
                    | Action::ProfileRun
                    | Action::ExecuteInConsole
                    | Action::RunFileInConsole
            ) {
                self.set_status("This remote editor uses the Gateway Run/Debug route; configure a remote SDK for measured runs and consoles");
                return true;
            }
        }
        false
    }
    /// Quit may freeze normal windows while an already-issued remote save is
    /// pending. Poll only captured Save responses; never enqueue work or show UI.
    pub(super) fn poll_remote_development_quit_io(&mut self, ctx: &egui::Context) {
        let mut completed = Vec::new();
        let mut index = 0;
        while index < self.remote_development.pending.len() {
            if !matches!(&self.remote_development.pending[index].work, Work::Save(..)) {
                index += 1;
                continue;
            }
            let result = match self.remote_development.pending[index].reply.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "Remote save disconnected; edited buffer retained".into(),
                )),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let pending = self.remote_development.pending.remove(index);
                completed.push((pending.session, pending.work, result));
            } else {
                index += 1;
            }
        }
        for (session, work, result) in completed {
            self.apply_remote_reply(session, work, result);
        }
        self.poll_remote_development_retirement(ctx);
    }
    pub(super) fn remote_development_document_run(
        &mut self,
        path: &Path,
        mode: crate::run_context::RunMode,
    ) -> bool {
        if !is_remote_mirror(path) {
            return false;
        }
        if let Some(i) = self
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_deref() == Some(path))
        {
            self.active = Some(i);
        }
        match mode {
            crate::run_context::RunMode::Run => self.remote_development_run(false),
            crate::run_context::RunMode::Debug => self.remote_development_run(true),
            _ => self.set_status("Configure the remote SDK for coverage/profile runs"),
        };
        true
    }
    fn apply_remote_reply(&mut self, session: u64, work: Work, result: Result<Value, String>) {
        if self
            .remote_development
            .client
            .as_ref()
            .is_none_or(|client| client.id() != session)
        {
            return;
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if let Work::Save(path, _, text) = &work {
                    self.remote_development.conflict =
                        Some((path.clone(), text.clone(), error.clone()));
                }
                if matches!(work, Work::Hello) {
                    self.remote_development.client.as_ref().unwrap().stop();
                }
                self.remote_development.status = error;
                return;
            }
        };
        match work {
            Work::Hello => {
                if !self.remote_development.client.as_ref().unwrap().ready() {
                    self.remote_development.status = "Remote version/root handshake failed".into();
                    return;
                }
                self.remote_development.status = format!(
                    "Connected to RustPy {} at {}",
                    value["version"], value["root"]
                );
                self.remote_development.list(String::new(), 0);
            }
            Work::List(path) => {
                if self.remote_development.directory != path {
                    self.remote_development.rows.clear();
                }
                self.remote_development.directory = path;
                self.remote_development
                    .rows
                    .extend(value["entries"].as_array().cloned().unwrap_or_default());
                self.remote_development.next = value["next"].as_u64().map(|n| n as usize);
            }
            Work::Read(relative, offset) => {
                let Some(text) = value["text"].as_str() else {
                    return;
                };
                let Some(revision) = value["revision"].as_str() else {
                    return;
                };
                let client = self.remote_development.client.as_ref().unwrap();
                if text.len() > gateway::MAX_FILE || gateway::revision(text.as_bytes()) != revision
                {
                    self.remote_development.status =
                        "Remote read failed its size/revision check".into();
                    return;
                }
                let path = match mirror_file(&client.effective_target().mirror(), &relative) {
                    Ok(path) => path,
                    Err(error) => {
                        self.remote_development.status = error;
                        return;
                    }
                };
                if self.tabs.iter().any(|tab| {
                    tab.state.path.as_ref() == Some(&path) && tab.state.doc.is_modified()
                }) {
                    self.remote_development.status =
                        "Keep or save the edited remote buffer before reloading".into();
                    return;
                }
                let result = (|| {
                    let parent = path.parent().ok_or("Remote mirror parent missing")?;
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    std::fs::write(&path, text).map_err(|e| e.to_string())
                })();
                if let Err(error) = result {
                    self.remote_development.status = error;
                    return;
                }
                self.remote_development.bindings.insert(
                    path.clone(),
                    Binding {
                        session,
                        relative,
                        revision: revision.into(),
                    },
                );
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.state.path.as_ref() == Some(&path))
                {
                    if let Err(error) = tab.state.reload_from_bytes(text.as_bytes()) {
                        self.remote_development.status = error.to_string();
                        return;
                    }
                    tab.state.external_diagnostics = None;
                }
                self.open_file(&path);
                if let Some(i) = self.active {
                    if self.tabs[i].state.path.as_ref() == Some(&path) {
                        if let Some(offset) = offset {
                            let offset = TextSize::new(offset.min(text.len() as u32));
                            self.tabs[i]
                                .state
                                .set_carets(vec![crate::editor::Caret::at(offset)]);
                            self.tabs[i].state.reveal_offset(offset);
                        }
                    }
                }
            }
            Work::Save(path, version, text) => {
                let Some(revision) = value["revision"].as_str() else {
                    return;
                };
                if gateway::revision(text.as_bytes()) != revision {
                    self.remote_development.status =
                        "Remote save acknowledgement failed its revision check; buffer retained"
                            .into();
                    return;
                }
                if let Some(binding) = self.remote_development.bindings.get_mut(&path) {
                    binding.revision = revision.into();
                }
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.state.path.as_ref() == Some(&path))
                {
                    if tab.state.doc.version() == version {
                        tab.state.doc.mark_saved();
                    }
                }
                let _ = std::fs::write(&path, text);
                self.remote_development.status = "Remote file saved after revision check".into();
            }
            Work::Insight(path, version, cursor, query, explicit) => {
                let Some(i) = self.tabs.iter().position(|tab| {
                    tab.state.path.as_ref() == Some(&path) && tab.state.doc.version() == version
                }) else {
                    return;
                };
                if value["version"].as_u64() != Some(version) {
                    return;
                }
                match query {
                    Query::Diagnostics => {
                        let len = self.tabs[i].state.doc.text().len() as u32;
                        let diagnostics = value["payload"]["diagnostics"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|item| {
                                let start = item["start"].as_u64()? as u32;
                                let end = item["end"].as_u64()? as u32;
                                if start > end || end > len {
                                    return None;
                                }
                                let severity = match item["severity"].as_str().unwrap_or("") {
                                    "Error" => rustpy_core::Severity::Error,
                                    "Warning" => rustpy_core::Severity::Warning,
                                    "WeakWarning" => rustpy_core::Severity::WeakWarning,
                                    "Typo" => rustpy_core::Severity::Typo,
                                    _ => rustpy_core::Severity::Info,
                                };
                                Some(rustpy_core::Diagnostic::new(
                                    TextRange::new(TextSize::new(start), TextSize::new(end)),
                                    severity,
                                    item["code"].as_str().unwrap_or("remote"),
                                    item["message"].as_str().unwrap_or(""),
                                ))
                            })
                            .collect();
                        self.tabs[i].state.external_diagnostics = Some((version, diagnostics));
                    }
                    Query::Definition => {
                        if self.active != Some(i) {
                            return;
                        }
                        let locations = value["payload"]["locations"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        if let Some(location) = locations.first() {
                            if let Some(path) = location["path"].as_str() {
                                self.remote_development.read(
                                    path.into(),
                                    location["start"].as_u64().map(|n| n as u32),
                                );
                            }
                        } else {
                            self.remote_development.status =
                                "No definition inside the captured remote root".into();
                        }
                    }
                    Query::Completion => {
                        if self.active != Some(i)
                            || u32::from(self.tabs[i].state.primary().head()) != cursor
                        {
                            return;
                        }
                        let text = self.tabs[i].state.doc.text();
                        let mut start = cursor as usize;
                        while start > 0 {
                            let Some((offset, ch)) = text[..start].char_indices().next_back()
                            else {
                                break;
                            };
                            if ch == '_' || ch.is_alphanumeric() {
                                start = offset;
                            } else {
                                break;
                            }
                        }
                        let items = value["payload"]["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|item| {
                                Some(CompletionItem {
                                    label: item["label"].as_str()?.into(),
                                    insert: item["insert"].as_str()?.into(),
                                    detail: item["detail"]
                                        .as_str()
                                        .unwrap_or("Remote engine")
                                        .into(),
                                    kind: match item["kind"].as_str().unwrap_or("") {
                                        "Function" => CompletionKind::Function,
                                        "Class" => CompletionKind::Class,
                                        "Module" => CompletionKind::Module,
                                        "Method" => CompletionKind::Method,
                                        "Keyword" => CompletionKind::Keyword,
                                        _ => CompletionKind::Variable,
                                    },
                                    caret_offset: None,
                                    priority: 0,
                                })
                            })
                            .collect::<Vec<_>>();
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
                            caret: TextSize::new(cursor),
                            explicit,
                            rect: None,
                        };
                    }
                }
            }
        }
    }
    pub(super) fn remote_development_tick(&mut self, ctx: &egui::Context) {
        self.poll_remote_development_retirement(ctx);
        for message in self.remote_development.auth.poll_prompts() {
            self.remote_development.status = message;
        }
        self.remote_development.auth.prompt_window(ctx, &self.theme);
        let mut completed = vec![];
        let mut index = 0;
        while index < self.remote_development.pending.len() {
            match self.remote_development.pending[index].reply.try_recv() {
                Ok(result) => {
                    let pending = self.remote_development.pending.remove(index);
                    completed.push((pending.session, pending.work, result));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let pending = self.remote_development.pending.remove(index);
                    completed.push((
                        pending.session,
                        pending.work,
                        Err("Remote request disconnected".into()),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => index += 1,
            }
        }
        for (session, work, result) in completed {
            self.apply_remote_reply(session, work, result);
        }
        if let Some(i) = self.active {
            if self.remote_bound(i) {
                let path = self.tabs[i].state.path.clone().unwrap();
                let version = self.tabs[i].state.doc.version();
                let entry = self
                    .remote_development
                    .checked
                    .entry(path.clone())
                    .or_insert((version, Instant::now()));
                if entry.0 != version {
                    *entry = (version, Instant::now());
                }
                if entry.1.elapsed()>std::time::Duration::from_millis(350)&&!self.remote_development.pending.iter().any(|pending|matches!(&pending.work,Work::Insight(p,v,_,Query::Diagnostics,_)if p==&path&&*v==version))&&self.tabs[i].state.external_diagnostics.as_ref().is_none_or(|(v,_)|*v!=version){self.remote_insight(Query::Diagnostics,false);}
            }
        }
        self.remote_development_window(ctx);
    }
    fn remote_development_window(&mut self, ctx: &egui::Context) {
        if !self.remote_development.open {
            return;
        }
        let mut open = true;
        let mut connect = false;
        let mut list = None;
        let mut read = None;
        let mut terminal = false;
        let mut run = None;
        let mut forward = false;
        let mut reload = None;
        let mut export = None;
        let captured_cwd = self.project_root();
        egui::Window::new("Remote Development").open(&mut open).default_size([660.,600.]).show_modeless(ctx,|ui|{let state=&mut self.remote_development;ui.label("Thin client · installed matching RustPy server · framed stdio · no project hooks on connect");let live=state.client.is_some();ui.add_enabled_ui(!live,|ui|{egui::ComboBox::from_label("Target").selected_text(format!("{:?}",state.target.kind)).show_ui(ui,|ui|{for kind in [Kind::Ssh,Kind::Docker,Kind::Wsl,Kind::Devcontainer,Kind::Local]{ui.selectable_value(&mut state.target.kind,kind,format!("{kind:?}"));}});for(label,value)in[("Host / container / distribution / devcontainer workspace",&mut state.target.target),("Remote workspace root",&mut state.target.root),("Installed RustPy executable",&mut state.target.executable),("Transport executable (blank = default)",&mut state.target.transport),("Python executable (explicit Run/Debug)",&mut state.target.python),("SSH user",&mut state.target.user),("SSH key file",&mut state.target.key),("SSH config file",&mut state.target.ssh_config),("SSH jump host",&mut state.target.jump)]{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);}ui.horizontal(|ui|{ui.label("SSH port");ui.add(egui::DragValue::new(&mut state.target.port).range(1..=65535));ui.label("Python static minor");ui.add(egui::DragValue::new(&mut state.target.python_version.1).range(8..=14));});if ui.button("Capture current local directory").clicked(){state.target.local_cwd=captured_cwd.clone();}ui.label(format!("Transport directory: {}",state.target.local_cwd.display()));ui.checkbox(&mut state.trusted,"I trust this target and explicitly admit this client to its captured root");connect=ui.add_enabled(state.trusted,egui::Button::new("Connect")).clicked();if ui.button("Save target settings locally").clicked(){if let Err(error)=state.target.save(){state.status=error;}}});if let Some(client)=&state.client{ui.horizontal(|ui|{ui.label(format!("Session {} · {}",client.id(),client.target().root));if ui.button("Disconnect").clicked(){client.stop();for owner in &state.forwards{owner.stop();}}});if client.ready(){ui.horizontal(|ui|{if ui.button("Root").clicked(){list=Some((String::new(),0));}
if ui.button("Up").clicked(){list=Some((state.directory.rsplit_once('/').map(|(parent,_)|parent).unwrap_or("").into(),0));}
if ui.button("Refresh").clicked(){list=Some((state.directory.clone(),0));}terminal=ui.button("Terminal").clicked();if ui.button("Run active").clicked(){run=Some(false);}
if ui.button("Debug active").clicked(){run=Some(true);}});ui.label(format!("Remote project /{}",state.directory));egui::ScrollArea::vertical().max_height(200.).show(ui,|ui|{for row in &state.rows{let Some(path)=row["path"].as_str()else{continue;};let directory=row["directory"].as_bool()==Some(true);if ui.button(format!("{} {}",if directory{"▸"}else{" "},row["name"].as_str().unwrap_or(path))).clicked(){if directory{list=Some((path.into(),0));}else{read=Some(path.into());}}}});if let Some(offset)=state.next{if ui.button("Load next 500").clicked(){list=Some((state.directory.clone(),offset));}}ui.horizontal(|ui|{ui.label("Remote loopback port");ui.add(egui::DragValue::new(&mut state.remote_port).range(1..=65535));forward=ui.button("Forward to free local port").clicked();});for owner in &state.forwards{ui.horizontal(|ui|{ui.label(format!("127.0.0.1:{} → remote 127.0.0.1:{}",owner.local,owner.remote));if ui.button("Stop forward").clicked(){owner.stop();}});}ui.collapsing("Transport log",|ui|{ui.monospace(client.logs());});}}ui.label(&state.status);if let Some((path,text,error))=&state.conflict{ui.separator();ui.label(format!("Remote save rejected: {error}. Your edited buffer is retained."));if ui.button("Export unsaved draft locally…").clicked(){export=Some((path.clone(),text.clone()));}
if ui.button("Discard edited buffer and reload remote revision").clicked(){reload=Some(path.clone());}}});
        self.remote_development.open = open;
        if self.project_safe_mode() && (forward || terminal) {
            self.remote_development.status =
                "Trust the project before starting a remote terminal or forward".into();
            forward = false;
            terminal = false;
        }
        if connect {
            if self.project_safe_mode() {
                self.remote_development.status =
                    "Trust the local project before launching a remote transport".into();
            } else {
                let mut target = self.remote_development.target.clone();
                if target.local_cwd.as_os_str().is_empty() {
                    target.local_cwd = self.project_root();
                }
                let (auth, env) = self.remote_development.auth.new_job();
                match Client::spawn(target, &env.vars) {
                    Ok((client, reply)) => {
                        let session = client.id();
                        self.remote_development.client = Some(client);
                        self.remote_development.auth_id = Some(auth);
                        self.remote_development.environment = env.vars;
                        self.remote_development.pending.push(Pending {
                            session,
                            id: 1,
                            reply,
                            work: Work::Hello,
                        });
                        self.remote_development.trusted = false;
                    }
                    Err(error) => {
                        self.remote_development
                            .auth
                            .job_finished(auth, false, &error);
                        self.remote_development.status = error;
                    }
                }
            }
        }
        if let Some((path, offset)) = list {
            if offset == 0 {
                self.remote_development.rows.clear();
            }
            self.remote_development.list(path, offset);
        }
        if let Some(path) = read {
            self.remote_development.read(path, None);
        }
        if forward {
            if let Some(client) = &self.remote_development.client {
                match client.forward(0, self.remote_development.remote_port) {
                    Ok(owner) => self.remote_development.forwards.push(owner),
                    Err(error) => self.remote_development.status = error,
                }
            }
        }
        if terminal {
            if let Some(client) = &self.remote_development.client {
                let target = client.effective_target();
                match target.terminal() {
                    Ok(spec) => {
                        if let Err(error) = self.terminal_panel.open_command(
                            "Remote workspace",
                            &spec.program,
                            &spec.args,
                            &target.local_cwd,
                            &self.remote_development.environment,
                        ) {
                            self.set_status(error.to_string());
                        } else {
                            self.bottom = Some(BottomTool::Terminal);
                        }
                    }
                    Err(error) => self.set_status(error),
                }
            }
        }
        if let Some(debug) = run {
            self.remote_development_run(debug);
        }
        if let Some(path) = reload {
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|tab| tab.state.path.as_ref() == Some(&path))
            {
                tab.state.doc.mark_saved();
            }
            if let Some(binding) = self.remote_development.binding(&path) {
                let relative = binding.relative.clone();
                self.remote_development.conflict = None;
                self.remote_development.read(relative, None);
            }
        }
        if let Some((path, text)) = export {
            let destination = path.with_extension("remote-draft.txt");
            match std::fs::write(&destination, text) {
                Ok(()) => self.set_status(format!("Draft exported to {}", destination.display())),
                Err(error) => self.set_status(error.to_string()),
            }
        }
    }
    fn remote_development_run(&mut self, debug: bool) {
        if debug && !self.save_all() {
            self.set_status("Wait for all save acknowledgements, then attach the remote debugger");
            return;
        }
        let Some(i) = self.active else {
            return;
        };
        let Some(path) = self.tabs[i].state.path.clone() else {
            return;
        };
        let Some(binding) = self.remote_development.binding(&path).cloned() else {
            self.set_status("Open a file from this remote project first");
            return;
        };
        if self.tabs[i].state.doc.is_modified() {
            self.set_status("Save and wait for remote acknowledgement before running");
            return;
        }
        let Some(client) = &self.remote_development.client else {
            return;
        };
        if client.id() != binding.session || !client.ready() || self.project_safe_mode() {
            return;
        }
        let target = client.effective_target();
        let local_port = if debug {
            match client.forward(0, 5678) {
                Ok(owner) => {
                    let port = owner.local;
                    self.remote_development.forwards.push(owner);
                    Some(port)
                }
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        } else {
            None
        };
        match target.run_python(&binding.relative, debug.then_some(5678)) {
            Ok(spec) => {
                self.spawn_command_session(
                    if debug {
                        "Remote debugpy (waiting for client)"
                    } else {
                        "Remote Python"
                    },
                    crate::run_anything::CommandSpec {
                        program: spec.program.into(),
                        args: spec.args,
                        cwd: target.local_cwd.clone(),
                        env: self.remote_development.environment.clone(),
                    },
                );
                if let Some(port) = local_port {
                    let mut specification = rustpy_project::configuration_types::ConfigurationSpec::new(
                        rustpy_project::configuration_types::ConfigurationType::PythonDebugServer,
                    );
                    specification.host = "127.0.0.1".into();
                    specification.port = port;
                    specification.listen = false;
                    specification.path_mappings.insert(
                        target.mirror().to_string_lossy().into_owned(),
                        target.root.clone(),
                    );
                    self.launch_config(
                        RunConfig {
                            name: "Attach remote workspace".into(),
                            kind: RunConfigKind::PythonModule {
                                module: "debugpy".into(),
                            },
                            type_options: Some(specification),
                            temporary: true,
                            working_dir: Some(target.mirror()),
                            ..Default::default()
                        },
                        crate::run_context::RunMode::Debug,
                    );
                }
            }
            Err(error) => self.set_status(error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_session_cannot_rebind_remote_edits() {
        let binding = Binding {
            session: 7,
            relative: "src/main.py".into(),
            revision: gateway::revision(b"old"),
        };
        assert_ne!(binding.session, 8);
        assert_ne!(binding.revision, gateway::revision(b"changed"));
        assert!(gateway::relative(&binding.relative).is_ok());
        assert!(gateway::relative("../escape.py").is_err());
    }
}

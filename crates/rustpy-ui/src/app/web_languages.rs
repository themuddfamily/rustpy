//! Installed LSP toolchains connected to shared documents and checked edit transactions.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::web_language_protocol as protocol;
use rustpy_run::language_server::{Client, Event};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
type Key = (PathBuf, String);
#[derive(Clone)]
struct Snapshot {
    root: PathBuf,
    path: PathBuf,
    view: egui::Id,
    document: rustpy_text::DocumentId,
    version: u64,
    caret: TextSize,
    source: String,
}
struct Open {
    snapshot: Snapshot,
    protocol: i64,
    diagnostics: Vec<Value>,
}
#[derive(Clone, Copy)]
enum Kind {
    Completion,
    ResolveCompletion,
    Definition,
    References,
    Hover,
    Format,
    Rename,
    Actions,
    ResolveAction,
    Command,
}
struct Pending {
    kind: Kind,
    snapshot: Snapshot,
    fallback: Option<Value>,
}
struct Server {
    client: Client,
    open: BTreeMap<PathBuf, Open>,
    pending: BTreeMap<u64, Pending>,
    spec: rustpy_settings::web_development::Server,
    resolve_completion: bool,
}
struct Completion {
    snapshot: Snapshot,
    key: Key,
    items: Vec<Value>,
}
struct ReportedDiagnostic {
    snapshot: Arc<Snapshot>,
    range: TextRange,
    message: String,
    provider: String,
}
struct Change {
    path: PathBuf,
    source: String,
    edits: Vec<(TextRange, String)>,
}
struct Plan {
    snapshot: Snapshot,
    title: String,
    changes: Vec<Change>,
    reply: Option<(Key, Value)>,
}
#[derive(Default)]
pub(super) struct State {
    servers: BTreeMap<Key, Server>,
    retired: Vec<Client>,
    suspended: std::sync::atomic::AtomicBool,
    waiting: Vec<(Key, Kind, Snapshot, Value)>,
    sequence: i64,
    completion: Option<Completion>,
    messages: VecDeque<String>,
    reported: Vec<ReportedDiagnostic>,
    actions: Vec<(Key, Snapshot, Value)>,
    plan: Option<Plan>,
    hover: String,
    pub rename: String,
    pub selected: String,
    pub initialization: String,
    configuration: String,
    last_auto: Option<std::time::Instant>,
}
impl State {
    pub(super) fn stop(&self) {
        self.suspended
            .store(true, std::sync::atomic::Ordering::Release);
        for client in &self.retired {
            client.stop();
        }
        for server in self.servers.values() {
            server.client.stop();
        }
    }
    pub(super) fn is_running(&self) -> bool {
        self.servers
            .values()
            .any(|server| server.client.is_running())
            || self.retired.iter().any(Client::is_running)
    }
    pub(super) fn stamps(&self) -> Vec<String> {
        self.servers
            .values()
            .filter(|server| server.client.is_running())
            .map(|server| {
                format!(
                    "Web language server · {} · {}",
                    server.client.stamp(),
                    server.spec.executable
                )
            })
            .chain(
                self.retired
                    .iter()
                    .filter(|client| client.is_running())
                    .map(|client| format!("Stopping web language server · {}", client.stamp())),
            )
            .collect()
    }
    fn message(&mut self, message: impl Into<String>) {
        if self.messages.len() >= 200 {
            self.messages.pop_front();
        }
        let message = message.into();
        self.messages
            .push_back(message.chars().take(4096).collect());
    }
}
impl IdeApp {
    fn web_snapshot(&self) -> Option<Snapshot> {
        if self.non_text_source_hidden() {
            return None;
        }
        let index = self.active?;
        let tab = self.tabs.get(index)?;
        let path = tab.state.path.as_ref()?;
        if protocol::language(path).is_none()
            || tab.state.is_large_file()
            || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024
        {
            return None;
        }
        let project = self.project.as_ref()?;
        if !self.project_scope()?.contains(path) {
            return None;
        }
        Some(Snapshot {
            root: project.root.clone(),
            path: path.clone(),
            view: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            source: tab.state.doc.text(),
        })
    }
    fn web_current(&self, snapshot: &Snapshot, caret: bool) -> bool {
        !self.project_safe_mode()
            && self
                .project_scope()
                .is_some_and(|scope| scope.contains(&snapshot.path))
            && self
                .project
                .as_ref()
                .is_some_and(|project| project.root == snapshot.root)
            && self.tabs.iter().any(|tab| {
                tab.view.id == snapshot.view
                    && tab.state.path.as_ref() == Some(&snapshot.path)
                    && tab.state.doc.identity() == snapshot.document
                    && tab.state.doc.version() == snapshot.version
                    && (!caret || tab.state.primary().head() == snapshot.caret)
            })
    }
    fn web_key(&self, snapshot: &Snapshot) -> Option<Key> {
        let (kind, _) = protocol::language(&snapshot.path)?;
        let kind = if matches!(kind, "typescript" | "html")
            && snapshot.root.join("angular.json").is_file()
        {
            "angular"
        } else {
            kind
        };
        Some((snapshot.root.clone(), kind.into()))
    }
    fn ensure_web_server(&mut self, snapshot: &Snapshot) -> Result<Key, String> {
        self.ensure_web_server_for(snapshot, None)
    }
    fn ensure_web_server_for(
        &mut self,
        snapshot: &Snapshot,
        provider: Option<&str>,
    ) -> Result<Key, String> {
        if self.project_safe_mode() {
            return Err("Trust the project before starting installed language tools".into());
        }
        if !self.settings.web_development.enabled {
            return Err("Enable Web language services in Settings".into());
        }
        if !self.web_current(snapshot, false) {
            return Err("Web document changed".into());
        }
        let key = if let Some(provider) = provider {
            (snapshot.root.clone(), provider.into())
        } else {
            self.web_key(snapshot).ok_or("Unsupported web language")?
        };
        let spec = self
            .settings
            .web_development
            .servers
            .get(&key.1)
            .filter(|server| server.enabled)
            .cloned()
            .ok_or("Enable this installed language server in Web Development settings")?;
        if self.web_languages.servers.get(&key).is_some_and(|server| {
            server.client.is_running() && !server.client.is_stopping() && server.spec == spec
        }) {
            return Ok(key);
        }
        if let Some(server) = self.web_languages.servers.remove(&key) {
            server.client.stop();
            self.web_languages.retired.push(server.client);
        }
        let executable = web_executable(&snapshot.root, &spec.executable);
        let uri = file_uri(&snapshot.root)?;
        let client = Client::start(
            &executable,
            &spec.arguments,
            &snapshot.root,
            &uri,
            spec.initialization.clone(),
        )
        .map_err(|error| {
            format!(
                "{}: {error}. Configure/install the standard language server explicitly.",
                spec.executable
            )
        })?;
        self.web_languages.message(format!(
            "Starting {} in {}",
            spec.executable,
            snapshot.root.display()
        ));
        self.web_languages.servers.insert(
            key.clone(),
            Server {
                client,
                open: BTreeMap::new(),
                pending: BTreeMap::new(),
                spec,
                resolve_completion: false,
            },
        );
        Ok(key)
    }
    fn web_request(&mut self, kind: Kind) -> bool {
        let Some(snapshot) = self.web_snapshot() else {
            return false;
        };
        self.web_languages
            .suspended
            .store(false, std::sync::atomic::Ordering::Release);
        let eslint = matches!(kind, Kind::Actions)
            && self
                .settings
                .web_development
                .servers
                .get("eslint")
                .is_some_and(|server| server.enabled);
        let key = match self
            .ensure_web_server_for(&snapshot, if eslint { Some("eslint") } else { None })
        {
            Ok(key) => key,
            Err(error) => {
                self.set_status(error);
                return !matches!(kind, Kind::Completion);
            }
        };
        let method = method(kind);
        let mut params = json!({"textDocument":{"uri":file_uri(&snapshot.path).unwrap_or_default()},"position":protocol::position(&snapshot.source,usize::from(snapshot.caret))});
        match kind {
            Kind::References => params["context"] = json!({"includeDeclaration":true}),
            Kind::Format => {
                params.as_object_mut().unwrap().remove("position");
                params["options"] = json!({"tabSize":4,"insertSpaces":true});
            }
            Kind::Rename => params["newName"] = json!(self.web_languages.rename),
            Kind::Actions => {
                let position = params["position"].clone();
                params.as_object_mut().unwrap().remove("position");
                params["range"] = json!({"start":position,"end":position});
                let diagnostics = self
                    .web_languages
                    .servers
                    .get(&key)
                    .and_then(|server| server.open.get(&snapshot.path))
                    .map(|open| open.diagnostics.clone())
                    .unwrap_or_default();
                params["context"] = json!({"diagnostics":diagnostics});
            }
            _ => {}
        }
        self.web_languages.waiting.retain(|(_, pending, saved, _)| {
            !(matches!(pending, Kind::Completion) && saved.view == snapshot.view)
        });
        if self.web_languages.waiting.len() >= 16 {
            self.web_languages.waiting.remove(0);
        }
        self.web_languages
            .waiting
            .push((key, kind, snapshot, params));
        self.set_status(format!("Web language service: {method}"));
        true
    }
    pub(super) fn web_completion(&mut self, explicit: bool) -> bool {
        if !explicit && !self.settings.web_development.automatic {
            return false;
        }
        if self.web_snapshot().is_none() {
            return false;
        }
        self.web_languages.completion = None;
        self.web_request(Kind::Completion)
    }
    pub(super) fn web_navigation(&mut self, offset: TextSize) -> bool {
        if self.web_snapshot().is_none() {
            return false;
        }
        if let Some(index) = self.active {
            if offset <= self.tabs[index].state.doc.len() {
                self.tabs[index]
                    .state
                    .set_carets(vec![crate::editor::Caret::at(offset)]);
            }
        }
        self.web_request(Kind::Definition)
    }
    pub(super) fn web_language_saved(&mut self, path: &Path) {
        if self.project_safe_mode() {
            return;
        }
        for server in self.web_languages.servers.values() {
            if server.client.ready && !server.client.is_stopping() && server.open.contains_key(path)
            {
                let _ = server.client.notify(
                    "textDocument/didSave",
                    json!({"textDocument":{"uri":file_uri(path).unwrap_or_default()}}),
                );
            }
        }
    }
    pub(super) fn web_references(&mut self) -> bool {
        self.web_request(Kind::References)
    }
    pub(super) fn web_hover(&mut self) -> bool {
        if self.web_snapshot().is_none() {
            return false;
        }
        self.dialog = Dialog::WebLanguages;
        self.web_request(Kind::Hover)
    }
    pub(super) fn web_format(&mut self) -> bool {
        let Some(snapshot) = self.web_snapshot() else {
            return false;
        };
        if protocol::language(&snapshot.path).is_some_and(|(kind, _)| kind == "json") {
            return false;
        }
        let active = self
            .web_key(&snapshot)
            .and_then(|key| self.web_languages.servers.get(&key))
            .is_some_and(|server| server.client.is_running());
        if !active && !self.settings.web_development.automatic {
            return false;
        }
        self.web_request(Kind::Format)
    }
    pub(super) fn web_actions(&mut self) -> bool {
        if self.web_snapshot().is_none() {
            return false;
        }
        self.dialog = Dialog::WebLanguages;
        self.web_request(Kind::Actions)
    }
    pub(super) fn web_rename(&mut self) -> bool {
        let Some(snapshot) = self.web_snapshot() else {
            return false;
        };
        let end = usize::from(snapshot.caret);
        let start = snapshot.source[..end]
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map_or(0, |at| {
                at + snapshot.source[at..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
            });
        self.web_languages.rename = snapshot.source[start..end].into();
        self.dialog = Dialog::WebLanguages;
        true
    }
    pub(super) fn stop_web_languages(&mut self) {
        self.web_languages.stop();
        let providers = self
            .settings
            .web_development
            .servers
            .keys()
            .map(|name| format!("{name}:"))
            .collect::<Vec<_>>();
        for tab in &mut self.tabs {
            if let Some((_, rows)) = &mut tab.state.external_diagnostics {
                rows.retain(|row| !providers.iter().any(|prefix| row.code.starts_with(prefix)));
            }
        }
        self.web_languages.reported.clear();
        self.web_languages.waiting.clear();
        self.web_languages.completion = None;
        self.web_languages.actions.clear();
        self.web_languages.plan = None;
    }
    pub(super) fn poll_web_languages(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        let root = self.project.as_ref().map(|project| project.root.clone());
        for (key, server) in &self.web_languages.servers {
            if root.as_ref() != Some(&key.0)
                || project_safe_mode
                || !self.settings.web_development.enabled
                || self.settings.web_development.servers.get(&key.1) != Some(&server.spec)
            {
                server.client.stop();
            }
        }
        self.web_languages.retired.retain(Client::is_running);
        for client in &self.web_languages.retired {
            for _ in client.events.try_iter().take(128) {}
        }
        if !self
            .web_languages
            .suspended
            .load(std::sync::atomic::Ordering::Acquire)
            && self.settings.web_development.automatic
            && !project_safe_mode
            && self
                .web_languages
                .last_auto
                .is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(2))
        {
            self.web_languages.last_auto = Some(std::time::Instant::now());
            if let Some(snapshot) = self.web_snapshot() {
                if let Err(error) = self.ensure_web_server(&snapshot) {
                    self.web_languages.message(error);
                }
            }
        }
        let keys = self
            .web_languages
            .servers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        for key in &keys {
            if let Some(server) = self.web_languages.servers.get(key) {
                for event in server.client.events.try_iter().take(128) {
                    events.push((key.clone(), event));
                }
            }
        }
        for (key, event) in events {
            match event {
                Event::Message(message) => self.web_message(&key, message),
                Event::Log(message) => self.web_languages.message(format!("{}: {message}", key.1)),
                Event::Error(error) => self.web_languages.message(format!("{}: {error}", key.1)),
                Event::Exited(code) => self
                    .web_languages
                    .message(format!("{} exited ({code:?})", key.1)),
            }
        }
        let snapshots = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let path = tab.state.path.as_ref()?;
                if protocol::language(path).is_none()
                    || tab.state.is_large_file()
                    || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024
                {
                    return None;
                }
                let root = root.clone()?;
                Some(Snapshot {
                    root,
                    path: path.clone(),
                    view: tab.view.id,
                    document: tab.state.doc.identity(),
                    version: tab.state.doc.version(),
                    caret: tab.state.primary().head(),
                    source: String::new(),
                })
            })
            .collect::<Vec<_>>();
        for key in &keys {
            let applicable = snapshots
                .iter()
                .filter(|snapshot| {
                    (self.web_key(snapshot).as_ref() == Some(key)
                        || key.1 == "eslint"
                            && protocol::language(&snapshot.path).is_some_and(|(kind, _)| {
                                matches!(kind, "typescript" | "vue" | "svelte" | "html")
                            }))
                        && self
                            .project_scope()
                            .is_some_and(|scope| scope.contains(&snapshot.path))
                })
                .cloned()
                .collect::<Vec<_>>();
            let Some(server) = self.web_languages.servers.get_mut(key).filter(|server| {
                server.client.ready && server.client.is_running() && !server.client.is_stopping()
            }) else {
                continue;
            };
            let closed = server
                .open
                .keys()
                .filter(|path| !applicable.iter().any(|snapshot| &snapshot.path == *path))
                .cloned()
                .collect::<Vec<_>>();
            for path in closed {
                let _ = server.client.notify(
                    "textDocument/didClose",
                    json!({"textDocument":{"uri":file_uri(&path).unwrap_or_default()}}),
                );
                server.open.remove(&path);
            }
            for mut snapshot in applicable {
                let changed = server.open.get(&snapshot.path).is_none_or(|open| {
                    open.snapshot.document != snapshot.document
                        || open.snapshot.version != snapshot.version
                });
                if !changed {
                    continue;
                }
                snapshot.source = self
                    .tabs
                    .iter()
                    .find(|tab| tab.view.id == snapshot.view)
                    .map(|tab| tab.state.doc.text())
                    .unwrap_or_default();
                self.web_languages.sequence = self.web_languages.sequence.saturating_add(1);
                let version = self.web_languages.sequence;
                let uri = file_uri(&snapshot.path).unwrap_or_default();
                let opened = server.open.contains_key(&snapshot.path);
                let notification = if opened {
                    "textDocument/didChange"
                } else {
                    "textDocument/didOpen"
                };
                let params = if opened {
                    json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":snapshot.source}]})
                } else {
                    json!({"textDocument":{"uri":uri,"version":version,"languageId":protocol::language(&snapshot.path).map(|(_,id)|id).unwrap_or("plaintext"),"text":snapshot.source}})
                };
                match server.client.notify(notification, params) {
                    Ok(()) => {
                        server.open.insert(
                            snapshot.path.clone(),
                            Open {
                                snapshot,
                                protocol: version,
                                diagnostics: Vec::new(),
                            },
                        );
                    }
                    Err(error) => {
                        server.client.stop();
                        self.web_languages.messages.push_back(error);
                    }
                }
            }
        }
        let waiting = std::mem::take(&mut self.web_languages.waiting);
        for (key, kind, snapshot, params) in waiting {
            if !self.web_current(&snapshot, true) {
                continue;
            }
            let Some(server) = self.web_languages.servers.get_mut(&key) else {
                continue;
            };
            if !server.client.ready {
                if server.client.is_running() {
                    self.web_languages
                        .waiting
                        .push((key, kind, snapshot, params));
                }
                continue;
            }
            match server.client.request(method(kind), params) {
                Ok(id) => {
                    if server.pending.len() >= 32 {
                        if let Some(old) = server.pending.keys().next().copied() {
                            server.client.cancel(old);
                            server.pending.remove(&old);
                        }
                    }
                    server.pending.insert(
                        id,
                        Pending {
                            kind,
                            snapshot,
                            fallback: None,
                        },
                    );
                }
                Err(error) => self.web_languages.message(error),
            }
        }
        if self.web_languages.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    fn web_message(&mut self, key: &Key, message: Value) {
        if self.project_safe_mode()
            || self
                .web_languages
                .servers
                .get(key)
                .is_none_or(|server| server.client.is_stopping())
        {
            return;
        }
        if let Some(method) = message["method"].as_str() {
            if !message["id"].is_null() {
                let id = message["id"].clone();
                if method == "workspace/applyEdit" {
                    let snapshot = self.web_languages.servers.get(key).and_then(|server| {
                        server
                            .pending
                            .values()
                            .find(|pending| matches!(pending.kind, Kind::Command))
                            .map(|pending| pending.snapshot.clone())
                    });
                    if let Some(snapshot) = snapshot {
                        match self.web_plan(
                            &snapshot,
                            &message["params"]["edit"],
                            "Language-server edit",
                        ) {
                            Ok(mut plan) => {
                                plan.reply = Some((key.clone(), id));
                                self.web_languages.plan = Some(plan);
                                self.dialog = Dialog::WebLanguages;
                                return;
                            }
                            Err(error) => self.web_languages.message(error),
                        }
                    }
                    if let Some(server) = self.web_languages.servers.get(key) {
                        let _=server.client.reply(id,json!({"applied":false,"failureReason":"No active user-requested edit or a stale workspace"}));
                    }
                    return;
                }
                let result = match method {
                    "workspace/configuration" => {
                        let configuration = &self.settings.web_development.configuration;
                        Value::Array(
                            message["params"]["items"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|item| {
                                    item["section"]
                                        .as_str()
                                        .filter(|section| !section.is_empty())
                                        .map(|section| {
                                            section
                                                .split('.')
                                                .fold(configuration, |value, key| &value[key])
                                                .clone()
                                        })
                                        .unwrap_or_else(|| configuration.clone())
                                })
                                .collect(),
                        )
                    }
                    "workspace/workspaceFolders" => {
                        json!([{"uri":file_uri(&key.0).unwrap_or_default(),"name":key.0.file_name().unwrap_or_default().to_string_lossy()}])
                    }
                    _ => Value::Null,
                };
                if let Some(server) = self.web_languages.servers.get(key) {
                    let _ = server.client.reply(id, result);
                }
                return;
            }
            match method {
                "textDocument/publishDiagnostics" => self.web_diagnostics(key, &message["params"]),
                "window/logMessage" | "window/showMessage" => self
                    .web_languages
                    .message(protocol::text(&message["params"]["message"])),
                _ => {}
            }
            return;
        }
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        if id == 1 {
            if !message["error"].is_null() {
                self.web_languages.message(format!(
                    "{} initialization failed: {}",
                    key.1, message["error"]
                ));
                if let Some(server) = self.web_languages.servers.get(key) {
                    server.client.stop();
                }
                return;
            }
            let encoding = message["result"]["capabilities"]["positionEncoding"]
                .as_str()
                .unwrap_or("utf-16");
            if encoding != "utf-16" {
                if let Some(server) = self.web_languages.servers.get(key) {
                    server.client.stop();
                }
                self.web_languages
                    .message("Language server selected an unsupported position encoding");
                return;
            }
            if let Some(server) = self.web_languages.servers.get_mut(key) {
                server.client.ready = true;
                server.resolve_completion = message["result"]["capabilities"]["completionProvider"]
                    ["resolveProvider"]
                    .as_bool()
                    .unwrap_or(false);
                let _ = server.client.notify("initialized", json!({}));
                let _ = server.client.notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings":self.settings.web_development.configuration}),
                );
            }
            return;
        }
        let pending = self
            .web_languages
            .servers
            .get_mut(key)
            .and_then(|server| server.pending.remove(&id));
        let Some(pending) = pending else { return };
        if !self.web_current(
            &pending.snapshot,
            !matches!(pending.kind, Kind::Format | Kind::Command),
        ) {
            return;
        }
        if !message["error"].is_null() {
            if matches!(pending.kind, Kind::ResolveCompletion) {
                if let Some(item) = pending.fallback {
                    self.web_insert_completion(&pending.snapshot, &item);
                    return;
                }
            }
            self.web_languages
                .message(format!("{}: {}", method(pending.kind), message["error"]));
            return;
        }
        let result = &message["result"];
        match pending.kind {
            Kind::Completion => self.web_show_completion(key, &pending.snapshot, result),
            Kind::ResolveCompletion => self.web_insert_completion(&pending.snapshot, result),
            Kind::Definition | Kind::References => self.web_locations(&pending.snapshot, result),
            Kind::Hover => self.web_languages.hover = protocol::text(&result["contents"]),
            Kind::Format => match protocol::edits(&pending.snapshot.source, result) {
                Ok(edits) => self.web_apply_single(&pending.snapshot, edits, "Format web document"),
                Err(error) => self.web_languages.message(error),
            },
            Kind::Rename => match self.web_plan(&pending.snapshot, result, "Rename web symbol") {
                Ok(plan) => {
                    self.web_languages.plan = Some(plan);
                    self.dialog = Dialog::WebLanguages;
                }
                Err(error) => self.web_languages.message(error),
            },
            Kind::Actions => {
                self.web_languages.actions = result
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(256)
                    .map(|action| (key.clone(), pending.snapshot.clone(), action.clone()))
                    .collect();
            }
            Kind::ResolveAction => {
                self.web_process_action(key.clone(), pending.snapshot, result.clone())
            }
            Kind::Command => {
                if result.get("changes").is_some() || result.get("documentChanges").is_some() {
                    match self.web_plan(&pending.snapshot, result, "Web command edits") {
                        Ok(plan) => {
                            self.web_languages.plan = Some(plan);
                            self.dialog = Dialog::WebLanguages;
                        }
                        Err(error) => self.web_languages.message(error),
                    }
                }
            }
        }
    }
    fn web_diagnostics(&mut self, key: &Key, params: &Value) {
        let Some(path) = params["uri"]
            .as_str()
            .and_then(|uri| url::Url::parse(uri).ok())
            .and_then(|uri| uri.to_file_path().ok())
        else {
            return;
        };
        let Some(open) = self
            .web_languages
            .servers
            .get(key)
            .and_then(|server| server.open.get(&path))
        else {
            return;
        };
        let snapshot = open.snapshot.clone();
        if !self.web_current(&snapshot, false) {
            return;
        }
        let version = params["version"].as_i64();
        let rows = params["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if version != Some(open.protocol) {
            if version.is_none() {
                self.web_languages
                    .reported
                    .retain(|row| row.snapshot.path != path || row.provider != key.1);
                let snapshot = Arc::new(snapshot);
                for row in rows.iter().take(128) {
                    if let Ok(range) = protocol::range(&snapshot.source, &row["range"]) {
                        self.web_languages.reported.push(ReportedDiagnostic {
                            snapshot: snapshot.clone(),
                            range,
                            message: protocol::text(&row["message"]),
                            provider: key.1.clone(),
                        });
                    }
                }
                self.web_languages.reported.truncate(512);
            }
            return;
        }
        self.web_languages
            .reported
            .retain(|row| row.snapshot.path != path || row.provider != key.1);
        let mut diagnostics: Vec<rustpy_core::Diagnostic> = rows
            .iter()
            .take(256)
            .filter_map(|row| {
                let range = protocol::range(&snapshot.source, &row["range"]).ok()?;
                let message = row["message"].as_str()?;
                let code = format!(
                    "{}:{}",
                    key.1,
                    row["code"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| row["code"].to_string())
                );
                Some(if row["severity"].as_i64() == Some(1) {
                    rustpy_core::Diagnostic::error(range, &code, message)
                } else {
                    rustpy_core::Diagnostic::warning(range, &code, message)
                })
            })
            .collect();
        if let Some(tab) = self.tabs.iter_mut().find(|tab| {
            tab.state.doc.identity() == snapshot.document
                && tab.state.doc.version() == snapshot.version
                && tab.state.path.as_ref() == Some(&snapshot.path)
        }) {
            if let Some((version, previous)) = &tab.state.external_diagnostics {
                if *version == snapshot.version {
                    diagnostics.extend(
                        previous
                            .iter()
                            .filter(|diagnostic| {
                                !diagnostic.code.starts_with(&format!("{}:", key.1))
                            })
                            .cloned(),
                    );
                }
            }
            diagnostics.truncate(512);
            tab.state.external_diagnostics = Some((snapshot.version, diagnostics));
        }
        if let Some(open) = self
            .web_languages
            .servers
            .get_mut(key)
            .and_then(|server| server.open.get_mut(&path))
        {
            open.diagnostics = rows;
        }
    }
    fn web_show_completion(&mut self, key: &Key, snapshot: &Snapshot, result: &Value) {
        let items = result
            .as_array()
            .or_else(|| result["items"].as_array())
            .into_iter()
            .flatten()
            .take(1000)
            .filter(|item| item["label"].is_string())
            .cloned()
            .collect::<Vec<_>>();
        let end = usize::from(snapshot.caret);
        let start = snapshot.source[..end]
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map_or(0, |at| {
                at + snapshot.source[at..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
            });
        let values = items
            .iter()
            .map(|item| CompletionItem {
                label: item["label"].as_str().unwrap_or_default().into(),
                insert: item["insertText"]
                    .as_str()
                    .or_else(|| item["label"].as_str())
                    .unwrap_or_default()
                    .into(),
                detail: item["detail"].as_str().unwrap_or("Language service").into(),
                kind: CompletionKind::Variable,
                caret_offset: None,
                priority: 0,
            })
            .collect::<Vec<_>>();
        let count = values.len();
        self.completion = CompletionPopup {
            visible: true,
            items: values,
            imports: vec![None; count],
            selected: 0,
            prefix_start: TextSize::new(start as u32),
            doc_version: snapshot.version,
            caret: snapshot.caret,
            rect: None,
            explicit: true,
        };
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.web_languages.completion = Some(Completion {
            snapshot: snapshot.clone(),
            key: key.clone(),
            items,
        });
    }
    pub(super) fn accept_web_completion(&mut self) -> bool {
        if self.non_text_source_hidden() {
            return false;
        }
        let Some(completion) = self.web_languages.completion.take() else {
            return false;
        };
        if !self.web_current(&completion.snapshot, true)
            || self.completion.doc_version != completion.snapshot.version
            || self.completion.caret != completion.snapshot.caret
        {
            return false;
        }
        let Some(item) = completion.items.get(self.completion.selected).cloned() else {
            return false;
        };
        if self
            .completion
            .items
            .get(self.completion.selected)
            .is_none_or(|shown| shown.label.as_str() != item["label"].as_str().unwrap_or_default())
        {
            return false;
        }
        self.completion.visible = false;
        if item.get("data").is_some_and(|data| !data.is_null())
            && self
                .web_languages
                .servers
                .get(&completion.key)
                .is_some_and(|server| server.resolve_completion)
        {
            if let Some(server) = self.web_languages.servers.get_mut(&completion.key) {
                match server
                    .client
                    .request("completionItem/resolve", item.clone())
                {
                    Ok(id) => {
                        server.pending.insert(
                            id,
                            Pending {
                                kind: Kind::ResolveCompletion,
                                snapshot: completion.snapshot,
                                fallback: Some(item),
                            },
                        );
                    }
                    Err(error) => self.web_languages.message(error),
                }
                return true;
            }
        }
        self.web_insert_completion(&completion.snapshot, &item);
        true
    }
    fn web_insert_completion(&mut self, snapshot: &Snapshot, item: &Value) {
        let mut edits = Vec::new();
        if let Some(edit) = item.get("textEdit") {
            let value = if edit.get("range").is_some() {
                edit.clone()
            } else {
                json!({"range":edit["replace"],"newText":edit["newText"]})
            };
            match protocol::edits(&snapshot.source, &json!([value])) {
                Ok(mut values) => edits.append(&mut values),
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        } else {
            let end = usize::from(snapshot.caret);
            let start = snapshot.source[..end]
                .rfind(|c: char| !rustpy_core::is_ident_continue(c))
                .map_or(0, |at| {
                    at + snapshot.source[at..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8)
                });
            edits.push((
                TextRange::new(TextSize::new(start as u32), snapshot.caret),
                item["insertText"]
                    .as_str()
                    .or_else(|| item["label"].as_str())
                    .unwrap_or_default()
                    .into(),
            ));
        }
        let mut snippet = None;
        let main_start = edits
            .first()
            .map(|(range, _)| range.start())
            .unwrap_or(snapshot.caret);
        if item["insertTextFormat"].as_i64() == Some(2) {
            let mut variables = BTreeMap::new();
            variables.insert(
                "TM_FILENAME".into(),
                snapshot
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            variables.insert(
                "TM_FILENAME_BASE".into(),
                snapshot
                    .path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            variables.insert(
                "TM_DIRECTORY".into(),
                snapshot
                    .path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .display()
                    .to_string(),
            );
            variables.insert("TM_FILEPATH".into(), snapshot.path.display().to_string());
            if let Some((_, text)) = edits.first_mut() {
                match protocol::snippet(text, &variables) {
                    Ok(expansion) => {
                        *text = expansion.text.clone();
                        snippet = Some(expansion);
                    }
                    Err(error) => {
                        self.set_status(error);
                        return;
                    }
                }
            }
        }
        if let Some(additional) = item.get("additionalTextEdits") {
            match protocol::edits(&snapshot.source, additional) {
                Ok(mut values) => edits.append(&mut values),
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        }
        edits.sort_by_key(|(range, _)| range.start());
        if edits
            .windows(2)
            .any(|pair| pair[0].0.end() > pair[1].0.start())
        {
            self.set_status("Conflicting language-server completion edits");
            return;
        }
        let shift = edits
            .iter()
            .filter(|(range, _)| range.end() <= main_start && range.start() != main_start)
            .map(|(range, text)| text.len() as i64 - i64::from(u32::from(range.len())))
            .sum::<i64>();
        self.web_apply_single(snapshot, edits, "Web completion");
        if let Some(expansion) = snippet {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| {
                tab.view.id == snapshot.view
                    && tab.state.doc.identity() == snapshot.document
                    && tab.state.doc.version() != snapshot.version
            }) {
                let base = (i64::from(u32::from(main_start)) + shift).max(0) as u32;
                let mut groups = BTreeMap::<u32, Vec<TextRange>>::new();
                for (index, start, end) in expansion.stops {
                    groups.entry(index).or_default().push(TextRange::new(
                        TextSize::new(base + start as u32),
                        TextSize::new(base + end as u32),
                    ));
                }
                let vars = groups
                    .into_iter()
                    .map(|(index, ranges)| (index.to_string(), ranges))
                    .collect::<Vec<_>>();
                let end =
                    TextSize::new(base + expansion.end.unwrap_or(expansion.text.len()) as u32);
                let caret = vars
                    .first()
                    .and_then(|(_, ranges)| ranges.first())
                    .map(|range| crate::editor::Caret::new(range.start(), range.end()))
                    .unwrap_or(crate::editor::Caret::at(end));
                tab.state.set_carets(vec![caret]);
                self.template_session = (!vars.is_empty()).then_some(TemplateSession {
                    tab_id: tab.view.id,
                    vars,
                    idx: 0,
                    end,
                    version: tab.state.doc.version(),
                });
            }
        }
    }
    fn web_apply_single(
        &mut self,
        snapshot: &Snapshot,
        edits: Vec<(TextRange, String)>,
        title: &str,
    ) {
        if !self.web_current(snapshot, false) {
            return;
        }
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.view.id == snapshot.view && tab.state.doc.identity() == snapshot.document
        }) {
            if self.refresh_file_permissions(index) {
                self.tabs[index].state.apply_text_edits(edits, title);
                self.focus_editor = true;
            }
        }
    }
    fn web_locations(&mut self, snapshot: &Snapshot, result: &Value) {
        let values = if let Some(values) = result.as_array() {
            values.clone()
        } else if result.is_object() {
            vec![result.clone()]
        } else {
            Vec::new()
        };
        let mut locations = Vec::new();
        for value in values.into_iter().take(1000) {
            let uri = value["uri"]
                .as_str()
                .or_else(|| value["targetUri"].as_str());
            let Some(path) = uri
                .and_then(|uri| url::Url::parse(uri).ok())
                .and_then(|uri| uri.to_file_path().ok())
            else {
                continue;
            };
            if !self.web_path_allowed(snapshot, &path) {
                continue;
            }
            let Ok(source) = self.web_source(&path) else {
                continue;
            };
            let range = value
                .get("range")
                .or_else(|| value.get("targetSelectionRange"))
                .or_else(|| value.get("targetRange"))
                .and_then(|range| protocol::range(&source, range).ok())
                .unwrap_or(TextRange::empty(TextSize::new(0)));
            locations.push(features::Location {
                path,
                range,
                is_stub: false,
            });
        }
        self.navigate_locations(locations, "No language-server source locations");
    }
    fn web_path_allowed(&self, snapshot: &Snapshot, path: &Path) -> bool {
        self.project
            .as_ref()
            .is_some_and(|project| project.root == snapshot.root)
            && self
                .project_scope()
                .is_some_and(|scope| scope.contains(&rustpy_core::vfs::normalize_path(path)))
    }
    fn web_source(&self, path: &Path) -> Result<String, String> {
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(path))
        {
            if usize::from(tab.state.doc.len()) > 2 * 1024 * 1024 {
                return Err("Web document exceeds 2 MiB".into());
            }
            return Ok(tab.state.doc.text());
        }
        use std::io::Read;
        let mut source = String::new();
        std::fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(2 * 1024 * 1024 + 1)
            .read_to_string(&mut source)
            .map_err(|error| error.to_string())?;
        if source.len() > 2 * 1024 * 1024 {
            return Err("Web file exceeds 2 MiB".into());
        }
        Ok(source)
    }
    fn web_plan(&self, snapshot: &Snapshot, value: &Value, title: &str) -> Result<Plan, String> {
        if !self.web_current(snapshot, false) {
            return Err("Source changed before server edit".into());
        }
        let mut documents = Vec::new();
        if let Some(changes) = value["changes"].as_object() {
            documents.extend(
                changes
                    .iter()
                    .map(|(uri, edits)| (uri.clone(), None, edits.clone())),
            );
        }
        if let Some(changes) = value["documentChanges"].as_array() {
            for change in changes {
                if change.get("kind").is_some() {
                    return Err("Language-server file create/move/delete requires an explicit project file action".into());
                }
                let uri = change["textDocument"]["uri"]
                    .as_str()
                    .ok_or("Missing edited document URI")?;
                documents.push((
                    uri.into(),
                    change["textDocument"]["version"].as_i64(),
                    change["edits"].clone(),
                ));
            }
        }
        if documents.len() > 128 {
            return Err("Web edit exceeds 128 files".into());
        }
        let mut bytes = 0;
        let mut changes = Vec::new();
        for (uri, version, edits) in documents {
            let path = url::Url::parse(&uri)
                .map_err(|error| error.to_string())?
                .to_file_path()
                .map_err(|_| "Web edit is not a local file URI")?;
            if !self.web_path_allowed(snapshot, &path) {
                return Err(format!(
                    "Web edit is outside project scope: {}",
                    path.display()
                ));
            }
            if let Some(version) = version {
                let known = self
                    .web_languages
                    .servers
                    .values()
                    .filter_map(|server| server.open.get(&path))
                    .any(|open| {
                        open.protocol == version && self.web_current(&open.snapshot, false)
                    });
                if !known {
                    return Err("Language server edit has a stale document version".into());
                }
            }
            let source = self.web_source(&path)?;
            bytes += source.len();
            if bytes > 8 * 1024 * 1024 {
                return Err("Web edit source preview exceeds 8 MiB".into());
            }
            let edits = protocol::edits(&source, &edits)?;
            if changes.iter().any(|change: &Change| change.path == path) {
                return Err("Multiple overlapping document edit groups".into());
            }
            changes.push(Change {
                path,
                source,
                edits,
            });
        }
        Ok(Plan {
            snapshot: snapshot.clone(),
            title: title.into(),
            changes,
            reply: None,
        })
    }
    fn web_apply_plan(&mut self) {
        let Some(plan) = self.web_languages.plan.take() else {
            return;
        };
        let outcome = if self.web_current(&plan.snapshot, false)
            && plan.changes.iter().all(|change| {
                self.web_path_allowed(&plan.snapshot, &change.path)
                    && self.web_source(&change.path).as_deref() == Ok(change.source.as_str())
            }) {
            let ops = plan
                .changes
                .into_iter()
                .map(|change| rustpy_refactor::refactor2::FileOp::Edit(change.path, change.edits))
                .collect();
            let failures = self.apply_file_ops_failures(ops, &plan.title);
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("\n"))
            }
        } else {
            Err("Source changed while reviewing web edits; request them again".into())
        };
        if let Some((key, id)) = plan.reply {
            if let Some(server) = self.web_languages.servers.get(&key) {
                let _ = server.client.reply(
                    id,
                    json!({"applied":outcome.is_ok(),"failureReason":outcome.as_ref().err()}),
                );
            }
        }
        if let Err(error) = outcome {
            self.web_languages.message(error);
        }
        self.dialog = Dialog::None;
    }
}
fn method(kind: Kind) -> &'static str {
    match kind {
        Kind::Completion => "textDocument/completion",
        Kind::ResolveCompletion => "completionItem/resolve",
        Kind::Definition => "textDocument/definition",
        Kind::References => "textDocument/references",
        Kind::Hover => "textDocument/hover",
        Kind::Format => "textDocument/formatting",
        Kind::Rename => "textDocument/rename",
        Kind::Actions => "textDocument/codeAction",
        Kind::ResolveAction => "codeAction/resolve",
        Kind::Command => "workspace/executeCommand",
    }
}
fn file_uri(path: &Path) -> Result<String, String> {
    crate::web_browser_workspace::file_url(path)
        .map_err(|_| "Language service requires an absolute local path".into())
}
fn web_executable(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() || path.components().count() > 1 {
        return if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
    }
    let local = root.join("node_modules/.bin").join(if cfg!(windows) {
        format!("{value}.cmd")
    } else {
        value.into()
    });
    if local.is_file() {
        local
    } else {
        path
    }
}

impl IdeApp {
    pub(super) fn open_web_languages(&mut self) {
        self.dialog = Dialog::WebLanguages;
    }
    pub(super) fn web_languages_dialog(&mut self, ctx: &egui::Context) {
        self.dialog = Dialog::WebLanguages;
        let mut open = true;
        let mut request = None;
        let mut start = false;
        let mut stop = false;
        let mut settings = false;
        let mut action = None;
        let mut apply = false;
        let mut cancel = false;
        let mut launch = None;
        let mut diagnostic = None;
        egui::Window::new("Web Development · Language Services").id(egui::Id::new("web_languages")).open(&mut open).resizable(true).default_width(850.0).show_modal(ctx,|ui|{
 ui.horizontal_wrapped(|ui|{if ui.button("Start / restart active language").clicked(){start=true;}
if ui.button("Stop language servers").clicked(){stop=true;}
if ui.button("Toolchains / Settings…").clicked(){settings=true;}});
 ui.label("Uses installed standard language servers. TypeScript/JavaScript/React use the TypeScript service; Vue, Angular and Svelte use their framework servers. No tool is installed automatically.");
 for(key,server)in &self.web_languages.servers{ui.label(format!("{} · {} · {}",key.1,key.0.display(),if server.client.ready&&server.client.is_running(){"ready"}else if server.client.is_running(){"starting / stopping"}else{"exited"}));}
 ui.horizontal_wrapped(|ui|{for(label,kind)in[("Completion",Kind::Completion),("Definition",Kind::Definition),("References",Kind::References),("Documentation",Kind::Hover),("Format",Kind::Format),("Code actions / ESLint fixes",Kind::Actions)]{if ui.button(crate::i18n::tr_static(label)).clicked(){request=Some(kind);}}});
 ui.horizontal(|ui|{ui.label("Rename symbol");ui.text_edit_singleline(&mut self.web_languages.rename);if ui.button("Preview rename").clicked()&&!self.web_languages.rename.trim().is_empty(){request=Some(Kind::Rename);}});
 ui.horizontal_wrapped(|ui|{for(label,key)in[("Run Node file",0),("Debug JavaScript",1),("npm install",2),("Prettier",3),("ESLint fix",4),("Preview HTML / component host",5)]{if ui.button(crate::i18n::tr_static(label)).clicked(){launch=Some((key,None));}}});
 if let Some(index)=self.active{if let Some(path)=self.tabs[index].state.path.as_ref(){let package=nearest_package(path,self.project.as_ref().map(|project|project.root.as_path()));if let Some(package)=package{if let Ok(source)=self.web_source(&package){if let Ok(value)=serde_json::from_str::<Value>(&source){if let Some(scripts)=value["scripts"].as_object(){ui.collapsing("package.json scripts",|ui|{for(name,command)in scripts.iter().take(256){if ui.button(format!("npm run {name}")).on_hover_text(command.as_str().unwrap_or_default()).clicked(){launch=Some((6,Some(name.clone())));}}});}}}}}}
 if !self.web_languages.hover.is_empty(){ui.collapsing("Language-service documentation",|ui|{ui.add(egui::TextEdit::multiline(&mut self.web_languages.hover).code_editor().interactive(false).desired_width(f32::INFINITY));});}
 for(index,(_,_,value))in self.web_languages.actions.iter().enumerate(){if ui.button(value["title"].as_str().unwrap_or("Language action")).clicked(){action=Some(index);}}
 if let Some(plan)=&self.web_languages.plan{ui.separator();ui.label(format!("{} · {} files",plan.title,plan.changes.len()));egui::ScrollArea::vertical().max_height(300.0).show(ui,|ui|{for change in &plan.changes{ui.collapsing(change.path.display().to_string(),|ui|{for(range,replacement)in &change.edits{let old=&change.source[usize::from(range.start())..usize::from(range.end())];ui.label(format!("{} → {}",old.chars().take(4000).collect::<String>(),replacement.chars().take(4000).collect::<String>()));}});}});ui.horizontal(|ui|{if ui.button("Apply reviewed edits").clicked(){apply=true;}
if ui.button("Cancel edit").clicked(){cancel=true;}});}
 ui.collapsing("Reported diagnostics (server omitted document version)", |ui| {
 ui.label("These locations are reviewed against the captured buffer. The server supplied no version, so they are not published as current inline inspections.");
 for (index,row) in self.web_languages.reported.iter().enumerate() {
 if ui.button(format!("{}:{} · {} · {}",row.snapshot.path.display(), row.snapshot.source[..usize::from(row.range.start())].bytes().filter(|byte| *byte == b'\n').count()+1,row.provider,row.message)).clicked(){diagnostic=Some(index);}
 }
 });
 ui.collapsing("Language-server messages",|ui|{egui::ScrollArea::vertical().max_height(240.0).stick_to_bottom(true).show(ui,|ui|{for message in &self.web_languages.messages{ui.label(message);}});});
 });
        if let Some(index) = diagnostic {
            if let Some(row) = self.web_languages.reported.get(index) {
                if self.web_current(&row.snapshot, false) {
                    let location = features::Location {
                        path: row.snapshot.path.clone(),
                        range: row.range,
                        is_stub: false,
                    };
                    self.navigate_locations(vec![location], "Reported diagnostic source changed");
                } else {
                    self.set_status(
                        "Reported diagnostic source changed; request diagnostics again",
                    );
                }
            }
        }
        if start {
            if let Some(snapshot) = self.web_snapshot() {
                if let Some(key) = self.web_key(&snapshot) {
                    if let Some(server) = self.web_languages.servers.get(&key) {
                        server.client.stop();
                    }
                }
                self.web_languages
                    .suspended
                    .store(false, std::sync::atomic::Ordering::Release);
                if let Err(error) = self.ensure_web_server(&snapshot) {
                    self.web_languages.message(error);
                }
            }
        }
        if stop {
            self.stop_web_languages();
        }
        if settings {
            self.settings_ui.page = settings_ui::SettingsPage::WebDevelopment;
            self.dialog = Dialog::Settings;
        }
        if let Some(kind) = request {
            self.web_request(kind);
        }
        if let Some(index) = action {
            self.web_choose_action(index);
        }
        if apply {
            self.web_apply_plan();
        }
        if cancel || !open {
            self.web_cancel_plan();
            if !open {
                self.dialog = Dialog::None;
            }
        }
        if let Some((kind, script)) = launch {
            self.web_launch(kind, script);
        }
    }
    fn web_cancel_plan(&mut self) {
        if let Some(plan) = self.web_languages.plan.take() {
            if let Some((key, id)) = plan.reply {
                if let Some(server) = self.web_languages.servers.get(&key) {
                    let _ = server.client.reply(
                        id,
                        json!({"applied":false,"failureReason":"User cancelled edit"}),
                    );
                }
            }
        }
    }
    fn web_choose_action(&mut self, index: usize) {
        let Some((key, snapshot, value)) = self.web_languages.actions.get(index).cloned() else {
            return;
        };
        self.web_process_action(key, snapshot, value);
    }
    fn web_process_action(&mut self, key: Key, snapshot: Snapshot, value: Value) {
        if !self.web_current(&snapshot, false) {
            self.web_languages
                .message("Source changed; request code actions again");
            return;
        }
        if let Some(edit) = value.get("edit") {
            match self.web_plan(
                &snapshot,
                edit,
                value["title"].as_str().unwrap_or("Web action"),
            ) {
                Ok(plan) => self.web_languages.plan = Some(plan),
                Err(error) => self.web_languages.message(error),
            }
            return;
        }
        let command = if value["command"].is_string() {
            value.clone()
        } else {
            value["command"].clone()
        };
        if let Some(name) = command["command"].as_str() {
            if let Some(server) = self.web_languages.servers.get_mut(&key) {
                match server.client.request(
                    "workspace/executeCommand",
                    json!({"command":name,"arguments":command["arguments"]}),
                ) {
                    Ok(id) => {
                        server.pending.insert(
                            id,
                            Pending {
                                kind: Kind::Command,
                                snapshot,
                                fallback: None,
                            },
                        );
                    }
                    Err(error) => self.web_languages.message(error),
                }
            }
        } else if value.get("data").is_some() {
            if let Some(server) = self.web_languages.servers.get_mut(&key) {
                match server.client.request("codeAction/resolve", value) {
                    Ok(id) => {
                        server.pending.insert(
                            id,
                            Pending {
                                kind: Kind::ResolveAction,
                                snapshot,
                                fallback: None,
                            },
                        );
                    }
                    Err(error) => self.web_languages.message(error),
                }
            }
        }
    }
    fn web_launch(&mut self, kind: usize, script: Option<String>) {
        if self.project_safe_mode() {
            return;
        }
        let Some(index) = self.active else { return };
        let Some(path) = self.tabs[index].state.path.clone() else {
            return;
        };
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf());
        let package = nearest_package(&path, Some(&root));
        let cwd = package
            .as_ref()
            .and_then(|path| path.parent())
            .unwrap_or(&root)
            .to_path_buf();
        let settings = self.settings.web_development.clone();
        let mut cfg = RunConfig {
            name: "Web task".into(),
            working_dir: Some(cwd.clone()),
            temporary: true,
            ..Default::default()
        };
        match kind {
            0 => {
                cfg.name = format!(
                    "Node · {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
                let mut spec = ConfigurationSpec::new(ConfigurationType::Node);
                spec.target = path.display().to_string();
                spec.executable = settings.node.into();
                cfg.type_options = Some(spec);
            }
            1 => {
                use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
                let mut spec = ConfigurationSpec::new(ConfigurationType::JavaScriptDebug);
                spec.target = path.display().to_string();
                spec.executable = settings.javascript_adapter.into();
                spec.adapter_arguments = settings.adapter_arguments;
                spec.debug_options = json!({"type":"pwa-node","request":"launch","program":path,"cwd":cwd,"runtimeExecutable":settings.node,"sourceMaps":true,"skipFiles":["<node_internals>/**"]});
                cfg.name = "Debug JavaScript / TypeScript".into();
                cfg.type_options = Some(spec);
                self.launch_config(cfg, crate::run_context::RunMode::Debug);
                return;
            }
            2 | 6 => {
                let args = if kind == 2 {
                    vec!["install".into()]
                } else {
                    vec!["run".into(), script.unwrap_or_default()]
                };
                cfg.name = crate::run_anything::quote_args(&args);
                if kind == 6 {
                    use rustpy_project::configuration_types::{
                        ConfigurationSpec, ConfigurationType,
                    };
                    let mut spec = ConfigurationSpec::new(ConfigurationType::Npm);
                    spec.target = args[1].clone();
                    spec.executable = settings.npm.into();
                    cfg.type_options = Some(spec);
                } else {
                    cfg.kind = RunConfigKind::Program {
                        program: settings.npm.into(),
                        args,
                    };
                }
            }
            3 | 4 => {
                let (name, tool) = if kind == 3 {
                    ("prettier", self.settings.save_actions.prettier.clone())
                } else {
                    ("eslint", self.settings.save_actions.eslint.clone())
                };
                let executable = if tool.executable.trim().is_empty() {
                    web_executable(&cwd, name)
                } else {
                    PathBuf::from(tool.executable)
                };
                cfg.name = format!("{name} · {}", path.display());
                let mut args = RunConfig::split_args(&tool.arguments);
                args.push(if kind == 3 { "--write" } else { "--fix" }.into());
                args.push(path.display().to_string());
                cfg.kind = RunConfigKind::Program {
                    program: executable,
                    args,
                };
            }
            5 => {
                self.open_web_file(&path, None, true);
                return;
            }
            _ => return,
        }
        self.launch_config(cfg, crate::run_context::RunMode::Run);
    }
    pub(super) fn web_development_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Web development toolchains");
        let settings = &mut self.settings.web_development;
        ui.checkbox(
            &mut settings.enabled,
            "Enable installed web language services",
        );
        ui.checkbox(
            &mut settings.automatic,
            "Start/synchronize services automatically in trusted projects",
        );
        for (label, value) in [
            ("Node executable", &mut settings.node),
            ("npm executable", &mut settings.npm),
            (
                "JavaScript DAP adapter executable",
                &mut settings.javascript_adapter,
            ),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(value);
            });
        }
        ui.label("JavaScript debugger uses the existing real TCP DAP bridge. Set an installed js-debug DAP server/launcher and arguments containing $Port$ / $Host$.");
        for value in &mut settings.adapter_arguments {
            ui.text_edit_singleline(value);
        }
        if ui.button("Add adapter argument").clicked() {
            settings.adapter_arguments.push(String::new());
        }
        if self.web_languages.selected.is_empty() {
            self.web_languages.selected = "typescript".into();
        }
        egui::ComboBox::from_id_salt("web_language_server_kind")
            .selected_text(&self.web_languages.selected)
            .show_ui(ui, |ui| {
                for name in settings.servers.keys() {
                    ui.selectable_value(&mut self.web_languages.selected, name.clone(), name);
                }
            })
            .combo_named(crate::i18n::tr_static("Language server"));
        if let Some(server) = settings.servers.get_mut(&self.web_languages.selected) {
            ui.checkbox(&mut server.enabled, "Enable this provider");
            ui.text_edit_singleline(&mut server.executable);
            ui.label("Arguments (one per row, passed without a shell)");
            for value in &mut server.arguments {
                ui.text_edit_singleline(value);
            }
            if ui.button("Add server argument").clicked() {
                server.arguments.push(String::new());
            }
            if ui.button("Edit initialization options").clicked() {
                self.web_languages.initialization =
                    serde_json::to_string_pretty(&server.initialization).unwrap_or_default();
            }
            ui.text_edit_multiline(&mut self.web_languages.initialization);
            if ui.button("Apply initialization JSON").clicked() {
                match serde_json::from_str::<Value>(&self.web_languages.initialization) {
                    Ok(value) => server.initialization = value,
                    Err(error) => self.web_languages.message(error.to_string()),
                }
            }
        }
        ui.label("Vue uses vue-language-server (standalone mode); Angular uses ngserver when angular.json is present; Svelte uses svelteserver. Configure initialization options/tool paths for the installed package versions. TypeScript/JSX/TSX and React use typescript-language-server/tsserver. HTML/CSS/JSON use vscode language servers. ESLint diagnostics/fixes may use the installed ESLint LSP provider or the existing save pipeline.");
        ui.collapsing("Language-server workspace configuration", |ui| {
            if ui.button("Load current configuration JSON").clicked() {
                self.web_languages.configuration =
                    serde_json::to_string_pretty(&settings.configuration).unwrap_or_default();
            }
            ui.text_edit_multiline(&mut self.web_languages.configuration);
            if ui.button("Apply workspace configuration JSON").clicked() {
                match serde_json::from_str::<Value>(&self.web_languages.configuration) {
                    Ok(value) => {
                        settings.configuration = value;
                        for server in self.web_languages.servers.values() {
                            let _ = server.client.notify(
                                "workspace/didChangeConfiguration",
                                json!({"settings":settings.configuration}),
                            );
                        }
                    }
                    Err(error) => self.web_languages.message(error.to_string()),
                }
            }
        });
        if ui.button("Open Web Development workspace").clicked() {
            self.pending_dialog = Some(Dialog::WebLanguages);
        }
    }
}
fn nearest_package(path: &Path, boundary: Option<&Path>) -> Option<PathBuf> {
    for directory in path.parent()?.ancestors() {
        if boundary.is_some_and(|root| !directory.starts_with(root)) {
            break;
        }
        let package = directory.join("package.json");
        if package.is_file() {
            return Some(package);
        }
    }
    None
}

#[cfg(test)]
mod fixtures {
    use super::*;
    fn document(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("component.tsx");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&path);
        (directory, app, path)
    }
    #[test]
    fn actual_completion_edits_shared_document_and_preserves_one_undo() {
        let source = "const π = 1;\nπ\n";
        let (_directory, mut app, _) = document(source);
        let snapshot = app.web_snapshot().unwrap();
        app.web_insert_completion(&snapshot, &json!({"label":"value","textEdit":{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},"newText":"${1:value}$0"},"insertTextFormat":2,"additionalTextEdits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"// import\n"}]}));
        assert_eq!(
            app.tabs[0].state.doc.text(),
            "// import\nconst π = 1;\nvalue\n"
        );
        let session = app.template_session.as_ref().unwrap();
        assert_eq!(session.vars.len(), 1);
        assert_eq!(&app.tabs[0].state.doc.text()[session.vars[0].1[0]], "value");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        // The same response must not apply again after the document version changed.
        app.web_apply_single(
            &snapshot,
            vec![(TextRange::empty(TextSize::new(0)), "stale".into())],
            "stale",
        );
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn workspace_review_rejects_external_uris_and_changed_source() {
        let (_directory, mut app, path) = document("const value = 1;\n");
        let snapshot = app.web_snapshot().unwrap();
        let edit = json!({"range":{"start":{"line":0,"character":6},"end":{"line":0,"character":11}},"newText":"renamed"});
        let mut changes = serde_json::Map::new();
        changes.insert(file_uri(&path).unwrap(), json!([edit.clone()]));
        let plan = app
            .web_plan(&snapshot, &json!({"changes":changes}), "Rename")
            .unwrap();
        assert_eq!(plan.changes.len(), 1);
        app.web_languages.plan = Some(plan);
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "// changed\n".into())],
            "change",
        );
        app.web_apply_plan();
        assert_eq!(
            app.tabs[0].state.doc.text(),
            "// changed\nconst value = 1;\n"
        );
        let current = app.web_snapshot().unwrap();
        assert!(app
            .web_plan(
                &current,
                &json!({"changes":{"file:///outside/project.ts":[edit]}}),
                "external"
            )
            .is_err());
        assert!(app
            .web_plan(
                &current,
                &json!({"documentChanges":[{"kind":"delete","uri":file_uri(&path).unwrap()}]}),
                "delete"
            )
            .is_err());
        app.safe_mode = true;
        assert!(!app.web_current(&current, false));
    }
    #[test]
    fn typed_node_and_npm_configuration_reach_real_program_lowering() {
        use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
        for (kind, target, expected) in [
            (ConfigurationType::Node, "src/main.js", vec!["src/main.js"]),
            (ConfigurationType::Npm, "build", vec!["run", "build"]),
        ] {
            let mut spec = ConfigurationSpec::new(kind);
            spec.target = target.into();
            let lowered = spec.materialize(&RunConfig::default()).unwrap();
            let RunConfigKind::Program { args, .. } = lowered.kind else {
                panic!("program lowering missing");
            };
            assert_eq!(args, expected);
        }
    }
}

#[cfg(test)]
#[path = "web_languages/configured_lsp.rs"]
mod configured_lsp;

#[cfg(all(test, target_os = "linux"))]
#[path = "web_languages/configured_js_debug.rs"]
mod configured_js_debug;

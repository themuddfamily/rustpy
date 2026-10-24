//! Database workspace consumer. Native handles never leave the retained worker.
use super::{DbEvent, DbPanel};
use rustpy_db::{
    session::{Reply, Request, Session},
    workspace::*,
    QueryResult,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};
#[derive(Default)]
pub(super) struct DatabaseWorkspace {
    pub model: Workspace,
    pub(super) native: super::native::NativeWorkspace,
    pub root: Option<PathBuf>,
    pub owners: Vec<Console>,
    pub retired: Vec<Console>,
    pub active: Option<u64>,
    pub next: u64,
    pub admitted: bool,
    pub selected: Option<u64>,
    pub draft: Source,
    pub secret: String,
    pub options: String,
    pub properties: bool,
    pub settings: bool,
    pub search: String,
    pub schema_filter: String,
    pub objects: HashMap<u64, Vec<Object>>,
    pub table: Option<Object>,
    pub filter: String,
    pub order: String,
    pub offset: usize,
    pub export_format: String,
    pub output_path: String,
    pub import_text: String,
    pub import_mapping: String,
    pub import_headers: bool,
    pub import_tsv: bool,
    pub selected_cell: Option<(usize, usize)>,
    pub cell_text: String,
    pub hidden: HashSet<String>,
    pub transpose: bool,
    pub column_order: Vec<String>,
    pub find: String,
    pub full_text: String,
    pub changes: Vec<Change>,
    pub design: String,
    pub design_open: bool,
    pub schema_reference: String,
    pub comparison: String,
    pub graph: String,
    pub output: String,
    pub scope_path: String,
    pub script_path: String,
    pub parameters: BTreeMap<String, String>,
    pub param_name: String,
    pub url_text: String,
    pub param_value: String,
    pub isolation: String,
    pub last_error: String,
    pub drivers: Option<std::sync::mpsc::Receiver<Result<Vec<String>, String>>>,
    pub driver_names: Vec<String>,
    pub change_owner: Option<u64>,
    pub extractor_id: Option<u64>,
    pub extractor_process: Option<std::process::Child>,
    pub migrations: Vec<rustpy_db::injection::Migration>,
    pub migration_job:
        Option<std::sync::mpsc::Receiver<(PathBuf, Vec<rustpy_db::injection::Migration>)>>,
    pub events: Vec<DbEvent>,
}
pub(super) struct Console {
    pub owner: Session,
    pub sql: String,
    pub results: Vec<(String, QueryResult)>,
    pub result: usize,
    pub messages: Vec<String>,
    pub running_sql: String,
    pub manual: bool,
    pub read_only: bool,
    pub dirty: bool,
    pub query_object: Option<Object>,
    pub result_object: Option<Object>,
    pub submitted: usize,
    pub navigate_error: Option<usize>,
}
impl DatabaseWorkspace {
    fn edit_selected_schemas(&mut self) {
        let Some(source) = self
            .model
            .sources
            .iter()
            .find(|source| Some(source.id) == self.selected)
        else {
            self.last_error = "Select a data source before editing its schemas".into();
            return;
        };
        self.draft = source.clone();
        self.draft.schemas = self
            .schema_filter
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        self.options = serde_json::to_string_pretty(&self.draft.options).unwrap_or_default();
        self.properties = true;
        self.events.push(DbEvent::Status(format!(
            "Editing schemas for {}",
            source.name
        )));
    }

    pub(super) fn queue(&mut self, change: Change) {
        if self
            .owners
            .iter()
            .any(|c| Some(c.owner.id) == self.change_owner && c.owner.busy && c.submitted > 0)
        {
            self.last_error = "Wait for the submitted changes before editing this batch".into();
            return;
        }
        if self
            .owners
            .iter()
            .find(|c| Some(c.owner.id) == self.active)
            .is_none_or(|c| Some(c.owner.source.id) != self.selected)
        {
            self.last_error = "Select the originating console before queuing changes".into();
            return;
        }
        if !self.changes.is_empty() && self.change_owner != self.active {
            self.last_error="Pending edits belong to another captured console; discard or submit them there first".into();
            return;
        }
        self.change_owner = self.active;
        self.changes.push(change);
    }

    pub fn load(&mut self, root: &Path) {
        if self.root.as_deref() == Some(root) {
            return;
        }
        self.native.project_changed();
        for mut owner in self.owners.drain(..) {
            owner.owner.stop();
            self.retired.push(owner);
        }
        self.model = Workspace::load(root);
        if self.model.sources.is_empty() {
            for (index, old) in rustpy_db::load_data_sources(root).into_iter().enumerate() {
                if let rustpy_db::DataSourceKind::Sqlite { path } = old.kind {
                    self.model.sources.push(Source {
                        id: index as u64 + 1,
                        name: old.name,
                        file: path,
                        ..Default::default()
                    });
                }
            }
        }
        self.root = Some(root.into());
        self.active = None;
        self.selected = None;
        self.objects.clear();
        self.table = None;
        self.changes.clear();
        self.url_text.clear();
        self.secret.clear();
        self.migrations.clear();
        self.events.clear();
        self.admitted = false;
        if let Some(child) = self.extractor_process.as_mut() {
            let _ = child.kill();
        }
        self.properties = false;
        self.last_error.clear();
    }
    pub(super) fn save(&mut self) {
        if let Some(root) = &self.root {
            if let Err(e) = self.model.save(root) {
                self.last_error = e.to_string();
            }
        }
    }
    pub fn poll(&mut self) {
        self.native
            .poll(self.root.as_deref(), self.admitted, &self.model.sources);
        for console in &mut self.owners {
            for reply in console.owner.poll() {
                match reply {
                    Reply::State {
                        auto_commit,
                        read_only,
                    } => {
                        console.manual = !auto_commit;
                        console.read_only = read_only;
                    }
                    Reply::Connected => console
                        .messages
                        .push("Connected. Refresh to introspect selected schemas.".into()),
                    Reply::Results(results) => {
                        let success = true;
                        self.model.history.push(History {
                            source: console.owner.source.id,
                            session: console.owner.name.clone(),
                            sql: console.running_sql.clone(),
                            success,
                            elapsed_ms: results.iter().map(|(_, r)| r.elapsed.as_millis()).sum(),
                        });
                        console.result_object = console.query_object.take();
                        self.selected_cell = None;
                        console.results = results;
                        console.result = 0;
                        console
                            .messages
                            .push(format!("{} result set(s)", console.results.len()));
                    }
                    Reply::Catalog(objects) => {
                        self.objects.insert(console.owner.id, objects);
                    }
                    Reply::Done(text) => {
                        if console.submitted > 0
                            && text.starts_with("Changes submitted")
                            && self.change_owner == Some(console.owner.id)
                        {
                            self.changes
                                .drain(..console.submitted.min(self.changes.len()));
                            console.submitted = 0;
                        }
                        if text.contains("committed") || text.contains("rolled back") {
                            console.dirty = false;
                        }
                        console.messages.push(text);
                    }
                    Reply::Error(text) => {
                        console.messages.push(text.clone());
                        self.last_error = text;
                        self.model.history.push(History {
                            source: console.owner.source.id,
                            session: console.owner.name.clone(),
                            sql: console.running_sql.clone(),
                            success: false,
                            elapsed_ms: 0,
                        });
                    }
                    Reply::Retired => console.messages.push("Session closed".into()),
                }
            }
            if console.messages.len() > 500 {
                console.messages.drain(..console.messages.len() - 500);
            }
        }
        for console in &mut self.retired {
            let _ = console.owner.poll();
        }
        self.retired.retain(|c| c.owner.alive());
        if let Some(receiver) = &self.migration_job {
            if let Ok((root, migrations)) = receiver.try_recv() {
                if self.root.as_ref() == Some(&root) {
                    self.migrations = migrations;
                }
                self.migration_job = None;
            }
        }
        if self.model.history.len() > 200 {
            self.model.history.drain(..self.model.history.len() - 200);
        }
        if let Some(receiver) = &self.drivers {
            if let Ok(result) = receiver.try_recv() {
                match result {
                    Ok(drivers) => self.driver_names = drivers,
                    Err(e) => self.last_error = e,
                }
                self.drivers = None;
            }
        }
        if self
            .extractor_process
            .as_mut()
            .is_some_and(|p| p.try_wait().ok().flatten().is_some())
        {
            self.extractor_process = None;
            self.extractor_id = None;
        }
    }
    pub(super) fn connect(&mut self, source: Source) -> Result<(), String> {
        if self.owners.len() + self.native.tab_count() >= 32
            || self.owners.iter().filter(|c| c.owner.alive()).count()
                + self.retired.len()
                + self.native.live_count()
                >= 16
        {
            return Err(
                "Close retained database tabs or wait for retiring sessions before opening another"
                    .into(),
            );
        }
        if !source.ddl_only && matches!(source.dialect, Dialect::Redis | Dialect::Mongo) {
            if !self.admitted {
                return Err("Trust this project before opening a native database session".into());
            }
            let root = self
                .root
                .as_deref()
                .ok_or("Open a project before connecting a native source")?;
            let id = self
                .next
                .checked_add(1)
                .ok_or("Database session ID limit reached")?;
            self.native.connect(
                id,
                source.clone(),
                std::mem::take(&mut self.secret),
                root,
                std::time::Duration::from_secs(
                    self.model.settings.timeout_seconds.clamp(1, 300) as u64
                ),
            )?;
            self.next = id;
            self.active = Some(id);
            self.selected = Some(source.id);
            self.table = None;
            return Ok(());
        }
        if self.owners.len() >= 32 {
            return Err("Close retained console tabs before opening more (limit 32)".into());
        }
        if !self.admitted {
            return Err(
                "Trust this project before opening database sessions or SSH tunnels".into(),
            );
        }
        if self.owners.iter().filter(|c| c.owner.alive()).count() + self.retired.len() >= 16 {
            return Err("Close a session before opening another (limit 16)".into());
        }
        self.next = self.next.saturating_add(1);
        let id = self.next;
        let owner = Session::start(
            id,
            source.clone(),
            format!("{} / console {id}", source.name),
            std::mem::take(&mut self.secret),
            self.model.settings.clone(),
        )
        .map_err(|e| e.to_string())?;
        self.owners.push(Console {
            owner,
            sql: String::new(),
            results: vec![],
            result: 0,
            messages: vec![],
            running_sql: String::new(),
            manual: !self.model.settings.auto_commit,
            read_only: source.read_only,
            dirty: false,
            query_object: None,
            result_object: None,
            submitted: 0,
            navigate_error: None,
        });
        self.active = Some(id);
        self.selected = Some(source.id);
        Ok(())
    }
    pub(super) fn send(&mut self, request: Request) {
        if !self.admitted {
            self.last_error = "Trust the project before executing database requests".into();
            return;
        }
        self.last_error.clear();
        if matches!(&request, Request::Submit(_)) && self.change_owner != self.active {
            self.last_error = "Pending changes belong to another captured console".into();
            return;
        }
        if let Some(console) = self
            .owners
            .iter_mut()
            .find(|c| Some(c.owner.id) == self.active)
        {
            if let Request::Query { sql, .. } = &request {
                console.running_sql = sql.clone();
                console.dirty |= console.manual;
            }
            let query = matches!(&request, Request::Query { .. } | Request::Explain(_));
            let submitted = if let Request::Submit(changes) = &request {
                changes.len()
            } else {
                0
            };
            match console.owner.send(request) {
                Ok(()) => {
                    if query {
                        console.query_object = self.table.clone();
                    }
                    console.submitted = submitted;
                }
                Err(e) => self.last_error = e.to_string(),
            }
        } else {
            self.last_error = "Open a query console first".into();
        }
    }
    pub(super) fn execute(&mut self, sql: String) {
        match rustpy_db::injection::bind_user_parameters(
            &sql,
            &self.model.settings.parameter_prefix,
            &self.parameters,
        ) {
            Ok(sql) => self.send(Request::Query {
                sql,
                limit: self.model.settings.result_limit.clamp(1, 100_000),
            }),
            Err(e) => self.last_error = e.to_string(),
        }
    }
    pub(super) fn page(&mut self) {
        if self
            .owners
            .iter()
            .find(|c| Some(c.owner.id) == self.active)
            .is_none_or(|c| Some(c.owner.source.id) != self.selected)
        {
            self.last_error = "Connect the selected data source before opening data".into();
            return;
        }
        if let Some(object) = self.table.clone() {
            let dialect = self.current_dialect();
            self.execute(dialect.page(
                &object.qualified(),
                &self.filter,
                &self.order,
                self.offset,
                self.model.settings.page_size.clamp(1, 10000),
            ));
        }
    }
    pub(super) fn current_dialect(&self) -> Dialect {
        self.owners
            .iter()
            .find(|c| Some(c.owner.id) == self.active)
            .map(|c| c.owner.source.dialect)
            .or_else(|| self.native.source(self.active).map(|source| source.dialect))
            .unwrap_or_default()
    }
    pub(super) fn result_object(&self) -> Option<&Object> {
        self.owners
            .iter()
            .find(|c| Some(c.owner.id) == self.active)?
            .result_object
            .as_ref()
    }
    pub(super) fn current_result(&self) -> Option<&QueryResult> {
        let c = self
            .owners
            .iter()
            .find(|c| Some(c.owner.id) == self.active)?;
        c.results.get(c.result).map(|(_, r)| r)
    }
}
impl DbPanel {
    /// Poll even with the tool window hidden; retained owners survive project switches.
    pub(crate) fn set_database_execution_allowed(&mut self, allowed: bool) {
        self.execution_allowed = allowed;
        self.workspace.admitted = allowed;
        if !allowed {
            self.workspace.native.stop_all();
        }
    }
    pub(crate) fn poll_database(&mut self) {
        self.workspace.poll();
        // A closed panel can drop its session before its worker has joined.
        // Keep those retained workers moving even while Database is hidden.
        let _ = rustpy_db::native::poll_retiring();
    }
    pub(crate) fn database_owners(&self) -> Vec<(u64, String, bool, bool)> {
        self.workspace
            .owners
            .iter()
            .chain(&self.workspace.retired)
            .filter(|c| c.owner.alive())
            .map(|c| {
                (
                    c.owner.id,
                    c.owner.name.clone(),
                    c.owner.busy,
                    c.owner.stopping,
                )
            })
            .chain(self.workspace.native.rows())
            .chain(
                self.workspace
                    .extractor_id
                    .map(|id| (id, "Database extractor".into(), true, false)),
            )
            .collect()
    }
    pub(crate) fn select_database_owner(&mut self, id: u64) -> bool {
        if let Some(source) = self.workspace.native.source(Some(id)) {
            self.workspace.active = Some(id);
            self.workspace.selected = Some(source.id);
            return true;
        }
        let Some(console) = self
            .workspace
            .owners
            .iter()
            .find(|console| console.owner.id == id)
        else {
            return false;
        };
        self.workspace.active = Some(id);
        self.workspace.selected = Some(console.owner.source.id);
        true
    }
    pub(crate) fn stop_database(&mut self, id: u64) {
        self.workspace.native.stop(id);
        if let Some(c) = self
            .workspace
            .owners
            .iter_mut()
            .chain(&mut self.workspace.retired)
            .find(|c| c.owner.id == id)
        {
            c.owner.stop();
        }
        if self.workspace.extractor_id == Some(id) {
            if let Some(p) = self.workspace.extractor_process.as_mut() {
                let _ = p.kill();
            }
        }
    }
    pub(crate) fn stop_all_database(&mut self) {
        self.workspace.native.stop_all();
        for c in self
            .workspace
            .owners
            .iter_mut()
            .chain(&mut self.workspace.retired)
        {
            c.owner.stop();
        }
        if let Some(p) = self.workspace.extractor_process.as_mut() {
            let _ = p.kill();
        }
    }
    pub(crate) fn database_native_pending_changes(&self) -> usize {
        self.workspace.native.pending_changes()
    }
    pub(crate) fn database_native_stamps(&self) -> Vec<String> {
        self.workspace.native.stamps()
    }
    pub(crate) fn database_pending_changes(&self) -> usize {
        self.workspace.changes.len()
            + self.workspace.owners.iter().filter(|c| c.dirty).count()
            + self.workspace.native.pending_changes()
    }
    pub(crate) fn database_has_owners(&self) -> bool {
        !self.database_owners().is_empty()
            || self.workspace.extractor_process.is_some()
            || !rustpy_db::native::poll_retiring().is_empty()
    }
    pub(crate) fn database_project_changed(&mut self, root: &Path) {
        self.workspace.load(root);
    }
    pub(super) fn workspace_ui(&mut self, ui: &mut egui::Ui, root: &Path) -> Vec<DbEvent> {
        self.workspace.load(root);
        self.workspace.admitted = self.execution_allowed;
        self.workspace.poll();
        let w = &mut self.workspace;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Database");
            if ui.button("+ Data source / Properties").clicked() {
                w.properties = true;
                w.draft = Source {
                    id: w.model.sources.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                    name: "Data source".into(),
                    ..Default::default()
                };
                w.options.clear();
            }
            if ui.button("From URL / clipboard").clicked() {
                if let Ok(mut clipboard) = crate::system_clipboard::open() {
                    if let Ok(text) = clipboard.get_text() {
                        match rustpy_db::injection::source_from_url(
                            &text,
                            w.model.sources.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                        ) {
                            Ok((source, secret)) => {
                                w.draft = source;
                                w.secret = secret;
                                w.properties = true;
                            }
                            Err(e) => w.last_error = e.to_string(),
                        }
                    }
                }
            }
            if ui.button("Settings").clicked() {
                w.settings = !w.settings;
            }
            if ui.button("Session manager").clicked() {
                w.output = w
                    .owners
                    .iter()
                    .chain(&w.retired)
                    .map(|c| {
                        format!(
                            "{}: {} · {}{}",
                            c.owner.id,
                            c.owner.name,
                            if c.owner.busy { "working" } else { "idle" },
                            if c.dirty {
                                " · uncommitted transaction"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        });
        for event in ui.input(|i| i.events.clone()) {
            if let egui::Event::Paste(text) = event {
                if let Ok((source, secret)) = rustpy_db::injection::source_from_url(
                    &text,
                    w.model.sources.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                ) {
                    w.draft = source;
                    w.secret = secret;
                    w.properties = true;
                }
            }
        }
        if !self.new_source_path.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Local file");
                ui.text_edit_singleline(&mut self.new_source_path);
                if ui.button("Create source from file").clicked() {
                    let path = root.join(&self.new_source_path);
                    let dialect = if path.extension().is_some_and(|e| e == "duckdb") {
                        Dialect::DuckDb
                    } else {
                        Dialect::Sqlite
                    };
                    w.draft = Source {
                        id: w.model.sources.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                        name: path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into(),
                        file: path,
                        dialect,
                        ..Default::default()
                    };
                    w.properties = true;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut w.url_text)
                    .password(true)
                    .hint_text("Database URL (credentials stay in this session)"),
            );
            if ui.button("Import URL").clicked() {
                match rustpy_db::injection::source_from_url(
                    &w.url_text,
                    w.model.sources.iter().map(|s| s.id).max().unwrap_or(0) + 1,
                ) {
                    Ok((source, secret)) => {
                        w.draft = source;
                        w.secret = secret;
                        w.properties = true;
                        w.url_text.clear();
                    }
                    Err(e) => w.last_error = e.to_string(),
                }
            }
        });
        w.properties_ui(ui);
        if w.settings {
            w.settings_ui(ui);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for source in w.model.sources.clone() {
                let response = ui.selectable_label(
                    w.selected == Some(source.id),
                    format!("{} · {:?}", source.name, source.dialect),
                );
                if response.clicked() {
                    w.selected = Some(source.id);
                    w.schema_filter = source.schemas.join(", ");
                    w.active = w
                        .owners
                        .iter()
                        .find(|c| c.owner.source == source && c.owner.alive() && !c.owner.stopping)
                        .map(|c| c.owner.id);
                    w.table = None;
                    self.selected_source = Some(source.name.clone());
                }
                response.context_menu(|ui| {
                    if ui.button("Properties / Schemas… / Introspection").clicked() {
                        w.draft = source.clone();
                        w.options =
                            serde_json::to_string_pretty(&source.options).unwrap_or_default();
                        w.properties = true;
                        ui.close();
                    }
                    if ui.button("Jump to query console / New console").clicked() {
                        if let Err(e) = w.connect(source.clone()) {
                            w.last_error = e;
                        }
                        ui.close();
                    }
                    if ui.button("Remove data source").clicked() {
                        w.model.sources.retain(|s| s.id != source.id);
                        w.save();
                        ui.close();
                    }
                });
            }
            if ui.button("Connect / New console").clicked() {
                if let Some(source) = w
                    .model
                    .sources
                    .iter()
                    .find(|s| Some(s.id) == w.selected)
                    .cloned()
                {
                    if let Err(e) = w.connect(source) {
                        w.last_error = e;
                    }
                }
            }
        });
        w.native.tabs(ui, &mut w.active, w.root.as_deref());
        ui.horizontal_wrapped(|ui| {
            for console in &w.owners {
                if ui
                    .selectable_label(
                        w.active == Some(console.owner.id),
                        format!(
                            "{}{}",
                            console.owner.name,
                            if console.owner.busy { " …" } else { "" }
                        ),
                    )
                    .clicked()
                {
                    w.active = Some(console.owner.id);
                    w.selected = Some(console.owner.source.id);
                    w.table = console.result_object.clone();
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                if let Some(id) = w.active.filter(|_| w.native.contains(w.active)) {
                    w.native.refresh(id);
                } else {
                    w.send(Request::Catalog);
                }
            }
            if ui.button("Close inactive console tab").clicked() {
                if let Some(id) = w.active {
                    if w.native.contains(Some(id)) {
                        match w.native.close_inactive(id) {
                            Ok(()) => w.active = None,
                            Err(error) => w.last_error = error,
                        }
                    } else if w
                        .owners
                        .iter()
                        .find(|c| c.owner.id == id)
                        .is_some_and(|c| !c.owner.alive())
                    {
                        w.owners.retain(|c| c.owner.id != id);
                        w.active = None;
                    } else {
                        w.last_error =
                            "Disconnect the console and wait for its worker before closing".into();
                    }
                }
            }
            if ui.button("Disconnect / Cancel").clicked() {
                if let Some(id) = w.active {
                    w.native.stop(id);
                }
                if let Some(c) = w.owners.iter_mut().find(|c| Some(c.owner.id) == w.active) {
                    c.owner.stop();
                }
            }
            if !w.native.contains(w.active) {
                ui.label("Speed search");
                ui.text_edit_singleline(&mut w.search);
            }
        });
        if !w.native.contains(w.active) {
            ui.horizontal(|ui| {
                ui.label("Schemas (comma separated)");
                ui.text_edit_singleline(&mut w.schema_filter);
                if ui.button("Edit selected schemas…").clicked() {
                    w.edit_selected_schemas();
                }
            });
        }
        if let Some(id) = w.active.filter(|_| w.native.contains(w.active)) {
            w.native
                .poll(w.root.as_deref(), w.admitted, &w.model.sources);
            w.native.ui(ui, id, w.admitted);
        } else {
            w.catalog_ui(ui);
            w.migrations_ui(ui);
            w.console_ui(ui);
            w.grid_ui(ui);
            w.pending_ui(ui);
            w.design_ui(ui);
        }
        if !w.last_error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &w.last_error);
        }
        if !w.output.is_empty() {
            egui::CollapsingHeader::new("Output / Diagrams / Comparison").show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut w.output)
                        .code_editor()
                        .desired_rows(8),
                );
                if ui.button("Copy output").clicked() {
                    ui.ctx().copy_text(w.output.clone());
                }
            });
        }
        if w.owners
            .iter()
            .chain(&w.retired)
            .any(|c| c.owner.alive() && c.owner.busy)
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
        std::mem::take(&mut w.events)
    }
}
impl DbPanel {
    /// Executes captured SQL only in an already admitted scoped session.
    pub(crate) fn open_scoped_sql(
        &mut self,
        path: Option<&Path>,
        sql: String,
    ) -> Result<(), String> {
        let source = path
            .and_then(|p| self.workspace.model.scope(p))
            .map(|s| s.source)
            .or(self.workspace.selected)
            .ok_or("Assign a database resolution scope or select a data source first")?;
        let configuration = self
            .workspace
            .model
            .sources
            .iter()
            .find(|s| s.id == source)
            .cloned()
            .ok_or("Scoped data source was removed")?;
        let console = self
            .workspace
            .owners
            .iter_mut()
            .find(|c| c.owner.source == configuration && c.owner.alive() && !c.owner.stopping)
            .ok_or("Connect the scoped data source explicitly before running SQL")?;
        self.workspace.active = Some(console.owner.id);
        self.workspace.selected = Some(source);
        console.sql = sql.clone();
        self.show_console = true;
        self.workspace.table = None;
        self.workspace.execute(sql);
        if self.workspace.last_error.is_empty() {
            Ok(())
        } else {
            Err(self.workspace.last_error.clone())
        }
    }
    pub(crate) fn edit_database_scope(&mut self, path: &Path) {
        self.workspace.scope_path = path.display().to_string();
        self.workspace.settings = true;
    }
    /// Facet/SQLAlchemy import authority: no settings module is imported here.
    pub(crate) fn import_database_source(&mut self, mut source: Source, secret: String) {
        source.id = self
            .workspace
            .model
            .sources
            .iter()
            .map(|s| s.id)
            .max()
            .unwrap_or(0)
            + 1;
        self.workspace.draft = source;
        self.workspace.secret = secret;
        self.workspace.properties = true;
    }
}

impl DbPanel {
    pub(crate) fn sql_schema_hints(
        &self,
        path: Option<&Path>,
        text: &str,
    ) -> (Vec<String>, Vec<String>) {
        let source = path
            .and_then(|p| self.workspace.model.scope(p))
            .map(|s| s.source)
            .or(self.workspace.selected);
        let objects = source
            .and_then(|s| {
                self.workspace
                    .owners
                    .iter()
                    .rev()
                    .find(|c| c.owner.source.id == s && c.owner.alive())
                    .and_then(|c| self.workspace.objects.get(&c.owner.id))
            })
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        (
            rustpy_db::injection::completions(text, objects),
            rustpy_db::injection::inspect_sql(text, objects),
        )
    }
}
impl DbPanel {
    /// Merely registers a local file; opening it never starts a database connection.
    pub(crate) fn register_database_file(&mut self, root: &Path, path: &Path) {
        self.workspace.load(root);
        let dialect = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("duckdb"))
        {
            Dialect::DuckDb
        } else {
            Dialect::Sqlite
        };
        if let Some(source) = self.workspace.model.sources.iter().find(|s| s.file == path) {
            self.workspace.selected = Some(source.id);
            return;
        }
        let source = Source {
            id: self
                .workspace
                .model
                .sources
                .iter()
                .map(|s| s.id)
                .max()
                .unwrap_or(0)
                + 1,
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            file: path.into(),
            dialect,
            ..Default::default()
        };
        self.workspace.selected = Some(source.id);
        self.workspace.draft = source.clone();
        self.workspace.properties = true;
        self.workspace.model.sources.push(source);
        self.workspace.save();
    }
}
impl DbPanel {
    pub(crate) fn database_diagram_snapshot(&self) -> Result<(String, Vec<Object>), String> {
        let console = self
            .workspace
            .owners
            .iter()
            .find(|c| Some(c.owner.id) == self.workspace.active)
            .ok_or("Select an admitted database console first")?;
        let objects = self
            .workspace
            .objects
            .get(&console.owner.id)
            .ok_or("Refresh database introspection before opening its ER diagram")?;
        Ok((
            serde_json::to_string(&console.owner.source).map_err(|e| e.to_string())?,
            objects.clone(),
        ))
    }
    pub(crate) fn open_diagram_object(
        &mut self,
        identity: &str,
        captured_json: &str,
    ) -> Result<(), String> {
        let captured: Object = serde_json::from_str(captured_json).map_err(|e| e.to_string())?;
        if !self.select_service(identity) {
            return Err(
                "Captured data source changed or was removed; refresh the ER diagram".into(),
            );
        }
        let node = self
            .workspace
            .active
            .and_then(|id| self.workspace.objects.get(&id))
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|node| node.qualified() == captured.qualified())
            })
            .cloned()
            .ok_or("Captured database object is no longer in the admitted catalog")?;
        if serde_json::to_string(&node).map_err(|e| e.to_string())? != captured_json {
            return Err(
                "Database introspection changed; refresh the ER diagram before opening this object"
                    .into(),
            );
        }
        self.workspace.table = Some(node);
        self.workspace.offset = 0;
        self.workspace.page();
        if self.workspace.last_error.is_empty() {
            Ok(())
        } else {
            Err(self.workspace.last_error.clone())
        }
    }
}

#[cfg(test)]
mod schema_selection_tests {
    use super::*;
    #[test]
    fn schema_selection_prepares_only_captured_source_draft_until_save() {
        let source = Source {
            id: 7,
            name: "selected".into(),
            schemas: vec!["old".into()],
            ..Default::default()
        };
        let mut workspace = DatabaseWorkspace {
            model: Workspace {
                sources: vec![source.clone()],
                ..Default::default()
            },
            selected: Some(7),
            schema_filter: " public, analytics, , ".into(),
            ..Default::default()
        };
        workspace.edit_selected_schemas();
        assert_eq!(workspace.draft.id, source.id);
        assert_eq!(workspace.draft.schemas, vec!["public", "analytics"]);
        assert_eq!(workspace.model.sources, vec![source]);
        assert!(workspace.properties);
        assert!(workspace.owners.is_empty());
        assert!(matches!(workspace.events.as_slice(), [DbEvent::Status(_)]));
        workspace.selected = Some(99);
        workspace.properties = false;
        workspace.edit_selected_schemas();
        assert!(!workspace.properties);
        assert!(!workspace.last_error.is_empty());
    }
}

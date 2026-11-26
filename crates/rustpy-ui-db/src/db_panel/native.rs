//! Native key/document consumers. These owners never enter SQL transaction UI.
use crate::a11y::AccessibleCombo;
use rustpy_db::{
    native::{
        NativeEvent, NativeReply, NativeRequest, NativeSession, RedisMutation, RedisRecord,
        RedisValue,
    },
    workspace::{Dialect, Source},
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
pub(super) struct NativeWorkspace {
    owners: Vec<NativeConsole>,
}
#[derive(Clone, Default)]
struct WriteOutcome(Arc<Mutex<WriteState>>);
#[derive(Default)]
struct WriteState {
    unconfirmed: bool,
    pending: Option<(u64, u64)>,
    last_operation: Option<(u64, u64)>,
}
impl WriteOutcome {
    fn get(&self) -> bool {
        self.0.lock().unwrap().unconfirmed
    }
    fn writer(&self) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .last_operation
            .map(|(owner, _)| owner)
    }
    fn operation(&self) -> Option<(u64, u64)> {
        self.0.lock().unwrap().last_operation
    }
    fn begin(&self, session: u64, request: u64) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.unconfirmed || state.pending.is_some() {
            return false;
        }
        state.unconfirmed = true;
        state.pending = Some((session, request));
        state.last_operation = Some((session, request));
        true
    }
    fn finish(&self, session: u64, request: u64, confirmed: bool) {
        let mut state = self.0.lock().unwrap();
        if state.pending == Some((session, request)) {
            state.pending = None;
            state.unconfirmed = !confirmed;
        }
    }
    fn retired(&self, session: u64) {
        let mut state = self.0.lock().unwrap();
        if state.pending.is_some_and(|(owner, _)| owner == session) {
            state.pending = None;
        }
    }
    fn can_resolve(&self) -> bool {
        let state = self.0.lock().unwrap();
        state.unconfirmed && state.pending.is_none()
    }
    fn resolve(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.pending.is_some() {
            return false;
        }
        state.unconfirmed = false;
        true
    }
}
struct NativeConsole {
    owner: NativeSession,
    root: PathBuf,
    canonical_root: PathBuf,
    connected: bool,
    stopping: bool,
    stale: bool,
    next_request: u64,
    pending: Option<Pending>,
    status: String,
    dirty: bool,
    unconfirmed_write: WriteOutcome,
    outcome_checked: bool,
    outcome_generation: Option<(u64, u64)>,
    scan_pattern: String,
    scan_cursor: u64,
    keys: Vec<Vec<u8>>,
    key_hex: String,
    record: Option<RedisRecord>,
    record_cursor: u64,
    load_string_all: bool,
    value_hex: String,
    field_hex: String,
    index: i64,
    score: String,
    expiry: String,
    collections: Vec<String>,
    collection: String,
    loaded_collection: Option<String>,
    filter: String,
    sort: String,
    offset: u64,
    limit: usize,
    documents: Vec<Value>,
    has_more: bool,
    selected: Option<usize>,
    document: String,
    new_document: bool,
}
struct Pending {
    id: u64,
    kind: PendingKind,
}
enum PendingKind {
    Read,
    RedisRead { cursor: u64 },
    MongoRead { collection: String },
    RedisWrite(Vec<u8>),
    MongoWrite,
}
impl PendingKind {
    fn writes(&self) -> bool {
        !matches!(
            self,
            Self::Read | Self::RedisRead { .. } | Self::MongoRead { .. }
        )
    }
}
impl NativeWorkspace {
    pub(super) fn tab_count(&self) -> usize {
        self.owners.len()
    }
    pub(super) fn live_count(&self) -> usize {
        self.owners.iter().filter(|c| c.owner.alive()).count()
    }
    pub(super) fn contains(&self, id: Option<u64>) -> bool {
        self.owners.iter().any(|c| Some(c.owner.id) == id)
    }
    pub(super) fn source(&self, id: Option<u64>) -> Option<&Source> {
        self.owners
            .iter()
            .find(|c| Some(c.owner.id) == id)
            .map(|c| &c.owner.source)
    }
    pub(super) fn connect(
        &mut self,
        id: u64,
        source: Source,
        secret: String,
        root: &Path,
        timeout: Duration,
    ) -> Result<(), String> {
        if self.owners.len() >= 32 || self.owners.iter().filter(|c| c.owner.alive()).count() >= 16 {
            return Err("Close a database session before opening another".into());
        }
        let canonical_root = dunce::canonicalize(root).map_err(|e| e.to_string())?;
        // Exact captured connection authority: changed endpoints must never inherit
        // a previous server's edit buffer or reconciliation acknowledgement.
        let authority = self
            .owners
            .iter()
            .find(|c| c.canonical_root == canonical_root && c.owner.source == source);
        let unconfirmed_write = authority
            .map(|c| c.unconfirmed_write.clone())
            .unwrap_or_default();
        let unresolved = self.owners.iter().find(|c| {
            c.canonical_root == canonical_root
                && c.owner.source == source
                && unconfirmed_write.get()
                && Some(c.owner.id) == unconfirmed_write.writer()
        });
        let retained_document = unresolved.map(|c| {
            (
                c.document.clone(),
                c.collection.clone(),
                c.new_document,
                c.record.clone(),
                c.key_hex.clone(),
                c.value_hex.clone(),
                c.record_cursor,
                c.index,
                c.field_hex.clone(),
                c.score.clone(),
                c.expiry.clone(),
            )
        });
        let owner = NativeSession::start(id, source, secret, timeout).map_err(|e| e.to_string())?;
        self.owners.push(NativeConsole {
            owner,
            root: root.into(),
            canonical_root,
            connected: false,
            stopping: false,
            stale: false,
            next_request: 0,
            pending: None,
            status: "Connecting…".into(),
            dirty: false,
            unconfirmed_write,
            outcome_checked: false,
            outcome_generation: None,
            scan_pattern: "*".into(),
            scan_cursor: 0,
            keys: Vec::new(),
            key_hex: String::new(),
            record: None,
            record_cursor: 0,
            load_string_all: false,
            value_hex: String::new(),
            field_hex: String::new(),
            index: 0,
            score: "0".into(),
            expiry: "60000".into(),
            collections: Vec::new(),
            collection: String::new(),
            loaded_collection: None,
            filter: "{}".into(),
            sort: "{}".into(),
            offset: 0,
            limit: 200,
            documents: Vec::new(),
            has_more: false,
            selected: None,
            document: "{}".into(),
            new_document: false,
        });
        if let Some((
            document,
            collection,
            new_document,
            record,
            key_hex,
            value_hex,
            record_cursor,
            index,
            field_hex,
            score,
            expiry,
        )) = retained_document
        {
            let c = self.owners.last_mut().unwrap();
            c.document = document;
            c.collection = collection;
            c.new_document = new_document;
            c.record = record;
            c.key_hex = key_hex;
            c.value_hex = value_hex;
            c.record_cursor = record_cursor;
            c.index = index;
            c.field_hex = field_hex;
            c.score = score;
            c.expiry = expiry;
            c.dirty = true;
        }
        Ok(())
    }
    pub(super) fn rows(&self) -> Vec<(u64, String, bool, bool)> {
        self.owners
            .iter()
            .filter(|c| c.owner.alive())
            .map(|c| {
                (
                    c.owner.id,
                    format!("{} / session {}", c.owner.source.name, c.owner.id),
                    c.owner.busy(),
                    c.stopping,
                )
            })
            .collect()
    }
    pub(super) fn stamps(&self) -> Vec<String> {
        self.owners
            .iter()
            .filter(|c| c.owner.alive() || c.dirty || c.unconfirmed_write.get())
            .map(|c| {
                format!(
                    "{}:next={}:dirty={}:unconfirmed={}:operation={:?}",
                    c.owner.lifecycle_stamp(),
                    c.next_request,
                    c.dirty,
                    c.unconfirmed_write.get(),
                    c.unconfirmed_write.operation(),
                )
            })
            .collect()
    }
    pub(super) fn pending_changes(&self) -> usize {
        self.owners
            .iter()
            .filter(|c| {
                c.dirty
                    || c.unconfirmed_write.get()
                    || c.pending.as_ref().is_some_and(|p| p.kind.writes())
            })
            .count()
    }
    pub(super) fn stop(&mut self, id: u64) {
        if let Some(c) = self.owners.iter_mut().find(|c| c.owner.id == id) {
            c.stop();
        }
    }
    pub(super) fn stop_all(&mut self) {
        for c in &mut self.owners {
            c.stop();
        }
    }
    pub(super) fn source_running(&self, source: &Source) -> bool {
        self.owners
            .iter()
            .any(|c| c.owner.source == *source && c.owner.alive() && !c.stopping)
    }
    pub(super) fn source_owner(&self, source: &Source) -> Option<u64> {
        self.owners
            .iter()
            .find(|c| c.owner.source == *source && c.owner.alive() && !c.stopping)
            .map(|c| c.owner.id)
    }
    pub(super) fn stop_source(&mut self, source: &Source) {
        for c in &mut self.owners {
            if c.owner.source == *source {
                c.stop();
            }
        }
    }
    pub(super) fn project_changed(&mut self) {
        for c in &mut self.owners {
            c.stale = true;
            if c.unconfirmed_write.get() {
                c.stop();
                continue;
            }
            c.dirty = false;
            c.keys.clear();
            c.record = None;
            c.documents.clear();
            c.document.clear();
            c.value_hex.clear();
            c.stop();
        }
        self.owners
            .retain(|c| c.owner.alive() || c.unconfirmed_write.get());
    }
    pub(super) fn poll(&mut self, root: Option<&Path>, admitted: bool, sources: &[Source]) {
        for c in &mut self.owners {
            let valid = admitted
                && root == Some(c.root.as_path())
                && dunce::canonicalize(&c.root).ok().as_ref() == Some(&c.canonical_root)
                && sources.iter().any(|source| source == &c.owner.source);
            if !valid && !c.stopping {
                c.stale = true;
                c.status="Connection settings, project location or trust changed. Reconnect before continuing.".into();
                c.stop();
            }
            let replies = c.owner.poll();
            for reply in replies {
                c.reply(reply, valid);
            }
            if !c.owner.alive() {
                c.unconfirmed_write.retired(c.owner.id);
            }
        }
    }
    pub(super) fn tabs(
        &mut self,
        ui: &mut egui::Ui,
        active: &mut Option<u64>,
        root: Option<&Path>,
    ) {
        ui.horizontal_wrapped(|ui| {
            for c in &self.owners {
                if root != Some(c.root.as_path()) {
                    continue;
                }
                if ui
                    .selectable_label(
                        *active == Some(c.owner.id),
                        format!(
                            "{} / session {}{}",
                            c.owner.source.name,
                            c.owner.id,
                            if c.stale {
                                " (source changed)"
                            } else if c.owner.busy() {
                                " …"
                            } else if !c.owner.alive() {
                                " (retired)"
                            } else {
                                ""
                            }
                        ),
                    )
                    .clicked()
                {
                    *active = Some(c.owner.id);
                }
            }
        });
    }
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, id: u64, admitted: bool) {
        let Some(c) = self.owners.iter_mut().find(|c| c.owner.id == id) else {
            return;
        };
        ui.push_id(("native-db-session",id,c.canonical_root.clone()),|ui| {
        ui.label("Changes apply immediately. Commit and Rollback affect SQL sessions only. Discard clears your local edits.");
        ui.label(&c.status);
        if let Some(pending) = &c.pending {
            let id = pending.id;
            if ui.button("Cancel operation").clicked() {
                c.owner.cancel(id);
                c.status=if pending.kind.writes(){"Cancellation requested. The change may already have applied. Wait for confirmation before retrying."}else{"Cancellation requested. Wait for the operation to finish."}.into();
            }
        }
        if ui.button("Disconnect").clicked() {
            c.stop();
        }
        if ui.add_enabled(!c.unconfirmed_write.get(), egui::Button::new("Discard local edits")).clicked() {
            c.discard();
        }
        if c.unconfirmed_write.get() && c.pending.is_none() {
            ui.label("A previous write has no confirmation and may already have applied. Further changes are blocked, including after reconnecting. Your local value or document and its identity have been retained.");
            let operation = c.unconfirmed_write.operation();
            if c.outcome_generation != operation { c.outcome_checked = false; c.outcome_generation = operation; }
            ui.add_enabled(c.unconfirmed_write.can_resolve(), egui::Checkbox::new(&mut c.outcome_checked, "I checked the server outcome and accept responsibility for resolving this write"));
            if ui.add_enabled(c.outcome_checked && c.unconfirmed_write.can_resolve(), egui::Button::new("Resolve unconfirmed write")).clicked() {
                if !c.unconfirmed_write.resolve() { return; }
                c.outcome_checked = false;
                c.status = "Write outcome explicitly resolved. Review the server value before making further changes.".into();
            }
        }
        let enabled = admitted
            && !c.stale
            && !c.stopping
            && c.connected
            && !c.owner.busy()
            && c.pending.is_none();
        ui.add_enabled_ui(enabled, |ui| match c.owner.source.dialect {
            Dialect::Redis => c.redis_ui(ui),
            Dialect::Mongo => c.mongo_ui(ui),
            _ => {}
        });
        });
    }
    pub(super) fn refresh(&mut self, id: u64) {
        if let Some(c) = self.owners.iter_mut().find(|c| c.owner.id == id) {
            c.refresh();
        }
    }
    pub(super) fn close_inactive(&mut self, id: u64) -> Result<(), String> {
        if let Some(c) = self.owners.iter().find(|c| c.owner.id == id) {
            if c.owner.alive() || c.dirty || c.unconfirmed_write.get() {
                return Err("Disconnect, wait for the connection to finish closing and resolve local edits or unconfirmed writes before closing this tab".into());
            }
        }
        self.owners.retain(|c| c.owner.id != id);
        Ok(())
    }
}
impl NativeConsole {
    fn stop(&mut self) {
        self.stopping = true;
        self.owner.stop();
    }
    fn discard(&mut self) {
        self.load_string_all = false;
        self.dirty = false;
        if let Some(record) = &self.record {
            self.value_hex = match &record.value {
                RedisValue::String(value) => hex(value),
                _ => String::new(),
            };
        }
        self.document = self
            .selected
            .and_then(|i| self.documents.get(i))
            .and_then(|v| serde_json::to_string_pretty(v).ok())
            .unwrap_or_else(|| "{}".into());
        self.new_document = false;
    }
    fn send(&mut self, request: NativeRequest, kind: PendingKind) {
        if dunce::canonicalize(&self.root).ok().as_ref() != Some(&self.canonical_root) {
            self.stale = true;
            self.status =
                "Project location changed. Reconnect from the current trusted project.".into();
            self.stop();
            return;
        }
        if self.stale
            || self.stopping
            || !self.connected
            || self.pending.is_some()
            || self.owner.busy()
        {
            self.status = "Wait for the connection to be ready.".into();
            return;
        }
        if kind.writes() && self.unconfirmed_write.get() {
            self.status =
                "Resolve the previous unconfirmed write before making another change.".into();
            return;
        }
        if kind.writes() && self.owner.source.read_only {
            self.status = "This data source is read-only.".into();
            return;
        }
        let Some(id) = self.next_request.checked_add(1) else {
            self.status = "Reconnect before continuing.".into();
            return;
        };
        let writes = kind.writes();
        if writes && !self.unconfirmed_write.begin(self.owner.id, id) {
            self.status =
                "Wait for the other console's write to finish or resolve its outcome.".into();
            return;
        }
        match self.owner.send(id, request) {
            Ok(()) => {
                self.next_request = id;
                if kind.writes() {
                    self.outcome_checked = false;
                }
                self.pending = Some(Pending { id, kind });
                self.status = "Operation in progress…".into();
            }
            Err(e) => {
                if writes {
                    self.unconfirmed_write.finish(self.owner.id, id, true);
                }
                self.status = e.to_string();
            }
        }
    }
    fn read(&mut self, request: NativeRequest) {
        if self.dirty {
            self.status = "Apply or discard your local edits before loading another value.".into();
            return;
        }
        let kind = match &request {
            NativeRequest::RedisRead { cursor, .. } => PendingKind::RedisRead { cursor: *cursor },
            NativeRequest::MongoFind { collection, .. } => PendingKind::MongoRead {
                collection: collection.clone(),
            },
            _ => PendingKind::Read,
        };
        self.send(request, kind);
    }
    fn refresh(&mut self) {
        match self.owner.source.dialect {
            Dialect::Redis => self.read(NativeRequest::RedisScan {
                cursor: 0,
                pattern: self.scan_pattern.as_bytes().to_vec(),
                count: 200,
            }),
            Dialect::Mongo => self.read(NativeRequest::MongoCollections),
            _ => {}
        }
    }
    fn find(&mut self) {
        if self.dirty {
            self.status =
                "Apply or discard local edits before changing the loaded collection snapshot."
                    .into();
            return;
        }
        if self.collection.is_empty() {
            self.status = "Choose a collection first.".into();
            return;
        }
        let filter = match json_object(&self.filter) {
            Ok(v) => v,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let sort = match json_object(&self.sort) {
            Ok(v) => v,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        self.read(NativeRequest::MongoFind {
            collection: self.collection.clone(),
            filter,
            sort,
            skip: self.offset,
            limit: self.limit.clamp(1, 1000),
        });
    }
    fn reply(&mut self, reply: NativeReply, valid: bool) {
        if reply.session_id != self.owner.id || reply.source_id != self.owner.source.id {
            return;
        }
        if reply.request_id == 0 {
            match reply.event {
                NativeEvent::Connected if valid => {
                    self.connected = true;
                    self.status = "Connected.".into();
                    self.refresh();
                }
                NativeEvent::Retired => {
                    self.connected = false;
                    self.stopping = true;
                    self.pending = None;
                    self.load_string_all = false;
                    if !self.stale {
                        self.status.push_str("\nDisconnected. A write interrupted before confirmation may already have changed the server. Reconnect and reload to check.");
                    }
                }
                NativeEvent::Error(error) if valid => {
                    self.status = format!("Connection error: {error}")
                }
                _ => {}
            }
            return;
        }
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.id == reply.request_id)
        {
            return;
        }
        let pending = self.pending.take().unwrap();
        if !valid || self.stale {
            return;
        }
        if pending.kind.writes() {
            let confirmed = matches!(
                &reply.event,
                NativeEvent::Mutation { .. } | NativeEvent::Conflict
            );
            self.unconfirmed_write
                .finish(self.owner.id, reply.request_id, confirmed);
        }
        match reply.event {
            NativeEvent::RedisKeys { cursor, keys } => {
                self.scan_cursor = cursor;
                self.keys = keys;
                self.status = format!(
                    "{} keys in this SCAN page; cursor {cursor}.",
                    self.keys.len()
                );
            }
            NativeEvent::RedisValue(record) => {
                let cursor = match pending.kind {
                    PendingKind::RedisRead { cursor } => cursor,
                    _ => 0,
                };
                self.publish_redis(record, cursor);
            }
            NativeEvent::MongoCollections(collections) => {
                self.collections = collections;
                self.status = "Collections loaded.".into();
            }
            NativeEvent::MongoDocuments {
                documents,
                has_more,
            } => {
                self.loaded_collection = match pending.kind {
                    PendingKind::MongoRead { collection } => Some(collection),
                    _ => None,
                };
                self.documents = documents;
                self.has_more = has_more;
                self.selected = None;
                if !self.unconfirmed_write.get() {
                    self.document = "{}".into();
                    self.new_document = false;
                    self.dirty = false;
                }
                self.status =
                    "Documents loaded. Extended JSON preserves BSON types and _id.".into();
            }
            NativeEvent::Mutation { affected } if affected > 0 => {
                self.dirty = false;
                self.status = format!("Saved a change to {affected} item(s).");
                match pending.kind {
                    PendingKind::RedisWrite(key) => self.read(NativeRequest::RedisRead {
                        key,
                        cursor: 0,
                        limit: 1000,
                    }),
                    PendingKind::MongoWrite => self.find(),
                    PendingKind::Read
                    | PendingKind::RedisRead { .. }
                    | PendingKind::MongoRead { .. } => {}
                }
            }
            NativeEvent::Conflict | NativeEvent::Mutation { affected: 0 } => {
                self.load_string_all = false;
                self.status="The server value changed. Your local edits have been kept. Discard them and reload before trying again.".into();
            }
            NativeEvent::Error(error) => {
                self.load_string_all = false;
                self.status = error;
            }
            _ => {
                self.status = "The response was unexpected. Your local edits have been kept.".into()
            }
        }
    }
    fn publish_redis(&mut self, mut record: RedisRecord, cursor: u64) {
        let mut origin = cursor;
        if cursor > 0 {
            if let RedisValue::String(chunk) = &record.value {
                let previous = self.record.as_ref().filter(|old| {
                    old.key == record.key
                        && old.version == record.version
                        && old.next_cursor == Some(cursor)
                });
                let Some(RedisValue::String(prefix)) = previous.map(|old| &old.value) else {
                    self.load_string_all = false;
                    self.status =
                        "The server value changed while loading. Reload the value before editing."
                            .into();
                    return;
                };
                if prefix.len().saturating_add(chunk.len()) > rustpy_db::native::MAX_BYTES {
                    self.load_string_all = false;
                    self.status="The complete value exceeds the 1 MiB editor limit and cannot be replaced here.".into();
                    return;
                }
                let mut full = prefix.clone();
                full.extend_from_slice(chunk);
                record.value = RedisValue::String(full);
                origin = 0;
            }
        }
        if self.unconfirmed_write.get() {
            self.status =
                "Server value read. Your unconfirmed write and local edits remain unresolved."
                    .into();
            return;
        }
        self.record_cursor = origin;
        self.index = i64::try_from(origin).unwrap_or(i64::MAX);
        self.field_hex.clear();
        self.key_hex = hex(&record.key);
        self.value_hex = match &record.value {
            RedisValue::String(value) => hex(value),
            _ => String::new(),
        };
        let next = record.next_cursor;
        let key = record.key.clone();
        self.record = Some(record);
        self.dirty = false;
        self.status="Value loaded. Changes to the value are checked before saving. Changes only to its expiration are not detected.".into();
        if self.load_string_all {
            if let Some(cursor) = next {
                self.read(NativeRequest::RedisRead {
                    key,
                    cursor,
                    limit: 1000,
                });
            } else {
                self.load_string_all = false;
            }
        }
    }
    fn redis_write(&mut self, mutation: RedisMutation) {
        let Some(record) = &self.record else {
            self.status = "Load the key before changing it.".into();
            return;
        };
        if bytes(&self.key_hex).ok().as_deref() != Some(record.key.as_slice()) {
            self.status = "Load the edited key first. Changes apply to the loaded key.".into();
            return;
        }
        if matches!(&mutation, RedisMutation::SetString(_))
            && matches!(&record.value, RedisValue::String(_))
            && (self.record_cursor != 0 || record.next_cursor.is_some())
        {
            self.status = "Load the complete value before replacing it.".into();
            return;
        }
        let key = record.key.clone();
        let expected = record.version.clone();
        self.send(
            NativeRequest::RedisCompare {
                key: key.clone(),
                expected,
                mutation,
            },
            PendingKind::RedisWrite(key),
        );
    }
    fn redis_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Redis keys and values");
        ui.horizontal(|ui| {
            ui.label("SCAN pattern (UTF-8)");
            ui.text_edit_singleline(&mut self.scan_pattern);
            if ui.button("Scan keys").clicked() {
                self.refresh();
            }
            if ui
                .add_enabled(self.scan_cursor != 0, egui::Button::new("Next key page"))
                .clicked()
            {
                self.read(NativeRequest::RedisScan {
                    cursor: self.scan_cursor,
                    pattern: self.scan_pattern.as_bytes().to_vec(),
                    count: 200,
                });
            }
        });
        egui::ScrollArea::vertical()
            .id_salt(("native-redis-keys", self.owner.id))
            .max_height(140.0)
            .show(ui, |ui| {
                for key in self.keys.clone() {
                    if ui.button(preview(&key)).clicked() {
                        self.read(NativeRequest::RedisRead {
                            key,
                            cursor: 0,
                            limit: 200,
                        });
                    }
                }
            });
        ui.horizontal(|ui| {
            let label = ui.label("Key bytes (hex)");
            ui.text_edit_singleline(&mut self.key_hex)
                .labelled_by(label.id);
            if ui.button("Load key").clicked() {
                match bytes(&self.key_hex) {
                    Ok(key) => self.read(NativeRequest::RedisRead {
                        key,
                        cursor: 0,
                        limit: 200,
                    }),
                    Err(e) => self.status = e,
                }
            }
        });
        let Some(record) = self.record.clone() else {
            return;
        };
        ui.label(format!(
            "Type: {} · TTL: {} ms (-1 persistent, -2 absent)",
            record.kind, record.ttl_ms
        ));
        ui.label(format!("Loaded key: {}", preview(&record.key)));
        egui::ScrollArea::vertical()
            .id_salt(("native-redis-value", self.owner.id))
            .max_height(180.0)
            .show(ui, |ui| match &record.value {
                RedisValue::Missing => {
                    ui.label("Key does not exist. Creating it will fail if another client creates it first.");
                }
                RedisValue::String(value) => {
                    ui.label(format!("{} binary bytes", value.len()));
                }
                RedisValue::List(values) => {
                    for (index, value) in values.iter().enumerate() {
                        if ui.button(format!("{}: {}", self.record_cursor.saturating_add(index as u64), preview(value))).clicked() {
                            if self.dirty {self.status="Discard or apply the local buffer before selecting another item.".into();continue;}
                            self.index = match i64::try_from(
                                self.record_cursor.saturating_add(index as u64),
                            ) {
                                Ok(index) => index,
                                Err(_) => {
                                    self.status =
                                        "List index is too large.".into();
                                    continue;
                                }
                            };
                            self.value_hex = hex(value);
                        }
                    }
                }
                RedisValue::Set(values) => {
                    for value in values {
                        if ui.button(preview(value)).clicked() {
                            if self.dirty {self.status="Discard or apply the local buffer before selecting another item.".into();continue;}
                            self.value_hex = hex(value);
                        }
                    }
                }
                RedisValue::Hash(values) => {
                    for (field, value) in values {
                        if ui
                            .button(format!("{} = {}", preview(field), preview(value)))
                            .clicked()
                        {
                            if self.dirty {self.status="Discard or apply the local buffer before selecting another field.".into();continue;}
                            self.field_hex = hex(field);
                            self.value_hex = hex(value);
                        }
                    }
                }
                RedisValue::SortedSet(values) => {
                    for (member, score) in values {
                        if ui
                            .button(format!("{} : {score}", preview(member)))
                            .clicked()
                        {
                            if self.dirty {self.status="Discard or apply the local buffer before selecting another member.".into();continue;}
                            self.value_hex = hex(member);
                            self.score = score.to_string();
                        }
                    }
                }
            });
        if let Some(cursor) = record.next_cursor {
            if ui.button("Next value page").clicked() {
                self.read(NativeRequest::RedisRead {
                    key: record.key.clone(),
                    cursor,
                    limit: 200,
                });
            }
        }
        let partial_string = matches!(&record.value, RedisValue::String(_))
            && (self.record_cursor != 0 || record.next_cursor.is_some());
        if partial_string {
            ui.label("Partial string bytes are read-only until the entire value snapshot has been loaded.");
            if let Some(cursor) = record.next_cursor {
                if ui
                    .add_enabled(
                        !self.dirty,
                        egui::Button::new("Load remaining string bytes"),
                    )
                    .clicked()
                {
                    self.load_string_all = true;
                    self.read(NativeRequest::RedisRead {
                        key: record.key.clone(),
                        cursor,
                        limit: 1000,
                    });
                }
            }
        }
        ui.add_enabled_ui(
            !self.owner.source.read_only && !self.unconfirmed_write.get(),
            |ui| {
                let label = ui.label("Value / member bytes (hex)");
                self.dirty |= ui
                    .add_enabled(
                        !partial_string,
                        egui::TextEdit::multiline(&mut self.value_hex).desired_rows(3),
                    )
                    .labelled_by(label.id)
                    .changed();
                ui.horizontal(|ui| {
                    let label = ui.label("Hash field bytes (hex)");
                    self.dirty |= ui
                        .text_edit_singleline(&mut self.field_hex)
                        .labelled_by(label.id)
                        .changed();
                    ui.label("List index");
                    self.dirty |= ui
                        .add(egui::DragValue::new(&mut self.index).range(0..=i64::MAX))
                        .changed();
                });
                let label = ui.label("Sorted-set score");
                self.dirty |= ui
                    .text_edit_singleline(&mut self.score)
                    .labelled_by(label.id)
                    .changed();
                ui.horizontal_wrapped(|ui| {
                    for (label, kind) in [
                        ("Apply value", 0),
                        ("Set hash field", 1),
                        ("Set list item", 2),
                        ("Add set member", 3),
                        ("Add sorted-set member", 4),
                        ("Delete hash field", 5),
                        ("Prepend list item", 6),
                        ("Append list item", 7),
                        ("Remove set member", 8),
                        ("Remove sorted-set member", 9),
                    ] {
                        if ui
                            .add_enabled(kind != 0 || !partial_string, egui::Button::new(label))
                            .clicked()
                        {
                            let value = match bytes(&self.value_hex) {
                                Ok(value) => value,
                                Err(error) => {
                                    self.status = error;
                                    continue;
                                }
                            };
                            let mutation = match kind {
                                0 => RedisMutation::SetString(value),
                                1 => match bytes(&self.field_hex) {
                                    Ok(field) => RedisMutation::HashSet { field, value },
                                    Err(e) => {
                                        self.status = e;
                                        continue;
                                    }
                                },
                                2 => RedisMutation::ListSet {
                                    index: self.index,
                                    value,
                                },
                                3 => RedisMutation::SetAdd(value),
                                5 => match bytes(&self.field_hex) {
                                    Ok(field) => RedisMutation::HashDelete { field },
                                    Err(e) => {
                                        self.status = e;
                                        continue;
                                    }
                                },
                                6 | 7 => RedisMutation::ListPush {
                                    value,
                                    left: kind == 6,
                                },
                                8 => RedisMutation::SetRemove(value),
                                9 => RedisMutation::SortedSetRemove(value),
                                _ => match self.score.parse::<f64>() {
                                    Ok(score) if score.is_finite() => RedisMutation::SortedSetAdd {
                                        member: value,
                                        score,
                                    },
                                    _ => {
                                        self.status = "Enter a finite sorted-set score.".into();
                                        continue;
                                    }
                                },
                            };
                            self.redis_write(mutation);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Expiration ms");
                    ui.text_edit_singleline(&mut self.expiry);
                    if ui.button("Set expiration").clicked() {
                        match self.expiry.parse::<i64>() {
                            Ok(millis) if millis > 0 => {
                                self.redis_write(RedisMutation::Expire { millis })
                            }
                            _ => {
                                self.status =
                                    "Expiration must be a positive millisecond count.".into()
                            }
                        }
                    }
                    if ui.button("Remove expiration").clicked() {
                        self.redis_write(RedisMutation::Expire { millis: -1 });
                    }
                    if ui.button("Delete key").clicked() {
                        self.redis_write(RedisMutation::Delete);
                    }
                });
            },
        );
        if self.owner.source.read_only {
            ui.label("This data source is read-only.");
        }
    }
    fn mongo_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("MongoDB documents (Extended JSON)");
        ui.horizontal(|ui| {
            if ui.button("Refresh collections").clicked() {
                self.read(NativeRequest::MongoCollections);
            }
            egui::ComboBox::from_id_salt(("native-mongo-collections", self.owner.id))
                .selected_text(&self.collection)
                .show_ui(ui, |ui| {
                    for collection in &self.collections {
                        ui.selectable_value(&mut self.collection, collection.clone(), collection);
                    }
                })
                .combo_named(crate::i18n::tr_static("Collection"));
        });
        ui.horizontal(|ui| {
            let label = ui.label("Collection");
            ui.text_edit_singleline(&mut self.collection)
                .labelled_by(label.id);
            ui.label("Page limit");
            ui.add(egui::DragValue::new(&mut self.limit).range(1..=1000));
        });
        let label = ui.label("Mongo filter (Extended JSON object)");
        ui.add(
            egui::TextEdit::multiline(&mut self.filter)
                .desired_rows(2)
                .code_editor(),
        )
        .labelled_by(label.id);
        let label = ui.label("Mongo sort (JSON object)");
        ui.text_edit_singleline(&mut self.sort)
            .labelled_by(label.id);
        ui.horizontal(|ui| {
            if ui.button("Find documents").clicked() {
                self.offset = 0;
                self.find();
            }
            if ui
                .add_enabled(self.offset > 0, egui::Button::new("Previous document page"))
                .clicked()
            {
                self.offset = self.offset.saturating_sub(self.limit as u64);
                self.find();
            }
            if ui
                .add_enabled(self.has_more, egui::Button::new("Next document page"))
                .clicked()
            {
                self.offset = self.offset.saturating_add(self.limit as u64);
                self.find();
            }
            ui.label(format!(
                "Requested offset {} · {} loaded documents from {}",
                self.offset,
                self.documents.len(),
                self.loaded_collection.as_deref().unwrap_or("no collection")
            ));
        });
        egui::ScrollArea::vertical()
            .id_salt(("native-mongo-documents", self.owner.id))
            .max_height(180.0)
            .show(ui, |ui| {
                for (index, document) in self.documents.iter().enumerate() {
                    let id = document
                        .get("_id")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "missing _id".into());
                    if ui
                        .selectable_label(self.selected == Some(index), id)
                        .clicked()
                    {
                        if self.dirty {
                            self.status =
                                "Apply or discard local edits before selecting another document."
                                    .into();
                        } else {
                            self.selected = Some(index);
                            self.document =
                                serde_json::to_string_pretty(document).unwrap_or_default();
                            self.new_document = false;
                        }
                    }
                }
            });
        ui.add_enabled_ui(
            !self.owner.source.read_only && !self.unconfirmed_write.get(),
            |ui| {
                if ui.button("New document").clicked() {
                    if self.dirty {
                        self.status = "Apply or discard the current local buffer first.".into();
                    } else {
                        self.new_document = true;
                        self.selected = None;
                        self.document = "{}".into();
                        self.dirty = true;
                    }
                }
                let label = ui.label("Document (canonical Extended JSON; preserve _id)");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::multiline(&mut self.document)
                            .desired_rows(10)
                            .code_editor(),
                    )
                    .labelled_by(label.id)
                    .changed();
                if ui
                    .button(if self.new_document {
                        "Insert document"
                    } else {
                        "Replace document"
                    })
                    .clicked()
                {
                    if !self.new_document
                        && self.loaded_collection.as_deref() != Some(self.collection.as_str())
                    {
                        self.status = "Load this collection before replacing a document.".into();
                        return;
                    }
                    if self.collection.is_empty() {
                        self.status = "Choose a collection first.".into();
                        return;
                    }
                    let replacement = match json_object(&self.document) {
                        Ok(v) => v,
                        Err(e) => {
                            self.status = e;
                            return;
                        }
                    };
                    if self.new_document {
                        let replacement =
                            match rustpy_db::native::prepare_mongo_document(replacement) {
                                Ok(value) => value,
                                Err(error) => {
                                    self.status = error.to_string();
                                    return;
                                }
                            };
                        self.document =
                            serde_json::to_string_pretty(&replacement).unwrap_or_default();
                        self.send(
                            NativeRequest::MongoInsert {
                                collection: self.collection.clone(),
                                document: replacement,
                            },
                            PendingKind::MongoWrite,
                        );
                    } else if let Some(expected) =
                        self.selected.and_then(|i| self.documents.get(i)).cloned()
                    {
                        if expected.get("_id").is_none()
                            || replacement.get("_id") != expected.get("_id")
                        {
                            self.status =
                                "Replacement must preserve the original _id exactly.".into();
                        } else {
                            self.send(
                                NativeRequest::MongoReplace {
                                    collection: self.collection.clone(),
                                    expected,
                                    replacement,
                                },
                                PendingKind::MongoWrite,
                            );
                        }
                    } else {
                        self.status =
                            "Select a loaded document or create a new document first.".into();
                    }
                }
                if ui.button("Delete document").clicked() {
                    if self.loaded_collection.as_deref() != Some(self.collection.as_str()) {
                        self.status = "Load this collection before deleting a document.".into();
                        return;
                    }
                    if let Some(expected) =
                        self.selected.and_then(|i| self.documents.get(i)).cloned()
                    {
                        self.send(
                            NativeRequest::MongoDelete {
                                collection: self.collection.clone(),
                                expected,
                            },
                            PendingKind::MongoWrite,
                        );
                    }
                }
            },
        );
        if self.owner.source.read_only {
            ui.label("This data source is read-only.");
            ui.code(&self.document);
        }
    }
}
fn hex(value: &[u8]) -> String {
    value
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn preview(value: &[u8]) -> String {
    format!(
        "{}{}",
        hex(&value[..value.len().min(32)]),
        if value.len() > 32 { " …" } else { "" }
    )
}
fn bytes(value: &str) -> Result<Vec<u8>, String> {
    rustpy_db::injection::parse_hex(value).map_err(|e| e.to_string())
}
fn json_object(value: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(value).map_err(|e| format!("Invalid Extended JSON: {e}"))?;
    if !value.is_object() {
        return Err("Enter a JSON object; canonical BSON wrappers must remain intact.".into());
    }
    Ok(value)
}

#[cfg(test)]
mod configured_tests {
    use super::*;
    use crate::db_panel::properties::ui_fixture::Fixture;
    use std::{
        io::Read,
        process::{Child, Command, Stdio},
        sync::Arc,
        time::Instant,
    };
    #[test]
    fn shared_write_authority_serializes_owners_and_rejects_old_acknowledgements() {
        let first = WriteOutcome::default();
        let second = first.clone();
        assert!(first.begin(11, 7));
        assert!(
            !second.begin(12, 1),
            "other console cannot dispatch concurrently"
        );
        assert!(
            !second.resolve(),
            "in-flight write cannot be resolved from another console"
        );
        first.finish(11, 7, false);
        assert!(second.get());
        assert!(second.can_resolve());
        assert!(second.resolve());
        assert!(second.begin(12, 1));
        first.finish(11, 7, true);
        first.retired(11);
        assert!(
            second.get(),
            "old acknowledgement/retirement cannot clear the new operation"
        );
        assert!(!second.can_resolve());
        second.retired(12);
        assert!(
            second.get(),
            "retirement alone does not confirm the remote outcome"
        );
        assert!(second.resolve());
        assert!(first.begin(11, 8));
        first.finish(11, 8, true);
        assert!(
            !second.get(),
            "matching acknowledged operation releases shared admission"
        );
    }

    struct NetGuard(Arc<rustpy_net::NetConfig>);
    impl NetGuard {
        fn direct() -> Self {
            let saved = rustpy_net::config();
            let mut selected = (*saved).clone();
            selected.proxy.mode = rustpy_net::ProxyMode::NoProxy;
            rustpy_net::configure(selected);
            Self(saved)
        }
    }
    impl Drop for NetGuard {
        fn drop(&mut self) {
            rustpy_net::configure((*self.0).clone());
        }
    }
    struct Provider {
        child: Child,
        owner: rustpy_run::ProcessTree,
        pid: u32,
        start: u64,
        stopped: bool,
    }
    fn starttime(pid: u32) -> Option<u64> {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()?
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()
    }
    impl Provider {
        fn start(mut command: Command, log: &Path) -> Self {
            command
                .stdin(Stdio::null())
                .stdout(std::fs::File::create(log).unwrap())
                .stderr(std::fs::File::options().append(true).open(log).unwrap());
            rustpy_run::ProcessTree::prepare(&mut command);
            let mut child = command
                .spawn()
                .expect("explicitly admitted native provider executable");
            let owner = rustpy_run::ProcessTree::new(&mut child).unwrap_or_else(|error| {
                let _ = child.kill();
                let _ = child.wait();
                panic!("attach owned native provider: {error}")
            });
            let pid = child.id();
            let start = starttime(pid).expect("exact provider /proc identity");
            eprintln!("owned native provider pid={pid} startticks={start}");
            Self {
                child,
                owner,
                pid,
                start,
                stopped: false,
            }
        }
        fn ready(&mut self, port: u16) {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(
                    self.owner.poll(&mut self.child).unwrap().is_none(),
                    "owned provider exited before readiness"
                );
                if std::net::TcpStream::connect_timeout(
                    &([127, 0, 0, 1], port).into(),
                    Duration::from_millis(50),
                )
                .is_ok()
                {
                    return;
                }
                assert!(Instant::now() < end, "owned loopback provider must listen");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn stop(&mut self) {
            if !self.stopped {
                self.owner.force(&mut self.child);
                self.stopped = true;
            }
            assert_ne!(
                starttime(self.pid),
                Some(self.start),
                "exact owned provider physically retired"
            );
            eprintln!(
                "retired native provider pid={} startticks={}",
                self.pid, self.start
            );
        }
    }
    impl Drop for Provider {
        fn drop(&mut self) {
            if !self.stopped {
                self.owner.force(&mut self.child);
                self.stopped = true;
            }
        }
    }
    struct Peer(NativeSession, u64);
    impl Peer {
        fn event(&mut self, request: NativeRequest) -> NativeEvent {
            self.1 += 1;
            self.0.send(self.1, request).unwrap();
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                for reply in self.0.poll() {
                    if reply.request_id == self.1 {
                        return reply.event;
                    }
                    assert!(
                        !matches!(reply.event, NativeEvent::Error(_) | NativeEvent::Retired),
                        "{:?}",
                        reply.event
                    );
                }
                assert!(Instant::now() < end, "actual native peer request completes");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            self.0.stop();
            while self.0.alive() {
                self.0.poll();
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn admitted(name: &str) -> PathBuf {
        assert_eq!(
            std::env::var("RUSTPY_NATIVE_UI_ADMITTED").as_deref(),
            Ok("1")
        );
        std::fs::read_to_string("/proc/self/stat")
            .expect("Linux physical-identity fixture; never silently skip");
        let path = PathBuf::from(
            std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} required")),
        );
        assert!(
            path.is_absolute() && path.is_file(),
            "explicit existing absolute provider executable required"
        );
        path
    }
    fn port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
    fn idle(f: &mut Fixture) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            f.frame(vec![]);
            let active = f.panel.workspace.active;
            let c = f
                .panel
                .workspace
                .native
                .owners
                .iter()
                .find(|c| Some(c.owner.id) == active)
                .expect("actual native owner");
            assert!(
                c.owner.alive(),
                "native worker retired unexpectedly: {}",
                c.status
            );
            if c.connected && c.pending.is_none() && !c.owner.busy() {
                return;
            }
            assert!(
                Instant::now() < end,
                "native operation completes: {}",
                c.status
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn active(f: &Fixture) -> &NativeConsole {
        f.panel
            .workspace
            .native
            .owners
            .iter()
            .find(|c| Some(c.owner.id) == f.panel.workspace.active)
            .unwrap()
    }
    fn open(f: &mut Fixture, source: Source, secret: &str) {
        f.panel.workspace.selected = Some(source.id);
        f.panel.workspace.model.sources.push(source);
        f.panel.workspace.secret = secret.into();
        f.frame(vec![]);
        f.activate("Connect / New console");
        assert!(
            f.panel.workspace.native.owners.is_empty(),
            "trust is required before opening a native connection"
        );
        f.panel.set_database_execution_allowed(true);
        f.activate("Connect / New console");
        idle(f);
        assert!(
            f.panel.workspace.owners.is_empty(),
            "no SQL/ODBC owner substitutes for native transport"
        );
    }
    fn load_key(f: &mut Fixture, key: &[u8]) {
        f.replace("Key bytes (hex)", &hex(key));
        f.activate("Load key");
        idle(f);
    }

    #[test]
    fn native_properties_publish_native_settings_without_odbc_or_execution() {
        for (dialect, port) in [(Dialect::Redis, 6379), (Dialect::Mongo, 27017)] {
            let mut f = Fixture::new();
            f.panel.workspace.draft = Source {
                id: 1,
                name: "native source".into(),
                dialect,
                ..Default::default()
            };
            f.panel.workspace.properties = true;
            f.panel.workspace.options = r#"{"tls":"false"}"#.into();
            f.frame(vec![]);
            assert!(f.nodes.values().any(|node| {
                node.value()
                    .is_some_and(|text| text.starts_with("Redis / MongoDB connection."))
            }));
            assert!(!f
                .nodes
                .values()
                .any(|node| node.label() == Some("List installed drivers")));
            assert!(f.panel.workspace.drivers.is_none());
            f.activate("Save properties");
            let saved = rustpy_db::workspace::Workspace::load(f.root.path());
            assert_eq!(saved.sources[0].dialect, dialect);
            assert_eq!(saved.sources[0].port, port);
            assert!(saved.sources[0].driver.is_empty());
            assert!(!f.panel.database_has_owners());
            f.retire();
        }
    }
    #[test]
    #[ignore = "requires RUSTPY_NATIVE_UI_ADMITTED=1 and absolute RUSTPY_TEST_REDIS_SERVER; owns a fresh authenticated loopback server; run this selector alone"]
    fn configured_owned_redis_widgets_preserve_binary_cas_readonly_and_retirement() {
        let binary = admitted("RUSTPY_TEST_REDIS_SERVER");
        let _net = NetGuard::direct();
        let mut f = Fixture::new();
        let mut nonce = [0; 32];
        std::fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut nonce)
            .unwrap();
        let secret = hex(&nonce).replace(' ', "");
        let port = port();
        let config = f.root.path().join("owned-redis.conf");
        std::fs::write(&config,format!("bind 127.0.0.1\nport {port}\nprotected-mode yes\ndaemonize no\nsave \"\"\nappendonly no\nrequirepass {secret}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut command = Command::new(binary);
        command.arg(&config).current_dir(f.root.path());
        let mut server = Provider::start(command, &f.root.path().join("redis.log"));
        server.ready(port);
        let source = Source {
            id: 11,
            name: "Owned Redis".into(),
            dialect: Dialect::Redis,
            host: "127.0.0.1".into(),
            port,
            database: "0".into(),
            ..Default::default()
        };
        open(&mut f, source.clone(), &secret);
        let key = [0xff, 0, b'k'];
        load_key(&mut f, &key);
        assert!(matches!(
            active(&f).record.as_ref().unwrap().value,
            RedisValue::Missing
        ));
        f.replace("Value / member bytes (hex)", "00 ff 80 41");
        f.activate("Apply value");
        idle(&mut f);
        assert_eq!(
            active(&f).record.as_ref().unwrap().value,
            RedisValue::String(vec![0, 255, 128, 65])
        );
        let expected = active(&f).record.as_ref().unwrap().version.clone();
        let mut peer = Peer(
            NativeSession::start(900, source.clone(), secret.clone(), Duration::from_secs(5))
                .unwrap(),
            0,
        );
        assert!(matches!(
            peer.event(NativeRequest::RedisCompare {
                key: key.to_vec(),
                expected,
                mutation: RedisMutation::SetString(vec![9, 8])
            }),
            NativeEvent::Mutation { affected: 1 }
        ));
        f.replace("Value / member bytes (hex)", "10 11");
        f.activate("Apply value");
        idle(&mut f);
        assert!(active(&f).status.contains("server value changed"));
        assert!(active(&f).dirty);
        assert!(
            matches!(peer.event(NativeRequest::RedisRead{key:key.to_vec(),cursor:0,limit:1000}),NativeEvent::RedisValue(RedisRecord{value:RedisValue::String(ref value),..}) if value==&[9,8])
        );
        f.activate("Discard local edits");
        load_key(&mut f, &key);
        f.activate("Set expiration");
        idle(&mut f);
        assert!(active(&f).record.as_ref().unwrap().ttl_ms > 0);
        f.activate("Remove expiration");
        idle(&mut f);
        assert_eq!(active(&f).record.as_ref().unwrap().ttl_ms, -1);
        f.activate("Scan keys");
        idle(&mut f);
        assert!(active(&f).keys.iter().any(|item| item.as_slice() == key));
        let expected = active(&f).record.as_ref().unwrap().version.clone();
        assert!(matches!(
            peer.event(NativeRequest::RedisCompare {
                key: key.to_vec(),
                expected,
                mutation: RedisMutation::SetString(vec![42; 1200])
            }),
            NativeEvent::Mutation { affected: 1 }
        ));
        load_key(&mut f, &key);
        assert!(active(&f).record.as_ref().unwrap().next_cursor.is_some());
        let before = active(&f).next_request;
        f.activate("Apply value");
        assert_eq!(
            active(&f).next_request,
            before,
            "partial strings cannot be written as whole values"
        );
        f.activate("Load remaining string bytes");
        idle(&mut f);
        assert_eq!(
            active(&f).record.as_ref().unwrap().value,
            RedisValue::String(vec![42; 1200])
        );
        for (key, label, kind) in [
            (b"hash".as_slice(), "Set hash field", "hash"),
            (b"list".as_slice(), "Append list item", "list"),
            (b"set".as_slice(), "Add set member", "set"),
            (b"zset".as_slice(), "Add sorted-set member", "zset"),
        ] {
            load_key(&mut f, key);
            f.replace("Value / member bytes (hex)", "aa bb");
            f.replace("Hash field bytes (hex)", "66");
            f.replace("Sorted-set score", "3.5");
            f.activate(label);
            idle(&mut f);
            assert_eq!(active(&f).record.as_ref().unwrap().kind, kind);
        }
        f.panel.workspace.model.sources[0].read_only = true;
        f.frame(vec![]);
        f.panel.workspace.secret = secret;
        f.activate("Connect / New console");
        idle(&mut f);
        load_key(&mut f, &key);
        let before = active(&f).next_request;
        f.activate("Delete key");
        assert_eq!(
            active(&f).next_request,
            before,
            "read-only native controls cannot publish a write"
        );
        let other = tempfile::tempdir().unwrap();
        f.panel.database_project_changed(other.path());
        f.panel.poll_database();
        assert!(
            f.panel
                .workspace
                .native
                .owners
                .iter()
                .all(|c| c.stale && c.record.is_none()),
            "old-root payloads cannot publish into the new project"
        );
        f.retire();
        drop(peer);
        server.stop();
    }

    #[test]
    #[ignore = "requires RUSTPY_NATIVE_UI_ADMITTED=1 and absolute RUSTPY_TEST_MONGOD; owns a fresh disposable loopback MongoDB; run this selector alone"]
    fn configured_owned_mongo_widgets_preserve_extended_ids_and_collection_cas() {
        let binary = admitted("RUSTPY_TEST_MONGOD");
        let _net = NetGuard::direct();
        let mut f = Fixture::new();
        let port = port();
        let data = f.root.path().join("mongo-data");
        std::fs::create_dir(&data).unwrap();
        let mut command = Command::new(binary);
        command
            .args(["--bind_ip", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .arg("--dbpath")
            .arg(&data)
            .args([
                "--nounixsocket",
                "--wiredTigerCacheSizeGB",
                "0.256",
                "--quiet",
                "--setParameter",
                "diagnosticDataCollectionEnabled=false",
            ])
            .current_dir(f.root.path());
        let mut server = Provider::start(command, &f.root.path().join("mongo.log"));
        server.ready(port);
        let source = Source {
            id: 12,
            name: "Owned MongoDB".into(),
            dialect: Dialect::Mongo,
            host: "127.0.0.1".into(),
            port,
            database: "rustpy_ui_owned".into(),
            ..Default::default()
        };
        open(&mut f, source.clone(), "");
        let preexisting = f.panel.workspace.active.unwrap();
        f.replace("Collection", "records");
        f.activate("New document");
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            r#"{"label":"other console draft"}"#,
        );
        f.activate("Connect / New console");
        idle(&mut f);
        assert_ne!(active(&f).owner.id, preexisting);
        assert!(
            Arc::ptr_eq(
                &active(&f).unconfirmed_write.0,
                &f.panel
                    .workspace
                    .native
                    .owners
                    .iter()
                    .find(|c| c.owner.id == preexisting)
                    .unwrap()
                    .unconfirmed_write
                    .0
            ),
            "two consoles opened before any write share authority"
        );
        f.replace("Collection", "records");
        f.activate("New document");
        let original = serde_json::json!({"_id":{"$oid":"507f1f77bcf86cd799439011"},"n":{"$numberLong":"9007199254740993"},"label":"original"});
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            &original.to_string(),
        );
        f.activate("Insert document");
        idle(&mut f);
        assert_eq!(active(&f).documents, vec![original.clone()]);
        let mut second = original.clone();
        second["_id"] = serde_json::json!({"$oid":"507f1f77bcf86cd799439012"});
        second["label"] = "second".into();
        f.activate("New document");
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            &second.to_string(),
        );
        f.activate("Insert document");
        idle(&mut f);
        // Configure a one-document page, then drive the actual filter/sort and
        // next/previous controls against the real server's returned pages.
        f.panel
            .workspace
            .native
            .owners
            .iter_mut()
            .find(|c| Some(c.owner.id) == f.panel.workspace.active)
            .unwrap()
            .limit = 1;
        f.replace("Mongo sort (JSON object)", r#"{"_id":1}"#);
        f.replace(
            "Mongo filter (Extended JSON object)",
            r#"{"label":"original"}"#,
        );
        f.activate("Find documents");
        idle(&mut f);
        assert_eq!(active(&f).documents, vec![original.clone()]);
        assert!(!active(&f).has_more);
        f.replace("Mongo filter (Extended JSON object)", "{}");
        f.activate("Find documents");
        idle(&mut f);
        assert!(active(&f).has_more);
        f.activate("Next document page");
        idle(&mut f);
        assert_eq!(active(&f).documents, vec![second.clone()]);
        f.activate(&second["_id"].to_string());
        f.activate("Delete document");
        idle(&mut f);
        f.activate("Previous document page");
        idle(&mut f);
        assert_eq!(active(&f).documents, vec![original.clone()]);
        let id = original["_id"].to_string();
        f.activate(&id);
        let before = active(&f).next_request;
        let mut changed = original.clone();
        changed["_id"] = serde_json::json!({"$oid":"507f1f77bcf86cd799439012"});
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            &changed.to_string(),
        );
        f.activate("Replace document");
        assert_eq!(active(&f).next_request, before);
        assert!(active(&f).status.contains("preserve"));
        let mut replacement = original.clone();
        replacement["label"] = "local".into();
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            &replacement.to_string(),
        );
        f.activate("Replace document");
        idle(&mut f);
        assert_eq!(active(&f).documents, vec![replacement.clone()]);
        f.activate(&id);
        let mut peer = Peer(
            NativeSession::start(901, source, String::new(), Duration::from_secs(5)).unwrap(),
            0,
        );
        assert!(matches!(
            peer.event(NativeRequest::MongoFind {
                collection: "records".into(),
                filter: serde_json::json!({}),
                sort: serde_json::json!({}),
                skip: 0,
                limit: 10
            }),
            NativeEvent::MongoDocuments { .. }
        ));
        let mut external = replacement.clone();
        external["label"] = "external".into();
        assert!(matches!(
            peer.event(NativeRequest::MongoReplace {
                collection: "records".into(),
                expected: replacement.clone(),
                replacement: external.clone()
            }),
            NativeEvent::Mutation { affected: 1 }
        ));
        replacement["label"] = "must conflict".into();
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            &replacement.to_string(),
        );
        f.activate("Replace document");
        idle(&mut f);
        assert!(active(&f).status.contains("server value changed"));
        assert!(active(&f).dirty);
        f.activate("Discard local edits");
        f.replace("Collection", "other");
        let before = active(&f).next_request;
        f.activate("Delete document");
        assert_eq!(
            active(&f).next_request,
            before,
            "a loaded _id cannot retarget another collection"
        );
        assert!(
            matches!(peer.event(NativeRequest::MongoFind{collection:"records".into(),filter:serde_json::json!({}),sort:serde_json::json!({}),skip:0,limit:10}),NativeEvent::MongoDocuments{ref documents,..} if documents==&vec![external])
        );
        f.replace("Collection", "records");
        f.activate("Find documents");
        idle(&mut f);
        f.activate(&id);
        f.activate("Delete document");
        idle(&mut f);
        assert!(active(&f).documents.is_empty());

        // Actual insertion starts without _id: the consumer must retain its client-generated
        // identity before dispatch, rather than letting a retry invent a second document.
        f.activate("New document");
        f.replace(
            "Document (canonical Extended JSON; preserve _id)",
            r#"{"label":"uncertain insert"}"#,
        );
        f.activate("Insert document");
        idle(&mut f);
        assert_eq!(active(&f).documents.len(), 1);
        let inserted = active(&f).documents[0].clone();
        assert!(
            inserted.get("_id").is_some(),
            "client-generated identity reaches the real server"
        );
        // Fault-inject the UI's uncertain-acknowledgement state after the genuine server write.
        // This control tests admission, not an invented transport or a claim of network failure.
        let id = f.panel.workspace.active.unwrap();
        let c = f
            .panel
            .workspace
            .native
            .owners
            .iter_mut()
            .find(|c| c.owner.id == id)
            .unwrap();
        c.document = serde_json::to_string_pretty(&inserted).unwrap();
        c.new_document = true;
        c.dirty = true;
        let authority = c.unconfirmed_write.clone();
        let original_request = c.next_request;
        assert!(authority.begin(c.owner.id, original_request));
        let retained = c.document.clone();
        let before = c.next_request;
        f.panel.workspace.active = Some(preexisting);
        let other_before = active(&f).next_request;
        f.activate("Insert document");
        assert_eq!(
            active(&f).next_request,
            other_before,
            "preexisting idle console cannot bypass another write"
        );
        f.activate(
            "I checked the server outcome and accept responsibility for resolving this write",
        );
        f.activate("Resolve unconfirmed write");
        assert!(
            !authority.can_resolve(),
            "other console cannot resolve the original in-flight owner"
        );
        assert!(authority.get());
        authority.finish(id, original_request, false);
        f.panel.workspace.active = Some(id);
        f.activate("Insert document");
        f.activate("Discard local edits");
        assert_eq!(
            active(&f).next_request,
            before,
            "uncertain insert cannot be resent by widgets"
        );
        assert_eq!(
            active(&f).document,
            retained,
            "discard cannot erase unresolved identity"
        );
        // Even a direct dispatch through the shared route must obey the same guard.
        let c = f
            .panel
            .workspace
            .native
            .owners
            .iter_mut()
            .find(|c| c.owner.id == id)
            .unwrap();
        c.send(
            NativeRequest::MongoInsert {
                collection: "records".into(),
                document: inserted.clone(),
            },
            PendingKind::MongoWrite,
        );
        assert_eq!(c.next_request, before, "all mutation dispatch is blocked");
        f.activate("Disconnect");
        let end = Instant::now() + Duration::from_secs(5);
        while active(&f).owner.alive() {
            f.frame(vec![]);
            assert!(Instant::now() < end, "old connection physically retires");
            std::thread::sleep(Duration::from_millis(1));
        }
        f.activate("Connect / New console");
        idle(&mut f);
        assert_ne!(active(&f).owner.id, id);
        assert!(
            active(&f).unconfirmed_write.get(),
            "reconnect does not resolve a write"
        );
        assert_eq!(active(&f).document, retained);
        let before = active(&f).next_request;
        f.activate("Insert document");
        f.activate("Resolve unconfirmed write");
        assert!(
            active(&f).unconfirmed_write.get(),
            "resolution requires separate explicit confirmation"
        );
        assert_eq!(active(&f).next_request, before);
        let docs = peer.event(NativeRequest::MongoFind {
            collection: "records".into(),
            filter: serde_json::json!({}),
            sort: serde_json::json!({}),
            skip: 0,
            limit: 10,
        });
        assert!(
            matches!(docs, NativeEvent::MongoDocuments {ref documents,..} if documents == &vec![inserted]),
            "only one real document exists"
        );
        f.activate(
            "I checked the server outcome and accept responsibility for resolving this write",
        );
        f.activate("Resolve unconfirmed write");
        assert!(!active(&f).unconfirmed_write.get());
        assert!(
            f.panel
                .workspace
                .native
                .owners
                .iter()
                .all(|c| !c.unconfirmed_write.get()),
            "resolution is shared with the retained connection"
        );
        f.activate("Discard local edits");
        f.retire();
        drop(peer);
        server.stop();
    }
}

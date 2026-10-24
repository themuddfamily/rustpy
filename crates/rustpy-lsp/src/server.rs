//! The server: protocol handshake, the main loop, document synchronisation,
//! request dispatch to worker threads (with cancellation), debounced
//! diagnostics and the project/interpreter model.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::{InitializeParams, InitializeResult, ServerInfo, Uri};
use rustpy_core::CancellationToken;
use rustpy_semantic::Analysis;
use rustpy_settings::Settings;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::caps::{self, ClientCaps};
use crate::config::{self, Config};
use crate::diagnostics::FileProblems;
use crate::docs::Doc;
use crate::line_index::Encoding;
use crate::paths::{self, StubDir};
use crate::project::{self, InterpreterInfo, ProjectFiles, ProjectModel};
use crate::snapshot::{Generation, HResult, LspError, Snapshot};
use crate::symbols::SymbolIndex;
use crate::{code_actions, completion, handlers};

/// How the server is set up (tests pass their own settings and stub directory).
#[derive(Clone, Debug, Default)]
pub struct ServerOptions {
    /// IDE settings to start from; `None` reads the user's `settings.json`.
    pub settings: Option<Settings>,
    /// Where bundled stubs are written for navigation; `None` uses the cache directory.
    pub stub_dir: Option<PathBuf>,
}

/// Diagnostics wait at most this long for interpreter discovery, so that
/// imports of installed packages are not reported unresolved at startup.
const DISCOVERY_GRACE: Duration = Duration::from_secs(4);

enum Task {
    Response(Response),
    Diagnostics {
        path: PathBuf,
        version: i32,
        epoch: u64,
        result: Option<crate::snapshot::HResult<(Vec<lsp_types::Diagnostic>, Arc<FileProblems>)>>,
    },
    Interpreter {
        generation: u64,
        info: Option<InterpreterInfo>,
    },
    /// Changes seen by the server's own file watcher.
    FilesChanged(Vec<(PathBuf, lsp_types::FileChangeType)>),
    /// The symbol index was refreshed; `changed`: the project's top-level names changed.
    IndexRefreshed {
        changed: bool,
    },
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Worker threads for requests and diagnostics.
struct Pool {
    tx: Sender<Job>,
}

impl Pool {
    fn new(threads: usize) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        for i in 0..threads {
            let rx: Receiver<Job> = rx.clone();
            // Deeply nested code recurses deeply in the parser and inference.
            let spawned = std::thread::Builder::new()
                .name(format!("rustpy-lsp-worker-{i}"))
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    for job in rx {
                        job();
                    }
                });
            if let Err(e) = spawned {
                log::error!("lsp: cannot start a worker thread: {e}");
            }
        }
        Self { tx }
    }

    fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.tx.send(Box::new(job));
    }
}

struct Server {
    sender: Sender<Message>,
    /// `initializationOptions`, the base of later configuration changes.
    init_options: serde_json::Value,
    /// Whether the IDE trusts the primary folder (external formatters may run).
    trusted: bool,
    caps: Arc<ClientCaps>,
    enc: Encoding,
    base_settings: Settings,
    config: Arc<Config>,
    model: ProjectModel,
    analysis: Arc<Analysis>,
    prepared: Arc<Generation>,
    state_epoch: u64,
    interpreter: Option<InterpreterInfo>,
    interpreter_ready: bool,
    discovery_generation: u64,
    started: Instant,
    docs: Arc<HashMap<PathBuf, Doc>>,
    files: Arc<ProjectFiles>,
    index: Arc<SymbolIndex>,
    stubs: Arc<StubDir>,
    problems: Arc<HashMap<PathBuf, (i32, Arc<FileProblems>)>>,
    pool: Pool,
    task_tx: Sender<Task>,
    pending: HashMap<RequestId, (CancellationToken, u64)>,
    diag_due: HashMap<PathBuf, Instant>,
    diag_running: HashMap<PathBuf, i32>,
    /// Bumped when the configuration or the analysis is replaced: diagnostics
    /// computed before are discarded (a new run is scheduled anyway).
    diag_epoch: u64,
    /// The server's own file watcher (when the client does not watch files for it).
    fs_watcher: Option<crate::watcher::FsWatcher>,
    /// Watcher changes waiting to be applied together, and when.
    fs_pending: Vec<(PathBuf, lsp_types::FileChangeType)>,
    fs_due: Option<Instant>,
    /// A background index refresh is running / another one was asked for meanwhile.
    index_refreshing: bool,
    index_stale: bool,
    shutdown: bool,
    next_request_id: i32,
}

/// Runs the server over stdin/stdout until the client sends `exit`.
pub fn run_stdio() -> anyhow::Result<()> {
    log::info!(
        "rustpy {} language server starting (stdio)",
        env!("CARGO_PKG_VERSION")
    );
    let (connection, io_threads) = Connection::stdio();
    let result = run(connection, ServerOptions::default());
    match result {
        // The reader thread ends after `exit`; the writer after the last message.
        Ok(()) => io_threads.join()?,
        // The reader may still be waiting for input: do not wait for it.
        Err(e) => {
            log::error!("language server stopped: {e:#}");
            return Err(e);
        }
    }
    log::info!("language server stopped");
    Ok(())
}

/// Runs the server on `connection` (stdio, a socket, or an in-memory pair).
pub fn run(connection: Connection, options: ServerOptions) -> anyhow::Result<()> {
    let (id, params) = connection.initialize_start()?;
    let init: InitializeParams = serde_json::from_value(params)?;
    let enc = Encoding::negotiate(
        init.capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_deref()),
    );
    let result = InitializeResult {
        capabilities: caps::server_capabilities(enc),
        server_info: Some(ServerInfo {
            name: "rustpy".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;
    let (mut server, tasks) = Server::new(connection.sender.clone(), &init, enc, options);
    server.after_initialized();
    server.main_loop(&connection.receiver, &tasks)
}

#[allow(deprecated)]
fn initial_folders(init: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &init.workspace_folders {
        return folders
            .iter()
            .filter_map(|f| paths::path_from_uri(&f.uri))
            .collect();
    }
    if let Some(p) = init.root_uri.as_ref().and_then(paths::path_from_uri) {
        return vec![p];
    }
    init.root_path
        .as_ref()
        .map(PathBuf::from)
        .into_iter()
        .collect()
}

impl Server {
    fn new(
        sender: Sender<Message>,
        init: &InitializeParams,
        enc: Encoding,
        options: ServerOptions,
    ) -> (Self, Receiver<Task>) {
        let base_settings = options.settings.unwrap_or_else(config::load_ide_settings);
        let init_options = init
            .initialization_options
            .clone()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::json!({}));
        let config = Arc::new(Config::new(&base_settings, Some(&init_options)));
        let folders = initial_folders(init);
        let model = match ProjectModel::try_load(folders.clone()) {
            Ok(model) => model,
            Err(error) => {
                report_project_error(&sender, error);
                ProjectModel::unavailable(folders)
            }
        };
        let analysis = Arc::new(project::build_analysis(&model, None));
        let stubs = Arc::new(StubDir::new(
            options.stub_dir.unwrap_or_else(StubDir::default_location),
        ));
        let (task_tx, task_rx) = crossbeam_channel::unbounded();
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 8);
        log::info!(
            "lsp: workspace folders {:?}, position encoding {:?}",
            model.folders,
            enc
        );
        let trusted = model
            .primary()
            .is_some_and(|r| project::is_trusted(&config.settings, r));
        let server = Self {
            sender,
            init_options,
            trusted,
            caps: Arc::new(ClientCaps::from(&init.capabilities)),
            enc,
            base_settings,
            config,
            files: Arc::new(ProjectFiles::new(&model)),
            model,
            analysis,
            prepared: Arc::new(OnceLock::new()),
            state_epoch: 0,
            interpreter: None,
            interpreter_ready: false,
            discovery_generation: 0,
            started: Instant::now(),
            docs: Arc::new(HashMap::new()),
            index: Arc::new(SymbolIndex::new(enc)),
            stubs,
            problems: Arc::new(HashMap::new()),
            pool: Pool::new(threads),
            task_tx,
            pending: HashMap::new(),
            diag_due: HashMap::new(),
            diag_running: HashMap::new(),
            diag_epoch: 0,
            fs_watcher: None,
            fs_pending: Vec::new(),
            fs_due: None,
            index_refreshing: false,
            index_stale: false,
            shutdown: false,
            next_request_id: 0,
        };
        (server, task_rx)
    }

    fn after_initialized(&mut self) {
        if self.caps.watched_files_dynamic {
            let watchers: Vec<lsp_types::FileSystemWatcher> = [
                "**/*.py",
                "**/*.pyi",
                "**/.editorconfig",
                "**/.rustpy/workspace.json",
                "**/.rustpy/project.json",
                "**/pyvenv.cfg",
            ]
            .iter()
            .map(|g| lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::String(g.to_string()),
                kind: None,
            })
            .collect();
            let options = lsp_types::DidChangeWatchedFilesRegistrationOptions { watchers };
            let reg = lsp_types::Registration {
                id: "rustpy-watched-files".into(),
                method: lsp_types::notification::DidChangeWatchedFiles::METHOD.into(),
                register_options: serde_json::to_value(options).ok(),
            };
            self.send_request::<lsp_types::request::RegisterCapability>(
                lsp_types::RegistrationParams {
                    registrations: vec![reg],
                },
            );
        }
        self.start_watcher();
        self.start_discovery();
        self.warm_index();
    }

    /// Watches the workspace folders itself when the client cannot.
    fn start_watcher(&mut self) {
        self.fs_watcher = None;
        if self.model.folders.is_empty() {
            return;
        }
        let tx = self.task_tx.clone();
        let mut roots = self.model.scope().walk_roots();
        for folder in &self.model.folders {
            if !roots.contains(folder) {
                roots.push(folder.clone());
            }
        }
        // Client glob registrations cover workspace folders. Attached roots
        // outside them still need our watcher, even for a dynamic client.
        if self.caps.watched_files_dynamic {
            roots.retain(|r| !self.model.folders.iter().any(|f| r.starts_with(f)));
        }
        if roots.is_empty() {
            return;
        }
        self.fs_watcher = crate::watcher::watch(&roots, move |changes| {
            let _ = tx.send(Task::FilesChanged(changes));
        });
    }

    // ----- main loop ---------------------------------------------------------------

    fn main_loop(
        &mut self,
        receiver: &Receiver<Message>,
        tasks: &Receiver<Task>,
    ) -> anyhow::Result<()> {
        loop {
            let timeout = self
                .next_wakeup()
                .map(|t| t.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            crossbeam_channel::select! {
                recv(receiver) -> msg => match msg {
                    Ok(Message::Request(req)) => self.on_request(req),
                    Ok(Message::Notification(n)) => {
                        if n.method == lsp_types::notification::Exit::METHOD {
                            return if self.shutdown { Ok(()) } else { Err(anyhow::anyhow!("exit notification received before shutdown")) };
                        }
                        self.on_notification(n);
                    }
                    Ok(Message::Response(r)) => {
                        if let Err(err) = r.response_result {
                            log::debug!("lsp: client answered request {} with an error: {}", r.id, err.message);
                        }
                    }
                    Err(_) => return Err(anyhow::anyhow!("the client closed the connection")),
                },
                recv(tasks) -> task => {
                    if let Ok(task) = task {
                        self.on_task(task);
                    }
                }
                default(timeout) => {}
            }
            if self.fs_due.is_some_and(|t| t <= Instant::now()) {
                self.fs_due = None;
                let changes = std::mem::take(&mut self.fs_pending);
                self.files_changed(changes);
            }
            self.run_due_diagnostics();
        }
    }

    /// When the next debounced diagnostics run (or batch of file changes) is due.
    fn next_wakeup(&self) -> Option<Instant> {
        let diagnostics = self.diag_due.values().min().copied().map(|due| {
            if self.interpreter_ready {
                due
            } else {
                due.max(self.started + DISCOVERY_GRACE)
            }
        });
        match (diagnostics, self.fs_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn send(&self, msg: Message) {
        let _ = self.sender.send(msg);
    }

    fn respond(&self, resp: Response) {
        self.send(Message::Response(resp));
    }

    fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        self.send(Message::Notification(Notification::new(
            N::METHOD.to_string(),
            params,
        )));
    }

    fn send_request<R: lsp_types::request::Request>(&mut self, params: R::Params) {
        self.next_request_id += 1;
        let id = RequestId::from(format!("rustpy-{}", self.next_request_id));
        self.send(Message::Request(Request::new(
            id,
            R::METHOD.to_string(),
            params,
        )));
    }

    fn log_message(&self, message: String) {
        log::info!("lsp: {message}");
        self.notify::<lsp_types::notification::LogMessage>(lsp_types::LogMessageParams {
            typ: lsp_types::MessageType::INFO,
            message,
        });
    }

    fn snapshot(&self, cancel: CancellationToken) -> Snapshot {
        Snapshot {
            analysis: self.analysis.clone(),
            prepared: self.prepared.clone(),
            config: self.config.clone(),
            caps: self.caps.clone(),
            enc: self.enc,
            docs: self.docs.clone(),
            files: self.files.clone(),
            index: self.index.clone(),
            stubs: self.stubs.clone(),
            problems: self.problems.clone(),
            python_version: self
                .interpreter
                .as_ref()
                .map(|i| i.version)
                .unwrap_or((3, 12)),
            interpreter: self.interpreter.as_ref().and_then(|i| i.path.clone()),
            trusted: self.trusted,
            cancel,
        }
    }

    // ----- requests ----------------------------------------------------------------

    fn on_request(&mut self, req: Request) {
        use lsp_types::request::*;
        if self.shutdown {
            self.respond(Response::new_err(
                req.id,
                ErrorCode::InvalidRequest as i32,
                "the server is shutting down".into(),
            ));
            return;
        }
        match req.method.as_str() {
            Shutdown::METHOD => {
                self.shutdown = true;
                self.respond(Response::new_ok(req.id, ()));
            }
            HoverRequest::METHOD => self.spawn::<HoverRequest>(req, handlers::hover),
            Completion::METHOD => self.spawn::<Completion>(req, completion::completion),
            ResolveCompletionItem::METHOD => {
                self.spawn::<ResolveCompletionItem>(req, completion::resolve)
            }
            SignatureHelpRequest::METHOD => {
                self.spawn::<SignatureHelpRequest>(req, handlers::signature_help)
            }
            GotoDefinition::METHOD => self.spawn::<GotoDefinition>(req, handlers::definition),
            GotoDeclaration::METHOD => self.spawn::<GotoDeclaration>(req, handlers::definition),
            GotoTypeDefinition::METHOD => {
                self.spawn::<GotoTypeDefinition>(req, handlers::type_definition)
            }
            GotoImplementation::METHOD => {
                self.spawn::<GotoImplementation>(req, handlers::implementation)
            }
            References::METHOD => self.spawn::<References>(req, handlers::references),
            DocumentHighlightRequest::METHOD => {
                self.spawn::<DocumentHighlightRequest>(req, handlers::document_highlight)
            }
            PrepareRenameRequest::METHOD => {
                self.spawn::<PrepareRenameRequest>(req, handlers::prepare_rename)
            }
            Rename::METHOD => self.spawn::<Rename>(req, handlers::rename),
            Formatting::METHOD => self.spawn::<Formatting>(req, handlers::formatting),
            RangeFormatting::METHOD => {
                self.spawn::<RangeFormatting>(req, handlers::range_formatting)
            }
            DocumentSymbolRequest::METHOD => {
                self.spawn::<DocumentSymbolRequest>(req, handlers::document_symbol)
            }
            WorkspaceSymbolRequest::METHOD => {
                self.spawn::<WorkspaceSymbolRequest>(req, handlers::workspace_symbol)
            }
            FoldingRangeRequest::METHOD => {
                self.spawn::<FoldingRangeRequest>(req, handlers::folding_range)
            }
            SelectionRangeRequest::METHOD => {
                self.spawn::<SelectionRangeRequest>(req, handlers::selection_range)
            }
            CodeActionRequest::METHOD => {
                self.spawn::<CodeActionRequest>(req, code_actions::code_action)
            }
            SemanticTokensFullRequest::METHOD => {
                self.spawn::<SemanticTokensFullRequest>(req, handlers::semantic_tokens_full)
            }
            SemanticTokensRangeRequest::METHOD => {
                self.spawn::<SemanticTokensRangeRequest>(req, handlers::semantic_tokens_range)
            }
            InlayHintRequest::METHOD => self.spawn::<InlayHintRequest>(req, handlers::inlay_hints),
            CallHierarchyPrepare::METHOD => {
                self.spawn::<CallHierarchyPrepare>(req, handlers::prepare_call_hierarchy)
            }
            CallHierarchyIncomingCalls::METHOD => {
                self.spawn::<CallHierarchyIncomingCalls>(req, handlers::incoming_calls)
            }
            CallHierarchyOutgoingCalls::METHOD => {
                self.spawn::<CallHierarchyOutgoingCalls>(req, handlers::outgoing_calls)
            }
            _ => self.respond(Response::new_err(
                req.id,
                ErrorCode::MethodNotFound as i32,
                format!("unsupported request: {}", req.method),
            )),
        }
    }

    /// Runs `handler` on a worker thread with a snapshot of the current state.
    fn spawn<R>(&mut self, req: Request, handler: fn(&Snapshot, R::Params) -> HResult<R::Result>)
    where
        R: lsp_types::request::Request,
        R::Params: DeserializeOwned + Send + 'static,
        R::Result: Serialize + 'static,
    {
        let id = req.id.clone();
        let params: R::Params = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => {
                self.respond(Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    format!("invalid {} parameters: {e}", req.method),
                ));
                return;
            }
        };
        let token = CancellationToken::new();
        self.pending
            .insert(id.clone(), (token.clone(), self.state_epoch));
        let mut snap = self.snapshot(token);
        let tx = self.task_tx.clone();
        let method = req.method;
        self.pool.spawn(move || {
            let started = Instant::now();
            let result = catch_unwind(AssertUnwindSafe(|| {
                snap.check_cancelled()?;
                snap.prepare();
                snap.check_cancelled()?;
                let value = handler(&snap, params)?;
                snap.check_cancelled()?;
                Ok::<_, LspError>(value)
            }));
            let resp = match result {
                Ok(Ok(value)) => Response::new_ok(id, value),
                Ok(Err(e)) => Response::new_err(id, e.code, e.message),
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    log::error!("lsp: {method} panicked: {msg}");
                    Response::new_err(
                        id,
                        ErrorCode::InternalError as i32,
                        format!("{method} failed: {msg}"),
                    )
                }
            };
            log::debug!("lsp: {method} took {:?}", started.elapsed());
            let _ = tx.send(Task::Response(resp));
        });
    }

    fn on_task(&mut self, task: Task) {
        match task {
            Task::Response(resp) => {
                // Answered already when the request was cancelled.
                if let Some((_, epoch)) = self.pending.remove(&resp.id) {
                    // Also protect clients using unversioned WorkspaceEdit.changes.
                    let resp = if epoch != self.state_epoch {
                        let id = resp.id.clone();
                        self.pool.spawn(move || drop(resp));
                        Response::new_err(
                            id,
                            ErrorCode::ContentModified as i32,
                            "the workspace changed while the request was running".into(),
                        )
                    } else {
                        resp
                    };
                    self.respond(resp);
                } else {
                    self.pool.spawn(move || drop(resp));
                }
            }
            Task::Diagnostics {
                path,
                version,
                epoch,
                result,
            } => {
                self.diag_running.remove(&path);
                let Some(doc) = self.docs.get(&path) else {
                    return;
                };
                if doc.version != version || epoch != self.diag_epoch {
                    return;
                }
                let Some(Ok((diagnostics, problems))) = result else {
                    return;
                };
                let uri = doc.uri.clone();
                Arc::make_mut(&mut self.problems).insert(path, (version, problems));
                let version = self.caps.diagnostic_versions.then_some(version);
                self.notify::<lsp_types::notification::PublishDiagnostics>(
                    lsp_types::PublishDiagnosticsParams {
                        uri,
                        diagnostics,
                        version,
                    },
                );
            }
            Task::Interpreter { generation, info } => {
                if generation != self.discovery_generation {
                    return;
                }
                match &info {
                    Some(i) => self.log_message(format!(
                        "using {} for module resolution ({} search paths)",
                        i.label,
                        i.search_paths.len()
                    )),
                    None => self.log_message(
                        "no Python interpreter: resolving against the bundled stdlib stubs only"
                            .into(),
                    ),
                }
                let changed = self.interpreter != info;
                self.interpreter = info;
                self.interpreter_ready = true;
                if changed {
                    self.rebuild_analysis();
                }
            }
            Task::IndexRefreshed { changed } => {
                self.index_refreshing = false;
                // New project names can be imported: recompute the quick fixes.
                if changed {
                    self.diag_epoch += 1;
                    self.reset_prepared();
                    self.clear_problems();
                    self.schedule_all(Duration::ZERO);
                }
                if self.index_stale {
                    self.warm_index();
                }
            }
            Task::FilesChanged(changes) => {
                // A burst (a checkout, a formatter run) is applied in one go.
                self.fs_pending.extend(changes);
                self.fs_due
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(150));
            }
        }
    }

    // ----- notifications -------------------------------------------------------------

    fn on_notification(&mut self, n: Notification) {
        use lsp_types::notification::*;
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                if let Some(p) = parse::<DidOpenTextDocument>(n) {
                    self.did_open(p);
                }
            }
            DidChangeTextDocument::METHOD => {
                if let Some(p) = parse::<DidChangeTextDocument>(n) {
                    self.did_change(p);
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Some(p) = parse::<DidCloseTextDocument>(n) {
                    self.did_close(p.text_document.uri);
                }
            }
            DidSaveTextDocument::METHOD => {
                // Other open files may depend on the saved one.
                self.schedule_all(Duration::ZERO);
                self.warm_index();
            }
            DidChangeWatchedFiles::METHOD => {
                if let Some(p) = parse::<DidChangeWatchedFiles>(n) {
                    let changes = p
                        .changes
                        .into_iter()
                        .filter_map(|c| paths::path_from_uri(&c.uri).map(|p| (p, c.typ)))
                        .collect();
                    self.files_changed(changes);
                }
            }
            DidChangeConfiguration::METHOD => {
                if let Some(p) = parse::<DidChangeConfiguration>(n) {
                    self.configuration_changed(p.settings);
                }
            }
            DidChangeWorkspaceFolders::METHOD => {
                if let Some(p) = parse::<DidChangeWorkspaceFolders>(n) {
                    let mut folders = self.model.folders.clone();
                    for removed in p
                        .event
                        .removed
                        .iter()
                        .filter_map(|f| paths::path_from_uri(&f.uri))
                    {
                        folders.retain(|f| *f != removed);
                    }
                    folders.extend(
                        p.event
                            .added
                            .iter()
                            .filter_map(|f| paths::path_from_uri(&f.uri)),
                    );
                    self.set_folders(folders);
                }
            }
            Cancel::METHOD => {
                if let Some(p) = parse::<Cancel>(n) {
                    let id = match p.id {
                        lsp_types::NumberOrString::Number(n) => RequestId::from(n),
                        lsp_types::NumberOrString::String(s) => RequestId::from(s),
                    };
                    if let Some((token, _)) = self.pending.remove(&id) {
                        token.cancel();
                        let e = LspError::cancelled();
                        self.respond(Response::new_err(id, e.code, e.message));
                    }
                }
            }
            _ => {}
        }
    }

    fn did_open(&mut self, p: lsp_types::DidOpenTextDocumentParams) {
        let td = p.text_document;
        let Some(real) = paths::path_from_uri(&td.uri) else {
            log::debug!("lsp: ignoring {} (not a file)", td.uri.as_str());
            return;
        };
        let path = self.stubs.virtual_path(&real).unwrap_or(real.clone());
        // A single file opened without a workspace: its folder is the project.
        if self.model.folders.is_empty() && !paths::is_virtual_stub(&path) {
            if let Some(parent) = real.parent() {
                self.set_folders(vec![parent.to_path_buf()]);
            }
        }
        let doc = Doc::new(td.uri, path.clone(), td.version, &td.language_id, td.text);
        let python = doc.is_python;
        Arc::make_mut(&mut self.docs).insert(path.clone(), doc);
        self.documents_changed();
        if python {
            self.diag_due.insert(path, Instant::now());
        }
    }

    fn did_change(&mut self, p: lsp_types::DidChangeTextDocumentParams) {
        let Some(path) = paths::path_from_uri(&p.text_document.uri)
            .map(|r| self.stubs.virtual_path(&r).unwrap_or(r))
        else {
            return;
        };
        let enc = self.enc;
        let docs = Arc::make_mut(&mut self.docs);
        let Some(doc) = docs.get_mut(&path) else {
            log::warn!(
                "lsp: change for a document that is not open: {}",
                p.text_document.uri.as_str()
            );
            return;
        };
        doc.apply_changes(p.text_document.version, p.content_changes, enc);
        let python = doc.is_python;
        self.documents_changed();
        if python {
            self.diag_due
                .insert(path, Instant::now() + self.config.diagnostics_delay);
        }
    }

    fn did_close(&mut self, uri: Uri) {
        let Some(path) =
            paths::path_from_uri(&uri).map(|r| self.stubs.virtual_path(&r).unwrap_or(r))
        else {
            return;
        };
        let Some(doc) = Arc::make_mut(&mut self.docs).remove(&path) else {
            return;
        };
        self.documents_changed();
        self.diag_due.remove(&path);
        Arc::make_mut(&mut self.problems).remove(&path);
        self.notify::<lsp_types::notification::PublishDiagnostics>(
            lsp_types::PublishDiagnosticsParams {
                uri: doc.uri,
                diagnostics: Vec::new(),
                version: None,
            },
        );
    }

    /// Files changed on disk (reported by the client or by the server's watcher).
    fn files_changed(&mut self, changes: Vec<(PathBuf, lsp_types::FileChangeType)>) {
        // Navigation materializes bundled stubs on disk. These owned cache
        // writes must not invalidate the request that produced them, including
        // when the configured cache lives inside a watched workspace.
        let changes: Vec<_> = changes
            .into_iter()
            .filter(|(path, _)| !self.stubs.contains_generated_path(path))
            // A watcher may report a write the analysis has already read (macOS
            // FSEvents arrive with latency): a file whose text is unchanged is no
            // change, and must not invalidate the requests in flight.
            .filter(|(path, typ)| {
                let python_source = path
                    .extension()
                    .is_some_and(|extension| extension == "py" || extension == "pyi");
                *typ != lsp_types::FileChangeType::CHANGED
                    || !python_source
                    || self.analysis.file_for_path(path).is_none_or(|file| {
                        std::fs::read_to_string(path)
                            .map_or(true, |disk| disk.as_str() != &*file.text)
                    })
            })
            .collect();
        let paths: Vec<PathBuf> = changes.iter().map(|(p, _)| p.clone()).collect();
        if paths.is_empty() {
            return;
        }
        if paths.iter().any(|p| {
            p.ends_with(".rustpy/workspace.json")
                || p.ends_with(".rustpy/project.json")
                || p.file_name().is_some_and(|n| n == "pyvenv.cfg")
        }) {
            match ProjectModel::try_load(self.model.folders.clone()) {
                Ok(model) if model != self.model => {
                    self.set_folders(self.model.folders.clone());
                    return;
                }
                Err(error) => report_project_error(&self.sender, error),
                _ => {}
            }
        }
        self.documents_changed();
        if changes
            .iter()
            .any(|(_, typ)| *typ != lsp_types::FileChangeType::CHANGED)
        {
            self.files.invalidate();
        }
        self.schedule_all(Duration::from_millis(100));
        self.warm_index();
    }

    fn configuration_changed(&mut self, settings: serde_json::Value) {
        let Some(section) = Config::section(&settings) else {
            return;
        };
        // A partial update keeps what the client said at initialization unless it says otherwise.
        let mut options = self.init_options.clone();
        config::merge(&mut options, section);
        let new = Config::new(&self.base_settings, Some(&options));
        let rediscover = new.interpreter != self.config.interpreter;
        self.config = Arc::new(new);
        self.documents_changed();
        self.diag_epoch += 1;
        self.trusted = self
            .model
            .primary()
            .is_some_and(|r| project::is_trusted(&self.config.settings, r));
        if rediscover {
            self.start_discovery();
        }
        Arc::make_mut(&mut self.problems).clear();
        self.schedule_all(Duration::ZERO);
    }

    // ----- project model -------------------------------------------------------------

    fn set_folders(&mut self, folders: Vec<PathBuf>) {
        match ProjectModel::try_load(folders) {
            Ok(model) => self.model = model,
            Err(error) => {
                report_project_error(&self.sender, error);
                return;
            }
        }
        self.trusted = self
            .model
            .primary()
            .is_some_and(|r| project::is_trusted(&self.config.settings, r));
        self.files = Arc::new(ProjectFiles::new(&self.model));
        log::info!("lsp: workspace folders {:?}", self.model.folders);
        self.start_watcher();
        self.rebuild_analysis();
        self.start_discovery();
        self.warm_index();
    }

    /// A fresh resolver for the current model and interpreter; workers
    /// prepare the open documents again and re-check them.
    fn rebuild_analysis(&mut self) {
        self.analysis = Arc::new(project::build_analysis(
            &self.model,
            self.interpreter.as_ref(),
        ));
        self.documents_changed();
        self.diag_epoch += 1;
        Arc::make_mut(&mut self.problems).clear();
        self.schedule_all(Duration::ZERO);
    }

    fn start_discovery(&mut self) {
        self.discovery_generation += 1;
        self.interpreter_ready = false;
        self.started = Instant::now();
        let generation = self.discovery_generation;
        let model = self.model.clone();
        let choice = self.config.interpreter.clone();
        let settings = self.config.settings.clone();
        let tx = self.task_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("rustpy-lsp-interpreters".into())
            .spawn(move || {
                let info = catch_unwind(AssertUnwindSafe(|| {
                    project::discover_interpreter(&model, &choice, &settings)
                }))
                .unwrap_or(None);
                let _ = tx.send(Task::Interpreter { generation, info });
            });
        if spawned.is_err() {
            self.interpreter_ready = true;
        }
    }

    /// Builds (or refreshes) the symbol index in the background: Add import
    /// fixes and auto-import completion use it.
    fn warm_index(&mut self) {
        if self.index_refreshing {
            self.index_stale = true;
            return;
        }
        self.index_refreshing = true;
        self.index_stale = false;
        let snap = self.snapshot(CancellationToken::new());
        let tx = self.task_tx.clone();
        self.pool.spawn(move || {
            let changed = catch_unwind(AssertUnwindSafe(|| handlers::refresh_index(&snap, false)))
                .ok()
                .flatten()
                .unwrap_or(false);
            let _ = tx.send(Task::IndexRefreshed { changed });
        });
    }

    // ----- documents and diagnostics --------------------------------------------------

    /// A new generation is prepared lazily by workers; old requests keep
    /// their own buffers and caches until they finish.
    fn documents_changed(&mut self) {
        self.state_epoch += 1;
        self.diag_epoch += 1;
        self.reset_prepared();
        self.clear_problems();
        self.schedule_all(self.config.diagnostics_delay);
    }

    fn clear_problems(&mut self) {
        let old = std::mem::replace(&mut self.problems, Arc::new(HashMap::new()));
        self.pool.spawn(move || drop(old));
    }

    fn reset_prepared(&mut self) {
        let old = std::mem::replace(&mut self.prepared, Arc::new(OnceLock::new()));
        if old.get().is_some() {
            // Large cached analyses must also be freed off the protocol thread.
            self.pool.spawn(move || drop(old));
        }
    }

    fn schedule_all(&mut self, delay: Duration) {
        let at = Instant::now() + delay;
        let paths: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|(p, d)| d.is_python && !paths::is_virtual_stub(p))
            .map(|(p, _)| p.clone())
            .collect();
        for p in paths {
            let due = self.diag_due.entry(p).or_insert(at);
            *due = (*due).min(at);
        }
    }

    fn run_due_diagnostics(&mut self) {
        if !self.interpreter_ready && self.started.elapsed() < DISCOVERY_GRACE {
            return;
        }
        let now = Instant::now();
        let due: Vec<PathBuf> = self
            .diag_due
            .iter()
            .filter(|(p, t)| **t <= now && !self.diag_running.contains_key(*p))
            .map(|(p, _)| p.clone())
            .collect();
        if due.is_empty() {
            return;
        }
        for path in due {
            self.diag_due.remove(&path);
            let Some(doc) = self.docs.get(&path) else {
                continue;
            };
            if !doc.is_python || paths::is_virtual_stub(&path) {
                continue;
            }
            let (uri, version, epoch) = (doc.uri.clone(), doc.version, self.diag_epoch);
            self.diag_running.insert(path.clone(), version);
            let mut snap = self.snapshot(CancellationToken::new());
            let tx = self.task_tx.clone();
            self.pool.spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    snap.prepare();
                    snap.doc(&uri)
                        .ok()
                        .map(|view| crate::diagnostics::compute(&snap, &view))
                }));
                let result = match result {
                    Ok(r) => r,
                    Err(_) => {
                        log::error!("lsp: diagnostics of {} panicked", uri.as_str());
                        None
                    }
                };
                let _ = tx.send(Task::Diagnostics {
                    path,
                    version,
                    epoch,
                    result,
                });
            });
        }
    }
}

fn parse<N: lsp_types::notification::Notification>(n: Notification) -> Option<N::Params>
where
    N::Params: DeserializeOwned,
{
    match serde_json::from_value(n.params) {
        Ok(p) => Some(p),
        Err(e) => {
            log::warn!("lsp: invalid {} parameters: {e}", N::METHOD);
            None
        }
    }
}

/// Shared structure errors are visible to clients; refresh keeps the last valid scope.
fn report_project_error(sender: &Sender<Message>, error: String) {
    log::error!("lsp: {error}");
    let params = lsp_types::LogMessageParams {
        typ: lsp_types::MessageType::ERROR,
        message: format!("RustPy project structure: {error}"),
    };
    let _ = sender.send(Message::Notification(Notification::new(
        lsp_types::notification::LogMessage::METHOD.into(),
        params,
    )));
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use lsp_types::{
        DidOpenTextDocumentParams, Position, RenameParams, TextDocumentItem,
        TextDocumentPositionParams,
    };
    use serde_json::json;

    #[test]
    fn cancelled_diagnostic_computation_and_code_actions_do_not_publish_or_replace_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        let uri = paths::uri_from_path(&path).unwrap();
        let init: InitializeParams = serde_json::from_value(json!({"rootUri": paths::uri_from_path(dir.path()).unwrap(), "capabilities": {}, "initializationOptions": {"interpreter": "none"}})).unwrap();
        let (server_conn, client) = Connection::memory();
        let (mut server, _) = Server::new(
            server_conn.sender,
            &init,
            Encoding::Utf16,
            ServerOptions {
                settings: Some(Settings::default()),
                stub_dir: Some(dir.path().join("stubs")),
            },
        );
        server.did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "python".into(),
                version: 1,
                text: "# a apple recieve\nmissing_name\n".into(),
            },
        });
        let token = CancellationToken::new();
        let mut snap = server.snapshot(token.clone());
        snap.prepare();
        let doc = snap.doc(&uri).unwrap();
        let previous = Arc::new(crate::diagnostics::problems(&snap, &doc).unwrap());
        assert!(!previous.is_empty());
        Arc::make_mut(&mut server.problems).insert(path.clone(), (1, previous.clone()));
        token.cancel();
        let error = crate::diagnostics::compute(&snap, &doc).unwrap_err();
        assert_eq!(error.code, ErrorCode::RequestCanceled as i32);
        let params = || {
            serde_json::from_value(json!({"textDocument": {"uri": uri}, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}}, "context": {"diagnostics": [], "only": ["quickfix"]}})).unwrap()
        };
        // Both the uncached fallback and a current cached result reject cancellation.
        assert_eq!(
            crate::code_actions::code_action(&snap, params())
                .unwrap_err()
                .code,
            ErrorCode::RequestCanceled as i32
        );
        Arc::make_mut(&mut snap.problems).insert(path.clone(), (1, previous.clone()));
        assert_eq!(
            crate::code_actions::code_action(&snap, params())
                .unwrap_err()
                .code,
            ErrorCode::RequestCanceled as i32
        );
        while client.receiver.try_recv().is_ok() {}
        server.diag_running.insert(path.clone(), 1);
        server.on_task(Task::Diagnostics {
            path: path.clone(),
            version: 1,
            epoch: server.diag_epoch,
            result: Some(Err(error)),
        });
        assert!(!server.diag_running.contains_key(&path));
        assert!(Arc::ptr_eq(&server.problems[&path].1, &previous));
        assert!(client.receiver.try_recv().is_err());
    }

    #[test]
    fn shared_diagnostics_keep_lsp_text_order_and_stub_supplement_policy() {
        let dir = tempfile::tempdir().unwrap();
        let init: InitializeParams = serde_json::from_value(json!({"rootUri": paths::uri_from_path(dir.path()).unwrap(), "capabilities": {}, "initializationOptions": {"interpreter": "none"}})).unwrap();
        let (server_conn, _client) = Connection::memory();
        let (mut server, _) = Server::new(
            server_conn.sender,
            &init,
            Encoding::Utf16,
            ServerOptions {
                settings: Some(Settings::default()),
                stub_dir: Some(dir.path().join("stubs")),
            },
        );
        let source = "import re\n# a apple recieve\nlabel = 'é'\nre.compile(r'[')\nmissing_name\n";
        for name in ["main.py", "stub.pyi"] {
            let uri = paths::uri_from_path(&dir.path().join(name)).unwrap();
            server.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "python".into(),
                    version: 1,
                    text: source.into(),
                },
            });
            let mut snap = server.snapshot(CancellationToken::new());
            snap.prepare();
            let doc = snap.doc(&uri).unwrap();
            let (diagnostics, problems) = crate::diagnostics::compute(&snap, &doc).unwrap();
            assert_eq!(diagnostics.len(), problems.len());
            if name.ends_with(".pyi") {
                assert!(problems.iter().all(|p| !matches!(
                    p.diagnostic.code.as_str(),
                    "Grammar" | "SpellCheckingInspection" | "PyDocstring" | "PyRegex"
                )));
            } else {
                assert!(problems
                    .windows(2)
                    .all(|p| p[0].diagnostic.range.start() <= p[1].diagnostic.range.start()));
                assert!(problems.iter().any(|p| p.diagnostic.code == "PyRegex"));
                assert!(problems
                    .iter()
                    .flat_map(|p| &p.fixes)
                    .all(|f| !f.title.starts_with("Save '")));
                for (diagnostic, problem) in diagnostics.iter().zip(problems.iter()) {
                    assert_eq!(
                        diagnostic.range,
                        doc.index.range(problem.diagnostic.range, snap.enc)
                    );
                    assert_eq!(diagnostic.source.as_deref(), Some("RustPy"));
                }
            }
        }
    }

    #[test]
    fn rename_snapshot_is_coherent_and_stale_results_are_rejected_for_both_edit_capabilities() {
        for document_changes in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("main.py");
            let helper = dir.path().join("helper.py");
            let main_text = "class Target: pass\n";
            let helper_text = "from main import Target\nx=Target()\n";
            std::fs::write(&main, main_text).unwrap();
            std::fs::write(&helper, helper_text).unwrap();
            let uri = |p: &std::path::Path| paths::uri_from_path(p).unwrap();
            let init: InitializeParams = serde_json::from_value(json!({"rootUri": uri(dir.path()), "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": document_changes}}}, "initializationOptions": {"interpreter": "none"}})).unwrap();
            let (server_conn, client) = Connection::memory();
            let (mut server, _) = Server::new(
                server_conn.sender,
                &init,
                Encoding::Utf16,
                ServerOptions {
                    settings: Some(Settings::default()),
                    stub_dir: Some(dir.path().join("stubs")),
                },
            );
            for (path, text) in [(&main, main_text), (&helper, helper_text)] {
                server.did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri(path),
                        language_id: "python".into(),
                        version: 1,
                        text: text.into(),
                    },
                });
            }
            let mut old = server.snapshot(CancellationToken::new());
            old.prepare();
            let epoch = server.state_epoch;
            server.did_change(serde_json::from_value(json!({"textDocument": {"uri": uri(&helper), "version": 2}, "contentChanges": [{"text": format!("{}{}", "\n".repeat(10), helper_text)}]})).unwrap());
            let mut current = server.snapshot(CancellationToken::new());
            current.prepare();
            assert_eq!(
                old.analysis.file_for_path(&helper).unwrap().text.as_ref(),
                helper_text
            );
            assert!(current
                .analysis
                .file_for_path(&helper)
                .unwrap()
                .text
                .starts_with("\n\n"));
            assert!(!Arc::ptr_eq(&old.analysis, &current.analysis));
            let params = RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri(&main) },
                    position: Position::new(0, 8),
                },
                new_name: "Changed".into(),
                work_done_progress_params: Default::default(),
            };
            let edit = handlers::rename(&old, params).unwrap().unwrap();
            let ranges: Vec<_> = if document_changes {
                let Some(lsp_types::DocumentChanges::Edits(edits)) = &edit.document_changes else {
                    panic!("versioned edits")
                };
                let helper_edits = edits
                    .iter()
                    .find(|d| d.text_document.uri == uri(&helper))
                    .unwrap();
                assert_eq!(helper_edits.text_document.version, Some(1));
                helper_edits
                    .edits
                    .iter()
                    .map(|e| match e {
                        lsp_types::OneOf::Left(e) => e.range,
                        _ => panic!("plain edit"),
                    })
                    .collect()
            } else {
                edit.changes.as_ref().unwrap()[&uri(&helper)]
                    .iter()
                    .map(|e| e.range)
                    .collect()
            };
            assert_eq!(ranges.len(), 2);
            assert!(ranges.contains(&lsp_types::Range::new(
                Position::new(0, 17),
                Position::new(0, 23)
            )));
            assert!(ranges.contains(&lsp_types::Range::new(
                Position::new(1, 2),
                Position::new(1, 8)
            )));
            let id = RequestId::from(1);
            server
                .pending
                .insert(id.clone(), (CancellationToken::new(), epoch));
            server.on_task(Task::Response(Response::new_ok(id, edit)));
            let Message::Response(response) = client
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            else {
                panic!("response")
            };
            assert_eq!(
                response.response_result.unwrap_err().code,
                ErrorCode::ContentModified as i32
            );
        }
    }

    #[test]
    fn generated_stub_events_preserve_pending_type_definition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let main = root.join("main.py");
        let text = "value = 'x'\n";
        std::fs::write(&main, text).unwrap();
        let uri = paths::uri_from_path(&main).unwrap();
        let cache = root.join(".stubs");
        let init: InitializeParams = serde_json::from_value(json!({"rootUri": paths::uri_from_path(&root).unwrap(), "capabilities": {}, "initializationOptions": {"interpreter": "none"}})).unwrap();
        let (server_conn, client) = Connection::memory();
        let (mut server, _) = Server::new(
            server_conn.sender,
            &init,
            Encoding::Utf16,
            ServerOptions {
                settings: Some(Settings::default()),
                stub_dir: Some(cache.clone()),
            },
        );
        server.did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "python".into(),
                version: 1,
                text: text.into(),
            },
        });
        let mut snap = server.snapshot(CancellationToken::new());
        snap.prepare();
        let epoch = server.state_epoch;
        let cached_files = server.files.get();
        let id = RequestId::from(71);
        server
            .pending
            .insert(id.clone(), (CancellationToken::new(), epoch));
        let params = serde_json::from_value(
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}),
        )
        .unwrap();
        let result = handlers::type_definition(&snap, params).unwrap().unwrap();
        let lsp_types::GotoDefinitionResponse::Array(locations) = &result else {
            panic!("type locations")
        };
        let generated = paths::path_from_uri(&locations[0].uri).unwrap();
        assert!(generated.starts_with(&cache));
        assert!(generated.ends_with("builtins.pyi"));
        assert!(generated.is_file(), "actual bundled stub materialization");
        server.on_task(Task::FilesChanged(vec![
            (cache, lsp_types::FileChangeType::CREATED),
            (generated, lsp_types::FileChangeType::CREATED),
        ]));
        // Apply the debounced batch exactly as the protocol loop does.
        server.fs_due = None;
        let changes = std::mem::take(&mut server.fs_pending);
        server.files_changed(changes);
        assert_eq!(
            server.state_epoch, epoch,
            "owned cache writes are not workspace edits"
        );
        assert!(
            Arc::ptr_eq(&cached_files, &server.files.get()),
            "cache events preserve the project file inventory"
        );
        server.on_task(Task::Response(Response::new_ok(id.clone(), result)));
        loop {
            if let Message::Response(response) = client
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            {
                assert_eq!(response.id, id);
                let value = response.response_result.unwrap();
                assert!(!value.as_array().unwrap().is_empty());
                break;
            }
        }
    }

    #[test]
    fn project_and_user_stub_events_still_invalidate_pending_responses() {
        for (name, mixed, created) in [
            ("main.py", false, false),
            ("main.py", false, true),
            ("user.pyi", false, false),
            ("user.pyi", false, true),
            (".stubs-other/user.pyi", false, false),
            (".stubs/../user.pyi", false, false),
            ("user.pyi", true, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dunce::canonicalize(dir.path()).unwrap();
            let cache = root.join(".stubs");
            let init: InitializeParams = serde_json::from_value(json!({"rootUri": paths::uri_from_path(&root).unwrap(), "capabilities": {}, "initializationOptions": {"interpreter": "none"}})).unwrap();
            let (server_conn, client) = Connection::memory();
            let (mut server, _) = Server::new(
                server_conn.sender,
                &init,
                Encoding::Utf16,
                ServerOptions {
                    settings: Some(Settings::default()),
                    stub_dir: Some(cache.clone()),
                },
            );
            let epoch = server.state_epoch;
            let cached_files = server.files.get();
            let id = RequestId::from(72);
            server
                .pending
                .insert(id.clone(), (CancellationToken::new(), epoch));
            let typ = if created {
                lsp_types::FileChangeType::CREATED
            } else {
                lsp_types::FileChangeType::CHANGED
            };
            let mut changes = vec![(root.join(name), typ)];
            if mixed {
                changes.push((
                    cache.join("builtins.pyi"),
                    lsp_types::FileChangeType::CREATED,
                ));
            }
            server.files_changed(changes);
            assert_eq!(server.state_epoch, epoch + 1, "{name}, mixed={mixed}");
            assert_eq!(
                Arc::ptr_eq(&cached_files, &server.files.get()),
                !created,
                "real creates invalidate the inventory; content-only edits retain it"
            );
            server.on_task(Task::Response(Response::new_ok(id.clone(), json!([]))));
            loop {
                if let Message::Response(response) = client
                    .receiver
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    assert_eq!(response.id, id);
                    assert_eq!(
                        response.response_result.unwrap_err().code,
                        ErrorCode::ContentModified as i32
                    );
                    break;
                }
            }
        }
    }
}

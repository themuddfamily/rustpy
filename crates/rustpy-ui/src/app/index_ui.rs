//! Symbol indexes in the IDE window (roadmap §7 "Shared indexes"): following
//! the project and its interpreter, handing new indexes to their consumers
//! (Search Everywhere / Go to Class / Go to Symbol, second-press completion,
//! the "Import" quick fix), the status bar indicator and Settings › Tools ›
//! Shared Indexes.

use super::*;
use crate::symbol_index::{default_bundle_path, format_age, format_size, IndexSnapshot, IndexView};
use rustpy_analysis::inspections::Problem;

/// Import fixes offered for one unresolved reference, all sources together.
const MAX_IMPORT_FIXES: usize = 8;
/// Width of the indexing progress bar in the status bar.
const STATUS_BAR_WIDTH: f32 = 70.0;

/// Which parts of the status bar indexing widget fit in `room` points:
/// `(text, bar)`.
fn status_widget_parts(room: f32, text_width: f32, spacing: f32) -> (bool, bool) {
    let bar = STATUS_BAR_WIDTH <= room;
    (bar && text_width + spacing + STATUS_BAR_WIDTH <= room, bar)
}

/// Which names to import an explicit Ctrl+Space adds to the lookup: `None`
/// none, `Some(false)` those of the library index and the standard library,
/// `Some(true)` also those of the other project modules. A deliberate second
/// press adds all of them. A first press that found nothing adds library names.
/// Import searches run in the background, including the project fallback.
pub(super) fn import_completion_scope(
    explicit: bool,
    second_press: bool,
    found_nothing: bool,
    prefix: &str,
) -> Option<bool> {
    if !explicit || (prefix.is_empty() && !second_press) {
        return None;
    }
    if second_press {
        Some(true)
    } else if found_nothing {
        Some(false)
    } else {
        None
    }
}

/// An explicit import search, accepted only while its editor snapshots match.
pub(super) struct ImportCompletionJob {
    path: PathBuf,
    version: u64,
    caret: TextSize,
    buffers: Vec<(PathBuf, u64)>,
    rx: crossbeam_channel::Receiver<Vec<rustpy_semantic::completion2::ImportCompletion>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    automatic: bool,
    source_analysis: Arc<Analysis>,
    source_epoch: u64,
    document: Option<rustpy_text::DocumentId>,
}

impl ImportCompletionJob {
    fn matches_editor(
        &self,
        path: Option<&Path>,
        version: u64,
        caret: TextSize,
        buffers: &[(PathBuf, u64)],
    ) -> bool {
        path == Some(self.path.as_path())
            && version == self.version
            && caret == self.caret
            && buffers == self.buffers
    }
}

impl Drop for ImportCompletionJob {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

// Explicit completion requests share one scan lane. Superseded queued work
// checks cancellation before touching the filesystem.
static IMPORT_SCAN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Builds the project fallback on a worker. Unsaved text replaces disk text;
/// excluded folders and attached content roots follow the project scope.
fn project_import_inputs(
    analysis: &Analysis,
    scope: Option<&rustpy_search::ProjectScope>,
    buffers: Vec<(PathBuf, u64, String)>,
) -> (Analysis, Vec<PathBuf>) {
    let resolver = rustpy_semantic::module::ModuleResolver::new(
        analysis.resolver.roots.clone(),
        analysis.resolver.search_paths.clone(),
        analysis.resolver.python_version,
    )
    .with_platform(&analysis.resolver.platform)
    .with_stub_roots(analysis.resolver.stub_roots.iter().cloned())
    .with_excluded(analysis.resolver.excluded_paths().iter().cloned());
    let snapshot = Analysis::new(resolver);
    snapshot.set_runtime_types((*analysis.runtime_types()).clone());
    let mut files = scope
        .map(rustpy_search::project_files_in)
        .unwrap_or_default();
    for (path, _, text) in buffers {
        if scope.is_none_or(|s| !s.contains(&path)) {
            continue;
        }
        snapshot.set_file_text(&path, &text);
        if !files.contains(&path) {
            files.push(path);
        }
    }
    (snapshot, files)
}

/// A button pressed on the Shared Indexes page (applied after drawing).
enum IndexAction {
    RefreshLibrary,
    RebuildLibrary,
    RefreshProject,
    RebuildProject,
    Delete(PathBuf),
    Export,
    Import,
}

impl IdeApp {
    /// Disk walks, parsing, and the lazy stdlib table all run off the UI thread.
    pub(super) fn start_import_completion(
        &mut self,
        analysis: Arc<Analysis>,
        file: Arc<rustpy_semantic::FileInfo>,
        caret: TextSize,
        with_project: bool,
        classes_only: bool,
    ) {
        self.start_import_completion_mode(analysis, file, caret, with_project, classes_only, false);
    }
    pub(super) fn start_import_completion_mode(
        &mut self,
        analysis: Arc<Analysis>,
        file: Arc<rustpy_semantic::FileInfo>,
        caret: TextSize,
        with_project: bool,
        classes_only: bool,
        automatic: bool,
    ) {
        let scope = with_project.then(|| self.project_scope()).flatten();
        let snapshot = self.everywhere.index.clone();
        let start = usize::from(self.completion.prefix_start);
        let prefix = file.text[start..usize::from(caret)].to_string();
        let buffers: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| t.state.doc.is_modified() && t.state.language == Language::Python)
            .filter_map(|t| {
                t.state
                    .path
                    .clone()
                    .filter(|p| scope.as_ref().is_some_and(|s| s.contains(p)))
                    .map(|p| (p, t.state.doc.version(), t.state.doc.text().to_string()))
            })
            .collect();
        let versions = self
            .tabs
            .iter()
            .filter(|t| t.state.language == Language::Python)
            .filter_map(|t| t.state.path.clone().map(|p| (p, t.state.doc.version())))
            .collect();
        let path = file.path.clone();
        let source_analysis = analysis.clone();
        let source_epoch = analysis.revision_excluding(&path);
        let document = self.active.map(|i| self.tabs[i].state.doc.identity());
        let version = self.completion.doc_version;
        let editor_caret = self
            .active
            .map(|i| self.tabs[i].state.primary().head())
            .unwrap_or(caret);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let _ = std::thread::Builder::new()
            .name("rustpy-import-completion".into())
            .stack_size(16 << 20)
            .spawn(move || {
                let _scan = IMPORT_SCAN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if worker_cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let names = if classes_only {
                    snapshot.import_class_names(&prefix, 200)
                } else {
                    snapshot.import_names(&prefix, 200)
                };
                // A truncated symbol index cannot establish uniqueness.
                if automatic && names.len() >= 200 {
                    return;
                }
                let (analysis, files) = project_import_inputs(&analysis, scope.as_ref(), buffers);
                if worker_cancelled.load(std::sync::atomic::Ordering::Relaxed)
                    || (automatic && files.len() > 4096)
                {
                    return;
                }
                let id = analysis.set_file_text(&file.path, &file.text);
                let Some(file) = analysis.file(id) else {
                    return;
                };
                let mut result = if classes_only {
                    rustpy_semantic::completion2::class_name_complete_with(
                        &analysis, &file, caret, &files, &names,
                    )
                } else {
                    rustpy_semantic::completion2::second_press_complete_with(
                        &analysis, &file, caret, &files, &names,
                    )
                };
                if classes_only {
                    result.extend(super::import_workflows::static_import_candidates(
                        &analysis, &file, caret, &files,
                    ));
                }
                let _ = tx.send(result);
            });
        self.import_completion = Some(ImportCompletionJob {
            path,
            version,
            caret: editor_caret,
            buffers: versions,
            automatic,
            source_analysis,
            source_epoch,
            document,
            rx,
            cancelled,
        });
        if !automatic {
            self.set_status("Searching import suggestions…");
        }
    }

    fn poll_import_completion(&mut self, ctx: &egui::Context) {
        let Some(job) = self.import_completion.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(crossbeam_channel::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
                return;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.import_completion = None;
                return;
            }
        };
        let job = self.import_completion.take().unwrap();
        let versions: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| t.state.language == Language::Python)
            .filter_map(|t| t.state.path.clone().map(|p| (p, t.state.doc.version())))
            .collect();
        let valid = self.active.and_then(|i| self.tabs.get(i)).is_some_and(|t| {
            job.matches_editor(
                t.state.path.as_deref(),
                t.state.doc.version(),
                t.state.primary().head(),
                &versions,
            )
        }) && self
            .active
            .is_some_and(|i| Some(self.tabs[i].state.doc.identity()) == job.document)
            && self
                .project
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(&p.analysis, &job.source_analysis))
            && job.source_analysis.revision_excluding(&job.path) == job.source_epoch
            && self.completion.doc_version == job.version
            && self.completion.caret == job.caret;
        if !valid {
            return;
        }
        if job.automatic {
            self.accept_unambiguous_import(result);
            return;
        }
        for ic in result.into_iter().filter(|ic| {
            !self
                .settings
                .editor
                .preferences
                .excluded_import(&format!("{}.{}", ic.module, ic.name))
        }) {
            self.completion.items.push(ic.item);
            self.completion.imports.push(Some((ic.module, ic.name)));
        }
        self.refresh_completion_order();
        self.completion.visible = !self.completion.items.is_empty();
        if std::env::var("RUSTPY_DEBUG_COMPLETION").is_ok() {
            log::info!(
                "debug-completion: imports received valid={valid} items={} visible={}",
                self.completion.items.len(),
                self.completion.visible
            );
        }
        self.set_status(if self.completion.visible {
            "Import suggestions ready"
        } else {
            "No suggestions"
        });
    }

    /// Every frame: follows the project and its interpreter, starts due
    /// refreshes, collects finished jobs and hands new indexes to consumers.
    pub(super) fn index_tick(&mut self, ctx: &egui::Context) {
        self.poll_shared_index_download(ctx);
        self.poll_basic_completion(ctx);
        self.poll_import_completion(ctx);
        if self.poll_completion_worker() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if let Some(p) = &self.project {
            self.symbol_indexes
                .set_project_excluded(p.tree.excluded.iter());
        }
        self.symbol_indexes.sync(
            self.project.as_ref().map(|p| p.root.as_path()),
            self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
        );
        self.symbol_indexes.maintain();
        crate::performance_trace::mode(self.settings.power_save_mode, self.symbol_indexes.busy());
        self.symbol_indexes.poll();
        // Cheap: the snapshot only shares what the index jobs computed.
        let snapshot = self.symbol_indexes.snapshot();
        crate::performance_trace::index(
            self.symbol_indexes.busy(),
            snapshot
                .project_files
                .as_ref()
                .map_or(0, |files| files.len()),
            snapshot.generation,
        );
        let current = &self.everywhere.index;
        if snapshot.generation != current.generation
            || snapshot.library_pending != current.library_pending
            || snapshot.project_pending != current.project_pending
        {
            self.everywhere.index = snapshot;
        }
        // Hierarchy queries take class bases from the persisted project index
        // (a no-op while the analysis already has this one).
        if let Some(p) = &self.project {
            p.analysis
                .set_project_index(self.everywhere.index.project.clone());
        }
        for m in std::mem::take(&mut self.symbol_indexes.messages) {
            self.set_status(m);
        }
        // "Run when indexing finishes".
        for pending in self.dumb_mode.take_ready(self.indexes_dumb()) {
            self.replay_after_indexing(pending, ctx);
        }
        if self.symbol_indexes.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Whether the indexes are being built (dumb mode).
    pub(super) fn indexes_dumb(&self) -> bool {
        #[cfg(test)]
        if let Some(forced) = self.dumb_mode.forced {
            return forced;
        }
        self.symbol_indexes.is_dumb()
    }

    /// Dumb mode: whether `action`, invoked at the active editor's carets,
    /// may run now (see [`Self::dumb_mode_admits_with`]).
    pub(super) fn dumb_mode_admits(&mut self, action: crate::actions::Action) -> bool {
        self.dumb_mode_admits_with(action, |app| match app.active {
            Some(index) => app.dumb_editor_target(index, None),
            None => crate::dumb_mode::Target::None,
        })
    }

    /// Dumb mode: whether `action` may run now. While the indexes are built,
    /// actions needing complete indexes are refused with a status-bar notice
    /// (see [`Self::index_status_widget`]) remembering where they were invoked
    /// (`target`, captured only then), and others run flagged as partial.
    pub(super) fn dumb_mode_admits_with(
        &mut self,
        action: crate::actions::Action,
        target: impl FnOnce(&Self) -> crate::dumb_mode::Target,
    ) -> bool {
        use crate::dumb_mode::{partial_text, policy, unavailable_text, DumbPolicy, Gate, Target};
        let dumb = self.indexes_dumb();
        let captured = (dumb && policy(action) == DumbPolicy::Wait).then(|| target(self));
        match self
            .dumb_mode
            .gate(action, dumb, move || captured.unwrap_or(Target::None))
        {
            Gate::Run => true,
            Gate::RunPartial => {
                self.set_status(partial_text(action));
                true
            }
            Gate::Blocked => {
                self.set_status(unavailable_text(action));
                false
            }
        }
    }

    /// The editor in tab `index` as a dumb-mode target: its carets, or a
    /// caret at `offset` (a gutter marker).
    pub(super) fn dumb_editor_target(
        &self,
        index: usize,
        offset: Option<rustpy_core::TextSize>,
    ) -> crate::dumb_mode::Target {
        let Some(tab) = self.tabs.get(index) else {
            return crate::dumb_mode::Target::None;
        };
        let doc = &tab.state.doc;
        let carets = match offset {
            Some(offset) => vec![crate::editor::Caret::at(offset)],
            None => tab.state.carets.clone(),
        };
        crate::dumb_mode::Target::Editor(crate::dumb_mode::EditorTarget::capture(
            doc.identity(),
            doc.version(),
            carets,
            &doc.text(),
        ))
    }

    /// Runs an action queued with "Run when indexing finishes" where it was
    /// invoked; skipped with a notice when that target is gone (the tab was
    /// closed, the file deleted, or edits moved or changed the target).
    pub(super) fn replay_after_indexing(
        &mut self,
        pending: crate::dumb_mode::Pending,
        ctx: &egui::Context,
    ) {
        use crate::dumb_mode::{skipped_text, Target};
        let action = pending.action;
        match pending.target {
            Target::None => self.handle_action(action, ctx),
            Target::Editor(target) => {
                let Some(index) = self.tabs.iter().position(|tab| {
                    let doc = &tab.state.doc;
                    doc.identity() == target.document
                        && target.still_valid(doc.identity(), doc.version(), &doc.text())
                }) else {
                    self.set_status(skipped_text(action));
                    return;
                };
                self.active = Some(index);
                self.group_active[self.tabs[index].group as usize] = Some(self.tabs[index].view.id);
                self.focus_editor = true;
                let state = &mut self.tabs[index].state;
                if let Some(primary) = target.carets.first() {
                    state.reveal_offset(primary.head());
                }
                state.set_carets(target.carets);
                self.handle_action(action, ctx);
            }
            Target::Path(path) => {
                if path.exists() {
                    self.project_path_action(path, action, ctx);
                } else {
                    self.set_status(skipped_text(action));
                }
            }
            Target::Paths(paths) => {
                let paths: Vec<_> = paths.into_iter().filter(|path| path.exists()).collect();
                if paths.is_empty() {
                    self.set_status(skipped_text(action));
                } else {
                    self.project_delete_many(paths);
                }
            }
        }
    }

    /// The dumb-mode notice: the refused action with "Run when indexing
    /// finishes", or what is queued to run then.
    fn dumb_mode_notice(&mut self, ui: &mut egui::Ui) {
        if !self.indexes_dumb() {
            return;
        }
        if let Some(action) = self.dumb_mode.notice() {
            if ui
                .small_button("Run when indexing finishes")
                .on_hover_text(crate::dumb_mode::unavailable_text(action))
                .clicked()
            {
                self.dumb_mode.queue_notice();
                self.set_status(format!(
                    "{} will run when indexing finishes",
                    action.text().trim_end_matches('…')
                ));
            }
        } else if self.dumb_mode.has_queued() {
            let names: Vec<&str> = self
                .dumb_mode
                .queued()
                .map(|action| action.text().trim_end_matches('…'))
                .collect();
            ui.label(
                egui::RichText::new(format!("Queued: {}", names.join(", ")))
                    .color(self.theme.text_dim),
            )
            .on_hover_text("Runs when indexing finishes");
        }
    }

    /// Status bar (right-to-left layout, added last): progress of a running
    /// index job; a click opens Settings › Tools › Shared Indexes. It only
    /// takes the room left between the status text on the left and the items
    /// on the right, dropping its text, then its bar, rather than drawing over
    /// the status text.
    pub(super) fn index_status_widget(&mut self, ui: &mut egui::Ui) {
        let Some(status) = self.symbol_indexes.status() else {
            return;
        };
        self.dumb_mode_notice(ui);
        let room = ui.available_width() - ui.spacing().item_spacing.x;
        let text_width = ui
            .painter()
            .layout_no_wrap(
                status.text.clone(),
                egui::TextStyle::Body.resolve(ui.style()),
                Color32::WHITE,
            )
            .size()
            .x;
        let (show_text, show_bar) =
            status_widget_parts(room, text_width, ui.spacing().item_spacing.x);
        let hover = format!(
            "{} — symbol indexes; click for Settings › Tools › Shared Indexes",
            status.text
        );
        let mut clicked = false;
        if show_text {
            clicked |= ui
                .add(
                    egui::Label::new(egui::RichText::new(&status.text).color(self.theme.text_dim))
                        .sense(Sense::click()),
                )
                .on_hover_text(&hover)
                .clicked();
        }
        if show_bar {
            let bar = egui::ProgressBar::new(status.fraction.unwrap_or(0.0))
                .desired_width(STATUS_BAR_WIDTH)
                .desired_height(6.0)
                .animate(status.fraction.is_none());
            clicked |= ui
                .add(bar)
                .interact(Sense::click())
                .on_hover_text(&hover)
                .clicked();
        }
        if clicked {
            self.settings_ui.page = settings_ui::SettingsPage::SharedIndexes;
            self.dialog = Dialog::Settings;
        }
    }

    /// Settings › Tools › Shared Indexes.
    pub(super) fn page_shared_indexes(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let now = rustpy_index::build::now_unix();
        let dim = |t: &str| egui::RichText::new(t).color(theme.text_dim);
        ui.label(dim("Go to Class and Go to Symbol with \"Include non-project items\", Search Everywhere and auto-import find the names of the interpreter's libraries in these indexes. They are kept on disk and validated against current file content on restart. Background refreshes reuse matching content when files change or packages are installed."));
        ui.add_space(8.0);
        let mut action: Option<IndexAction> = None;
        let library = self.symbol_indexes.library_view();
        index_section(
            ui,
            &theme,
            "Interpreter libraries",
            library.as_ref(),
            "No Python interpreter is selected for this project.",
            now,
            (IndexAction::RefreshLibrary, IndexAction::RebuildLibrary),
            &mut action,
        );
        ui.add_space(6.0);
        let project = self.symbol_indexes.project_view();
        index_section(
            ui,
            &theme,
            "Project",
            project.as_ref(),
            "No project is open.",
            now,
            (IndexAction::RefreshProject, IndexAction::RebuildProject),
            &mut action,
        );
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Shared index bundles").strong());
        ui.label(dim("A bundle records the symbols of library files by their content. Imported here (or placed in a shared-indexes folder next to the RustPy executable), it lets RustPy skip parsing every file whose content it contains: export the standard library where it is indexed, and a fresh install indexes it without parsing."));
        let version = library
            .as_ref()
            .and_then(|v| v.index.as_ref())
            .map(|i| i.meta().python_version.clone())
            .or_else(|| {
                self.symbol_indexes
                    .library_target()
                    .map(|t| t.version.clone())
            })
            .unwrap_or_default();
        let page = &mut self.symbol_indexes.page;
        let suggested = default_bundle_path(&version, page.include_packages)
            .display()
            .to_string();
        if page.export_path.is_empty() || page.export_path == page.export_default {
            page.export_path = suggested.clone();
        }
        page.export_default = suggested;
        let can_export = library.as_ref().is_some_and(|v| v.index.is_some());
        ui.horizontal(|ui| {
            ui.label("Export to:");
            ui.add(egui::TextEdit::singleline(&mut page.export_path).desired_width(360.0));
            ui.checkbox(&mut page.include_packages, "Include site-packages").on_hover_text("Also export the packages installed in site-packages, private ones included (only the standard library otherwise). Other sys.path entries, such as the sources of editable installs, are never exported.");
            if ui.add_enabled(can_export, egui::Button::new("Export")).on_disabled_hover_text("The interpreter libraries are not indexed yet").clicked() {
                action = Some(IndexAction::Export);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Import:");
            ui.add(
                egui::TextEdit::singleline(&mut page.import_path)
                    .desired_width(360.0)
                    .hint_text("path to a .rpidx bundle"),
            );
            if ui
                .add_enabled(
                    !page.import_path.trim().is_empty(),
                    egui::Button::new("Import"),
                )
                .clicked()
            {
                action = Some(IndexAction::Import);
            }
        });
        self.shared_index_download_ui(ui);
        match &self.symbol_indexes.last_message {
            Some(Ok(m)) => {
                ui.colored_label(theme.success, m);
            }
            Some(Err(m)) => {
                ui.colored_label(theme.error, m);
            }
            None => {}
        }
        let builtin = crate::symbol_index::builtin_bundle_dirs();
        if !builtin.is_empty() {
            ui.label(dim(&format!(
                "Built-in bundles: {}",
                builtin
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Stored indexes").strong());
        let dir = self.symbol_indexes.store().dir().to_path_buf();
        let listing = self.symbol_indexes.listing().to_vec();
        let total: u64 = listing.iter().filter(|s| !s.builtin).map(|s| s.size).sum();
        ui.label(dim(&format!(
            "{} · {} in {} file(s)",
            dir.display(),
            format_size(total),
            listing.iter().filter(|s| !s.builtin).count()
        )));
        if listing.is_empty() {
            ui.label(dim("No indexes are stored yet."));
        } else {
            egui::Grid::new("stored_indexes")
                .striped(true)
                .num_columns(8)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        "Kind", "Name", "Python", "Files", "Symbols", "Size", "Updated", "",
                    ] {
                        ui.label(egui::RichText::new(h).strong());
                    }
                    ui.end_row();
                    for s in &listing {
                        let kind = if s.builtin {
                            "Built-in bundle"
                        } else {
                            s.meta.kind.label()
                        };
                        ui.label(kind);
                        match &s.error {
                            Some(e) => {
                                ui.label(
                                    egui::RichText::new(
                                        s.path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_default(),
                                    )
                                    .color(theme.warning),
                                )
                                .on_hover_text(format!("{e}\n{}", s.path.display()));
                            }
                            None => {
                                ui.label(&s.meta.label).on_hover_text(format!(
                                    "{}\n{}",
                                    s.meta.owner,
                                    s.path.display()
                                ));
                            }
                        }
                        ui.label(&s.meta.python_version);
                        ui.label(group_digits(s.meta.files));
                        ui.label(group_digits(s.meta.symbols));
                        ui.label(format_size(s.size));
                        ui.label(if s.error.is_some() {
                            String::new()
                        } else {
                            format_age(s.meta.updated, now)
                        });
                        if s.builtin {
                            ui.label(dim("read-only"));
                        } else if ui
                            .small_button("Delete")
                            .on_hover_text("Delete this index file (it is rebuilt when needed)")
                            .clicked()
                        {
                            action = Some(IndexAction::Delete(s.path.clone()));
                        }
                        ui.end_row();
                    }
                });
        }
        let Some(action) = action else { return };
        let indexes = &mut self.symbol_indexes;
        match action {
            IndexAction::RefreshLibrary => indexes.refresh_library(std::time::Duration::ZERO),
            IndexAction::RebuildLibrary => indexes.rebuild_library(),
            IndexAction::RefreshProject => indexes.refresh_project(std::time::Duration::ZERO),
            IndexAction::RebuildProject => indexes.rebuild_project(),
            IndexAction::Delete(path) => match indexes.delete(&path) {
                Ok(()) => self.settings_ui.message = Some(format!("Deleted {}", path.display())),
                Err(e) => {
                    self.settings_ui.message =
                        Some(format!("Cannot delete {}: {e}", path.display()))
                }
            },
            IndexAction::Export => {
                let dest = PathBuf::from(indexes.page.export_path.trim());
                let packages = indexes.page.include_packages;
                indexes.export_bundle(dest, packages);
            }
            IndexAction::Import => {
                let src = PathBuf::from(indexes.page.import_path.trim());
                indexes.import_bundle(src);
            }
        }
    }
}

/// One of the two indexes in use: what it holds, the last refresh, a running
/// job, and Refresh / Rebuild / Delete.
#[allow(clippy::too_many_arguments)]
fn index_section(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    title: &str,
    view: Option<&IndexView>,
    missing: &str,
    now: u64,
    actions: (IndexAction, IndexAction),
    action: &mut Option<IndexAction>,
) {
    let dim = |t: String| egui::RichText::new(t).color(theme.text_dim);
    let Some(v) = view else {
        ui.label(egui::RichText::new(title).strong());
        ui.label(dim(missing.to_string()));
        return;
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
        ui.label(dim(v.label.clone()));
    });
    ui.indent(title, |ui| {
        match &v.index {
            Some(i) => {
                let m = i.meta();
                ui.label(format!("{} files · {} symbols · {} modules · updated {}", group_digits(m.files), group_digits(m.symbols), group_digits(i.module_count() as u64), format_age(m.updated, now)));
            }
            None if v.suppressed => {
                ui.label(egui::RichText::new("Deleted: not rebuilt until you choose Rebuild").color(theme.warning));
            }
            None if v.running.is_some() => {
                ui.label(dim("Not indexed yet".into()));
            }
            None => {
                ui.label(dim("No index".into()));
            }
        }
        if let Some((done, total)) = v.running {
            let what = if v.rebuilding { "Rebuilding" } else { "Refreshing" };
            let (text, fraction) = if total == 0 { (format!("{what}: listing files…"), 0.0) } else { (format!("{what}: {done}/{total} files"), done as f32 / total as f32) };
            ui.add(egui::ProgressBar::new(fraction).desired_width(320.0).text(text).animate(total == 0));
        } else if let Some(s) = &v.stats {
            ui.label(dim(format!("Last refresh: {}", s.summary())));
        }
        if let Some(e) = &v.error {
            if e != "cancelled" {
                ui.label(egui::RichText::new(format!("Error: {e}")).color(theme.error));
            }
        }
        ui.horizontal(|ui| {
            let (refresh, rebuild) = actions;
            let idle = v.running.is_none();
            if ui.add_enabled(idle && !v.suppressed, egui::Button::new("Refresh")).on_hover_text("Validate file content and refresh changed symbols").clicked() {
                *action = Some(refresh);
            }
            if ui.button("Rebuild").on_hover_text("Discard cached symbols and rebuild from current files (shared bundles are still used)").clicked() {
                *action = Some(rebuild);
            }
            if let Some(file) = v.file.as_ref().filter(|f| f.is_file()) {
                if ui.button("Delete").on_hover_text(file.display().to_string()).clicked() {
                    *action = Some(IndexAction::Delete(file.clone()));
                }
            }
        });
    });
}

/// `12,826`.
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Adds "Import 'from m import X'" quick fixes from the interpreter's
/// libraries to unresolved references, after the import fixes the
/// inspections found (project modules, common stdlib names).
pub(super) fn add_library_import_fixes(
    problems: &mut [Problem],
    file: &rustpy_semantic::db::FileInfo,
    index: &IndexSnapshot,
) {
    if index.library.is_none() {
        return;
    }
    for p in problems.iter_mut() {
        if p.diagnostic.code != "PyUnresolvedReferences" {
            continue;
        }
        let Some(name) = p
            .diagnostic
            .message
            .strip_prefix("Unresolved reference '")
            .and_then(|r| r.strip_suffix('\''))
        else {
            continue;
        };
        let mut at = p
            .fixes
            .iter()
            .rposition(|f| f.title.starts_with("Import "))
            .map_or(0, |i| i + 1);
        let mut count = p
            .fixes
            .iter()
            .filter(|f| f.title.starts_with("Import "))
            .count();
        for module in index.import_modules(name, MAX_IMPORT_FIXES) {
            if count >= MAX_IMPORT_FIXES {
                break;
            }
            if file.module_name.as_deref() == Some(module.as_str()) {
                continue;
            }
            let title = format!("Import 'from {module} import {name}'");
            if p.fixes.iter().any(|f| f.title == title) {
                continue;
            }
            let (range, text) = rustpy_semantic::completion2::import_edit_for(file, &module, name);
            if text.is_empty() {
                continue;
            }
            p.fixes.insert(
                at,
                QuickFix {
                    title,
                    edits: vec![(range, text)],
                },
            );
            at += 1;
            count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_index::IndexStore;

    fn dumb_app(text: &str) -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text(text, Language::Python));
        app.dumb_mode.forced = Some(true);
        app
    }

    fn head(app: &IdeApp) -> TextSize {
        app.tabs[0].state.primary().head()
    }

    fn status(app: &IdeApp) -> String {
        app.status.as_ref().map(|s| s.0.clone()).unwrap_or_default()
    }

    const SOURCE: &str = "class Base:\n    def run(self):\n        pass\n";

    #[test]
    fn gutter_implementations_are_gated_and_replayed_at_the_marker() {
        let mut app = dumb_app(SOURCE);
        let ctx = egui::Context::default();
        let marker = TextSize::new(SOURCE.find("run").unwrap() as u32);
        app.gutter_actions_intent(
            0,
            crate::editor::gutter_actions::Intent {
                offset: marker,
                kind: crate::editor::gutter_actions::Kind::Implementations,
            },
        );
        assert_eq!(app.dumb_mode.notice(), Some(Action::GotoImplementation));
        assert!(
            status(&app).contains("not available while indexing"),
            "{}",
            status(&app)
        );
        app.dumb_mode.queue_notice();
        // The caret moves on meanwhile; the replay goes back to the marker.
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        app.dumb_mode.forced = Some(false);
        app.index_tick(&ctx);
        assert!(!app.dumb_mode.has_queued());
        assert_eq!(head(&app), marker);
        assert!(!status(&app).contains("was not run"), "{}", status(&app));
    }

    #[test]
    fn editor_context_actions_queue_at_their_carets_and_skip_when_the_target_changed() {
        let mut app = dumb_app(SOURCE);
        let ctx = egui::Context::default();
        let caret = TextSize::new(SOURCE.find("Base").unwrap() as u32 + 1);
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(caret)]);
        app.handle_action(Action::FindUsages, &ctx);
        assert_eq!(app.dumb_mode.notice(), Some(Action::FindUsages));
        app.dumb_mode.queue_notice();
        // Text inserted before the target moves it: the queued action is skipped.
        let doc = &mut app.tabs[0].state.doc;
        doc.edit(
            vec![rustpy_text::Edit::insert(TextSize::new(0), "import os\n")],
            &[],
            &[],
            rustpy_text::EditGroup::Separate,
            None,
        )
        .unwrap();
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        app.dumb_mode.forced = Some(false);
        app.index_tick(&ctx);
        assert_eq!(
            status(&app),
            "Find Usages was not run: its target changed while indexing"
        );
        assert_eq!(head(&app), TextSize::new(0), "the caret is left alone");
    }

    #[test]
    fn code_vision_find_usages_waits_at_its_declaration() {
        let mut app = dumb_app(SOURCE);
        let ctx = egui::Context::default();
        let start = SOURCE.find("Base").unwrap() as u32;
        let target = crate::editor::code_vision_view::FindUsagesTarget {
            document: app.tabs[0].state.doc.identity(),
            version: app.tabs[0].state.doc.version(),
            path: "/p/main.py".into(),
            range: rustpy_core::TextRange::new(TextSize::new(start), TextSize::new(start + 4)),
            name: "Base".into(),
        };
        app.code_vision_editor_event(
            0,
            &crate::editor::view::ViewEvent::CodeVisionFindUsages(target),
        );
        assert_eq!(app.dumb_mode.notice(), Some(Action::FindUsages));
        app.dumb_mode.queue_notice();
        app.dumb_mode.forced = Some(false);
        app.index_tick(&ctx);
        assert!(!app.dumb_mode.has_queued());
        assert_eq!(
            head(&app),
            TextSize::new(start),
            "replayed at the declaration"
        );
    }

    #[test]
    fn project_tree_refactorings_are_gated_with_their_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.py");
        let mut app = dumb_app(SOURCE);
        let ctx = egui::Context::default();
        app.project_path_action(path.clone(), Action::FindUsages, &ctx);
        assert_eq!(app.dumb_mode.notice(), Some(Action::FindUsages));
        app.dumb_mode.queue_notice();
        // The file no longer exists when indexing finishes: skipped.
        app.dumb_mode.forced = Some(false);
        app.index_tick(&ctx);
        assert_eq!(
            status(&app),
            "Find Usages was not run: its target changed while indexing"
        );
    }

    #[test]
    fn the_status_widget_shrinks_to_the_room_left() {
        // Text and bar, the bar alone, nothing: never wider than the room.
        assert_eq!(status_widget_parts(400.0, 300.0, 14.0), (true, true));
        assert_eq!(status_widget_parts(300.0, 300.0, 14.0), (false, true));
        assert_eq!(status_widget_parts(60.0, 300.0, 14.0), (false, false));
        assert_eq!(status_widget_parts(-20.0, 10.0, 14.0), (false, false));
    }

    #[test]
    fn only_a_second_ctrl_space_parses_project_modules() {
        // A first press that found nothing: library and stdlib names at once.
        assert_eq!(
            import_completion_scope(true, false, true, "Fanc"),
            Some(false)
        );
        // A deliberate second press (the lookup open, or nothing found at the same caret): everything.
        assert_eq!(
            import_completion_scope(true, true, false, "Fanc"),
            Some(true)
        );
        assert_eq!(
            import_completion_scope(true, true, true, "Fanc"),
            Some(true)
        );
        // A first press with results, completion while typing, no prefix: nothing to import.
        assert_eq!(import_completion_scope(true, false, false, "Fanc"), None);
        assert_eq!(import_completion_scope(false, false, true, "Fanc"), None);
        assert_eq!(import_completion_scope(true, true, true, ""), Some(true));
        assert_eq!(import_completion_scope(true, false, true, ""), None);
    }

    #[test]
    fn import_results_require_the_same_editor_snapshots_and_cancel_on_drop() {
        let (_tx, rx) = crossbeam_channel::bounded(1);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let buffers = vec![
            (PathBuf::from("main.py"), 2),
            (PathBuf::from("models.py"), 4),
        ];
        let job = ImportCompletionJob {
            path: PathBuf::from("main.py"),
            version: 2,
            caret: TextSize::new(5),
            buffers: buffers.clone(),
            automatic: false,
            source_analysis: Arc::new(Analysis::for_project(Path::new("/test"))),
            source_epoch: 0,
            document: None,
            rx,
            cancelled: cancelled.clone(),
        };
        assert!(job.matches_editor(Some(Path::new("main.py")), 2, TextSize::new(5), &buffers));
        assert!(!job.matches_editor(Some(Path::new("other.py")), 2, TextSize::new(5), &buffers));
        assert!(!job.matches_editor(Some(Path::new("main.py")), 3, TextSize::new(5), &buffers));
        assert!(!job.matches_editor(Some(Path::new("main.py")), 2, TextSize::new(6), &buffers));
        let mut changed = buffers.clone();
        changed[1].1 += 1;
        assert!(!job.matches_editor(Some(Path::new("main.py")), 2, TextSize::new(5), &changed));
        assert!(!job.matches_editor(
            Some(Path::new("main.py")),
            2,
            TextSize::new(5),
            &buffers[..1]
        ));
        drop(job);
        assert!(cancelled.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn background_import_fallback_keeps_unsaved_definitions_and_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let changed = root.join("models.py");
        std::fs::write(&changed, "class OldModel:\n    pass\n").unwrap();
        std::fs::write(root.join("helpers.py"), "class FancyHelper:\n    pass\n").unwrap();
        let exported = root.join("public.py");
        std::fs::write(&exported, "from helpers import FancyHelper\n").unwrap();
        let excluded = root.join("generated");
        std::fs::create_dir(&excluded).unwrap();
        std::fs::write(
            excluded.join("model.py"),
            "class FancyExcluded:\n    pass\n",
        )
        .unwrap();
        let attached = tempfile::tempdir().unwrap();
        std::fs::write(
            attached.path().join("extra.py"),
            "class FancyAttached:\n    pass\n",
        )
        .unwrap();
        let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![root.to_path_buf(), attached.path().to_path_buf()],
            Vec::new(),
            (3, 12),
        ));
        let path = root.join("main.py");
        let id = db.set_file_text(&path, "Fancy");
        let file = db.file(id).unwrap();
        let scope = rustpy_search::ProjectScope::new(root)
            .with_excluded([excluded.clone()])
            .with_content_roots([attached.path().to_path_buf()]);
        let buffers = vec![
            (
                exported,
                1,
                "from helpers import FancyHelper\n# unsaved comment\n".into(),
            ),
            (changed.clone(), 1, "class FancyUnsaved:\n    pass\n".into()),
            (
                excluded.join("model.py"),
                1,
                "class FancySecret:\n    pass\n".into(),
            ),
        ];
        let (snapshot, files) = project_import_inputs(&db, Some(&scope), buffers);
        assert!(files.contains(&changed));
        let id = snapshot.set_file_text(&path, &file.text);
        let file = snapshot.file(id).unwrap();
        let found = rustpy_semantic::completion2::second_press_complete_with(
            &snapshot,
            &file,
            TextSize::new(5),
            &files,
            &[],
        );
        let project_names: Vec<_> = found
            .iter()
            .filter(|c| {
                c.module == "models" || c.module == "helpers" || c.module.starts_with("generated")
            })
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(project_names, vec!["FancyHelper", "FancyUnsaved"]);
        assert!(
            found
                .iter()
                .any(|c| c.module == "public" && c.name == "FancyHelper"),
            "unsaved imported classes remain importable"
        );
        assert!(
            found
                .iter()
                .any(|c| c.module == "extra" && c.name == "FancyAttached"),
            "attached content roots remain searchable"
        );
        assert_eq!(
            db.current_text(&changed).as_deref(),
            Some("class OldModel:\n    pass\n"),
            "worker snapshots never overwrite the shared analysis"
        );
        let (snapshot, files) = project_import_inputs(
            &db,
            None,
            vec![(changed.clone(), 1, "class FancyUnsaved:\n    pass\n".into())],
        );
        assert!(
            files.is_empty() && snapshot.cached_disk_files() == (0, 0),
            "first press searches libraries only"
        );
        assert_eq!(
            snapshot.current_text(&changed).as_deref(),
            Some("class OldModel:\n    pass\n"),
            "project overlay was not loaded"
        );
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(12_826), "12,826");
        assert_eq!(group_digits(1_442_984), "1,442,984");
    }

    #[test]
    fn unresolved_names_get_library_import_fixes() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(site.join("frames")).unwrap();
        std::fs::write(
            site.join("frames/__init__.py"),
            "from .core import DataFrame\n",
        )
        .unwrap();
        std::fs::write(site.join("frames/core.py"), "class DataFrame:\n    pass\n").unwrap();
        std::fs::create_dir_all(site.join("tables")).unwrap();
        std::fs::write(
            site.join("tables/__init__.py"),
            "class DataFrame:\n    pass\n",
        )
        .unwrap();
        let mut indexes =
            crate::symbol_index::SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        let interpreter = Interpreter {
            path: PathBuf::from("/fake/python3"),
            version: "3.12.3".into(),
            prefix: PathBuf::from("/fake"),
            sys_path: vec![site],
            is_venv: false,
            platform: "linux".into(),
            kind: "system".into(),
        };
        indexes.set_library_interpreter(Some(&interpreter));
        let start = std::time::Instant::now();
        while indexes.busy() && start.elapsed() < std::time::Duration::from_secs(30) {
            indexes.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        indexes.poll();
        let snapshot = indexes.snapshot();
        let db = Analysis::for_project(Path::new("/proj"));
        let path = PathBuf::from("/proj/main.py");
        let src = "import os\n\ndf = DataFrame()\nx = os.getcwd()\n";
        let id = db.set_file_text(&path, src);
        let file = db.file(id).unwrap();
        let mut problems = inspections::inspect(&db, &file, &InspectionOptions::default());
        let unresolved = problems
            .iter()
            .position(|p| p.diagnostic.message == "Unresolved reference 'DataFrame'")
            .expect("an unresolved reference");
        add_library_import_fixes(&mut problems, &file, &snapshot);
        let titles: Vec<&str> = problems[unresolved]
            .fixes
            .iter()
            .map(|f| f.title.as_str())
            .collect();
        let first_import = titles
            .iter()
            .position(|t| t.starts_with("Import "))
            .unwrap();
        assert_eq!(
            &titles[first_import..first_import + 2],
            &[
                "Import 'from frames import DataFrame'",
                "Import 'from tables import DataFrame'"
            ],
            "{titles:?}"
        );
        // Import fixes stay together before the others (Create class …), and the edit adds the import.
        let imports = titles.iter().filter(|t| t.starts_with("Import ")).count();
        assert!(
            titles[..imports].iter().all(|t| t.starts_with("Import ")),
            "{titles:?}"
        );
        assert!(
            imports < titles.len(),
            "the other fixes are kept: {titles:?}"
        );
        let fix = &problems[unresolved].fixes[first_import];
        assert_eq!(
            fix.edits,
            vec![(
                TextRange::empty(TextSize::new(10)),
                "from frames import DataFrame\n".to_string()
            )]
        );
        // Without a library index nothing is added.
        let mut again = inspections::inspect(&db, &file, &InspectionOptions::default());
        let before = again[unresolved].fixes.len();
        add_library_import_fixes(&mut again, &file, &IndexSnapshot::default());
        assert_eq!(again[unresolved].fixes.len(), before);
    }
}

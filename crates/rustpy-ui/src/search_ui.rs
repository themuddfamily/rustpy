//! Navigation popups: Find in Files, Go to File, Recent Files, Search
//! Everywhere (roadmap §2.7, §1.4).

use crate::a11y::DialogWindow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align2, Color32, Sense, Vec2};
use rustpy_core::{ProgressReporter, TaskHandle, TaskManager, TextRange, TextSize};
use rustpy_search::{
    FileIndex, ProjectScope, SearchControl, SearchMatch, SearchOptions, SearchResults,
    SearchSources, SearchSummary,
};

use crate::actions::{Action, Keymap};
use crate::symbol_index::{FoundSymbol, IndexSnapshot};
use crate::theme::UiTheme;
use rustpy_index::{RootKind, SymbolKind};

#[path = "search_ui/everywhere_features.rs"]
mod everywhere_features;
pub use everywhere_features::{Destination, ExtraItem};
#[path = "search_ui/file_features.rs"]
mod file_features;
pub use file_features::{FileFeatures, Grouping, SavedSearch};

/// File edits `(path, [(range, new text)])`, ranges in the editor's (or the
/// file's) current text.
pub type FileEdits = Vec<(PathBuf, Vec<(TextRange, String)>)>;

/// What a popup asks the application to do.
pub enum NavEvent {
    OpenFile {
        path: PathBuf,
        line: Option<usize>,
        column: Option<usize>,
    },
    RunAction(Action),
    SearchDestination(Destination),
    SearchMemory(rustpy_settings::navigation_search::NavigationSearch),
    Status(String),
    /// Files were modified on disk (replace in files).
    FilesChanged,
    /// Replace in Files: apply these edits to open editors (undoable) or to
    /// the files on disk.
    ApplyEdits {
        edits: FileEdits,
        label: String,
    },
    ShowFileSearch {
        search: Box<SavedSearch>,
        new_tab: bool,
    },
    /// Pin search results in the Find tool window.
    ShowInFindWindow {
        title: String,
        matches: Vec<SearchMatch>,
    },
}

/// Find in Files / Replace in Files state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SearchScope {
    #[default]
    Project,
    OpenFiles,
    /// The directory chosen in the Project view, else the current file's.
    CurrentDirectory,
    Tests,
    Module,
    Custom,
    Named,
    Changed,
    Libraries,
    Pattern,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::Project => "Project Files",
            SearchScope::OpenFiles => "Open Files",
            SearchScope::CurrentDirectory => "Directory",
            SearchScope::Tests => "Project Test Files",
            SearchScope::Module => "Module / content root",
            SearchScope::Custom => "Custom paths",
            SearchScope::Named => "Named scope",
            SearchScope::Changed => "Changed files",
            SearchScope::Libraries => "Library files",
            SearchScope::Pattern => "Custom scope pattern",
        }
    }
}

/// Pause after the last keystroke before a search starts.
const DEBOUNCE: f64 = 0.15;

enum JobMsg {
    Batch(Vec<SearchMatch>),
    Done(Result<SearchSummary, String>),
}

/// A search running as a background task (the status bar shows it, and
/// its cancel button stops it).
struct SearchJob {
    control: Arc<SearchControl>,
    rx: crossbeam_channel::Receiver<JobMsg>,
    task: TaskHandle<()>,
}

/// How often a running search's progress is passed on to its task.
const PROGRESS_POLL: std::time::Duration = std::time::Duration::from_millis(40);

#[derive(Default)]
pub struct FindInFiles {
    pub visible: bool,
    pub replace_mode: bool,
    pub scope: SearchScope,
    pub last_scope: SearchScope,
    /// Open editor paths (for the Open Files scope and unsaved text) and the current file's directory.
    pub open_files: Vec<PathBuf>,
    pub current_dir: Option<PathBuf>,
    /// Directory chosen with *Find in Files…* in the Project view (Directory scope).
    pub directory: Option<PathBuf>,
    /// Folders marked *Excluded*: never searched (unless picked as the directory).
    pub excluded: Vec<PathBuf>,
    /// The project's scope: its attached content roots are searched with the project.
    project_scope: Option<ProjectScope>,
    pub history: Vec<String>,
    pub features: FileFeatures,
    last_feature_key: u64,
    pub query: String,
    pub replacement: String,
    pub options: SearchOptions,
    pub results: SearchResults,
    pub last_query: String,
    pub last_options: SearchOptions,
    pub selected: usize,
    pub focus_query: bool,
    pub error: Option<String>,
    /// Occurrences unticked in the replace preview: `(path, line, start column)`.
    skipped: HashSet<(PathBuf, usize, usize)>,
    job: Option<SearchJob>,
    /// When the query or options last changed: the search starts after [`DEBOUNCE`].
    pending_since: Option<f64>,
    last_directory: Option<PathBuf>,
    /// Replacement text per result (replace preview), with the inputs it was made for.
    previews: Vec<String>,
    preview_key: Option<(String, String, SearchOptions, usize)>,
    context: Option<MatchContext>,
    /// The text field (query, file mask or replacement) that last had the
    /// keyboard: the toggles and scope buttons give it back after a click.
    keyboard_field: Option<egui::Id>,
    /// The query field was the last widget with the keyboard, or a result was
    /// clicked since: Up/Down/Enter go to the result list while nothing has it.
    list_keys: bool,
}

/// Code around the selected result: `(path, line, [(line, text)])`.
type MatchContext = (PathBuf, usize, Vec<(usize, String)>);

/// The keys pressed in one frame of the Find in Files popup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PopupKeys {
    up: bool,
    down: bool,
    enter: bool,
    escape: bool,
}

/// What a frame's keys ask the Find in Files popup to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopupKeyAction {
    Close,
    /// Open the result with this index (and close the popup).
    Open(usize),
}

/// A one-line field of the Find in Files popup. Enter does not end the input
/// (egui's single-line default drops the keyboard, so whatever is typed next
/// would go nowhere).
fn popup_field<'t>(text: &'t mut String, width: f32, hint: &str) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text)
        .desired_width(width)
        .hint_text(hint)
        .return_key(None)
}

impl FindInFiles {
    pub fn open(&mut self, replace: bool, seed: Option<String>) {
        self.visible = true;
        self.replace_mode = replace;
        self.focus_query = true;
        // The Directory scope follows the current file unless a folder was picked.
        self.directory = None;
        if let Some(s) = seed {
            if !s.is_empty() && !s.contains('\n') {
                self.query = s;
            }
        }
        self.last_query.clear();
    }

    /// *Find in Files…* / *Replace in Files…* on a folder of the Project view.
    pub fn open_in_directory(&mut self, dir: PathBuf, replace: bool, seed: Option<String>) {
        self.open(replace, seed);
        self.directory = Some(dir);
        self.scope = SearchScope::CurrentDirectory;
    }

    /// The project's scope (content roots, excluded folders), set by the
    /// application every frame.
    pub fn set_project_scope(&mut self, scope: Option<ProjectScope>) {
        self.project_scope = scope;
    }

    /// What a search of the current scope walks besides its root: the
    /// project's content roots (Project and Test files), and the excluded
    /// folders it skips. A directory picked inside an excluded folder is
    /// searched as picked.
    fn walk_extent(&self, search_root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut excluded = self.excluded.clone();
        let mut content_roots = Vec::new();
        match self.scope {
            SearchScope::Project
            | SearchScope::Tests
            | SearchScope::Named
            | SearchScope::Pattern => {
                if let Some(scope) = self
                    .project_scope
                    .as_ref()
                    .filter(|s| s.root == search_root)
                {
                    content_roots = scope.content_roots.clone();
                    excluded.extend(
                        scope
                            .excluded
                            .iter()
                            .filter(|x| !self.excluded.contains(x))
                            .cloned(),
                    );
                }
            }
            SearchScope::CurrentDirectory => excluded.retain(|x| !search_root.starts_with(x)),
            SearchScope::OpenFiles
            | SearchScope::Module
            | SearchScope::Custom
            | SearchScope::Changed
            | SearchScope::Libraries => {}
        }
        (content_roots, excluded)
    }

    /// True while a search is waiting for the debounce or still running.
    pub fn is_searching(&self) -> bool {
        self.pending_since.is_some() || self.job.is_some()
    }

    /// Escape closes the popup wherever its keyboard is. Up/Down move through
    /// the results and Enter opens the selected one while `list_keys` (the
    /// keyboard is in the query field, or nowhere after a click on a result).
    fn on_keys(&mut self, keys: PopupKeys, list_keys: bool) -> Option<PopupKeyAction> {
        if keys.escape {
            return Some(PopupKeyAction::Close);
        }
        if !list_keys {
            return None;
        }
        let n = self.results.matches.len();
        if keys.down && self.selected + 1 < n {
            self.selected += 1;
        }
        if keys.up && self.selected > 0 {
            self.selected -= 1;
        }
        (keys.enter && self.selected < n).then_some(PopupKeyAction::Open(self.selected))
    }

    fn search_root(&self, root: &Path) -> PathBuf {
        match self.scope {
            SearchScope::CurrentDirectory => self
                .directory
                .clone()
                .or_else(|| self.current_dir.clone())
                .unwrap_or_else(|| root.to_path_buf()),
            SearchScope::Module => self
                .project_scope
                .as_ref()
                .and_then(|scope| {
                    scope
                        .walk_roots()
                        .into_iter()
                        .filter(|path| {
                            self.current_dir
                                .as_ref()
                                .is_some_and(|directory| directory.starts_with(path))
                        })
                        .max_by_key(|path| path.components().count())
                })
                .unwrap_or_else(|| root.to_path_buf()),
            _ => root.to_path_buf(),
        }
    }

    fn cancel_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.control.cancel();
            job.task.cancel();
        }
    }

    /// Starts a background search with the open editors' text in place of the disk.
    fn start(&mut self, root: &Path, buffer_text: &dyn Fn(&Path) -> Option<String>) {
        self.cancel_job();
        self.error = None;
        self.results = SearchResults::default();
        self.selected = 0;
        self.skipped.clear();
        self.context = None;
        // This run captures all query inputs, including explicit starts outside
        // the debounce path. The next frame must not restart identical work.
        self.last_query = self.query.clone();
        self.last_options = self.options.clone();
        self.last_scope = self.scope;
        self.last_directory = self.directory.clone();
        self.last_feature_key = self.feature_key();
        if self.query.is_empty() {
            return;
        }
        if !self.query.trim().is_empty() {
            let q = self.query.clone();
            self.history.retain(|h| h != &q);
            self.history.insert(0, q);
            self.history.truncate(15);
        }
        if !self.options.file_mask.trim().is_empty() {
            self.features
                .masks
                .retain(|mask| mask != &self.options.file_mask);
            self.features
                .masks
                .insert(0, self.options.file_mask.clone());
            self.features.masks.truncate(20);
        }
        let mut options = self.options.clone();
        if self.scope == SearchScope::Tests && options.file_mask.trim().is_empty() {
            options.file_mask = "test_*.py, *_test.py, tests.py".into();
        }
        if let Err(e) = rustpy_search::build_regex(&self.query, &options) {
            self.error = Some(e);
            return;
        }
        let overrides: HashMap<PathBuf, String> = self
            .open_files
            .iter()
            .filter_map(|p| buffer_text(p).map(|t| (p.clone(), t)))
            .collect();
        let only = self.extra_only(root);
        let search_root = self.search_root(root);
        let (mut content_roots, excluded) = self.walk_extent(&search_root);
        let scope_filter = match self.scope {
            SearchScope::Named => self
                .features
                .scope_filters
                .get(&self.features.named_scope)
                .cloned(),
            SearchScope::Pattern => self.features.custom_filter.clone(),
            _ => None,
        };
        if matches!(self.scope, SearchScope::Named | SearchScope::Pattern) {
            if let Some(error) = &self.features.scope_error {
                self.error = Some(error.clone());
                return;
            }
        }
        if let Some(filter) = &scope_filter {
            content_roots.extend(self.features.library_roots.iter().cloned());
            content_roots.extend(filter.context.scratch_roots.iter().cloned());
        }

        let sources = SearchSources {
            ignored_patterns: self
                .project_scope
                .as_ref()
                .map(|scope| scope.ignored_patterns.clone())
                .unwrap_or_default(),
            overrides,
            excluded,
            only,
            content_roots,
            scope_filter,
        };
        let control = Arc::new(SearchControl::default());
        let (tx, rx) = crossbeam_channel::unbounded();
        let query = self.query.clone();
        let thread_control = control.clone();
        let title = format!(
            "Find in Files: '{}'",
            crate::progress_ui::ellipsize(&query, 40)
        );
        // No exclusive task key: `start` cancels this popup's previous search itself.
        let task = TaskManager::global().task(title).spawn(move |progress| {
            progress.set_text(format!("Searching for '{query}'"));
            let batch_tx = tx.clone();
            let sink = move |batch: Vec<SearchMatch>| {
                let _ = batch_tx.send(JobMsg::Batch(batch));
            };
            // The search runs on a scoped thread; this one passes its progress
            // on to the task and a cancel from the status bar on to the search.
            let r = std::thread::scope(|scope| {
                let search = scope.spawn(|| {
                    rustpy_search::find_in_files_with(
                        &search_root,
                        &query,
                        &options,
                        &sources,
                        &thread_control,
                        &sink,
                    )
                });
                while !search.is_finished() {
                    if progress.is_cancelled() {
                        thread_control.cancel();
                    }
                    progress.set_step(
                        thread_control
                            .files_done
                            .load(std::sync::atomic::Ordering::Relaxed),
                        thread_control
                            .files_total
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                    std::thread::sleep(PROGRESS_POLL);
                }
                search
                    .join()
                    .unwrap_or_else(|_| Err("the search stopped unexpectedly".into()))
            });
            let _ = tx.send(JobMsg::Done(r));
            Ok(())
        });
        self.job = Some(SearchJob { control, rx, task });
    }

    /// Takes the results streamed so far; true when something arrived.
    fn poll(&mut self) -> bool {
        let Some(job) = &self.job else { return false };
        let mut got = false;
        let mut done = None;
        while let Ok(msg) = job.rx.try_recv() {
            got = true;
            match msg {
                JobMsg::Batch(b) => self.results.matches.extend(b),
                JobMsg::Done(r) => done = Some(r),
            }
        }
        if got {
            rustpy_search::sort_matches(&mut self.results.matches);
        }
        self.results.files_searched = job
            .control
            .files_done
            .load(std::sync::atomic::Ordering::Relaxed);
        if let Some(r) = done {
            match r {
                Ok(s) => {
                    self.results.files_searched = s.files_searched;
                    self.results.truncated = s.truncated;
                }
                Err(e) => self.error = Some(e),
            }
            self.job = None;
        }
        got
    }

    /// Edits for *Replace All*: every result that is still ticked, computed
    /// against the editor text (or the file on disk) as it is now.
    pub fn replace_edits(&self, buffer_text: &dyn Fn(&Path) -> Option<String>) -> FileEdits {
        let mut keep: BTreeMap<PathBuf, HashSet<(usize, usize)>> = BTreeMap::new();
        for m in &self.results.matches {
            if !self.skipped.contains(&(m.path.clone(), m.line, m.start)) {
                keep.entry(m.path.clone())
                    .or_default()
                    .insert((m.line, m.start));
            }
        }
        let mut options = self.options.clone();
        if self.scope == SearchScope::Tests && options.file_mask.trim().is_empty() {
            options.file_mask = "test_*.py, *_test.py, tests.py".into();
        }
        let mut out = Vec::new();
        for (path, wanted) in keep {
            let Some(text) = buffer_text(&path)
                .or_else(|| rustpy_search::file_search::read_text(&path, options.max_file_bytes))
            else {
                continue;
            };
            let Ok(edits) = rustpy_search::replace_edits(
                &text,
                &self.query,
                &self.replacement,
                &options,
                &|line, col| wanted.contains(&(line, col)),
            ) else {
                continue;
            };
            if !edits.is_empty() {
                out.push((
                    path,
                    edits
                        .into_iter()
                        .map(|(r, s)| {
                            (
                                TextRange::new(
                                    TextSize::new(r.start as u32),
                                    TextSize::new(r.end as u32),
                                ),
                                s,
                            )
                        })
                        .collect(),
                ));
            }
        }
        out
    }

    /// Unticks (or ticks again) one occurrence in the replace preview.
    pub fn set_skipped(&mut self, path: &Path, line: usize, start: usize, skipped: bool) {
        let key = (path.to_path_buf(), line, start);
        if skipped {
            self.skipped.insert(key);
        } else {
            self.skipped.remove(&key);
        }
    }

    fn included_count(&self) -> usize {
        self.results
            .matches
            .iter()
            .filter(|m| !self.skipped.contains(&(m.path.clone(), m.line, m.start)))
            .count()
    }

    fn refresh_previews(&mut self) {
        let key = (
            self.query.clone(),
            self.replacement.clone(),
            self.options.clone(),
            self.results.matches.len(),
        );
        if self.preview_key.as_ref() == Some(&key) {
            return;
        }
        self.previews = match rustpy_search::build_regex(&self.query, &self.options) {
            Ok(re) => self
                .results
                .matches
                .iter()
                .map(|m| {
                    rustpy_search::replacement_text(
                        &re,
                        &m.line_text,
                        m.start,
                        &self.replacement,
                        self.options.regex,
                    )
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        self.preview_key = Some(key);
    }

    fn refresh_context(&mut self, buffer_text: &dyn Fn(&Path) -> Option<String>) {
        let Some(m) = self.results.matches.get(self.selected) else {
            self.context = None;
            return;
        };
        if self
            .context
            .as_ref()
            .is_some_and(|(p, l, _)| *p == m.path && *l == m.line)
        {
            return;
        }
        let text = buffer_text(&m.path)
            .or_else(|| std::fs::read_to_string(&m.path).ok())
            .unwrap_or_default();
        let first = m.line.saturating_sub(2);
        let lines: Vec<(usize, String)> = text
            .lines()
            .enumerate()
            .skip(first)
            .take(5)
            .map(|(i, l)| (i, l.to_string()))
            .collect();
        self.context = Some((m.path.clone(), m.line, lines));
    }

    /// Draws the popup. `buffer_text` returns an open editor's text (unsaved
    /// changes included) for a path, `None` for files that are not open.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: &Path,
        buffer_text: &dyn Fn(&Path) -> Option<String>,
    ) -> Vec<NavEvent> {
        let mut events = Vec::new();
        if !self.visible {
            self.cancel_job();
            self.pending_since = None;
            return events;
        }
        let now = ctx.input(|i| i.time);
        self.poll();
        let mut open = true;
        let title = if self.replace_mode {
            "Replace in Files"
        } else {
            "Find in Files"
        };
        let mut close = false;
        let mut pin = false;
        let mut replace_all = false;
        let mut replace_selected = false;
        let mut open_match: Option<usize> = None;
        let mut toggles: Vec<(PathBuf, usize, usize, bool)> = Vec::new();
        if self.replace_mode {
            self.refresh_previews();
        }
        self.refresh_context(buffer_text);
        // Escape closes an open "Recent" menu first, not the popup.
        let menu_open = egui::Popup::is_any_open(ctx);
        // A click on a toggle or a scope gives the keyboard back to the text field.
        let mut refocus = false;
        let mut query_id = None;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_size([860.0, 560.0])
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 60.0))
            .open(&mut open)
            .show_modeless(ctx, |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(popup_field(&mut self.query, 420.0, "Search for…"));
                    query_id = Some(resp.id);
                    if self.focus_query {
                        // The sizing pass may already have focused this field.
                        // Do not overwrite a real End/cursor event on the next
                        // visible pass by selecting the entire query again.
                        if !resp.has_focus() {
                            resp.request_focus();
                            crate::theme::select_all_text(ui, &resp, &self.query);
                        }
                        self.focus_query = !ui.is_visible();
                    }
                    if resp.has_focus() {
                        self.keyboard_field = Some(resp.id);
                    }
                    // The result list takes Up/Down/Enter from the query field, and
                    // while nothing has the keyboard after the query or a click on a
                    // result (not after another field or window took it).
                    let focus = ui.memory(|m| m.focused());
                    if let Some(id) = focus {
                        self.list_keys = id == resp.id;
                    }
                    let list_keys = resp.has_focus() || (focus.is_none() && self.list_keys);
                    let keys = ui.input(|i| PopupKeys {
                        up: i.key_pressed(egui::Key::ArrowUp),
                        down: i.key_pressed(egui::Key::ArrowDown),
                        enter: i.key_pressed(egui::Key::Enter),
                        escape: i.key_pressed(egui::Key::Escape) && !menu_open,
                    });
                    match self.on_keys(keys, list_keys) {
                        Some(PopupKeyAction::Close) => close = true,
                        Some(PopupKeyAction::Open(i)) => open_match = Some(i),
                        None => {}
                    }
                    ui.menu_button("Recent", |ui| {
                        ui.label(
                            egui::RichText::new("Recent searches")
                                .color(theme.text_dim)
                                .small(),
                        );
                        if self.history.is_empty() {
                            ui.label(egui::RichText::new("(none)").color(theme.text_dim));
                        }
                        let hist = self.history.clone();
                        for h in hist {
                            if ui.button(&h).clicked() {
                                self.query = h;
                                refocus = true;
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Search history");
                    refocus |= ui
                        .toggle_value(&mut self.options.case_sensitive, "Cc")
                        .on_hover_text("Match case")
                        .clicked();
                    refocus |= ui
                        .toggle_value(&mut self.options.whole_words, "W")
                        .on_hover_text("Words")
                        .clicked();
                    refocus |= ui
                        .toggle_value(&mut self.options.regex, ".*")
                        .on_hover_text("Regex")
                        .clicked();
                    ui.label(egui::RichText::new("File mask:").color(theme.text_dim));
                    let mask = ui.add(popup_field(&mut self.options.file_mask, 120.0, "*.py"));
                    if mask.has_focus() {
                        self.keyboard_field = Some(mask.id);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Scope:").color(theme.text_dim));
                    for s in [
                        SearchScope::Project,
                        SearchScope::OpenFiles,
                        SearchScope::CurrentDirectory,
                        SearchScope::Tests,
                    ] {
                        if ui.selectable_label(self.scope == s, s.label()).clicked() {
                            self.scope = s;
                            refocus = true;
                        }
                    }
                    if self.scope == SearchScope::CurrentDirectory {
                        let dir = self.search_root(root);
                        let shown = dir
                            .strip_prefix(root)
                            .ok()
                            .map(|r| {
                                if r.as_os_str().is_empty() {
                                    ".".to_string()
                                } else {
                                    r.display().to_string()
                                }
                            })
                            .unwrap_or_else(|| dir.display().to_string());
                        ui.label(egui::RichText::new(shown).monospace().color(theme.text));
                    }
                });
                self.extended_controls(ui);
                if self.replace_mode {
                    ui.horizontal(|ui| {
                        let resp =
                            ui.add(popup_field(&mut self.replacement, 420.0, "Replace with…"));
                        if resp.has_focus() {
                            self.keyboard_field = Some(resp.id);
                        }
                        let n = self.included_count();
                        if ui
                            .add_enabled(
                                self.selected < self.results.matches.len() && !self.is_searching(),
                                egui::Button::new("Replace selected"),
                            )
                            .clicked()
                        {
                            replace_selected = true;
                        }
                        let ready = n > 0 && self.job.is_none() && self.pending_since.is_none();
                        if ui
                            .add_enabled(ready, egui::Button::new(format!("Replace All ({n})")))
                            .on_hover_text(
                                "Replaces the ticked occurrences; open editors can undo it",
                            )
                            .clicked()
                        {
                            replace_all = true;
                            refocus = true;
                        }
                    });
                }
                if let Some(e) = &self.error {
                    ui.label(
                        egui::RichText::new(format!("Invalid pattern: {e}")).color(theme.error),
                    );
                }
                ui.horizontal(|ui| {
                    let n = self.results.matches.len();
                    let summary = match &self.job {
                        Some(job) => format!(
                            "Searching… {}/{} files · {n} matches so far",
                            job.control
                                .files_done
                                .load(std::sync::atomic::Ordering::Relaxed),
                            job.control
                                .files_total
                                .load(std::sync::atomic::Ordering::Relaxed)
                        ),
                        None if self.pending_since.is_some() => "Searching…".to_string(),
                        None if self.results.truncated => format!(
                            "{n}+ matches in {} files (truncated)",
                            self.results.files_searched
                        ),
                        None => format!("{n} matches in {} files", self.results.files_searched),
                    };
                    ui.label(egui::RichText::new(summary).color(theme.text_dim).small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(n > 0, egui::Button::new("Open in Find Window"))
                            .on_hover_text("Keep these results in the Find tool window")
                            .clicked()
                        {
                            pin = true;
                        }
                    });
                });
                ui.separator();
                let mono = egui::FontId::monospace(12.5);
                let context = self.context.clone();
                // The code preview sits at the bottom and the result list takes the rest
                // (laid out bottom-up so the window keeps its size).
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if let Some((path, line, lines)) = &context {
                        for (i, l) in lines.iter().rev() {
                            let color = if i == line {
                                theme.text
                            } else {
                                theme.text_dim
                            };
                            ui.label(
                                egui::RichText::new(format!("{:>5}  {l}", i + 1))
                                    .font(mono.clone())
                                    .color(color),
                            );
                        }
                        let rel = path
                            .strip_prefix(root)
                            .unwrap_or(path)
                            .display()
                            .to_string();
                        ui.label(
                            egui::RichText::new(format!(
                                "{rel}:{}  (double-click or Enter to open)",
                                line + 1
                            ))
                            .small()
                            .color(theme.text_dim),
                        );
                        ui.separator();
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("find_in_files_rows")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut last_file: Option<&Path> = None;
                                for (idx, m) in self.results.matches.iter().enumerate() {
                                    if last_file != Some(m.path.as_path()) {
                                        let rel = m
                                            .path
                                            .strip_prefix(root)
                                            .unwrap_or(&m.path)
                                            .display()
                                            .to_string();
                                        ui.horizontal(|ui| {
                                            if self.replace_mode {
                                                let all_on =
                                                    !self.results.matches.iter().any(|o| {
                                                        o.path == m.path
                                                            && self.skipped.contains(&(
                                                                o.path.clone(),
                                                                o.line,
                                                                o.start,
                                                            ))
                                                    });
                                                let mut on = all_on;
                                                let checkbox = ui
                                                    .checkbox(&mut on, "")
                                                    .on_hover_text("Replace in this file");
                                                checkbox.ctx.accesskit_node_builder(
                                                    checkbox.id,
                                                    |node| {
                                                        node.set_label(format!(
                                                            "Include replacements in {rel}"
                                                        ))
                                                    },
                                                );
                                                if checkbox.changed() {
                                                    for o in self
                                                        .results
                                                        .matches
                                                        .iter()
                                                        .filter(|o| o.path == m.path)
                                                    {
                                                        toggles.push((
                                                            o.path.clone(),
                                                            o.line,
                                                            o.start,
                                                            !on,
                                                        ));
                                                    }
                                                }
                                            }
                                            ui.label(
                                                egui::RichText::new(rel).strong().color(theme.text),
                                            );
                                        });
                                        last_file = Some(m.path.as_path());
                                    }
                                    let selected = idx == self.selected;
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(
                                            ui.available_width(),
                                            20.0 * m.line_text.lines().count().clamp(1, 32) as f32,
                                        ),
                                        Sense::click(),
                                    );
                                    resp.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::SelectableLabel,
                                            true,
                                            selected,
                                            find_match_label(m, root),
                                        )
                                    });
                                    resp.ctx.accesskit_node_builder(resp.id, |node| {
                                        node.clear_toggled();
                                        node.set_selected(selected);
                                    });
                                    if selected {
                                        ui.painter().rect_filled(
                                            rect,
                                            2.0,
                                            theme.selection_inactive_bg,
                                        );
                                        if ui.input(|i| {
                                            i.key_pressed(egui::Key::ArrowDown)
                                                || i.key_pressed(egui::Key::ArrowUp)
                                        }) {
                                            resp.scroll_to_me(None);
                                        }
                                    } else if resp.hovered() {
                                        ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
                                    }
                                    let mut x = rect.left() + 18.0;
                                    let y = rect.center().y;
                                    let skipped =
                                        self.skipped.contains(&(m.path.clone(), m.line, m.start));
                                    if self.replace_mode {
                                        let mut on = !skipped;
                                        let cb = ui.put(
                                            egui::Rect::from_center_size(
                                                egui::Pos2::new(x + 6.0, y),
                                                Vec2::splat(18.0),
                                            ),
                                            egui::Checkbox::without_text(&mut on),
                                        );
                                        cb.ctx.accesskit_node_builder(cb.id, |node| {
                                            node.set_label(format!(
                                                "Include replacement at {}",
                                                find_match_label(m, root)
                                            ))
                                        });
                                        if cb.changed() {
                                            toggles.push((m.path.clone(), m.line, m.start, !on));
                                        }
                                        x += 20.0;
                                    }
                                    let line_no = format!("{:>5}  ", m.line.saturating_add(1));
                                    let g = ui.fonts_mut(|f| {
                                        f.layout_no_wrap(
                                            line_no.clone(),
                                            mono.clone(),
                                            theme.text_dim,
                                        )
                                    });
                                    ui.painter().galley(
                                        egui::Pos2::new(x, y - g.size().y / 2.0),
                                        g.clone(),
                                        theme.text_dim,
                                    );
                                    x += g.size().x;
                                    let text = m.line_text.trim_start();
                                    let trimmed = m.line_text.len() - text.len();
                                    let (s, e) = (
                                        m.start.saturating_sub(trimmed).min(text.len()),
                                        m.end.saturating_sub(trimmed).min(text.len()),
                                    );
                                    let (s, e) =
                                        if text.is_char_boundary(s) && text.is_char_boundary(e) {
                                            (s, e)
                                        } else {
                                            (0, 0)
                                        };
                                    let found = if theme.dark {
                                        Color32::from_rgb(0x32, 0x59, 0x3D)
                                    } else {
                                        Color32::from_rgb(0xE5, 0xF1, 0xE5)
                                    };
                                    let removed = if theme.dark {
                                        Color32::from_rgb(0x65, 0x3A, 0x3A)
                                    } else {
                                        Color32::from_rgb(0xF5, 0xD6, 0xD6)
                                    };
                                    let replacement = self
                                        .previews
                                        .get(idx)
                                        .filter(|_| self.replace_mode && !skipped);
                                    let mut parts: Vec<(String, Option<Color32>, bool)> = vec![
                                        (text[..s].to_string(), None, false),
                                        (
                                            text[s..e].to_string(),
                                            Some(if replacement.is_some() {
                                                removed
                                            } else {
                                                found
                                            }),
                                            replacement.is_some(),
                                        ),
                                    ];
                                    if let Some(r) = replacement {
                                        parts.push((r.clone(), Some(found), false));
                                    }
                                    parts.push((text[e..].to_string(), None, false));
                                    for (piece, bg, strike) in parts {
                                        if piece.is_empty() {
                                            continue;
                                        }
                                        let g = ui.fonts_mut(|f| {
                                            f.layout_no_wrap(piece, mono.clone(), theme.text)
                                        });
                                        let r = egui::Rect::from_min_size(
                                            egui::Pos2::new(x, rect.top() + 2.0),
                                            Vec2::new(g.size().x, 16.0),
                                        );
                                        if let Some(bg) = bg {
                                            ui.painter().rect_filled(r, 2.0, bg);
                                        }
                                        ui.painter().galley(
                                            egui::Pos2::new(x, y - g.size().y / 2.0),
                                            g.clone(),
                                            theme.text,
                                        );
                                        if strike {
                                            ui.painter().line_segment(
                                                [
                                                    egui::Pos2::new(r.left(), y),
                                                    egui::Pos2::new(r.right(), y),
                                                ],
                                                egui::Stroke::new(1.0, theme.text),
                                            );
                                        }
                                        x += g.size().x;
                                    }
                                    if resp.clicked() {
                                        self.selected = idx;
                                        // Up/Down/Enter go on from the clicked result.
                                        self.list_keys = true;
                                    }
                                    if resp.double_clicked() {
                                        open_match = Some(idx);
                                    }
                                }
                            });
                    });
                });
            });
        for (path, line, start, skip) in toggles {
            self.set_skipped(&path, line, start, skip);
        }
        if let Some(id) = self
            .keyboard_field
            .or(query_id)
            .filter(|_| refocus && !close)
        {
            // The popup's toggles do not keep the keyboard.
            ctx.memory_mut(|m| m.request_focus(id));
        }
        if let Some(idx) = open_match {
            if let Some(m) = self.results.matches.get(idx) {
                events.push(NavEvent::OpenFile {
                    path: m.path.clone(),
                    line: Some(m.line),
                    column: Some(m.start),
                });
                close = true;
            }
        }
        if pin {
            let scope = if self.scope == SearchScope::CurrentDirectory {
                format!(
                    "{} {}",
                    self.scope.label(),
                    self.search_root(root)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )
            } else {
                self.scope.label().to_string()
            };
            let title = format!(
                "Occurrences of '{}' in {scope} ({})",
                self.query,
                self.results.matches.len()
            );
            events.push(NavEvent::ShowFileSearch {
                search: Box::new(self.snapshot(title)),
                new_tab: self.features.new_tab,
            });
            close = true;
        }
        if replace_all || replace_selected {
            let edits = if replace_selected {
                Self::saved_edits(
                    &self.snapshot(self.query.clone()),
                    Some(self.selected),
                    buffer_text,
                )
            } else {
                self.replace_edits(buffer_text)
            };
            if !edits.is_empty() {
                events.push(NavEvent::ApplyEdits {
                    edits,
                    label: "Replace in Files".into(),
                });
            }
            // Search again so the list shows what is left.
            self.last_query.clear();
        }
        // Query, options or scope changed: (re)start the search after a short pause.
        if self.query != self.last_query
            || self.options != self.last_options
            || self.scope != self.last_scope
            || self.directory != self.last_directory
            || self.feature_key() != self.last_feature_key
        {
            self.last_feature_key = self.feature_key();
            self.last_query = self.query.clone();
            self.last_options = self.options.clone();
            self.last_scope = self.scope;
            self.last_directory = self.directory.clone();
            self.cancel_job();
            self.pending_since = Some(now);
        }
        if let Some(since) = self.pending_since {
            if now - since >= DEBOUNCE {
                self.pending_since = None;
                self.start(root, buffer_text);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    DEBOUNCE - (now - since) + 0.01,
                ));
            }
        }
        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }
        if !open || close {
            self.visible = false;
            self.cancel_job();
            self.pending_since = None;
        }
        if self.visible && !menu_open {
            if let Some(owner) = query_id.filter(|&id| ctx.memory(|m| m.focused()) == Some(id)) {
                let status = if self.is_searching() {
                    format!("Searching; {} results so far", self.results.matches.len())
                } else if let Some(error) = &self.error {
                    format!("Search error: {error}")
                } else if let Some(found) = self.results.matches.get(self.selected) {
                    let included = if self.replace_mode {
                        if self
                            .skipped
                            .contains(&(found.path.clone(), found.line, found.start))
                        {
                            "; excluded from replacement"
                        } else {
                            "; included in replacement"
                        }
                    } else {
                        ""
                    };
                    format!(
                        "Result {} of {}: {}{included}",
                        self.selected + 1,
                        self.results.matches.len(),
                        find_match_label(found, root)
                    )
                } else {
                    "No results".into()
                };
                crate::a11y::announce(
                    ctx,
                    owner,
                    &format!(
                        "{title}. {status}. Up and Down to select; Enter to open; Escape to close"
                    ),
                );
            }
        }
        events
    }
}

/// Display context is bounded; exact match authority remains in SearchMatch.
fn find_match_label(found: &SearchMatch, root: &Path) -> String {
    let column = found
        .line_text
        .get(..found.start)
        .map(|s| s.chars().count().saturating_add(1));
    let preview: String = found.line_text.chars().take(160).collect();
    let tail = if preview.len() < found.line_text.len() {
        "…"
    } else {
        ""
    };
    format!(
        "{}: line {}, column {}: {preview}{tail}",
        found
            .path
            .strip_prefix(root)
            .unwrap_or(&found.path)
            .display(),
        found.line.saturating_add(1),
        column
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EverywhereTab {
    All,
    /// Go to Class.
    Classes,
    Files,
    Symbols,
    Actions,
    Text,
    Git,
    Endpoints,
    Settings,
}

impl EverywhereTab {
    /// The action whose shortcut, pressed again while the tab is shown,
    /// toggles "Include non-project items".
    fn action(self) -> Option<Action> {
        match self {
            EverywhereTab::Classes => Some(Action::GoToClass),
            EverywhereTab::Files => Some(Action::GoToFile),
            EverywhereTab::Symbols => Some(Action::GoToSymbol),
            _ => None,
        }
    }
}

/// One row in Search Everywhere / Go to File / Recent Files.
#[derive(Clone, Debug)]
pub enum NavItem {
    Extra {
        item: ExtraItem,
        score: i32,
    },
    File {
        path: PathBuf,
        score: i32,
    },
    Symbol {
        name: String,
        detail: String,
        path: PathBuf,
        line: usize,
        score: i32,
        /// `None` for symbols found without an index (line scan).
        kind: Option<SymbolKind>,
        /// Enclosing classes (`Outer.Inner`), empty at module level.
        container: String,
        /// Shown on the right instead of the file path (library symbols:
        /// module and library kind).
        location: String,
    },
    Action {
        action: Action,
        score: i32,
    },
    /// An External Tool (Settings › Tools › External Tools).
    Tool {
        tool: crate::external_tools::ToolEntry,
        score: i32,
    },
}

/// Most symbols listed for one query.
const SYMBOL_LIMIT: usize = 60;

impl NavItem {
    /// A row for an indexed symbol: project symbols rank above library ones
    /// with the same match quality.
    fn from_found(f: FoundSymbol) -> NavItem {
        let h = f.hit;
        let location = if f.library {
            format!("{} · {}", h.module, library_label(h.root_kind))
        } else {
            String::new()
        };
        NavItem::Symbol {
            name: h.name,
            detail: h.detail,
            path: h.path,
            line: h.line as usize,
            score: h.score + if f.library { 10 } else { 20 },
            kind: Some(h.kind),
            container: h.container,
            location,
        }
    }
}

fn library_label(kind: RootKind) -> &'static str {
    match kind {
        RootKind::Stdlib => "stdlib",
        RootKind::Site => "site-packages",
        RootKind::Project | RootKind::Other => "library",
    }
}

/// Badge letter and color of a symbol kind (`S` when unknown).
fn symbol_badge(kind: Option<SymbolKind>) -> (&'static str, Color32) {
    match kind {
        Some(SymbolKind::Class) => ("C", Color32::from_rgb(0x59, 0xA8, 0x69)),
        Some(SymbolKind::Function) => ("f", Color32::from_rgb(0xC7, 0x7C, 0x2E)),
        Some(SymbolKind::Method) => ("m", Color32::from_rgb(0xC0, 0x56, 0x56)),
        Some(SymbolKind::Variable | SymbolKind::Constant | SymbolKind::Import) => {
            ("v", Color32::from_rgb(0x8E, 0x6C, 0xC2))
        }
        None => ("S", Color32::from_rgb(0x59, 0xA8, 0x69)),
    }
}

impl NavItem {
    fn score(&self) -> i32 {
        match self {
            NavItem::File { score, .. }
            | NavItem::Symbol { score, .. }
            | NavItem::Action { score, .. }
            | NavItem::Tool { score, .. }
            | NavItem::Extra { score, .. } => *score,
        }
    }
}

/// Search Everywhere popup (also used for Go to File / Go to Symbol / Find Action).
pub struct SearchEverywhere {
    pub visible: bool,
    pub tab: EverywhereTab,
    pub query: String,
    pub selected: usize,
    pub items: Vec<NavItem>,
    last_query: String,
    last_tab: EverywhereTab,
    focus: bool,
    pub recent_files: Vec<PathBuf>,
    /// Project symbols found by a line scan ([`scan_symbols`]; classes have an
    /// empty detail): used until the project is indexed. (name, detail, path,
    /// line). Only [`Self::rescan_symbols`] fills it, so Go to Class can tell
    /// classes by their empty detail (the index gives classes with bases one).
    symbols: Vec<(String, String, PathBuf, usize)>,
    symbols_scanned_at: f64,
    /// Open editors with unsaved changes: their symbols (line scan) replace
    /// what the index or the scan found in those files.
    unsaved: Vec<(String, String, PathBuf, usize)>,
    unsaved_paths: HashSet<PathBuf>,
    /// The project and library symbol indexes (see `crate::symbol_index`).
    pub index: IndexSnapshot,
    /// "Include non-project items": library symbols and files too.
    pub include_non_project: bool,
    last_include: bool,
    last_generation: u64,
    /// Project file index and scope: Go to File results come from the cached list.
    file_index: Option<FileIndex>,
    project_scope: Option<ProjectScope>,
    /// File index generation the items were computed with (a finished rebuild refreshes them).
    file_index_generation: u64,
    /// The file index was not built yet when the items were computed.
    file_indexing: bool,
    pub memory: rustpy_settings::navigation_search::NavigationSearch,
    scope_filter: String,
    last_scope_filter: String,
    preview: bool,
    providers: everywhere_features::Snapshot,
    provider_job: Option<TaskHandle<everywhere_features::Snapshot>>,
    provider_buffers: HashMap<PathBuf, String>,
    git_limit: usize,
    abbreviation: String,
}

impl NavItem {
    /// Full unelided authority used by the painted row and live selection.
    fn accessible_label(&self, root: &Path, keymap: &Keymap) -> String {
        match self {
            Self::Extra { item, .. } => format!("{} — {}", item.label, item.detail),
            Self::File { path, .. } => {
                format!("File {}", path.strip_prefix(root).unwrap_or(path).display())
            }
            Self::Symbol {
                name,
                detail,
                path,
                line,
                container,
                location,
                ..
            } => {
                let origin = if location.is_empty() {
                    path.strip_prefix(root)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                } else {
                    location.clone()
                };
                format!(
                    "Symbol {name}{detail}, in {container}, {origin}, line {}",
                    line.saturating_add(1)
                )
            }
            Self::Action { action, .. } => format!(
                "Action {}, {}",
                action.text(),
                keymap.shortcut_text(*action)
            ),
            Self::Tool { tool, .. } => format!(
                "External tool {}, {}, {}",
                tool.name, tool.group, tool.shortcut
            ),
        }
    }
}
impl Default for SearchEverywhere {
    fn default() -> Self {
        Self {
            visible: false,
            tab: EverywhereTab::All,
            query: String::new(),
            selected: 0,
            items: Vec::new(),
            last_query: "\u{0}".into(),
            last_tab: EverywhereTab::All,
            focus: false,
            recent_files: Vec::new(),
            symbols: Vec::new(),
            symbols_scanned_at: -1000.0,
            unsaved: Vec::new(),
            unsaved_paths: HashSet::new(),
            index: IndexSnapshot::default(),
            include_non_project: false,
            last_include: false,
            last_generation: 0,
            file_index: None,
            project_scope: None,
            file_index_generation: 0,
            file_indexing: false,
            memory: Default::default(),
            scope_filter: String::new(),
            last_scope_filter: String::new(),
            preview: true,
            providers: Default::default(),
            provider_job: None,
            provider_buffers: HashMap::new(),
            git_limit: 100,
            abbreviation: String::new(),
        }
    }
}

impl SearchEverywhere {
    pub fn open(&mut self, tab: EverywhereTab) {
        self.visible = true;
        self.tab = tab;
        self.query.clear();
        self.last_query = "\u{0}".into();
        self.selected = 0;
        self.focus = true;
    }

    /// Line-scans the project's symbols (used while there is no project
    /// index) unless that happened less than `max_age` seconds before `now`.
    pub fn rescan_symbols(&mut self, root: &Path, now: f64, max_age: f64) {
        if now - self.symbols_scanned_at > max_age {
            self.symbols = scan_symbols(root);
            self.symbols_scanned_at = now;
        }
    }

    /// The text of open editors with unsaved changes, `(path, text)`: Go to
    /// Symbol / Class find what they declare now rather than what is on disk.
    pub fn set_unsaved(&mut self, buffers: impl IntoIterator<Item = (PathBuf, String)>) {
        self.unsaved.clear();
        self.unsaved_paths.clear();
        for (path, text) in buffers {
            self.unsaved.extend(symbols_in_text(&path, &text));
            self.unsaved_paths.insert(path);
        }
    }

    /// Project symbols for the "Import" quick fix of unresolved references:
    /// the project index's module-level declarations (else the line scan),
    /// with unsaved editor text in place of the files on disk.
    pub fn import_fix_symbols(
        &self,
    ) -> impl Iterator<Item = (String, String, PathBuf, usize)> + '_ {
        let listed: Box<dyn Iterator<Item = (String, String, PathBuf, usize)> + '_> =
            match self.index.project_symbol_list() {
                Some(indexed) => Box::new(indexed),
                None => Box::new(self.symbols.iter().cloned()),
            };
        listed
            .filter(|s| !self.unsaved_paths.contains(&s.2))
            .chain(self.unsaved.iter().cloned())
            // Nested symbols are searchable, not module-level import targets.
            .filter(|symbol| !symbol.0.contains('.'))
    }

    /// The project's file index and scope (set by the application every frame).
    pub fn set_index(&mut self, index: FileIndex, scope: Option<ProjectScope>) {
        self.file_index = Some(index);
        self.project_scope = scope;
    }

    pub fn note_recent(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(50);
    }

    fn rebuild(
        &mut self,
        root: &Path,
        keymap: &Keymap,
        tools: &[crate::external_tools::ToolEntry],
    ) {
        let q = self.query.trim();
        let mut items: Vec<NavItem> = Vec::new();
        let tab = self.tab;
        self.file_indexing = false;
        self.file_index_generation = self
            .file_index
            .as_ref()
            .map(FileIndex::generation)
            .unwrap_or(0);
        if q.is_empty() {
            if matches!(tab, EverywhereTab::All | EverywhereTab::Files) {
                for p in &self.recent_files {
                    if p.exists() {
                        items.push(NavItem::File {
                            path: p.clone(),
                            score: 0,
                        });
                    }
                }
            }
            if tab == EverywhereTab::Actions {
                for a in Action::ALL.iter().take(60) {
                    items.push(NavItem::Action {
                        action: *a,
                        score: 0,
                    });
                }
            }
            if matches!(tab, EverywhereTab::Files) {
                self.add_pinned(&mut items);
                self.items = items;
                self.selected = 0;
                return;
            }
        }
        if !q.is_empty() && matches!(tab, EverywhereTab::All | EverywhereTab::Files) {
            // Scored against the cached project list: no disk walk per keystroke.
            let found = match (&self.file_index, &self.project_scope) {
                (Some(index), Some(scope)) => match index.files_for(scope) {
                    Some(files) => rustpy_search::find_files_among(&scope.root, &files, q, 40),
                    None => {
                        self.file_indexing = true;
                        Vec::new()
                    }
                },
                _ => rustpy_search::find_files(root, q, 40),
            };
            for (p, s) in found {
                items.push(NavItem::File { path: p, score: s });
            }
            // Library files (Go to File with non-project items).
            if self.include_non_project {
                if let Some(library) = &self.index.library {
                    let (name_query, _) = rustpy_search::split_line_suffix(q);
                    let name_query = name_query.trim();
                    if !name_query.is_empty() && !name_query.contains('/') {
                        for (p, s) in library.search_files(
                            &|name: &str| everywhere_features::match_score(name, name_query),
                            40,
                        ) {
                            // Project sources on `sys.path` are found above already.
                            if !self
                                .index
                                .project_files
                                .as_ref()
                                .is_some_and(|f| f.contains(&p))
                            {
                                items.push(NavItem::File {
                                    path: p,
                                    score: s - 10,
                                });
                            }
                        }
                    }
                }
            }
        }
        if !q.is_empty()
            && matches!(
                tab,
                EverywhereTab::All | EverywhereTab::Classes | EverywhereTab::Symbols
            )
        {
            let classes_only = tab == EverywhereTab::Classes;
            let unsaved = &self.unsaved_paths;
            if self.index.project.is_some()
                || (self.include_non_project && self.index.library.is_some())
            {
                let found = self.index.search(
                    q,
                    classes_only,
                    self.include_non_project,
                    SYMBOL_LIMIT,
                    &everywhere_features::match_score,
                );
                items.extend(
                    found
                        .into_iter()
                        .filter(|f| f.library || !unsaved.contains(&f.hit.path))
                        .map(NavItem::from_found),
                );
            }
            // Until the project is indexed, the symbols found by a line scan;
            // and those of unsaved editors (a line scan too: classes have no
            // detail there).
            let scanned: &[(String, String, PathBuf, usize)] = if self.index.project.is_none() {
                &self.symbols
            } else {
                &[]
            };
            for (name, detail, path, line) in scanned
                .iter()
                .filter(|s| !unsaved.contains(&s.2))
                .chain(&self.unsaved)
            {
                if classes_only && !detail.is_empty() {
                    continue;
                }
                let (container, leaf) = name.rsplit_once('.').unwrap_or(("", name.as_str()));
                if let Some(s) = everywhere_features::match_score(leaf, q) {
                    items.push(NavItem::Symbol {
                        name: leaf.to_owned(),
                        detail: detail.clone(),
                        path: path.clone(),
                        line: *line,
                        score: s + 20,
                        kind: None,
                        container: container.to_owned(),
                        location: String::new(),
                    });
                }
            }
        }
        if !q.is_empty() && matches!(tab, EverywhereTab::All | EverywhereTab::Actions) {
            for a in Action::ALL {
                let text = a.text();
                // In a translated UI the English name finds the action too.
                let english = (text != a.english_text())
                    .then(|| everywhere_features::match_score(a.english_text(), q))
                    .flatten();
                if let Some(s) = everywhere_features::match_score(text, q)
                    .max(english)
                    .max(everywhere_features::match_score(a.id(), q))
                    .max(keymap.abbreviation_score(*a, q))
                {
                    items.push(NavItem::Action {
                        action: *a,
                        score: s - if tab == EverywhereTab::All { 100 } else { 0 },
                    });
                }
            }
            for t in tools {
                // By name, or by its group ("External Tools", "Formatters").
                if let Some(s) = everywhere_features::match_score(&t.name, q).or_else(|| {
                    everywhere_features::match_score(&format!("{} {}", t.group, t.name), q)
                        .map(|s| s - 50)
                }) {
                    items.push(NavItem::Tool {
                        tool: t.clone(),
                        score: s - if tab == EverywhereTab::All { 100 } else { 0 },
                    });
                }
            }
        }
        self.additional_items(root, q, tab, &mut items);
        if !self.scope_filter.is_empty() {
            items.retain(|item| match item {
                NavItem::File { path, .. } | NavItem::Symbol { path, .. } => path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .contains(&self.scope_filter),
                NavItem::Extra { item, .. } => match &item.destination {
                    Destination::File(path, _) => path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .contains(&self.scope_filter),
                    _ => true,
                },
                _ => true,
            });
        }
        items.sort_by(|a, b| self.ranked_score(b).total_cmp(&self.ranked_score(a)));

        items.truncate(80);
        self.items = items;
        self.selected = 0;
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: &Path,
        keymap: &Keymap,
    ) -> Vec<NavEvent> {
        let mut events = Vec::new();
        if !self.visible {
            return events;
        }
        if let Some(result) = self.provider_job.as_mut().and_then(|job| job.try_result()) {
            if let Ok(snapshot) = result {
                self.providers = snapshot;
                self.last_query = "\0".into();
            }
            self.provider_job = None;
        }
        if self.provider_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        let file_index_changed = self
            .file_index
            .as_ref()
            .is_some_and(|i| i.generation() != self.file_index_generation);
        if file_index_changed || self.scope_filter != self.last_scope_filter {
            if let Some(paths) = self
                .file_index
                .as_ref()
                .zip(self.project_scope.as_ref())
                .and_then(|(index, scope)| index.files_for(scope))
            {
                self.start_providers(
                    root,
                    paths
                        .iter()
                        .filter(|path| {
                            path.strip_prefix(root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .contains(&self.scope_filter)
                        })
                        .take(256)
                        .cloned()
                        .collect(),
                    self.provider_buffers.clone(),
                );
            }
        }
        if self.query != self.last_query
            || self.tab != self.last_tab
            || self.include_non_project != self.last_include
            || self.index.generation != self.last_generation
            || file_index_changed
            || self.scope_filter != self.last_scope_filter
        {
            let selected = self.selected;
            let same_query = self.query == self.last_query && self.tab == self.last_tab;
            self.rebuild(root, keymap, &crate::external_tools::published_tools(ctx));
            // New index data for the same query keeps the selected row.
            if same_query {
                self.selected = selected.min(self.items.len().saturating_sub(1));
            }
            self.last_query = self.query.clone();
            self.last_tab = self.tab;
            self.last_include = self.include_non_project;
            self.last_generation = self.index.generation;
            // The popup takes its new size in the next frame.
            ctx.request_repaint();
        }
        if self.file_indexing {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        let mut close = false;
        let width = 720.0;
        let area = egui::Area::new(egui::Id::new("search_everywhere")).order(egui::Order::Foreground).anchor(Align2::CENTER_TOP, Vec2::new(0.0, 80.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(theme.panel_bg).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(width);
                // Pressing Go to Class / File / Symbol again toggles non-project
                // items (not the press that opened the popup in this frame).
                let again = !self.focus && self.tab.action().is_some_and(|tab_action| {
                    ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key, physical_key, pressed: true, repeat: false, modifiers } if keymap.lookup_key(*key, *physical_key, *modifiers) == Some(tab_action))))
                });
                if again {
                    self.include_non_project = !self.include_non_project;
                }
                ui.horizontal(|ui| {
                    for &(t, label, _) in everywhere_features::TABS {
                        if ui.selectable_label(self.tab == t, label).clicked() {
                            self.tab = t;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.tab != EverywhereTab::Actions {
                            let tip = match self.tab.action() {
                                Some(a) => format!("Also search the interpreter's libraries (stdlib, site-packages); press {} again to toggle", keymap.shortcut_text(a)),
                                None => "Also search the interpreter's libraries (stdlib, site-packages)".to_string(),
                            };
                            ui.checkbox(&mut self.include_non_project, "Include non-project items").on_hover_text(tip);
                        }
                        ui.label(egui::RichText::new("Tab to switch · Esc to close").color(theme.text_dim).small());
                    });
                });
                let hint = match self.tab {
                    EverywhereTab::All => "Type to search files, symbols and actions (recent files shown by default)",
                    EverywhereTab::Classes => "Type a class name (Class.member and module.Class work too)",
                    EverywhereTab::Files => "Type a file name, path/part or name:line",
                    EverywhereTab::Symbols => "Type a class, function, method or variable name",
                    EverywhereTab::Actions => "Type an action name or action ID",
                    EverywhereTab::Text => "Search project text (bounded snapshot)",
                    EverywhereTab::Git => "Search commits and branches",
                    EverywhereTab::Endpoints => "Search HTTP method, path or handler",
                    EverywhereTab::Settings => "Search settings pages and toggles",
                };
                ui.horizontal(|ui| {
                    ui.label("Path scope:");
                    ui.add(egui::TextEdit::singleline(&mut self.scope_filter).desired_width(160.0).hint_text("directory / path fragment"));
                    ui.checkbox(&mut self.preview, "Preview");
                    if let Some(NavItem::File { path, .. } | NavItem::Symbol { path, .. }) = self.items.get(self.selected) {
                        let path = path.clone();
                        let pinned = self.memory.pinned.contains(&path);
                        if ui.button(if pinned { "Unpin" } else { "Pin recent" }).clicked() {
                            self.memory.pinned.retain(|p| p != &path);
                            if !pinned && self.memory.pinned.len()<64 { self.memory.pinned.push(path); }
                            self.last_query = "\0".into(); events.push(NavEvent::SearchMemory(self.memory.clone()));
                        }
                    }
                });
                ui.collapsing("Custom abbreviation for selected action or file", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.abbreviation).desired_width(120.0).hint_text("abbreviation"));
                        if ui.button("Bind abbreviation").clicked() {
                            if let Some(item) = self.items.get(self.selected).cloned() {
                                let alias = self.abbreviation.clone();
                                if self.bind_abbreviation(&alias, &item) { events.push(NavEvent::SearchMemory(self.memory.clone())); }
                            }
                        }
                    });
                });
                // Tab switches the result tab: take it before egui's focus
                // traversal moves the keyboard off the query field.
                let tab = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
                let resp = ui.add(egui::TextEdit::singleline(&mut self.query).desired_width(f32::INFINITY).hint_text(hint).font(egui::FontId::proportional(15.0)).lock_focus(true).return_key(None));
                if self.focus || tab {
                    resp.request_focus();
                    self.focus = !ui.is_visible();
                }
                // Single-line TextEdit only locks Tab via lock_focus. Keep its
                // arrow filter after the widget configures focus, so egui's
                // end-of-pass navigation cannot move Enter onto another row.
                ui.memory_mut(|memory| memory.set_focus_lock_filter(resp.id, egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                }));
                let (up, down, enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                let (up, down, enter) = (up && resp.has_focus(), down && resp.has_focus(), enter && resp.has_focus());
                if down && self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                if up && self.selected > 0 {
                    self.selected -= 1;
                }
                if esc {
                    close = true;
                }
                if tab {
                    let index = everywhere_features::TABS.iter().position(|(tab,_,_)| *tab == self.tab).unwrap_or(0);
                    self.tab = everywhere_features::TABS[(index+1)%everywhere_features::TABS.len()].0;
                }
                if matches!(self.tab, EverywhereTab::Text | EverywhereTab::Git | EverywhereTab::Endpoints) {
                    ui.label(egui::RichText::new("Text/endpoint snapshot: up to 256 files / 4 MiB; choose a path scope to refine.").small().color(theme.text_dim));
                    if self.tab == EverywhereTab::Git && self.providers.git_count >= self.git_limit && self.git_limit < 4096 && ui.button("Load more Git history").clicked() {
                            self.git_limit = (self.git_limit * 4).min(4096);
                            let paths = self.providers.sources.iter().map(|(path,_)|path.clone()).collect();
                            self.start_providers(root, paths, self.provider_buffers.clone());
                            self.last_query = "\0".into();
                    }
                }
                if self.include_non_project && self.index.library_pending && self.tab != EverywhereTab::Actions {
                    ui.label(egui::RichText::new("Indexing the interpreter's libraries… non-project results are incomplete").color(theme.text_dim).small());
                } else if self.index.project_pending && self.tab != EverywhereTab::Actions {
                    // Dumb mode: Go to Class / Symbol run on partial indexes.
                    ui.label(egui::RichText::new("Indexing the project… results may be incomplete").color(theme.text_dim).small());
                }
                if !close && resp.has_focus() {
                    let selected = self.items.get(self.selected).map(|item| format!(
                        "Result {} of {}: {}", self.selected + 1, self.items.len(), item.accessible_label(root, keymap)
                    )).unwrap_or_else(|| if self.file_indexing { "Indexing project files".into() } else { "No results".into() });
                    crate::a11y::announce(ctx, resp.id, &format!("Search Everywhere. {selected}. Up and Down to select; Enter to open; Tab to switch category; Escape to close"));
                }
                ui.separator();
                let mut activate: Option<usize> = None;
                if enter {
                    activate = Some(self.selected);
                }
                // As tall as the results (up to 420 px): a size remembered from an
                // earlier, shorter list must not clip it to two rows.
                let rows_h = (self.items.len() as f32 * 24.0).min(420.0);
                egui::ScrollArea::vertical().max_height(420.0).min_scrolled_height(rows_h).auto_shrink([false, true]).show(ui, |ui| {
                    for (idx, item) in self.items.iter().enumerate() {
                        let selected = idx == self.selected;
                        let accessible_label = item.accessible_label(root, keymap);
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
                        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &accessible_label));
                        resp.ctx.accesskit_node_builder(resp.id, |node| {
                            node.clear_toggled();
                            node.set_selected(selected);
                        });
                        if selected {
                            ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
                            if up || down {
                                resp.scroll_to_me(None);
                            }
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
                        }
                        let fg = if selected { Color32::WHITE } else { theme.text };
                        let dim = if selected { Color32::from_gray(220) } else { theme.text_dim };
                        let y = rect.center().y;
                        match item {
                            NavItem::Extra { item, .. } => {
                                ui.painter().text(egui::pos2(rect.left()+8.0,y), Align2::LEFT_CENTER, &item.label, egui::FontId::proportional(13.5), fg);
                                resp.clone().on_hover_text(&item.detail);
                            }
                            NavItem::File { path, .. } => {
                                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                // Files outside the project (libraries) show their whole directory.
                                let dir = path.parent().map(|d| d.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| d.display().to_string())).unwrap_or_default();
                                let dir = elide_start(ui, &dir, &egui::FontId::proportional(12.0), rect.width() * 0.6);
                                badge(ui, rect.left() + 12.0, y, "F", Color32::from_rgb(0x3E, 0x7C, 0xB8));
                                ui.painter().text(egui::Pos2::new(rect.left() + 28.0, y), Align2::LEFT_CENTER, name, egui::FontId::proportional(13.5), fg);
                                ui.painter().text(egui::Pos2::new(rect.right() - 8.0, y), Align2::RIGHT_CENTER, dir, egui::FontId::proportional(12.0), dim);
                            }
                            NavItem::Symbol { name, detail, path, kind, container, location, .. } => {
                                let (letter, color) = symbol_badge(*kind);
                                badge(ui, rect.left() + 12.0, y, letter, color);
                                // Right: the file (project) or module and library (non-project).
                                let right = if location.is_empty() { path.strip_prefix(root).unwrap_or(path).display().to_string() } else { location.clone() };
                                let small = egui::FontId::proportional(12.0);
                                let right = elide_start(ui, &right, &small, rect.width() * 0.45);
                                let right = ui.painter().layout_no_wrap(right, small.clone(), dim);
                                let right_x = rect.right() - 8.0 - right.size().x;
                                ui.painter().galley(egui::Pos2::new(right_x, y - right.size().y / 2.0), right, dim);
                                // Left: name, signature and enclosing class, clipped before the right part.
                                let mut job = egui::text::LayoutJob::default();
                                job.append(name, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(13.5), color: fg, ..Default::default() });
                                if !detail.is_empty() {
                                    job.append(detail, 0.0, egui::TextFormat { font_id: small.clone(), color: dim, ..Default::default() });
                                }
                                if !container.is_empty() {
                                    job.append(&format!("  in {container}"), 0.0, egui::TextFormat { font_id: small.clone(), color: dim, ..Default::default() });
                                }
                                let left = ui.painter().layout_job(job);
                                let x = rect.left() + 28.0;
                                let clip = egui::Rect::from_min_max(egui::Pos2::new(x, rect.top()), egui::Pos2::new((right_x - 12.0).max(x + 80.0), rect.bottom()));
                                ui.painter().with_clip_rect(clip).galley(egui::Pos2::new(x, y - left.size().y / 2.0), left, fg);
                            }
                            NavItem::Action { action, .. } => {
                                badge(ui, rect.left() + 12.0, y, "A", Color32::from_rgb(0x9E, 0x88, 0x0D));
                                ui.painter().text(egui::Pos2::new(rect.left() + 28.0, y), Align2::LEFT_CENTER, action.text(), egui::FontId::proportional(13.5), fg);
                                ui.painter().text(egui::Pos2::new(rect.right() - 8.0, y), Align2::RIGHT_CENTER, keymap.shortcut_text(*action), egui::FontId::proportional(12.0), dim);
                            }
                            NavItem::Tool { tool, .. } => {
                                badge(ui, rect.left() + 12.0, y, "T", Color32::from_rgb(0x4E, 0xAA, 0x4E));
                                let group = if tool.group.is_empty() { "External Tools".to_string() } else { tool.group.clone() };
                                ui.painter().text(egui::Pos2::new(rect.left() + 28.0, y), Align2::LEFT_CENTER, format!("{} ({group})", tool.name), egui::FontId::proportional(13.5), fg);
                                ui.painter().text(egui::Pos2::new(rect.right() - 8.0, y), Align2::RIGHT_CENTER, &tool.shortcut, egui::FontId::proportional(12.0), dim);
                            }
                        }
                        if resp.clicked() {
                            activate = Some(idx);
                        }
                    }
                    if self.file_indexing {
                        ui.label(egui::RichText::new("Indexing project files…").color(theme.text_dim));
                    } else if self.items.is_empty() {
                        let can_widen = !self.include_non_project && !self.query.trim().is_empty() && self.tab != EverywhereTab::Actions && (self.index.library.is_some() || self.index.library_pending);
                        if can_widen {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Nothing found in the project.").color(theme.text_dim));
                                if ui.link("Include non-project items").clicked() {
                                    self.include_non_project = true;
                                }
                            });
                        } else {
                            ui.label(egui::RichText::new("Nothing found").color(theme.text_dim));
                        }
                    }
                });
                if self.preview { self.preview_ui(ui); }
                if let Some(idx) = activate {
                    if let Some(item) = self.items.get(idx).cloned() {
                        self.learn_selection(&item);
                        events.push(NavEvent::SearchMemory(self.memory.clone()));
                        let (_, suffix) = rustpy_search::split_line_suffix(self.query.trim());
                        match item {
                            NavItem::Extra { item, .. } => {
                                if let Destination::Tab(tab) = item.destination { self.tab = tab; self.query.clear(); self.focus = true; }
                                else { events.push(NavEvent::SearchDestination(item.destination)); }
                            }
                            NavItem::File { path, .. } => events.push(NavEvent::OpenFile { path, line: suffix.map(|(l, _)| l.saturating_sub(1)), column: suffix.and_then(|(_, c)| c).map(|c| c.saturating_sub(1)) }),
                            NavItem::Symbol { path, line, .. } => events.push(NavEvent::OpenFile { path, line: Some(line), column: None }),
                            NavItem::Action { action, .. } => events.push(NavEvent::RunAction(action)),
                            // On the active editor's file, as Tools › External Tools does.
                            NavItem::Tool { tool, .. } => crate::external_tools::request_run(ctx, crate::external_tools::ToolRequest { name: tool.name, file: None }),
                        }
                        close = !self.query.is_empty() || !matches!(self.items.get(idx), Some(NavItem::Extra { item: ExtraItem { destination: Destination::Tab(_), .. }, .. }));
                    }
                }
            });
        });
        // A click outside closes the popup.
        if ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p))
        }) {
            close = true;
        }
        if close {
            self.visible = false;
            if let Some(job) = &self.provider_job {
                job.cancel();
            }
        }
        events
    }
}

/// `text` shortened at its start (`…/lib/python3.12`) to fit `max_width`.
fn elide_start(ui: &egui::Ui, text: &str, font: &egui::FontId, max_width: f32) -> String {
    let width = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE)
        .size()
        .x;
    if width <= max_width || text.is_empty() {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let keep = ((chars.len() as f32 * max_width / width) as usize)
        .saturating_sub(1)
        .max(1)
        .min(chars.len());
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("…{tail}")
}

fn badge(ui: &egui::Ui, x: f32, y: f32, text: &str, color: Color32) {
    let r = egui::Rect::from_center_size(egui::Pos2::new(x, y), Vec2::splat(15.0));
    ui.painter().rect_filled(r, 7.5, color);
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(9.5),
        Color32::WHITE,
    );
}

/// A project symbol for Go to Symbol: `(name, signature detail, path, 0-based line)`.
pub type Symbol = (String, String, PathBuf, usize);

/// Collects class/function symbols from project Python files (for Go to Symbol).
pub fn scan_symbols(root: &Path) -> Vec<Symbol> {
    scan_symbols_with_progress(root, &ProgressReporter::new())
}

/// [`scan_symbols`] as the project indexing task: reports the files done to
/// `progress` and stops (with what it found so far) once it is cancelled.
pub fn scan_symbols_with_progress(root: &Path, progress: &ProgressReporter) -> Vec<Symbol> {
    let mut out = Vec::new();
    progress.set_details("Scanning files…");
    let files: Vec<PathBuf> = rustpy_search::project_files_with_progress(root, &[], progress)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("py"))
        .collect();
    for (done, path) in files.iter().enumerate() {
        if progress.is_cancelled() {
            break;
        }
        if done.is_multiple_of(16) {
            progress.set_step(done, files.len());
            progress.set_details(
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if text.len() > 2 * 1024 * 1024 {
            continue;
        }
        out.extend(symbols_in_text(path, &text));
        if out.len() > 200_000 {
            break;
        }
    }
    out
}

/// `def` / `class` names of one file's text: `(name, signature detail, path, line)`.
pub fn symbols_in_text(path: &Path, text: &str) -> Vec<Symbol> {
    let extracted = rustpy_index::extract(text);
    extracted
        .symbols
        .iter()
        .filter(|symbol| symbol.kind.is_definition())
        .map(|symbol| {
            let mut name = symbol.name.clone();
            let mut parent = symbol.parent;
            let mut remaining = 16;
            while let Some(index) = parent {
                if remaining == 0 {
                    break;
                }
                remaining -= 1;
                let Some(owner) = extracted.symbols.get(index as usize) else {
                    break;
                };
                name = format!("{}.{name}", owner.name);
                parent = owner.parent;
            }
            // Existing navigation protocol: an empty detail identifies a class.
            let detail = if symbol.kind == rustpy_index::SymbolKind::Class {
                String::new()
            } else if symbol.detail.is_empty() {
                symbol.kind.label().to_owned()
            } else {
                symbol.detail.clone()
            };
            (name, detail, path.to_path_buf(), symbol.line as usize)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch project directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rustpy-find-in-files-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for (rel, text) in files {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            Self(dir.canonicalize().unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The project index of `root`, as the IDE builds it in the background.
    fn project_index(root: &Path) -> Arc<rustpy_index::SymbolIndex> {
        let roots = vec![rustpy_index::RootSpec {
            path: root.to_path_buf(),
            kind: RootKind::Project,
        }];
        let files: Vec<rustpy_index::SourceFile> = rustpy_search::project_files(root)
            .into_iter()
            .map(|path| rustpy_index::SourceFile { root: 0, path })
            .collect();
        let progress = rustpy_index::Progress::default();
        let (index, _) = rustpy_index::refresh(
            None,
            rustpy_index::IndexMeta::default(),
            &roots,
            files,
            &[],
            &rustpy_index::BuildOptions::default(),
            &progress,
        )
        .unwrap();
        Arc::new(index.expect("a new index"))
    }

    fn symbol_names(se: &SearchEverywhere) -> Vec<String> {
        se.items
            .iter()
            .filter_map(|i| {
                if let NavItem::Symbol { name, .. } = i {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn go_to_class_finds_classes_with_bases_with_and_without_the_project_index() {
        let s = Scratch::new("goto-class", &[("models.py", "class Base:\n    pass\n\n\nclass Order(Base):\n    def total(self):\n        pass\n\n\ndef order_total(order):\n    pass\n")]);
        let keymap = Keymap::default_keymap();
        let mut se = SearchEverywhere {
            tab: EverywhereTab::Classes,
            query: "order".into(),
            ..Default::default()
        };
        // Indexed: the project index knows `Order` is a class (its detail is `(Base)`).
        se.index = IndexSnapshot {
            generation: 1,
            project: Some(project_index(&s.0)),
            ..Default::default()
        };
        se.rebuild(&s.0, &keymap, &[]);
        assert_eq!(symbol_names(&se), vec!["Order"]);
        // The project index deleted in Settings: the line scan stands in and
        // still lists the class with bases (not the function).
        se.index = IndexSnapshot {
            generation: 2,
            ..Default::default()
        };
        se.rescan_symbols(&s.0, 100.0, 5.0);
        se.rebuild(&s.0, &keymap, &[]);
        assert_eq!(symbol_names(&se), vec!["Order"]);
        se.tab = EverywhereTab::Symbols;
        se.rebuild(&s.0, &keymap, &[]);
        assert_eq!(symbol_names(&se), vec!["Order", "order_total"]);
        // Scanned at most every `max_age` seconds.
        std::fs::write(s.0.join("more.py"), "class OrderLine:\n    pass\n").unwrap();
        se.rescan_symbols(&s.0, 102.0, 5.0);
        se.rebuild(&s.0, &keymap, &[]);
        assert!(!symbol_names(&se).contains(&"OrderLine".to_string()));
        se.rescan_symbols(&s.0, 106.0, 5.0);
        se.rebuild(&s.0, &keymap, &[]);
        assert!(symbol_names(&se).contains(&"OrderLine".to_string()));
    }

    /// Runs a background search to the end, polling like the popup does each frame.
    fn search(f: &mut FindInFiles, root: &Path, buffers: &dyn Fn(&Path) -> Option<String>) {
        f.start(root, buffers);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while f.job.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "the search never finished"
            );
            f.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn found(f: &FindInFiles, root: &Path) -> Vec<(PathBuf, usize, usize)> {
        f.results
            .matches
            .iter()
            .map(|m| {
                (
                    m.path.strip_prefix(root).unwrap().to_path_buf(),
                    m.line,
                    m.start,
                )
            })
            .collect()
    }

    #[test]
    fn zero_width_replace_preview_produces_edits_for_open_and_closed_files() {
        let s = Scratch::new(
            "zero-width-replace",
            &[("a.py", "disk\n"), ("b.py", "x\ny\n")],
        );
        let open = s.0.join("a.py");
        let buffers = |p: &Path| (p == open).then(|| "é\n中\n".to_string());
        let mut f = FindInFiles {
            query: "^".into(),
            replacement: "start_".into(),
            open_files: vec![open.clone()],
            options: SearchOptions {
                regex: true,
                ..Default::default()
            },
            ..Default::default()
        };
        search(&mut f, &s.0, &buffers);
        assert_eq!(f.results.matches.len(), 4);
        f.skipped.insert((open.clone(), 1, 0));
        let edits = f.replace_edits(&buffers);
        assert_eq!(edits.len(), 2);
        for (path, edits) in edits {
            let text = buffers(&path).unwrap_or_else(|| std::fs::read_to_string(&path).unwrap());
            if path == open {
                let mut editor =
                    crate::editor::EditorState::from_text(&text, crate::editor::Language::Python);
                assert!(!editor.doc.is_modified());
                assert!(editor.apply_text_edits(edits.clone(), "Replace in Files"));
                assert_eq!(editor.doc.text(), "start_é\n中\n");
                assert!(editor.doc.is_modified());
                assert_eq!(std::fs::read_to_string(&open).unwrap(), "disk\n");
                assert!(editor.undo());
                assert_eq!(editor.doc.text(), text);
                assert!(!editor.doc.is_modified());
            }
            let edits: Vec<_> = edits
                .into_iter()
                .map(|(r, text)| (usize::from(r.start())..usize::from(r.end()), text))
                .collect();
            let expected = if path == open {
                "start_é\n中\n"
            } else {
                "start_x\nstart_y\n"
            };
            assert_eq!(rustpy_search::apply_edits(&text, &edits), expected);
        }
        assert_eq!(std::fs::read_to_string(open).unwrap(), "disk\n");
    }

    #[test]
    fn background_search_reads_unsaved_buffers_and_skips_excluded_folders() {
        let s = Scratch::new(
            "buffers",
            &[
                ("a.py", "needle = 1\n"),
                ("b.py", "x = 0\n"),
                ("gen/c.py", "needle()\n"),
            ],
        );
        let root = s.0.clone();
        let b = root.join("b.py");
        let mut f = FindInFiles {
            query: "needle".into(),
            open_files: vec![b.clone()],
            excluded: vec![root.join("gen")],
            ..Default::default()
        };
        // b.py has an unsaved needle on a line that is not on disk.
        let buffers = |p: &Path| (p == b).then(|| "x = 0\ny = needle\n".to_string());
        search(&mut f, &root, &buffers);
        assert!(f.error.is_none(), "{:?}", f.error);
        assert_eq!(
            found(&f, &root),
            vec![(PathBuf::from("a.py"), 0, 0), (PathBuf::from("b.py"), 1, 4)]
        );
        assert_eq!(
            f.results.files_searched, 2,
            "the excluded folder is not walked"
        );
        assert!(!f.is_searching());
    }

    #[test]
    fn a_new_search_cancels_the_one_still_running() {
        let s = Scratch::new("cancel", &[("a.py", "needle\n")]);
        let root = s.0.clone();
        let none = |_: &Path| None;
        let mut f = FindInFiles {
            query: "needle".into(),
            ..Default::default()
        };
        f.start(&root, &none);
        let first = f.job.as_ref().unwrap().control.clone();
        f.query = "needl".into();
        f.start(&root, &none);
        assert!(first.is_cancelled(), "the previous search is told to stop");
        assert!(!f.job.as_ref().unwrap().control.is_cancelled());
        // An invalid pattern reports an error instead of searching.
        f.options.regex = true;
        f.query = "(".into();
        f.start(&root, &none);
        assert!(f.job.is_none() && f.error.is_some());
    }

    #[test]
    fn replace_all_edits_the_editor_text_minus_unticked_occurrences() {
        // open.py is open with unsaved text; its disk copy has other matches.
        let s = Scratch::new(
            "replace",
            &[
                ("open.py", "foo foo foo foo\n"),
                ("closed.py", "foo = foo\n"),
            ],
        );
        let root = s.0.clone();
        let open = root.join("open.py");
        let buffer = "foo\nbar(foo, foo)\n";
        let buffers = |p: &Path| (p == open).then(|| buffer.to_string());
        let mut f = FindInFiles {
            query: "foo".into(),
            replacement: "baz".into(),
            replace_mode: true,
            open_files: vec![open.clone()],
            ..Default::default()
        };
        search(&mut f, &root, &buffers);
        assert_eq!(
            f.results.matches.len(),
            5,
            "3 in the buffer (not 4 on disk) + 2 in closed.py"
        );
        // Untick the second `foo` of `bar(foo, foo)`.
        f.set_skipped(&open, 1, 9, true);
        assert_eq!(f.included_count(), 4);
        let edits: HashMap<PathBuf, Vec<(TextRange, String)>> =
            f.replace_edits(&buffers).into_iter().collect();
        let ranges = |p: &Path| -> Vec<(std::ops::Range<usize>, String)> {
            edits[p]
                .iter()
                .map(|(r, t)| (usize::from(r.start())..usize::from(r.end()), t.clone()))
                .collect()
        };
        assert_eq!(
            ranges(&open),
            vec![(0..3, "baz".to_string()), (8..11, "baz".to_string())],
            "offsets are in the editor text"
        );
        assert_eq!(
            rustpy_search::apply_edits(buffer, &ranges(&open)),
            "baz\nbar(baz, foo)\n"
        );
        let closed = root.join("closed.py");
        assert_eq!(
            rustpy_search::apply_edits("foo = foo\n", &ranges(&closed)),
            "baz = baz\n",
            "closed files use the disk text"
        );
        // Ticking it again brings it back.
        f.set_skipped(&open, 1, 9, false);
        assert_eq!(
            f.replace_edits(&buffers)
                .iter()
                .map(|(_, e)| e.len())
                .sum::<usize>(),
            5
        );
    }

    fn result(path: &str, line: usize) -> SearchMatch {
        SearchMatch {
            path: PathBuf::from(path),
            line,
            start: 0,
            end: 6,
            line_text: "needle".into(),
        }
    }

    #[test]
    fn popup_keys_move_open_and_close() {
        let mut f = FindInFiles {
            results: SearchResults {
                matches: vec![result("a.py", 0), result("b.py", 3)],
                ..Default::default()
            },
            ..Default::default()
        };
        let down = PopupKeys {
            down: true,
            ..Default::default()
        };
        assert_eq!(f.on_keys(down, true), None);
        assert_eq!(f.on_keys(down, true), None);
        assert_eq!(f.selected, 1, "stops at the last result");
        assert_eq!(
            f.on_keys(
                PopupKeys {
                    up: true,
                    ..Default::default()
                },
                true
            ),
            None
        );
        assert_eq!(f.selected, 0);
        let enter = PopupKeys {
            enter: true,
            ..Default::default()
        };
        assert_eq!(f.on_keys(enter, true), Some(PopupKeyAction::Open(0)));
        // Typing in the file mask or the replacement: the keys are the field's ...
        assert_eq!(f.on_keys(enter, false), None);
        assert_eq!(f.on_keys(down, false), None);
        assert_eq!(f.selected, 0);
        // ... but Escape closes the popup from anywhere in it.
        assert_eq!(
            f.on_keys(
                PopupKeys {
                    escape: true,
                    ..Default::default()
                },
                false
            ),
            Some(PopupKeyAction::Close)
        );
        // Nothing to open yet (still searching).
        f.results.matches.clear();
        assert_eq!(f.on_keys(enter, true), None);
    }

    /// One headless frame of the popup with `events`; returns what it asks the app to do.
    fn popup_frame(
        ctx: &egui::Context,
        f: &mut FindInFiles,
        events: Vec<egui::Event>,
    ) -> Vec<NavEvent> {
        let theme = UiTheme::for_name("Dark");
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let mut out = Vec::new();
        ctx.run_ui(input, |ui| {
            out.extend(f.ui(ui.ctx(), &theme, Path::new("/nonexistent-root"), &|_| None))
        })
        .drop_without_applying_deltas();
        out
    }

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn enter_in_the_query_field_opens_the_selected_result_and_keeps_the_keyboard() {
        let ctx = egui::Context::default();
        // `last_query` = `query`: no search starts, the results below stay.
        let mut f = FindInFiles {
            visible: true,
            focus_query: true,
            query: "needle".into(),
            last_query: "needle".into(),
            ..Default::default()
        };
        assert!(popup_frame(&ctx, &mut f, vec![]).is_empty());
        // The pre-filled query is selected (typing replaces it): go to its end.
        popup_frame(&ctx, &mut f, vec![press(egui::Key::End)]);
        // Nothing found yet: Enter must not take the keyboard away from the query.
        assert!(popup_frame(&ctx, &mut f, vec![press(egui::Key::Enter)]).is_empty());
        popup_frame(&ctx, &mut f, vec![egui::Event::Text("s".into())]);
        assert_eq!(
            f.query, "needles",
            "typing after Enter still reaches the field"
        );
        assert!(f.visible);
        f.query = "needle".into();
        f.last_query = "needle".into();
        f.results.matches = vec![result("a.py", 0), result("b.py", 3)];
        popup_frame(&ctx, &mut f, vec![press(egui::Key::ArrowDown)]);
        assert_eq!(f.selected, 1);
        let events = popup_frame(&ctx, &mut f, vec![press(egui::Key::Enter)]);
        assert!(
            matches!(events.as_slice(), [NavEvent::OpenFile { path, line: Some(3), .. }] if path == Path::new("b.py")),
            "Enter opens the selected result"
        );
        assert!(!f.visible, "and closes the popup");
    }

    #[test]
    fn result_keys_after_a_click_on_a_result_but_not_after_another_field_took_the_keyboard() {
        let ctx = egui::Context::default();
        let mut f = FindInFiles {
            visible: true,
            focus_query: true,
            query: "needle".into(),
            last_query: "needle".into(),
            ..Default::default()
        };
        f.results.matches = vec![result("a.py", 0), result("b.py", 3)];
        popup_frame(&ctx, &mut f, vec![]);
        let query = ctx
            .memory(|m| m.focused())
            .expect("the query field has the keyboard");
        // A click on a result row: the query field gives the keyboard up, nothing has it.
        ctx.memory_mut(|m| m.surrender_focus(query));
        popup_frame(&ctx, &mut f, vec![press(egui::Key::ArrowDown)]);
        assert_eq!(f.selected, 1, "the arrows still move through the results");
        // Another window's field takes the keyboard, then drops it on its own Enter:
        // that Enter is not the popup's.
        let other = egui::Id::new("go to line field");
        ctx.memory_mut(|m| m.request_focus(other));
        popup_frame(&ctx, &mut f, vec![]);
        ctx.memory_mut(|m| m.surrender_focus(other));
        assert!(popup_frame(&ctx, &mut f, vec![press(egui::Key::Enter)]).is_empty());
        assert!(f.visible);
        // Back in the query field, Enter opens the selected result.
        f.focus_query = true;
        popup_frame(&ctx, &mut f, vec![]);
        let events = popup_frame(&ctx, &mut f, vec![press(egui::Key::Enter)]);
        assert!(
            matches!(events.as_slice(), [NavEvent::OpenFile { path, line: Some(3), .. }] if path == Path::new("b.py"))
        );
    }

    #[test]
    fn escape_closes_the_popup_after_enter_and_without_it() {
        let ctx = egui::Context::default();
        for keys in [
            vec![press(egui::Key::Escape)],
            vec![press(egui::Key::Enter), press(egui::Key::Escape)],
        ] {
            let mut f = FindInFiles {
                visible: true,
                focus_query: true,
                query: "x".into(),
                last_query: "x".into(),
                ..Default::default()
            };
            popup_frame(&ctx, &mut f, vec![]);
            for key in keys {
                assert!(f.visible);
                popup_frame(&ctx, &mut f, vec![key]);
            }
            assert!(!f.visible, "Escape closes the popup");
        }
    }

    #[test]
    fn project_scope_searches_content_roots_and_a_picked_excluded_folder() {
        let s = Scratch::new(
            "scope",
            &[
                ("app/src/a.py", "needle\n"),
                ("app/vendor/v.py", "needle\n"),
                ("shared/s.py", "needle\n"),
                ("other/o.py", "needle\n"),
            ],
        );
        let root = s.0.join("app");
        let scope = ProjectScope::new(&root)
            .with_content_roots([s.0.join("shared")])
            .with_excluded([root.join("vendor")]);
        let mut f = FindInFiles {
            query: "needle".into(),
            excluded: vec![root.join("vendor")],
            ..Default::default()
        };
        f.set_project_scope(Some(scope));
        search(&mut f, &root, &|_| None);
        let files: Vec<PathBuf> = f.results.matches.iter().map(|m| m.path.clone()).collect();
        assert_eq!(
            files,
            vec![root.join("src/a.py"), s.0.join("shared/s.py")],
            "content roots are searched, excluded folders are not"
        );
        // Test files: the content roots too, with the test file mask.
        std::fs::write(s.0.join("shared/test_s.py"), "needle\n").unwrap();
        f.scope = SearchScope::Tests;
        search(&mut f, &root, &|_| None);
        assert_eq!(
            found(&f, &s.0),
            vec![(PathBuf::from("shared/test_s.py"), 0, 0)]
        );
        // An excluded folder picked in the Project view is searched as picked.
        f.open_in_directory(root.join("vendor"), false, None);
        search(&mut f, &root, &|_| None);
        assert_eq!(found(&f, &root), vec![(PathBuf::from("vendor/v.py"), 0, 0)]);
    }

    #[test]
    fn go_to_file_uses_the_cached_index() {
        let s = Scratch::new(
            "goto",
            &[
                ("src/app.py", "def needle():\n    pass\n"),
                ("vendor/needle_lib.py", "class Needle:\n    needle = 1\n"),
            ],
        );
        let root = s.0.clone();
        let scope = ProjectScope::new(&root).with_excluded([root.join("vendor")]);
        let index = FileIndex::new();
        let mut se = SearchEverywhere::default();
        se.set_index(index.clone(), Some(scope.clone()));
        se.open(EverywhereTab::Files);
        se.query = "needle".into();
        se.rebuild(&root, &Keymap::default(), &[]);
        assert!(
            se.file_indexing && se.items.is_empty(),
            "no walk on the UI thread before the index exists"
        );
        index.rebuild_now(ProjectScope::new(&root));
        assert_ne!(
            index.generation(),
            se.file_index_generation,
            "a finished build refreshes the popup"
        );
        se.rebuild(&root, &Keymap::default(), &[]);
        assert!(!se.file_indexing);
        // The index was built before `vendor` was excluded: the popup still leaves it out.
        let paths: Vec<PathBuf> = se
            .items
            .iter()
            .filter_map(|i| {
                if let NavItem::File { path, .. } = i {
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect();
        assert!(paths.is_empty(), "{paths:?}");
        se.query = "app".into();
        se.rebuild(&root, &Keymap::default(), &[]);
        assert!(
            matches!(se.items.first(), Some(NavItem::File { path, .. }) if path.ends_with("src/app.py"))
        );
    }

    #[test]
    fn directory_scope_follows_the_folder_picked_in_the_project_view() {
        let s = Scratch::new(
            "dir",
            &[("src/a.py", "needle\n"), ("tests/test_a.py", "needle\n")],
        );
        let root = s.0.clone();
        let mut f = FindInFiles {
            current_dir: Some(root.join("src")),
            ..Default::default()
        };
        f.open_in_directory(root.join("tests"), true, Some("needle".into()));
        assert!(f.visible && f.replace_mode);
        assert_eq!(f.scope, SearchScope::CurrentDirectory);
        assert_eq!(f.search_root(&root), root.join("tests"));
        search(&mut f, &root, &|_| None);
        assert_eq!(
            found(&f, &root),
            vec![(PathBuf::from("tests/test_a.py"), 0, 0)]
        );
        // A plain Find in Files goes back to the current file's directory.
        f.open(false, None);
        assert_eq!(f.search_root(&root), root.join("src"));
    }
    #[test]
    fn everywhere_actual_query_keys_publish_selection_tree_and_escape_removes_owner() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("α.py");
        let second = root.path().join("β space.py");
        std::fs::write(&first, "x = 1\n").unwrap();
        std::fs::write(&second, "y = 2\n").unwrap();
        let mut popup = SearchEverywhere {
            recent_files: vec![first.clone(), second.clone()],
            ..Default::default()
        };
        popup.open(EverywhereTab::Files);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let theme = UiTheme::dark();
        let keymap = Keymap::default_keymap();
        let pass_diagnostics = std::cell::RefCell::new(Vec::new());
        let paint = |popup: &mut SearchEverywhere, time, key: Option<egui::Key>| {
            let events = key
                .map(|key| {
                    vec![
                        egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                        egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: false,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                })
                .unwrap_or_default();
            let mut events_out = Vec::new();
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1000.0, 650.0),
                    )),
                    ..Default::default()
                },
                |_| {
                    let before = format!(
                        "selected={} query={:?} last_query={:?} visible={} focus={:?}",
                        popup.selected,
                        popup.query,
                        popup.last_query,
                        popup.visible,
                        ctx.memory(|memory| memory.focused())
                    );
                    let emitted = popup.ui(&ctx, &theme, root.path(), &keymap);
                    let descriptions: Vec<_> = emitted
                        .iter()
                        .map(|event| match event {
                            NavEvent::SearchMemory(_) => "SearchMemory".to_string(),
                            NavEvent::OpenFile { path, line, column } => {
                                format!("OpenFile({path:?}, {line:?}, {column:?})")
                            }
                            NavEvent::Status(message) => format!("Status({message:?})"),
                            NavEvent::RunAction(action) => format!("RunAction({action:?})"),
                            NavEvent::SearchDestination(_) => "SearchDestination".to_string(),
                            _ => "other navigation event".to_string(),
                        })
                        .collect();
                    pass_diagnostics.borrow_mut().push(format!(
                        "time={time} key={key:?} pass={} before=[{before}] after=[selected={} query={:?} last_query={:?} visible={} focus={:?}] events={descriptions:?}",
                        ctx.current_pass_index(),
                        popup.selected,
                        popup.query,
                        popup.last_query,
                        popup.visible,
                        ctx.memory(|memory| memory.focused())
                    ));
                    events_out.extend(emitted);
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            (update, events_out)
        };
        paint(&mut popup, 0.0, None);
        paint(&mut popup, 0.1, None);
        let query_owner = ctx.memory(|memory| memory.focused()).unwrap();
        let (update, events) = paint(&mut popup, 0.2, Some(egui::Key::ArrowDown));
        assert!(events.is_empty());
        assert_eq!(popup.selected, 1);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(query_owner),
            "ArrowDown selects a result while the query retains keyboard ownership"
        );
        let status = update
            .nodes
            .iter()
            .find(|(_, n)| {
                n.role() == egui::accesskit::Role::Status
                    && n.label()
                        .is_some_and(|s| s.starts_with("Search Everywhere."))
            })
            .expect("actual focused query announcement");
        assert!(status
            .1
            .label()
            .unwrap()
            .contains("Result 2 of 2: File β space.py"));
        assert!(update
            .nodes
            .iter()
            .any(|(_, n)| n.label() == Some("File β space.py") && n.is_selected() == Some(true)));
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| n.children().contains(&status.0)),
            "status is linked to actual query owner"
        );
        let (_, events) = paint(&mut popup, 0.3, Some(egui::Key::Enter));
        assert!(
            matches!(events.as_slice(), [NavEvent::SearchMemory(_), NavEvent::OpenFile { path, .. }] if path == &second),
            "activation must survive all egui passes; got {} events\n{}",
            events.len(),
            pass_diagnostics.borrow().join("\n")
        );
        assert!(!popup.visible);
        let (update, _) = paint(&mut popup, 0.4, None);
        assert!(!update
            .nodes
            .iter()
            .any(|(_, n)| n.role() == egui::accesskit::Role::Status
                && n.label()
                    .is_some_and(|s| s.starts_with("Search Everywhere."))));
        popup.open(EverywhereTab::Files);
        paint(&mut popup, 0.5, None);
        paint(&mut popup, 0.6, Some(egui::Key::Escape));
        assert!(!popup.visible);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "x = 1\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "y = 2\n");
    }
    #[test]
    fn find_in_files_actual_keys_announce_unicode_match_and_preserve_field_ownership() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("α space.py");
        let source = "π needle\n😀 needle\n";
        std::fs::write(&path, source).unwrap();
        let mut popup = FindInFiles::default();
        popup.open(false, Some("needle".into()));
        search(&mut popup, root.path(), &|_| None);
        popup.last_query = popup.query.clone();
        popup.last_options = popup.options.clone();
        assert_eq!(popup.results.matches.len(), 2);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let theme = UiTheme::dark();
        let paint = |popup: &mut FindInFiles, time, key: Option<egui::Key>| {
            let events = key
                .map(|key| {
                    vec![
                        egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                        egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: false,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                })
                .unwrap_or_default();
            let mut events_out = Vec::new();
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1100.0, 800.0),
                    )),
                    ..Default::default()
                },
                |_| events_out.extend(popup.ui(&ctx, &theme, root.path(), &|_| None)),
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            (update, events_out)
        };
        paint(&mut popup, 0.0, None);
        paint(&mut popup, 0.1, None);
        assert!(
            popup.pending_since.is_none(),
            "an explicit completed search already captured these inputs"
        );
        assert!(
            popup.job.is_none(),
            "painting must not restart identical search work"
        );
        let (update, _) = paint(&mut popup, 0.2, Some(egui::Key::ArrowDown));
        assert_eq!(popup.selected, 1);
        let status = update
            .nodes
            .iter()
            .find(|(_, n)| {
                n.role() == egui::accesskit::Role::Status
                    && n.label().is_some_and(|s| s.starts_with("Find in Files."))
            })
            .expect("real query-owned current result");
        assert!(status
            .1
            .label()
            .unwrap()
            .contains("Result 2 of 2: α space.py: line 2, column 3: 😀 needle"));
        assert!(update
            .nodes
            .iter()
            .any(|(_, n)| n.children().contains(&status.0)));
        assert!(update.nodes.iter().any(|(_, n)| n
            .label()
            .is_some_and(|s| s.starts_with("α space.py: line 2, column 3"))
            && n.is_selected() == Some(true)));
        paint(&mut popup, 0.3, Some(egui::Key::Tab));
        paint(&mut popup, 0.4, None);
        let (update, events) = paint(&mut popup, 0.5, Some(egui::Key::ArrowUp));
        assert!(events.is_empty());
        assert_eq!(
            popup.selected, 1,
            "foreign toolbar focus retains its key ownership"
        );
        assert!(!update
            .nodes
            .iter()
            .any(|(_, n)| n.role() == egui::accesskit::Role::Status
                && n.label().is_some_and(|s| s.starts_with("Find in Files."))));
        popup.focus_query = true;
        paint(&mut popup, 0.6, None);
        let (_, events) = paint(&mut popup, 0.7, Some(egui::Key::Enter));
        assert!(
            matches!(events.as_slice(), [NavEvent::OpenFile { path: actual, line: Some(1), column: Some(5) }] if actual == &path)
        );
        assert!(!popup.visible);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
        popup.open(false, Some("[".into()));
        popup.options.regex = true;
        search(&mut popup, root.path(), &|_| None);
        assert!(popup.error.is_some(), "real invalid regex diagnosis");
        popup.last_query = popup.query.clone();
        popup.last_options = popup.options.clone();
        popup.query = "needle".into();
        paint(&mut popup, 0.8, None);
        let (update, _) = paint(&mut popup, 0.85, None);
        assert!(popup.pending_since.is_some());
        assert!(
            popup.error.is_some(),
            "old error is retained until the debounce starts"
        );
        let status = update
            .nodes
            .iter()
            .find(|(_, n)| {
                n.role() == egui::accesskit::Role::Status
                    && n.label().is_some_and(|s| s.starts_with("Find in Files."))
            })
            .expect("current pending query announcement");
        assert!(status.1.label().unwrap().contains("Searching;"));
        assert!(!status.1.label().unwrap().contains("Search error:"));
    }
    #[test]
    fn unsaved_symbol_overlay_keeps_variables_and_receiver_attributes() {
        let symbols = symbols_in_text(
            Path::new("/unsaved-symbol-index/current.py"),
            "value=1\nclass C:\n def __init__(this):\n  this.item=1\n def method(this): pass\n",
        );
        assert!(symbols
            .iter()
            .any(|(name, detail, _, line)| name == "value" && !detail.is_empty() && *line == 0));
        assert!(symbols
            .iter()
            .any(|(name, detail, _, line)| name == "C" && detail.is_empty() && *line == 1));
        assert!(symbols
            .iter()
            .any(|(name, detail, _, line)| name == "C.item" && !detail.is_empty() && *line == 3));
        assert!(symbols
            .iter()
            .any(|(name, detail, _, _)| name == "C.method" && !detail.is_empty()));
    }
    #[test]
    fn indexed_and_unsaved_member_queries_use_same_leaf_and_container() {
        let source = "class C:\n def __init__(self):\n  self.item=1\n";
        let scratch = Scratch::new("symbol-overlay-parity", &[("model.py", source)]);
        let mut search = SearchEverywhere {
            tab: EverywhereTab::Symbols,
            query: "item".into(),
            ..Default::default()
        };
        search.index = IndexSnapshot {
            generation: 1,
            project: Some(project_index(&scratch.0)),
            ..Default::default()
        };
        let keymap = Keymap::default_keymap();
        search.rebuild(&scratch.0, &keymap, &[]);
        let indexed: Vec<_> = search
            .items
            .iter()
            .filter_map(|item| match item {
                NavItem::Symbol {
                    name,
                    container,
                    line,
                    ..
                } => Some((name.clone(), container.clone(), *line)),
                _ => None,
            })
            .collect();
        assert_eq!(indexed, [("item".to_owned(), "C".to_owned(), 2)]);
        search.set_unsaved([(scratch.0.join("model.py"), source.to_owned())]);
        search.rebuild(&scratch.0, &keymap, &[]);
        let unsaved: Vec<_> = search
            .items
            .iter()
            .filter_map(|item| match item {
                NavItem::Symbol {
                    name,
                    container,
                    line,
                    ..
                } => Some((name.clone(), container.clone(), *line)),
                _ => None,
            })
            .collect();
        assert_eq!(unsaved, indexed);
        assert!(search
            .import_fix_symbols()
            .all(|symbol| !symbol.0.contains('.')));
    }
}

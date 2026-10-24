//! Navigation in the IDE window (roadmap §2.7, §1.5): Go to Declaration or
//! Usages and Show Usages, Go to Type Declaration, Go to Test (and Create New
//! Test), the navigation history behind Back/Forward and Recent Locations,
//! Recent Files and the Switcher, and the editor/Find-window side of Find and
//! Replace in Files.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::nav_history::{NavHistory, NavPoint};
use crate::search_ui::FileEdits;
use crate::switcher::{
    FileEntry, PlaceEntry, PopupOutcome, RecentFilesPopup, RecentLocationsPopup, SwitcherPopup,
    ToolEntry,
};
use rustpy_search::SearchMatch;
use rustpy_test::related;

/// Tool windows listed by Recent Files and the Switcher.
const TOOL_WINDOWS: &[(&str, Action)] = &[
    ("Project", Action::ToggleProjectView),
    ("Bookmarks", Action::ToggleBookmarksView),
    ("Structure", Action::ToggleStructureView),
    ("Find", Action::ToggleFindView),
    ("Run", Action::ToggleRunView),
    ("Debug", Action::ToggleDebugView),
    ("Problems", Action::ToggleProblemsView),
    ("Terminal", Action::ToggleTerminal),
    ("Python Console", Action::TogglePythonConsole),
    ("Git", Action::ToggleGitView),
    ("TODO", Action::ToggleTodoView),
    ("Services", Action::ToggleServicesView),
    ("Notifications", Action::ToggleNotificationsView),
    ("AI Assistant", Action::ToggleAiAssistant),
    ("Learn", Action::LearnIdeFeatures),
    ("Python Packages", Action::TogglePythonPackages),
    ("Tests", Action::ToggleTestsView),
    ("Coverage", Action::ToggleCoverageView),
    ("Profiler", Action::ToggleProfilerView),
    ("Concurrency Diagram", Action::ToggleConcurrencyView),
    ("HTTP Client", Action::ToggleHttpClientView),
    ("Database", Action::ToggleDatabaseView),
    ("Web Preview", Action::ToggleWebPreview),
    ("Endpoints", Action::ToggleEndpointsView),
    ("Diagram", Action::ToggleDiagramView),
    ("Documentation", Action::ToggleDocumentationView),
    ("SciView", Action::ToggleSciView),
    ("Branches", Action::ToggleBranchesView),
    ("Pull Requests", Action::GitHubPullRequests),
    ("Other VCS", Action::ToggleOtherVcsView),
    ("Hierarchy", Action::ToggleHierarchyView),
];

/// What a row of a navigation chooser (`ChoiceKind::Nav`) does.
#[derive(Clone, Debug)]
pub(super) enum NavChoice {
    Go(PathBuf, TextRange),
    /// 0-based line and character column.
    GoLine(PathBuf, usize, usize),
    /// Show Usages: move the popup's results into the Find tool window.
    UsagesInFindWindow,
    /// Go to Test: write a new test module and open it.
    CreateTest,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum FindTab {
    #[default]
    Usages,
    Text,
}

/// Navigation state of the IDE window.
#[derive(Default)]
pub(super) struct NavState {
    pub(super) history: NavHistory,
    pub(super) details: super::navigation_history::HistoryRuntime,
    /// Active file at the last observation (the MRU order changes when it does).
    last_active: Option<PathBuf>,
    pub(super) recent_files: RecentFilesPopup,
    switcher: SwitcherPopup,
    recent_locations: RecentLocationsPopup,
    /// Rows of the open `ChoiceKind::Nav` popup.
    choices: Vec<NavChoice>,
    /// Show Usages results for "Open in Find Window": `(title, usages)`.
    popup_usages: Option<(String, Vec<Usage>)>,
    popup_usage_target: Option<super::usage_results_ui::Target>,
    popup_usage_hits: Option<Vec<rustpy_search::usages::results::Hit>>,
    /// Source file and symbols at the caret for *Create New Test…*.
    create_test: Option<(PathBuf, Vec<String>)>,
    /// Find in Files results pinned to the Find tool window: `(title, matches)`.
    pinned: Option<(String, Vec<SearchMatch>)>,
    find_tab: FindTab,
    seen_usages_title: String,
}

impl NavState {
    /// True while Recent Files, the Switcher or Recent Locations owns the keyboard.
    pub(super) fn popup_visible(&self) -> bool {
        self.recent_files.visible || self.switcher.visible || self.recent_locations.visible
    }
}

/// `a` and `b` name the same file (paths of open editors are canonical).
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

impl IdeApp {
    // ----- navigation history -------------------------------------------------

    /// The active editor's caret as a history place.
    pub(super) fn caret_point(&self) -> Option<NavPoint> {
        let tab = self.active.and_then(|i| self.tabs.get(i))?;
        let path = tab.state.path.clone()?;
        let pos = tab.state.primary_position();
        let mut point = NavPoint::new(path, pos.line as usize, pos.column as usize);
        point.context = Some(self.capture_navigation_context());
        Some(point)
    }

    /// Records the caret before an explicit jump (kept even when it is short).
    pub(super) fn mark_nav_jump(&mut self) {
        let point = self.navigation_point();
        self.nav.history.mark_jump(point);
    }

    /// Per-frame observer of the active editor: feeds the navigation history
    /// and keeps Recent Files in most-recently-used order (tab clicks,
    /// Alt+←/→, the Switcher, … all count).
    pub(super) fn observe_navigation(&mut self) {
        self.observe_navigation_edits();
        let context = self.capture_navigation_context();
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            let mut point = NavPoint::new(PathBuf::new(), 0, 0);
            point.context = Some(context);
            self.nav.history.observe(point, 0);
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let version = tab.state.doc.version();
        let pos = tab.state.primary_position();
        if self.nav.last_active.as_ref() != Some(&path) {
            self.nav.last_active = Some(path.clone());
            self.record_recent(&path);
            self.everywhere.note_recent(&path);
        }
        let mut point = NavPoint::new(path, pos.line as usize, pos.column as usize);
        point.context = Some(context);
        self.nav.history.observe(point, version);
    }

    /// Opens `path` with the caret at 0-based `line` and character `col`.
    pub(super) fn open_at(&mut self, path: &Path, line: usize, col: usize) {
        self.open_file(path);
        if let Some(tab) = self
            .active
            .and_then(|i| self.tabs.get_mut(i))
            .filter(|t| t.state.path.as_deref().is_some_and(|p| same_file(p, path)))
        {
            let off = tab
                .state
                .doc
                .position_to_offset(rustpy_core::Position::new(line as u32, col as u32));
            tab.state.reveal_offset(off);
            tab.state.set_carets(vec![crate::editor::Caret::at(off)]);
            tab.state.last_op = crate::editor::state::LastOp::Other;
        }
        self.focus_editor = true;
    }

    /// Back (Ctrl+Alt+←) / Forward (Ctrl+Alt+→).
    pub(super) fn nav_history_step(&mut self, back: bool) {
        let current = Some(self.navigation_point());
        let open: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.state.path.clone())
            .collect();
        let valid = |p: &NavPoint| {
            p.path.as_os_str().is_empty()
                || p.path.starts_with("<typeshed>")
                || p.path.exists()
                || open.contains(&p.path)
        };
        let target = if back {
            self.nav.history.back(current, valid)
        } else {
            self.nav.history.forward(current, valid)
        };
        match target {
            Some(p) => self.restore_navigation_point(p),
            None => self.set_status(if back {
                "No previous location"
            } else {
                "No next location"
            }),
        }
    }

    // ----- Recent Files, Switcher, Recent Locations -------------------------------

    fn relative_dir(&self, path: &Path) -> String {
        let dir = path.parent().unwrap_or(path);
        match self
            .project
            .as_ref()
            .and_then(|p| dir.strip_prefix(&p.root).ok())
        {
            Some(rel) => rel.display().to_string(),
            None => dir.display().to_string(),
        }
    }

    pub(super) fn file_entry(
        &self,
        path: &Path,
        current: Option<&Path>,
        edited: &HashSet<PathBuf>,
    ) -> FileEntry {
        let modified = self
            .tabs
            .iter()
            .any(|t| t.state.path.as_deref() == Some(path) && t.state.doc.is_modified());
        FileEntry {
            path: path.to_path_buf(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir: self.relative_dir(path),
            edited: modified || edited.contains(path),
            modified,
            current: current == Some(path),
        }
    }

    fn edited_files(&self) -> HashSet<PathBuf> {
        self.nav
            .history
            .changed_places()
            .into_iter()
            .map(|p| p.path)
            .collect()
    }

    pub(super) fn tool_entries(&self) -> Vec<ToolEntry> {
        TOOL_WINDOWS
            .iter()
            .map(|(label, a)| ToolEntry {
                label: Self::framework_tool_for_action(*a)
                    .map(layout::tool_ref_id)
                    .map(|id| self.layout.tools.saved.placement(&id).title)
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| (*label).to_string()),
                shortcut: self.keymap.shortcut_text(*a),
            })
            .collect()
    }

    /// Recent files, most recently used first (the current file on top).
    pub(super) fn recent_file_entries(&self) -> Vec<FileEntry> {
        let current = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.clone());
        let edited = self.edited_files();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out: Vec<FileEntry> = Vec::new();
        let open: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.state.path.clone())
            .collect();
        for p in current
            .iter()
            .chain(self.recent_files.iter())
            .chain(open.iter())
        {
            if !seen.insert(p.clone()) || p.starts_with("<typeshed>") {
                continue;
            }
            if p.exists() || open.contains(p) {
                out.push(self.file_entry(p, current.as_deref(), &edited));
            }
        }
        out
    }

    /// Recent Files (Ctrl+E); pressing it again toggles *Show edited only*.
    pub(super) fn open_recent_files_popup(&mut self) {
        if self.nav.recent_files.visible {
            self.nav.recent_files.toggle_edited();
            return;
        }
        let mut files = self.recent_file_entries();
        files.truncate(self.settings.navigation_history.recent_limit.clamp(10, 100));
        let tools = self.tool_entries();
        self.nav.recent_files.open(files, tools, false);
        self.focus_editor = false;
    }

    /// Switcher (Ctrl+Tab): open editors in MRU order; releasing the held
    /// modifiers activates the selection.
    pub(super) fn open_switcher_popup(&mut self, ctx: &egui::Context) {
        if self.nav.switcher.visible {
            return;
        }
        let current = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let edited = self.edited_files();
        let mut editors: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| {
                let mut row = match tab.state.path.as_deref() {
                    Some(path) => self.file_entry(path, None, &edited),
                    None => FileEntry {
                        path: PathBuf::new(),
                        name: tab.state.title(),
                        dir: "Unsaved editor".into(),
                        edited: tab.state.doc.is_modified(),
                        modified: tab.state.doc.is_modified(),
                        current: false,
                    },
                };
                row.current = Some(tab.view.id) == current;
                (tab.view.id, row)
            })
            .collect();
        // Saved editors retain MRU order; unvisited/pathless editors retain tab order.
        editors.sort_by_key(|(id, row)| {
            if Some(*id) == current {
                0
            } else {
                self.recent_files
                    .iter()
                    .position(|p| p == &row.path)
                    .map(|i| i + 1)
                    .unwrap_or(usize::MAX)
            }
        });
        let hold = ctx.input(|i| i.modifiers);
        let tools = self.tool_entries();
        self.nav.switcher.open_editors(editors, tools, hold);
        self.focus_editor = false;
    }

    /// Lines of a file like the editor counts them (a trailing newline starts
    /// an empty last line): the open editor, else the analysis copy (stubs),
    /// else the disk.
    fn file_lines(&self, path: &Path) -> Vec<String> {
        let split = |t: &str| -> Vec<String> {
            t.split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect()
        };
        if let Some(t) = self
            .tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(path))
        {
            return split(&t.state.doc.text());
        }
        if path.starts_with("<typeshed>") {
            if let Some(f) = self
                .project
                .as_ref()
                .and_then(|p| p.analysis.file_for_path(path))
            {
                return split(&f.text);
            }
        }
        std::fs::read_to_string(path)
            .map(|t| split(&t))
            .unwrap_or_default()
    }

    fn place_entry(
        &self,
        p: &NavPoint,
        cache: &mut HashMap<PathBuf, Vec<String>>,
    ) -> Option<PlaceEntry> {
        let lines = cache
            .entry(p.path.clone())
            .or_insert_with(|| self.file_lines(&p.path));
        if lines.is_empty() {
            return None;
        }
        let line = p.line.min(lines.len() - 1);
        let context = (line.saturating_sub(1)..=(line + 1).min(lines.len() - 1))
            .map(|l| (l, lines[l].clone()))
            .collect();
        let name = p
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut point = p.clone();
        point.line = line;
        Some(PlaceEntry {
            point,
            title: format!("{name}:{}", line + 1),
            dir: self.relative_dir(&p.path),
            lines: context,
        })
    }

    /// Recent Locations (Ctrl+Shift+E): visited and changed places with code.
    pub(super) fn open_recent_locations_popup(&mut self) {
        if self.nav.recent_locations.visible {
            self.nav.recent_locations.toggle_changed();
            return;
        }
        let current = self.caret_point();
        let mut cache = HashMap::new();
        let recent: Vec<PlaceEntry> = self
            .nav
            .history
            .recent_places()
            .into_iter()
            .filter(|p| !current.as_ref().is_some_and(|c| c.same_line(p)))
            .take(self.settings.navigation_history.recent_limit.clamp(10, 100))
            .filter_map(|p| self.place_entry(&p, &mut cache))
            .collect();
        let changed: Vec<PlaceEntry> = self
            .nav
            .history
            .changed_places()
            .into_iter()
            .take(self.settings.navigation_history.recent_limit.clamp(10, 100))
            .filter_map(|p| self.place_entry(&p, &mut cache))
            .collect();
        if recent.is_empty() && changed.is_empty() {
            self.set_status("No recent locations");
            return;
        }
        let changed_only = recent.is_empty();
        self.nav
            .recent_locations
            .open(recent, changed, changed_only);
        self.focus_editor = false;
    }

    /// Shows a tool window (the Switcher and Recent Files never toggle one off).
    fn show_tool_window(&mut self, action: Action) {
        if let Some(tool) = Self::framework_tool_for_action(action) {
            self.open_framework_tool(&layout::tool_ref_id(tool));
        }
        let bottom = match action {
            Action::ToggleFindView => Some(BottomTool::Find),
            Action::ToggleRunView => Some(BottomTool::Run),
            Action::ToggleDebugView => Some(BottomTool::Debug),
            Action::ToggleProblemsView => Some(BottomTool::Problems),
            Action::ToggleTerminal => Some(BottomTool::Terminal),
            Action::TogglePythonConsole => Some(BottomTool::PythonConsole),
            Action::ToggleGitView => Some(BottomTool::Git),
            Action::ToggleTodoView => Some(BottomTool::Todo),
            Action::ToggleServicesView => Some(BottomTool::Services),
            Action::ToggleNotificationsView => Some(BottomTool::Notifications),
            _ => None,
        };
        if let Some(tool) = bottom {
            self.bottom = Some(tool);
            match tool {
                BottomTool::Terminal => self.terminal_panel.request_focus(),
                BottomTool::PythonConsole => self.py_console.request_focus(),
                _ => {}
            }
            return;
        }
        let left = match action {
            Action::ToggleProjectView => Some(LeftTool::Project),
            Action::ToggleBookmarksView => Some(LeftTool::Bookmarks),
            Action::ToggleStructureView => Some(LeftTool::Structure),
            _ => None,
        };
        if let Some(l) = left {
            self.left_tool = l;
            self.layout.left_visible = true;
        } else if let Some(layout::ToolRef::Bottom(tool)) = Self::framework_tool_for_action(action)
        {
            self.bottom = Some(tool);
        }
    }

    /// Draws Recent Files, the Switcher and Recent Locations at the top level
    /// (so they work without an open editor) and acts on their outcome.
    pub(super) fn nav_popups(&mut self, ctx: &egui::Context) {
        if !self.nav.popup_visible() {
            return;
        }
        let theme = self.theme.clone();
        let recent_keys = self.keymap.shortcuts(Action::RecentFiles).to_vec();
        let switch_keys = self.keymap.shortcuts(Action::Switcher).to_vec();
        let location_keys = self.keymap.shortcuts(Action::RecentLocations).to_vec();
        let mono = egui::FontId::new(12.0, crate::theme::mono_family(false, false));
        let mut outcomes: Vec<PopupOutcome> = Vec::new();
        outcomes.extend(self.nav.recent_files.ui(ctx, &theme, &recent_keys));
        outcomes.extend(self.nav.switcher.ui(ctx, &theme, &switch_keys));
        outcomes.extend(
            self.nav
                .recent_locations
                .ui(ctx, &theme, &location_keys, mono),
        );
        for outcome in outcomes {
            match outcome {
                PopupOutcome::Cancel => self.focus_editor = true,
                PopupOutcome::File(path) => {
                    self.mark_nav_jump();
                    self.open_file(&path);
                }
                PopupOutcome::Editor(id) => {
                    // A closed captured editor is never replaced by a different tab at its index.
                    if let Some(i) = self.tabs.iter().position(|t| t.view.id == id) {
                        self.active = Some(i);
                        self.group_active[self.tabs[i].group as usize] = Some(id);
                        if let Some(path) = self.tabs[i].state.path.clone() {
                            self.record_recent(&path);
                        }
                    }
                    self.focus_editor = true;
                }
                PopupOutcome::Tool(i) => {
                    if let Some((_, action)) = TOOL_WINDOWS.get(i) {
                        self.show_tool_window(*action);
                    }
                }
                PopupOutcome::Place(p) => {
                    self.mark_nav_jump();
                    self.restore_navigation_point(p);
                }
            }
        }
    }

    // ----- Go to Declaration or Usages, Show Usages ------------------------------

    /// 1-based line of `offset` in `path` (open editor, analysis copy — stubs
    /// included — or disk).
    pub(super) fn location_line(&self, path: &Path, offset: TextSize) -> usize {
        if let Some(t) = self
            .tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(path))
        {
            return t.state.doc.line_of(offset) + 1;
        }
        let text: Option<String> = self
            .project
            .as_ref()
            .and_then(|p| p.analysis.file_for_path(path))
            .map(|f| f.text.to_string())
            .or_else(|| std::fs::read_to_string(path).ok());
        // Counted in bytes: the offset need not be a char boundary of a text that changed on disk.
        text.map(|t| {
            t.as_bytes()[..usize::from(offset).min(t.len())]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1
        })
        .unwrap_or(1)
    }

    /// `path` relative to the project root when it is inside it.
    pub(super) fn display_path(&self, path: &Path) -> String {
        match self
            .project
            .as_ref()
            .and_then(|p| path.strip_prefix(&p.root).ok())
        {
            Some(rel) => rel.display().to_string(),
            None => path.display().to_string(),
        }
    }

    /// Goes to the only location, or offers a chooser titled `title`.
    fn choose_locations(&mut self, locs: Vec<features::Location>, title: &str, empty: &str) {
        match locs.len() {
            0 => self.set_status(empty),
            1 => self.navigate_to(&locs[0].path.clone(), locs[0].range),
            _ => {
                let items: Vec<(String, String)> = locs
                    .iter()
                    .map(|l| {
                        (
                            l.path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            format!(
                                "{}:{}",
                                self.display_path(&l.path),
                                self.location_line(&l.path, l.range.start())
                            ),
                        )
                    })
                    .collect();
                self.nav.choices = locs
                    .into_iter()
                    .map(|l| NavChoice::Go(l.path, l.range))
                    .collect();
                self.choice = Some(ChoicePopup::new(ChoiceKind::Nav, title.to_string(), items));
            }
        }
    }

    /// Show Usages (Ctrl+Alt+F7) at the caret.
    pub(super) fn show_usages_popup(&mut self) {
        let Some(off) = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.state.primary().head())
        else {
            return;
        };
        self.show_usages_at(off);
    }

    /// Show Usages: one usage is opened, several are listed in a popup at
    /// the caret (with "Open in Find Window"). Also what Ctrl+B / Ctrl+Click
    /// do on a declaration.
    pub(super) fn show_usages_at(&mut self, offset: TextSize) {
        let Some(i) = self.active else { return };
        self.sync_analysis_now();
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Show Usages works on Python files in a project");
            return;
        };
        let name = self.tabs[i]
            .state
            .doc
            .word_range_at(offset)
            .map(|r| self.tabs[i].state.doc.slice(r))
            .unwrap_or_default();
        let files = self.project_files();
        let options = self.usage_options(&file.path);
        let hits = rustpy_search::usages::results::search(
            &analysis,
            &file,
            offset,
            &files,
            &options,
            &rustpy_core::ProgressReporter::new(),
        )
        .unwrap_or_default();
        let all: Vec<Usage> = hits.iter().map(|hit| hit.usage.clone()).collect();
        let found = all.clone();
        self.nav.popup_usage_target = Some(super::usage_results_ui::Target {
            path: file.path.clone(),
            offset,
            name: name.clone(),
        });
        match found.len() {
            0 if all.is_empty() => self.set_status(format!("Cannot find usages of '{name}'")),
            0 => self.set_status(format!("No usages of '{name}' found in the project")),
            1 => {
                let (path, range) = (found[0].path.clone(), found[0].range);
                self.navigate_to(&path, range);
                self.set_status(format!("The only usage of '{name}'"));
            }
            n => {
                let mut items: Vec<(String, String)> = found
                    .iter()
                    .map(|u| {
                        (
                            format!(
                                "{}:{}",
                                u.path
                                    .file_name()
                                    .map(|f| f.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                u.line + 1
                            ),
                            u.line_text.trim().to_string(),
                        )
                    })
                    .collect();
                let mut choices: Vec<NavChoice> = found
                    .iter()
                    .map(|u| NavChoice::Go(u.path.clone(), u.range))
                    .collect();
                items.push((
                    "Open in Find Window".into(),
                    format!("all {} result(s) of '{name}'", all.len()),
                ));
                choices.push(NavChoice::UsagesInFindWindow);
                self.nav.choices = choices;
                self.nav.popup_usage_hits = Some(hits);
                self.nav.popup_usages = Some((format!("Usages of '{name}' ({})", all.len()), all));
                self.choice = Some(ChoicePopup::new(
                    ChoiceKind::Nav,
                    format!("Usages of '{name}' ({n})"),
                    items,
                ));
            }
        }
    }

    /// A row of a `ChoiceKind::Nav` popup was chosen.
    pub(super) fn apply_nav_choice(&mut self, idx: usize) {
        match self.nav.choices.get(idx).cloned() {
            Some(NavChoice::Go(path, range)) => self.navigate_to(&path, range),
            Some(NavChoice::GoLine(path, line, col)) => {
                self.mark_nav_jump();
                self.open_at(&path, line, col);
            }
            Some(NavChoice::UsagesInFindWindow) => {
                if let Some((title, list)) = self.nav.popup_usages.take() {
                    self.usages_title = title.clone();
                    self.usage_ui.adopt(
                        title.clone(),
                        self.nav.popup_usage_hits.take().unwrap_or_else(|| {
                            list.iter()
                                .cloned()
                                .map(rustpy_search::usages::results::Hit::from_usage)
                                .collect()
                        }),
                    );
                    self.usage_ui.target = self.nav.popup_usage_target.take();
                    self.usages = list;
                    self.nav.seen_usages_title = self.usages_title.clone();
                    self.nav.find_tab = FindTab::Usages;
                    self.bottom = Some(BottomTool::Find);
                }
            }
            Some(NavChoice::CreateTest) => self.create_test(),
            None => {}
        }
    }

    // ----- Go to Type Declaration, Go to Test ----------------------------------

    /// Go to Type Declaration (Ctrl+Shift+B).
    pub(super) fn go_to_type_declaration(&mut self) {
        self.sync_analysis_now();
        let Some(i) = self.active else { return };
        let caret = self.tabs[i].state.primary().head();
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Go to Type Declaration works on Python files in a project");
            return;
        };
        let locs = features::type_definition_at(&analysis, &file, caret);
        self.choose_locations(
            locs,
            "Choose Type Declaration",
            "Cannot find type declaration",
        );
    }

    /// Go to Test (Ctrl+Shift+T): from a module to its tests, from a test to
    /// its subject; with no test, offers *Create New Test…*.
    pub(super) fn go_to_test(&mut self) {
        let Some(i) = self.active else { return };
        let Some(path) =
            self.tabs[i].state.path.clone().filter(|p| {
                p.extension().is_some_and(|e| e == "py") && !p.starts_with("<typeshed>")
            })
        else {
            self.set_status("Go to Test works on Python files");
            return;
        };
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let symbols: Vec<String> = self.tabs[i]
            .state
            .breadcrumbs()
            .into_iter()
            .map(|(n, _)| n.trim_end_matches("()").to_string())
            .collect();
        let syms: Vec<&str> = symbols.iter().map(String::as_str).collect();
        let is_test = related::is_test_module(&root, &path);
        let targets = if is_test {
            related::subject_candidates(&root, &path, &syms)
        } else {
            related::test_candidates(&root, &path, &syms)
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let new_test = related::new_test_path(&root, &path);
        match targets.len() {
            0 if is_test => self.set_status(format!("No test subject found for {name}")),
            0 => {
                self.nav.create_test = Some((path.clone(), symbols.clone()));
                self.nav.choices = vec![NavChoice::CreateTest];
                self.choice = Some(ChoicePopup::new(
                    ChoiceKind::Nav,
                    format!("No tests found for {name}"),
                    vec![("Create New Test…".into(), self.display_path(&new_test))],
                ));
            }
            1 => {
                let t = &targets[0];
                let (line, col) = t.position.unwrap_or((0, 0));
                let target = t.path.clone();
                self.mark_nav_jump();
                self.open_at(&target, line, col);
            }
            _ => {
                let mut items: Vec<(String, String)> = Vec::new();
                let mut choices: Vec<NavChoice> = Vec::new();
                for t in &targets {
                    let file = t
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let symbol = t
                        .symbol
                        .as_ref()
                        .map(|s| format!("  ·  {s}"))
                        .unwrap_or_default();
                    items.push((file, format!("{}{symbol}", self.display_path(&t.path))));
                    let (line, col) = t.position.unwrap_or((0, 0));
                    choices.push(NavChoice::GoLine(t.path.clone(), line, col));
                }
                if !is_test {
                    items.push(("Create New Test…".into(), self.display_path(&new_test)));
                    choices.push(NavChoice::CreateTest);
                    self.nav.create_test = Some((path.clone(), symbols.clone()));
                }
                self.nav.choices = choices;
                let title = if is_test {
                    format!("Choose Test Subject for {name}")
                } else {
                    format!("Choose Test for {name}")
                };
                self.choice = Some(ChoicePopup::new(ChoiceKind::Nav, title, items));
            }
        }
    }

    /// Create New Test…: writes `tests/test_<module>.py` (pytest or unittest,
    /// as the project uses) with a test for the symbol at the caret, and opens it.
    fn create_test(&mut self) {
        let Some((src, _symbols)) = self.nav.create_test.take() else {
            return;
        };
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .or_else(|| src.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let target = related::new_test_path(&root, &src);
        if target.exists() {
            self.mark_nav_jump();
            self.open_file(&target);
            return;
        }
        self.open_generate_test_picker(src);
    }

    // ----- Find / Replace in Files ---------------------------------------------

    /// Applies Replace in Files edits: open editors get one undoable step each;
    /// closed files are rewritten on disk. Either way the previous text is kept
    /// as a Local History snapshot.
    pub(super) fn apply_file_edits(&mut self, edits: FileEdits, label: &str) {
        let analysis = self.project.as_ref().map(|p| p.analysis.clone());
        let before = format!("Before {label}");
        let (mut count, mut files, mut on_disk) = (0usize, 0usize, 0usize);
        for (path, file_edits) in edits {
            if file_edits.is_empty() {
                continue;
            }
            let n = file_edits.len();
            if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.state.path.as_deref() == Some(path.as_path()))
            {
                self.snapshot(i, &before);
                if self.tabs[i].state.apply_text_edits(file_edits, label) {
                    count += n;
                    files += 1;
                }
                continue;
            }
            let python = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("py" | "pyi")
            );
            let result = super::refactor_ui::transform_closed_file(&path, python, |text| {
                let ranges: Vec<_> = file_edits
                    .into_iter()
                    .map(|(r, s)| (usize::from(r.start())..usize::from(r.end()), s))
                    .collect();
                let next = rustpy_search::apply_edits(text, &ranges);
                let changed = next != text;
                if changed {
                    self.snapshot_text(&path, text, &before);
                }
                Ok((next, changed))
            });
            if matches!(result, Ok(true)) {
                count += n;
                files += 1;
                on_disk += 1;
                if let Some(a) = &analysis {
                    a.close_file(&path);
                }
            }
        }
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
            p.tree.invalidate_all();
        }
        let undo = if on_disk > 0 {
            format!(
                "Undo (Ctrl+Z) reverts open editors, Local History the {on_disk} closed file(s)"
            )
        } else {
            "Undo (Ctrl+Z) reverts it".to_string()
        };
        self.set_status(format!(
            "Replaced {count} occurrence(s) in {files} file(s); {undo}"
        ));
    }

    pub(super) fn select_usage_results(&mut self) {
        self.nav.find_tab = FindTab::Usages;
        self.bottom = Some(BottomTool::Find);
    }

    /// Keeps Find in Files results in the Find tool window.
    pub(super) fn pin_find_results(&mut self, title: String, matches: Vec<SearchMatch>) {
        self.file_results.active = None;
        self.nav.pinned = Some((title, matches));
        self.nav.find_tab = FindTab::Text;
        self.bottom = Some(BottomTool::Find);
    }

    /// The Find tool window: Find Usages results and pinned Find in Files results.
    pub(super) fn find_tool_view(&mut self, ui: &mut egui::Ui) {
        if self.usages_title != self.nav.seen_usages_title {
            // A new Find Usages search: show it.
            self.nav.seen_usages_title = self.usages_title.clone();
            self.nav.find_tab = FindTab::Usages;
        }
        if self.nav.pinned.is_none() {
            self.nav.find_tab = FindTab::Usages;
        } else if self.usages.is_empty()
            && self.usage_ui.target.is_none()
            && !self.usage_ui.settings
        {
            self.nav.find_tab = FindTab::Text;
        }
        if let Some((title, _)) = &self.nav.pinned {
            let title = title.clone();
            let mut close = false;
            ui.horizontal(|ui| {
                if !self.usages.is_empty()
                    && ui
                        .selectable_label(self.nav.find_tab == FindTab::Usages, &self.usages_title)
                        .clicked()
                {
                    self.nav.find_tab = FindTab::Usages;
                }
                if ui
                    .selectable_label(self.nav.find_tab == FindTab::Text, &title)
                    .clicked()
                {
                    self.nav.find_tab = FindTab::Text;
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Close these search results")
                    .clicked()
                {
                    close = true;
                }
            });
            ui.separator();
            if close {
                self.nav.pinned = None;
                self.nav.find_tab = FindTab::Usages;
            }
        }
        match self.nav.find_tab {
            FindTab::Usages => self.usages_view(ui),
            FindTab::Text => self.pinned_results_view(ui),
        }
    }

    fn pinned_results_view(&mut self, ui: &mut egui::Ui) {
        if self.file_results.active.is_some() {
            self.file_search_results_view(ui);
            return;
        }
        let Some((_, matches)) = &self.nav.pinned else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.tool_lists.legacy_find_query)
                    .hint_text("Speed search results"),
            );
            ui.checkbox(&mut self.tool_lists.legacy_find_group, "Group by file");
            ui.checkbox(&mut self.tool_lists.legacy_find_to, "Autoscroll to source");
            ui.checkbox(
                &mut self.tool_lists.legacy_find_from,
                "Autoscroll from source",
            );
            ui.checkbox(&mut self.tool_lists.legacy_find_preview, "Preview");
        });
        let mut follow_scroll = false;
        if self.tool_lists.legacy_find_from {
            if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                if let Some(path) = tab.state.path.as_ref() {
                    let line = tab.state.doc.line_of(tab.state.primary().head());
                    if let Some(found) = matches
                        .iter()
                        .filter(|found| &found.path == path)
                        .min_by_key(|found| found.line.abs_diff(line))
                    {
                        let selected = (path.clone(), found.line);
                        follow_scroll =
                            self.tool_lists.legacy_find_selected.as_ref() != Some(&selected);
                        self.tool_lists.legacy_find_selected = Some(selected);
                    }
                }
            }
        }
        let theme = self.theme.clone();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let mono = egui::FontId::monospace(12.5);
        let hit = if theme.dark {
            Color32::from_rgb(0x32, 0x59, 0x3D)
        } else {
            Color32::from_rgb(0xE5, 0xF1, 0xE5)
        };
        let mut open: Option<(PathBuf, usize, usize)> = None;
        egui::ScrollArea::vertical()
            .id_salt("pinned_find_results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut last: Option<&Path> = None;
                for m in matches {
                    if !crate::project_tree::speed_search_matches(
                        &format!("{} {}", m.path.display(), m.line_text),
                        &self.tool_lists.legacy_find_query,
                    ) {
                        continue;
                    }
                    if self.tool_lists.legacy_find_group && last != Some(m.path.as_path()) {
                        ui.label(
                            egui::RichText::new(
                                m.path
                                    .strip_prefix(&root)
                                    .unwrap_or(&m.path)
                                    .display()
                                    .to_string(),
                            )
                            .strong()
                            .color(theme.text),
                        );
                        last = Some(m.path.as_path());
                    }
                    let text = m.line_text.trim_start();
                    let cut = m.line_text.len() - text.len();
                    let (s, e) = (
                        m.start.saturating_sub(cut).min(text.len()),
                        m.end.saturating_sub(cut).min(text.len()),
                    );
                    let (s, e) = if text.is_char_boundary(s) && text.is_char_boundary(e) {
                        (s, e)
                    } else {
                        (0, 0)
                    };
                    let mut job = egui::text::LayoutJob::default();
                    let plain = egui::TextFormat {
                        font_id: mono.clone(),
                        color: theme.text,
                        ..Default::default()
                    };
                    job.append(&text[..s], 0.0, plain.clone());
                    job.append(
                        &text[s..e],
                        0.0,
                        egui::TextFormat {
                            background: hit,
                            ..plain.clone()
                        },
                    );
                    job.append(&text[e..], 0.0, plain);
                    let resp = ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new(format!("{:>4}", m.line + 1))
                                .monospace()
                                .color(theme.text_dim),
                        );
                        ui.add(egui::Label::new(job).sense(Sense::click()))
                    });
                    if resp.inner.clicked() {
                        self.tool_lists.legacy_find_selected = Some((m.path.clone(), m.line));
                        if self.tool_lists.legacy_find_to {
                            open = Some((m.path.clone(), m.line, m.start));
                        }
                    }
                    if follow_scroll
                        && self.tool_lists.legacy_find_selected.as_ref()
                            == Some(&(m.path.clone(), m.line))
                    {
                        resp.inner.scroll_to_me(Some(egui::Align::Center));
                    }
                    if resp.inner.double_clicked() {
                        open = Some((m.path.clone(), m.line, m.start));
                    }
                }
            });
        if self.tool_lists.legacy_find_preview {
            if let Some((path, line)) = &self.tool_lists.legacy_find_selected {
                self.tool_source_preview(ui, path, *line);
            }
        }
        if let Some((path, line, column)) = open {
            let ctx = ui.ctx().clone();
            self.handle_nav_event(
                NavEvent::OpenFile {
                    path,
                    line: Some(line),
                    column: Some(column),
                },
                &ctx,
            );
        }
    }

    /// Text of an open editor for Find/Replace in Files (unsaved changes
    /// included); `None` for files that are not open or not editable.
    pub(super) fn buffer_text(tabs: &[Tab], path: &Path) -> Option<String> {
        tabs.iter()
            .find(|t| {
                t.state.path.as_deref() == Some(path)
                    && t.image.is_none()
                    && !t.state.doc.is_read_only()
            })
            .map(|t| t.state.doc.text())
    }
}

#[cfg(test)]
#[path = "switcher_connected_tests.rs"]
mod switcher_connected_tests;

#[cfg(test)]
#[path = "recent_navigation_connected_tests.rs"]
mod recent_navigation_connected_tests;

impl IdeApp {
    /// Explicit counterpart navigation never turns a declaration into Show Usages.
    pub(super) fn go_to_source_or_stub(&mut self) {
        let Some(index) = self.active else { return };
        let offset = self.tabs[index].state.primary().head();
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        self.navigate_locations(
            features::source_stub_counterpart_at(&analysis, &file, offset),
            "No matching source or stub declaration",
        );
    }
}

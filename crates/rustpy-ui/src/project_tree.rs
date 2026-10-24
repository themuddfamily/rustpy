//! Project tool window: lazy file tree (roadmap §2.7 Project View, §0.3).
//!
//! A click gives the tree the keyboard (a single click previews the file but
//! the tree keeps focus), as do Alt+1 and Select Opened File. With focus,
//! Up/Down/Home/End/Page Up/Page Down move the selection, Left/Right collapse
//! and expand folders (or go to the parent / first child), Enter opens the
//! file in the editor, Delete deletes, F2 or Rename (Shift+F6) renames, Esc
//! returns to the editor and typing starts a speed search over the visible
//! rows.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_project::project::{ProjectModel, RootKind};

use crate::actions::{Action, Keymap};
use crate::theme::UiTheme;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeAction {
    /// Single click: preview the file; the tree keeps the keyboard.
    OpenFile(PathBuf),
    /// Double-click or Enter: open as a regular (non-preview) tab and focus the editor.
    OpenFileDoubleClick(PathBuf),
    OpenInRightSplit(PathBuf),
    GenerateNativeStub(PathBuf),
    SelectDir(PathBuf),
    /// New file in a directory with a template kind.
    NewFile(PathBuf, &'static str),
    NewFromTemplate(PathBuf, String),
    EditFileTemplates,
    NewDataSource(PathBuf),
    /// New › .gitignore File: the template picker for this folder.
    NewGitignore(PathBuf),
    NewDirectory(PathBuf),
    NewPackage(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    CompareFile(PathBuf),
    CopyPath(PathBuf),
    CopyReference(PathBuf),
    /// Run a script, a test file or the tests of a directory.
    Run(PathBuf),
    ContextRun(Box<crate::run_context::PathRequest>),
    /// Debug a script, a test file or the tests of a directory.
    Debug(PathBuf),
    OpenInTerminal(PathBuf),
    AddToGitignore(PathBuf),
    RevealInFiles(PathBuf),
    /// Mark Directory as (`None` = unmark).
    MarkAs(PathBuf, Option<RootKind>),
    /// Local History › Show History (a folder lists deleted files too).
    LocalHistory(PathBuf),
    LocalHistoryLabel(PathBuf),
    /// Find (false) / Replace (true) in Files scoped to a directory.
    FindInDirectory(PathBuf, bool),
    /// "Scratches and Consoles" › New Scratch File.
    NewScratch,
    /// A scratch file's "Change Language": rename it to this extension.
    ChangeScratchLanguage(PathBuf, String),
    /// Esc in the tree: the editor gets the keyboard back.
    FocusEditor,
    Invoke(PathBuf, Action),
    InvokeMany(Vec<PathBuf>, Action),
    Transfer(Vec<PathBuf>, PathBuf, bool),
    Clipboard(Vec<PathBuf>, bool),
    Paste(PathBuf),
    DeleteMany(Vec<PathBuf>),
    Analyze(PathBuf, &'static str),
    OpenAssociated(PathBuf),
    Encoding(PathBuf, &'static str),
    LineEndings(PathBuf, &'static str),
    CreateGist(PathBuf),
}

/// What a key press means in the focused Project view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// Enter: open the file in the editor, or expand/collapse a folder.
    Open,
    Compare,
    Delete,
    Rename,
    Escape,
    Backspace,
}

impl TreeKey {
    /// The tree's meaning of a key press. `action` is what the keymap binds
    /// the chord to: Rename and Safe Delete act on the selected file.
    pub fn from_key(key: Key, mods: Modifiers, action: Option<Action>) -> Option<TreeKey> {
        match action {
            Some(Action::CompareFiles) => return Some(TreeKey::Compare),
            Some(Action::Rename) => return Some(TreeKey::Rename),
            Some(Action::SafeDelete) => return Some(TreeKey::Delete),
            _ => {}
        }
        if key == Key::D && (mods.ctrl || mods.command || mods.mac_cmd) && !mods.alt && !mods.shift
        {
            return Some(TreeKey::Compare);
        }
        if mods.ctrl || mods.alt || mods.command || mods.mac_cmd {
            return None;
        }
        Some(match key {
            Key::ArrowUp => TreeKey::Up,
            Key::ArrowDown => TreeKey::Down,
            Key::ArrowLeft => TreeKey::Left,
            Key::ArrowRight => TreeKey::Right,
            Key::Home => TreeKey::Home,
            Key::End => TreeKey::End,
            Key::PageUp => TreeKey::PageUp,
            Key::PageDown => TreeKey::PageDown,
            _ if mods.shift => return None,
            Key::Enter => TreeKey::Open,
            Key::Delete => TreeKey::Delete,
            Key::F2 => TreeKey::Rename,
            Key::Escape => TreeKey::Escape,
            Key::Backspace => TreeKey::Backspace,
            _ => return None,
        })
    }
}

/// Files nested under a file of one directory (file nesting), by parent path.
type NestedFiles = HashMap<PathBuf, Vec<Entry>>;

/// A shown row with everything painting needs, derived once per tree change
/// (see `ProjectTree::rows`) so that painting never stats or lists the disk.
#[derive(Clone, Debug)]
struct PaintRow {
    entry: Entry,
    depth: usize,
    is_root: bool,
    /// Under the "Scratches and Consoles" node (scratch context menu).
    in_scratches: bool,
    /// A folder, or a file with files nested under it.
    expandable: bool,
    /// A library or stub path: read-only tree operations.
    stub_library: bool,
    excluded: bool,
    kind: Option<RootKind>,
    detail: String,
}

#[cfg(test)]
thread_local! {
    /// How often this thread rebuilt the shown rows (tests: cached painting).
    static ROW_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A row as shown, in display order (folders expanded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// The project root or the scratches node (not renamed or deleted from the tree).
    pub is_root: bool,
    pub depth: usize,
}

/// Speed search over a row name: a case-insensitive substring, or fragments
/// that each start a word (`mpy` → `main.py`, `tmp` → `test_main.py`).
pub fn speed_search_matches(name: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let chars: Vec<char> = name.chars().collect();
    let n: Vec<char> = chars.iter().map(|&c| lower(c)).collect();
    let p: Vec<char> = pattern.chars().map(lower).collect();
    if n.windows(p.len()).any(|w| w == p.as_slice()) {
        return true;
    }
    let word_start = |i: usize| {
        i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i].is_uppercase() && chars[i - 1].is_lowercase())
            || (chars[i].is_ascii_digit() && !chars[i - 1].is_ascii_digit())
    };
    // rest[i]: p[j..] matches with p[j] at i (continuing the fragment) or at a later word start.
    let mut rest = vec![true; n.len() + 1];
    for j in (0..p.len()).rev() {
        let mut next = vec![false; n.len() + 1];
        let mut later = false;
        for i in (0..n.len()).rev() {
            let fits = n[i] == p[j] && rest[i + 1];
            later |= fits && word_start(i);
            next[i] = fits || later;
        }
        // The pattern's first character must start a word.
        if j == 0 {
            return later;
        }
        rest = next;
    }
    false
}

pub struct ProjectTree {
    display_name: String,
    project_configuration: ProjectModel,
    pub options: rustpy_settings::ProjectViewSettings,
    /// Runtime mirror of the one persisted Appearance preference.
    pub show_details: bool,
    pub selected_paths: HashSet<PathBuf>,
    /// Set with `set_allowed_paths` (it invalidates the shown rows).
    allowed_paths: Option<HashSet<PathBuf>>,
    pub scope_colors: HashMap<PathBuf, Color32>,
    pub problem_counts: HashMap<PathBuf, usize>,
    pub coverage_percent: HashMap<PathBuf, f32>,
    pub library_roots: Vec<PathBuf>,
    pub extended_definitions: Vec<PathBuf>,
    drag_paths: Vec<PathBuf>,
    flatten_fallback: HashSet<PathBuf>,
    flattened_packages: HashSet<PathBuf>,
    pub root: PathBuf,
    pub expanded: HashSet<PathBuf>,
    pub source_roots: HashSet<PathBuf>,
    pub excluded: HashSet<PathBuf>,
    pub test_roots: HashSet<PathBuf>,
    pub resource_roots: HashSet<PathBuf>,
    pub template_dirs: HashSet<PathBuf>,
    pub namespace_packages: HashSet<PathBuf>,
    /// Content roots besides `root`, shown as further top-level nodes.
    pub content_roots: Vec<PathBuf>,
    /// Read-only stub library shown below project and scratch roots.
    pub python_stub_roots: Vec<PathBuf>,
    pub show_python_stubs: bool,
    pub selected: Option<PathBuf>,
    new_template_groups: Vec<(String, Vec<String>)>,
    pub show_hidden: bool,
    pub ignored_patterns: String,
    cache: HashMap<PathBuf, Vec<Entry>>,
    /// Bumped whenever directory listings or other unhashed inputs change.
    revision: u64,
    /// The shown rows and the `rows_key` they were built for.
    row_cache: Option<(u64, Arc<[PaintRow]>)>,
    /// Path to reveal & select on next render (autoscroll from source).
    reveal: Option<PathBuf>,
    /// VCS status per path (files and propagated directories).
    pub vcs_status: HashMap<PathBuf, rustpy_vcs::FileStatus>,
    /// The IDE scratches folder shown as "Scratches and Consoles" (None: hidden).
    pub scratches_dir: Option<PathBuf>,
    pub scratch_languages: Vec<(String, String)>,
    /// Speed search typed while the tree has focus (`None`: not searching).
    speed_search: Option<String>,
    /// The speed search matches no visible row (shown in red).
    speed_search_miss: bool,
    /// Row to scroll into view on the next render (keyboard navigation).
    scroll_to: Option<PathBuf>,
    /// Take the keyboard on the next render (Alt+1, Select Opened File).
    focus_requested: bool,
    /// Rows that fit in the view (Page Up / Page Down).
    page_rows: usize,
}

const HIDDEN_DIRS: &[&str] = &[
    ".git",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "node_modules",
    ".rustpy",
];

impl ProjectTree {
    pub fn new(root: PathBuf) -> Self {
        let mut t = Self {
            display_name: rustpy_project::ProjectModel::display_name(&root),
            show_details: true,
            project_configuration: ProjectModel::default(),
            options: Default::default(),
            selected_paths: HashSet::new(),
            allowed_paths: None,
            scope_colors: HashMap::new(),
            problem_counts: HashMap::new(),
            coverage_percent: HashMap::new(),
            library_roots: Vec::new(),
            extended_definitions: Vec::new(),
            drag_paths: Vec::new(),
            flatten_fallback: HashSet::new(),
            flattened_packages: HashSet::new(),
            root: root.clone(),
            expanded: HashSet::new(),
            source_roots: HashSet::new(),
            excluded: HashSet::new(),
            test_roots: HashSet::new(),
            resource_roots: HashSet::new(),
            template_dirs: HashSet::new(),
            namespace_packages: HashSet::new(),
            content_roots: Vec::new(),
            python_stub_roots: Vec::new(),
            show_python_stubs: false,
            selected: None,
            new_template_groups: Vec::new(),
            show_hidden: false,
            ignored_patterns: String::new(),
            cache: HashMap::new(),
            revision: 0,
            row_cache: None,
            reveal: None,
            vcs_status: HashMap::new(),
            scratches_dir: Some(crate::scratches::scratch_dir()),
            scratch_languages: Vec::new(),
            speed_search: None,
            speed_search_miss: false,
            scroll_to: None,
            focus_requested: false,
            page_rows: 10,
        };
        t.expanded.insert(root);
        t
    }

    /// Supplied by App on project/template changes; no settings IO in painting.
    pub fn set_new_template_groups(&mut self, groups: Vec<(String, Vec<String>)>) {
        self.new_template_groups = groups;
    }

    #[cfg(test)]
    pub(crate) fn new_template_groups_for_test(&self) -> &[(String, Vec<String>)] {
        &self.new_template_groups
    }

    fn marks(&self, kind: RootKind) -> &HashSet<PathBuf> {
        match kind {
            RootKind::Sources => &self.source_roots,
            RootKind::Tests => &self.test_roots,
            RootKind::Resources => &self.resource_roots,
            RootKind::Templates => &self.template_dirs,
            RootKind::NamespacePackage => &self.namespace_packages,
            RootKind::Excluded => &self.excluded,
        }
    }

    /// How a directory is marked (Mark Directory as).
    pub fn kind_of(&self, dir: &Path) -> Option<RootKind> {
        RootKind::ALL
            .into_iter()
            .find(|k| self.marks(*k).contains(dir))
    }

    /// Takes the content roots and folder marks of a project model.
    pub fn set_model(&mut self, m: &ProjectModel) {
        self.revision += 1;
        self.project_configuration = m.clone();
        let set = |v: &[PathBuf]| v.iter().cloned().collect::<HashSet<PathBuf>>();
        self.source_roots = set(&m.source_roots);
        self.test_roots = set(&m.test_roots);
        self.resource_roots = set(&m.resource_roots);
        self.template_dirs = set(&m.template_dirs);
        self.namespace_packages = set(&m.namespace_packages);
        self.excluded = set(&m.excluded);
        self.content_roots = m.content_roots.clone();
    }

    /// The content roots and folder marks shown, as a project model.
    pub fn model(&self) -> ProjectModel {
        let list = |s: &HashSet<PathBuf>| {
            let mut v: Vec<PathBuf> = s.iter().cloned().collect();
            v.sort();
            v
        };
        ProjectModel {
            frameworks: self.project_configuration.frameworks.clone(),
            sdk: self.project_configuration.sdk.clone(),
            language_level: self.project_configuration.language_level,
            modules: self.project_configuration.modules.clone(),
            content_roots: self.content_roots.clone(),
            source_roots: list(&self.source_roots),
            test_roots: list(&self.test_roots),
            resource_roots: list(&self.resource_roots),
            template_dirs: list(&self.template_dirs),
            namespace_packages: list(&self.namespace_packages),
            excluded: list(&self.excluded),
        }
    }

    /// Directories imports resolve from: the root, content roots, Sources and Test Sources roots.
    pub fn import_roots(&self) -> Vec<PathBuf> {
        self.model().import_roots(&self.root)
    }

    pub fn invalidate(&mut self, dir: &Path) {
        self.revision += 1;
        self.cache.remove(dir);
    }

    pub fn invalidate_all(&mut self) {
        self.revision += 1;
        self.cache.clear();
    }

    /// Limits the tree to these paths and their ancestors (`None`: everything).
    pub fn set_allowed_paths(&mut self, allowed: Option<HashSet<PathBuf>>) {
        self.revision += 1;
        self.allowed_paths = allowed;
    }

    /// The top-level node `path` is under: the project root or an extra content root.
    fn top_of(&self, path: &Path) -> Option<PathBuf> {
        if self.show_python_stubs && path.starts_with("<typeshed>") {
            return Some(PathBuf::from("<typeshed>"));
        }
        std::iter::once(&self.root)
            .chain(self.content_roots.iter())
            .chain(self.python_stub_roots.iter())
            .chain(self.library_roots.iter())
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .cloned()
    }

    pub fn reveal(&mut self, path: &Path) {
        if self.options.show_libraries && self.is_stub_library_path(path) {
            self.expanded.insert(PathBuf::from("<external-libraries>"));
            self.expanded.insert(PathBuf::from("<python-stubs>"));
            if path.starts_with("<typeshed>/@compiled") {
                self.expanded
                    .insert(PathBuf::from("<extended-definitions>"));
            }
        }
        if let Some(top) = self.top_of(path) {
            let mut p = path.parent();
            while let Some(dir) = p {
                if !dir.starts_with(&top) {
                    break;
                }
                self.expanded.insert(dir.to_path_buf());
                if dir == top {
                    break;
                }
                p = dir.parent();
            }
            self.selected = Some(path.to_path_buf());
            self.reveal = Some(path.to_path_buf());
        }
    }

    fn raw_children(&mut self, dir: &Path) -> Vec<Entry> {
        if dir == Path::new("<external-libraries>") {
            let mut entries: Vec<_> = self
                .library_roots
                .iter()
                .map(|path| Entry {
                    path: path.clone(),
                    name: path.display().to_string(),
                    is_dir: true,
                })
                .collect();
            if let Some(stubs) = self.stub_library_entry() {
                entries.push(stubs);
            }
            if !self.extended_definitions.is_empty() {
                entries.push(Entry {
                    path: "<extended-definitions>".into(),
                    name: "Skeletons and Extended Definitions".into(),
                    is_dir: true,
                });
            }
            return entries;
        }
        if dir == Path::new("<extended-definitions>") {
            return self
                .extended_definitions
                .iter()
                .map(|path| Entry {
                    path: path.clone(),
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    is_dir: false,
                })
                .collect();
        }

        if let Some(c) = self.cache.get(dir) {
            return c.clone();
        }
        let mut entries: Vec<Entry> = if let Some(entries) = self.stub_children(dir) {
            entries
        } else {
            match std::fs::read_dir(dir) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                            || (e.file_type().map(|t| t.is_symlink()).unwrap_or(false)
                                && e.path().is_dir());
                        Entry {
                            path: e.path(),
                            name,
                            is_dir,
                        }
                    })
                    .collect(),
                Err(_) => Vec::new(),
            }
        };
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.cache.insert(dir.to_path_buf(), entries.clone());
        entries
    }

    pub fn configure(&mut self, options: &rustpy_settings::ProjectViewSettings) {
        if self.options != *options {
            self.flatten_fallback.clear();
            self.flattened_packages.clear();
        }
        self.options = options.clone();
    }

    fn permitted(&self, path: &Path) -> bool {
        if self.options.view == "Scratches and Consoles"
            && self
                .scratches_dir
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
        {
            return true;
        }
        (self.options.show_excluded || !self.excluded.iter().any(|root| path.starts_with(root)))
            && self.allowed_paths.as_ref().is_none_or(|allowed| {
                allowed.contains(path)
                    || self.is_stub_library_path(path)
                    || self.library_roots.iter().any(|root| path.starts_with(root))
            })
    }

    fn nested_parent(&self, name: &str, siblings: &HashMap<&str, &Entry>) -> Option<PathBuf> {
        let parent = self.nesting_candidate(name, siblings)?;
        let mut visited = HashSet::from([PathBuf::from(name)]);
        let mut current = parent.clone();
        for _ in 0..64 {
            let filename = current.file_name()?.to_string_lossy();
            if !visited.insert(PathBuf::from(filename.as_ref())) {
                return None;
            }
            match self.nesting_candidate(&filename, siblings) {
                Some(next) => current = next,
                None => return Some(parent),
            }
        }
        None
    }

    /// `siblings`: the files of the directory by name (a lookup, not a scan).
    fn nesting_candidate(&self, name: &str, siblings: &HashMap<&str, &Entry>) -> Option<PathBuf> {
        for rule in self.options.nesting_rules.split(';').take(32) {
            let Some((parent, children)) = rule.split_once('=') else {
                continue;
            };
            for suffix in children.split(',').take(16) {
                let Some(stem) = name
                    .strip_suffix(suffix.trim())
                    .filter(|_| !suffix.trim().is_empty())
                else {
                    continue;
                };
                let candidate = format!("{stem}{}", parent.trim());
                if candidate != name {
                    if let Some(entry) = siblings
                        .get(candidate.as_str())
                        .filter(|entry| self.permitted(&entry.path))
                    {
                        return Some(entry.path.clone());
                    }
                }
            }
        }
        None
    }

    fn package_directory(&self, path: &Path) -> bool {
        path.join("__init__.py").is_file()
            || path.join("__init__.pyi").is_file()
            || self
                .namespace_packages
                .iter()
                .any(|root| path.starts_with(root))
    }

    /// The rows shown under directory `dir`, and the files nested under each
    /// file of it (file nesting), computed in one pass over the directory.
    fn dir_children(&mut self, dir: &Path) -> (Vec<Entry>, NestedFiles) {
        let flatten = self.options.flatten_packages
            && !self
                .flatten_fallback
                .iter()
                .any(|root| dir.starts_with(root));
        let siblings = self.raw_children(dir);
        let files: HashMap<&str, &Entry> = siblings
            .iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| (entry.name.as_str(), entry))
            .collect();
        let mut nested = NestedFiles::new();
        let mut nested_away = HashSet::new();
        for entry in siblings.iter().filter(|entry| !entry.is_dir) {
            if !self.permitted(&entry.path) {
                continue;
            }
            if let Some(parent) = self.nested_parent(&entry.name, &files) {
                nested.entry(parent).or_default().push(entry.clone());
                nested_away.insert(entry.path.as_path());
            }
        }
        let mut entries: Vec<_> = siblings
            .iter()
            .filter(|entry| {
                !rustpy_settings::editor_pages::matches(&self.ignored_patterns, &entry.path)
                    && (self.show_hidden
                        || !(HIDDEN_DIRS.contains(&entry.name.as_str())
                            || (entry.is_dir
                                && entry.name.starts_with('.')
                                && entry.name != ".github")))
                    && self.permitted(&entry.path)
                    && (entry.is_dir || !nested_away.contains(entry.path.as_path()))
            })
            .cloned()
            .collect();
        if self.options.compact_packages || flatten {
            let mut expanded = Vec::new();
            for mut entry in entries {
                if entry.is_dir && self.package_directory(&entry.path) {
                    for _ in 0..32 {
                        let children = self.raw_children(&entry.path);
                        let packages: Vec<_> = children
                            .iter()
                            .filter(|child| child.is_dir && self.package_directory(&child.path))
                            .collect();
                        let other = children.iter().any(|child| {
                            !matches!(
                                child.name.as_str(),
                                "__init__.py" | "__init__.pyi" | "__pycache__"
                            ) && (!child.is_dir || !self.package_directory(&child.path))
                        });
                        if flatten {
                            let mut pending = vec![(entry.clone(), 0usize)];
                            let mut flattened = Vec::new();
                            let mut exhausted = false;
                            while let Some((package, depth)) = pending.pop() {
                                if depth > 32 || flattened.len() + pending.len() > 4096 {
                                    exhausted = true;
                                    break;
                                }
                                for child in
                                    self.raw_children(&package.path)
                                        .into_iter()
                                        .filter(|child| {
                                            child.is_dir && self.package_directory(&child.path)
                                        })
                                {
                                    let name = format!("{}.{}", package.name, child.name);
                                    pending.push((Entry { name, ..child }, depth + 1));
                                }
                                flattened.push(package);
                            }
                            if exhausted {
                                self.flatten_fallback.insert(entry.path.clone());
                                expanded.push(entry.clone());
                            } else {
                                self.flattened_packages
                                    .extend(flattened.iter().map(|package| package.path.clone()));
                                expanded.extend(flattened);
                            }
                            break;
                        }
                        if packages.len() != 1 || other {
                            break;
                        }
                        entry.name = format!("{}.{}", entry.name, packages[0].name);
                        entry.path = packages[0].path.clone();
                    }
                }
                if !expanded
                    .iter()
                    .any(|existing: &Entry| existing.path == entry.path)
                {
                    expanded.push(entry);
                }
            }
            entries = expanded;
        }
        if flatten && self.flattened_packages.contains(dir) {
            entries.retain(|entry| !entry.is_dir || !self.package_directory(&entry.path));
        }
        entries.sort_by(|a, b| {
            (if self.options.folders_first {
                b.is_dir.cmp(&a.is_dir)
            } else {
                std::cmp::Ordering::Equal
            })
            .then_with(|| {
                if self.options.sort_by_type {
                    a.path.extension().cmp(&b.path.extension())
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        (entries, nested)
    }

    pub fn selection(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self.selected_paths.iter().cloned().collect();
        if paths.is_empty() {
            paths.extend(self.selected.iter().cloned());
        }
        paths.sort();
        paths
    }

    /// The widget that holds the tree's keyboard focus.
    /// Painted row of the primary selection in the current pass (accessibility).
    fn active_row_key() -> egui::Id {
        Self::focus_id().with("active-row")
    }

    pub fn focus_id() -> egui::Id {
        egui::Id::new("project_tree_focus")
    }

    /// Whether the Project view has the keyboard.
    pub fn has_focus(ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(Self::focus_id()))
    }

    /// Gives the tree the keyboard on its next render.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// The speed search being typed, if any.
    pub fn speed_search(&self) -> Option<&str> {
        self.speed_search.as_deref()
    }

    pub fn refresh_display_name(&mut self) {
        self.display_name = rustpy_project::ProjectModel::display_name(&self.root);
    }

    fn root_entry(&self) -> Entry {
        let name = self.display_name.clone();
        Entry {
            path: self.root.clone(),
            name,
            is_dir: true,
        }
    }

    fn scratches_entry(&self) -> Option<Entry> {
        self.scratches_dir.clone().map(|path| Entry {
            path,
            name: "Scratches and Consoles".into(),
            is_dir: true,
        })
    }

    fn stub_library_entry(&self) -> Option<Entry> {
        self.show_python_stubs.then(|| Entry {
            path: PathBuf::from("<python-stubs>"),
            name: "Python Stubs".into(),
            is_dir: true,
        })
    }

    fn is_stub_library_path(&self, path: &Path) -> bool {
        path.starts_with("<external-libraries>")
            || path.starts_with("<extended-definitions>")
            || self.library_roots.iter().any(|root| path.starts_with(root))
            || path.starts_with("<typeshed>")
            || path.starts_with("<python-stubs>")
            || self
                .python_stub_roots
                .iter()
                .any(|root| path.starts_with(root))
    }

    fn stub_children(&self, dir: &Path) -> Option<Vec<Entry>> {
        if dir == Path::new("<python-stubs>") {
            return Some(
                std::iter::once(Entry {
                    path: PathBuf::from("<typeshed>"),
                    name: "Bundled typeshed".into(),
                    is_dir: true,
                })
                .chain(self.python_stub_roots.iter().map(|path| Entry {
                    path: path.clone(),
                    name: path.display().to_string(),
                    is_dir: true,
                }))
                .collect(),
            );
        }
        let relative = dir.strip_prefix("<typeshed>").ok()?;
        let module = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(".");
        let prefix = if module.is_empty() {
            String::new()
        } else {
            format!("{module}.")
        };
        let mut entries = HashMap::new();
        for name in rustpy_semantic::typeshed::all_modules() {
            if name == module {
                let path = dir.with_extension("pyi");
                entries.insert(
                    path.clone(),
                    Entry {
                        path,
                        name: "__init__.pyi".into(),
                        is_dir: false,
                    },
                );
                continue;
            }
            let Some(tail) = name.strip_prefix(&prefix) else {
                continue;
            };
            let head = tail.split('.').next().unwrap_or(tail);
            let child_module = format!("{prefix}{head}");
            let is_dir = tail.contains('.') || rustpy_semantic::typeshed::is_package(&child_module);
            let path = dir.join(if is_dir {
                head.to_string()
            } else {
                format!("{head}.pyi")
            });
            entries.insert(
                path.clone(),
                Entry {
                    path,
                    name: if is_dir {
                        head.to_string()
                    } else {
                        format!("{head}.pyi")
                    },
                    is_dir,
                },
            );
        }
        Some(entries.into_values().collect())
    }

    /// The rows as shown, top to bottom.
    pub fn visible_rows(&mut self) -> Vec<VisibleRow> {
        self.rows()
            .iter()
            .map(|row| VisibleRow {
                path: row.entry.path.clone(),
                name: row.entry.name.clone(),
                is_dir: row.entry.is_dir,
                is_root: row.is_root,
                depth: row.depth,
            })
            .collect()
    }

    /// Everything the shown rows are derived from, as one fingerprint. The
    /// directory listings themselves are covered by `revision` (bumped by
    /// `invalidate*`, which the file watcher drives); the rest is in memory.
    fn rows_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        fn unordered(set: &HashSet<PathBuf>, hasher: &mut impl Hasher) {
            let mut sum = 0u64;
            for path in set {
                let mut one = std::collections::hash_map::DefaultHasher::new();
                path.hash(&mut one);
                sum = sum.wrapping_add(one.finish());
            }
            (set.len(), sum).hash(hasher);
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.revision.hash(&mut h);
        self.root.hash(&mut h);
        self.display_name.hash(&mut h);
        for set in [
            &self.expanded,
            &self.source_roots,
            &self.test_roots,
            &self.resource_roots,
            &self.template_dirs,
            &self.namespace_packages,
            &self.excluded,
        ] {
            unordered(set, &mut h);
        }
        let o = &self.options;
        (
            &o.view,
            o.flatten_packages,
            o.compact_packages,
            o.show_excluded,
            o.sort_by_type,
            o.folders_first,
            o.show_libraries,
            &o.nesting_rules,
        )
            .hash(&mut h);
        (
            self.show_hidden,
            &self.ignored_patterns,
            self.show_python_stubs,
        )
            .hash(&mut h);
        (
            &self.content_roots,
            &self.library_roots,
            &self.python_stub_roots,
            &self.extended_definitions,
            &self.scratches_dir,
        )
            .hash(&mut h);
        self.allowed_paths.as_ref().map(HashSet::len).hash(&mut h);
        h.finish()
    }

    /// The shown rows, rebuilt only when something they derive from changed:
    /// painting an unchanged tree touches neither the disk nor the listings.
    fn rows(&mut self) -> Arc<[PaintRow]> {
        let key = self.rows_key();
        if let Some((cached, rows)) = &self.row_cache {
            if *cached == key {
                return rows.clone();
            }
        }
        let mut rows = Vec::new();
        let root = self.root_entry();
        self.collect_rows(&root, 0, true, false, None, &mut rows);
        for path in self.content_roots.clone() {
            if path.starts_with(&root.path) {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let entry = Entry {
                path,
                name,
                is_dir: true,
            };
            self.collect_rows(&entry, 0, true, false, None, &mut rows);
        }
        if let Some(scratches) = self.scratches_entry() {
            self.collect_rows(&scratches, 0, true, true, None, &mut rows);
        }
        if self.options.show_libraries
            && self.options.view == "Project"
            && (!self.library_roots.is_empty()
                || self.show_python_stubs
                || !self.extended_definitions.is_empty())
        {
            let external = Entry {
                path: "<external-libraries>".into(),
                name: "External Libraries".into(),
                is_dir: true,
            };
            self.collect_rows(&external, 0, true, false, None, &mut rows);
        }
        let rows: Arc<[PaintRow]> = rows.into();
        #[cfg(test)]
        ROW_BUILDS.with(|builds| builds.set(builds.get() + 1));
        self.row_cache = Some((key, rows.clone()));
        rows
    }

    /// `nested`: the files nested under each file of the parent directory.
    fn collect_rows(
        &mut self,
        entry: &Entry,
        depth: usize,
        is_root: bool,
        in_scratches: bool,
        nested: Option<&NestedFiles>,
        rows: &mut Vec<PaintRow>,
    ) {
        let stub_library = self.is_stub_library_path(&entry.path);
        let expanded = self.expanded.contains(&entry.path);
        // A folder is expandable as such: list it only when it is expanded
        // (a collapsed folder of a large project must not be read).
        let (children, nested_here) = if entry.is_dir && !expanded {
            (Vec::new(), None)
        } else if entry.is_dir {
            let (children, nested) = self.dir_children(&entry.path);
            (children, Some(nested))
        } else if stub_library {
            (Vec::new(), None)
        } else {
            let children = nested
                .and_then(|nested| nested.get(&entry.path))
                .cloned()
                .unwrap_or_default();
            (children, None)
        };
        let expandable = entry.is_dir || !children.is_empty();
        let kind = if entry.is_dir {
            self.kind_of(&entry.path)
        } else {
            None
        };
        rows.push(PaintRow {
            entry: entry.clone(),
            depth,
            is_root,
            in_scratches,
            expandable,
            stub_library,
            excluded: self
                .excluded
                .iter()
                .any(|root| entry.path.starts_with(root)),
            kind,
            detail: self.row_detail(entry, is_root),
        });
        if expandable && expanded {
            let nested = nested_here.as_ref().or(nested);
            for child in &children {
                self.collect_rows(child, depth + 1, false, in_scratches, nested, rows);
            }
        }
    }

    fn select_row(&mut self, row: &VisibleRow) {
        self.selected_paths.clear();
        self.selected_paths.insert(row.path.clone());
        self.selected = Some(row.path.clone());
        self.scroll_to = Some(row.path.clone());
    }

    /// A key press while the tree has the keyboard.
    pub fn key(&mut self, key: TreeKey) -> Option<TreeAction> {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return None;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| &r.path == s));
        if let Some(pattern) = self.speed_search.clone() {
            match key {
                // Up/Down step through the rows the speed search matches.
                TreeKey::Up | TreeKey::Down => {
                    let from = cur.unwrap_or(0);
                    let hit = if key == TreeKey::Down {
                        (from + 1..rows.len())
                            .find(|&i| speed_search_matches(&rows[i].name, &pattern))
                    } else {
                        (0..from)
                            .rev()
                            .find(|&i| speed_search_matches(&rows[i].name, &pattern))
                    };
                    if let Some(i) = hit {
                        self.select_row(&rows[i]);
                    }
                    return None;
                }
                TreeKey::Escape => {
                    self.speed_search = None;
                    return None;
                }
                TreeKey::Backspace => {
                    let mut pattern = pattern;
                    pattern.pop();
                    if pattern.is_empty() {
                        self.speed_search = None;
                    } else {
                        self.speed_search = Some(pattern);
                        self.find_match(&rows, cur.unwrap_or(0));
                    }
                    return None;
                }
                _ => self.speed_search = None,
            }
        }
        let last = rows.len() - 1;
        let Some(i) = cur else {
            // Nothing selected yet: a navigation key selects the first (End: last) row.
            match key {
                TreeKey::End => self.select_row(&rows[last]),
                TreeKey::Up
                | TreeKey::Down
                | TreeKey::Home
                | TreeKey::PageUp
                | TreeKey::PageDown
                | TreeKey::Left
                | TreeKey::Right => self.select_row(&rows[0]),
                TreeKey::Escape if self.options.focus_editor_on_escape => {
                    return Some(TreeAction::FocusEditor)
                }
                TreeKey::Escape => return None,
                _ => {}
            }
            return None;
        };
        let row = &rows[i];
        let page = self.page_rows.max(1);
        match key {
            TreeKey::Up => self.select_row(&rows[i.saturating_sub(1)]),
            TreeKey::Down => self.select_row(&rows[(i + 1).min(last)]),
            TreeKey::Home => self.select_row(&rows[0]),
            TreeKey::End => self.select_row(&rows[last]),
            TreeKey::PageUp => self.select_row(&rows[i.saturating_sub(page)]),
            TreeKey::PageDown => self.select_row(&rows[(i + page).min(last)]),
            TreeKey::Left => {
                if self.expanded.contains(&row.path) {
                    self.expanded.remove(&row.path);
                } else if let Some(parent) =
                    rows[..i].iter().rposition(|r| r.depth + 1 == row.depth)
                {
                    self.select_row(&rows[parent]);
                }
            }
            TreeKey::Right => {
                if self.rows().get(i).is_some_and(|row| row.expandable) {
                    if !self.expanded.contains(&row.path) {
                        self.expanded.insert(row.path.clone());
                    } else if let Some(child) = rows.get(i + 1).filter(|c| c.depth == row.depth + 1)
                    {
                        self.select_row(child);
                    }
                }
            }
            TreeKey::Open if !row.is_dir => {
                return Some(TreeAction::OpenFileDoubleClick(row.path.clone()));
            }
            TreeKey::Open => {
                if !self.expanded.remove(&row.path) {
                    self.expanded.insert(row.path.clone());
                }
            }
            TreeKey::Compare if !row.is_dir => {
                return Some(TreeAction::CompareFile(row.path.clone()));
            }
            TreeKey::Compare => {}
            TreeKey::Delete if !row.is_root && !self.is_stub_library_path(&row.path) => {
                return Some(if self.selection().len() > 1 {
                    TreeAction::DeleteMany(self.selection())
                } else {
                    TreeAction::Delete(row.path.clone())
                });
            }
            TreeKey::Rename if !row.is_root && !self.is_stub_library_path(&row.path) => {
                return Some(TreeAction::Rename(row.path.clone()));
            }
            TreeKey::Escape => {
                if self.options.focus_editor_on_escape {
                    return Some(TreeAction::FocusEditor);
                }
            }
            TreeKey::Delete | TreeKey::Rename | TreeKey::Backspace => {}
        }
        None
    }

    /// Text typed while the tree has the keyboard: grows the speed search and
    /// selects the first visible row it matches, from the selection on.
    pub fn type_text(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        // A space alone does not start a search.
        if text.is_empty() || (self.speed_search.is_none() && text.trim().is_empty()) {
            return;
        }
        let pattern = format!("{}{text}", self.speed_search.as_deref().unwrap_or(""));
        self.speed_search = Some(pattern);
        let rows = self.visible_rows();
        let cur = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| &r.path == s))
            .unwrap_or(0);
        self.find_match(&rows, cur);
    }

    /// Selects the first row the speed search matches at or after `from`,
    /// wrapping around; the selection stays when nothing matches.
    fn find_match(&mut self, rows: &[VisibleRow], from: usize) {
        let Some(pattern) = self.speed_search.clone() else {
            return;
        };
        let hit = (0..rows.len())
            .map(|k| (from + k) % rows.len())
            .find(|&i| speed_search_matches(&rows[i].name, &pattern));
        self.speed_search_miss = hit.is_none();
        if let Some(i) = hit {
            self.select_row(&rows[i]);
        }
    }

    /// Draws the tree. `keyboard` is false while a dialog or popup owns the
    /// keyboard; `keymap` tells which chords mean Rename and Safe Delete.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        keymap: &Keymap,
        keyboard: bool,
    ) -> Option<TreeAction> {
        let mut action = None;
        let area = ui.available_rect_before_wrap();
        let focus = ui.interact(area, Self::focus_id(), Sense::focusable_noninteractive());
        if keyboard {
            // A click anywhere in the tree (a row, the empty space below) gives it the keyboard.
            // egui drops the focus of a widget when the pointer is pressed or clicked outside
            // it, and this one covers the tree without sensing the pointer: take it back.
            let clicked_here = ui.input(|i| i.pointer.any_pressed() || i.pointer.any_click())
                && ui.rect_contains_pointer(area);
            if std::mem::take(&mut self.focus_requested) || clicked_here {
                focus.request_focus();
            }
        } else if Self::has_focus(ui.ctx()) {
            focus.surrender_focus();
        }
        // The widget's focus, also while the window itself is in the background
        // (`Response::has_focus` is false then: that must not end a speed search).
        let focused = keyboard && Self::has_focus(ui.ctx());
        if focused {
            let command = ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
            if command {
                for (key, cut) in [(Key::C, false), (Key::X, true)] {
                    if ui.input_mut(|input| {
                        let modifiers = input.modifiers;
                        input.consume_key(modifiers, key)
                    }) {
                        action = Some(TreeAction::Clipboard(self.selection(), cut));
                    }
                }
                if ui.input_mut(|input| {
                    let modifiers = input.modifiers;
                    input.consume_key(modifiers, Key::V)
                }) {
                    let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
                    action = Some(TreeAction::Paste(if path.is_dir() {
                        path
                    } else {
                        path.parent().unwrap_or(&self.root).into()
                    }));
                }
                if ui.input_mut(|input| {
                    let modifiers = input.modifiers;
                    input.consume_key(modifiers, Key::A)
                }) {
                    self.selected_paths = self
                        .visible_rows()
                        .into_iter()
                        .filter(|row| !row.is_root)
                        .map(|row| row.path)
                        .collect();
                }
            }
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    Self::focus_id(),
                    egui::EventFilter {
                        // Tree input has no Tab command: let egui move to the
                        // next component instead of locking focus in the tree.
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            for input in take_tree_input(ui, keymap) {
                match input {
                    TreeInput::Key(k) => {
                        if let Some(a) = self.key(k) {
                            action = Some(a);
                        }
                    }
                    TreeInput::Text(t) => self.type_text(&t),
                }
            }
        } else {
            // Leaving the tree ends the speed search.
            self.speed_search = None;
        }
        self.page_rows = ((area.height() / 22.0) as usize).saturating_sub(1).max(1);
        let rows = self.rows();
        let style = RowStyle::new(ui.ctx());
        // Only the rows in view are laid out and painted; a target row out of
        // view is scrolled to by its position.
        let target = self
            .reveal
            .as_ref()
            .map(|path| (path, Some(egui::Align::Center)))
            .or(self.scroll_to.as_ref().map(|path| (path, None)))
            .and_then(|(path, align)| {
                rows.iter()
                    .position(|row| &row.entry.path == path)
                    .map(|index| (index, align))
            });
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .id_salt("project_tree")
            .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
                ui.set_min_width(ui.available_width());
                if let Some((index, align)) = target.filter(|(index, _)| !range.contains(index)) {
                    let step = ROW_HEIGHT + ui.spacing().item_spacing.y;
                    let top = ui.max_rect().top() + (index as f32 - range.start as f32) * step;
                    let rect = Rect::from_min_size(
                        Pos2::new(ui.max_rect().left(), top),
                        Vec2::new(ui.available_width().max(100.0), ROW_HEIGHT),
                    );
                    ui.scroll_to_rect(rect, align);
                }
                for row in &rows[range] {
                    self.row(ui, theme, &style, row, &mut action, focused);
                }
            });
        let active = ui.ctx().data_mut(|data| {
            let active = data.get_temp::<egui::Id>(Self::active_row_key());
            data.remove::<egui::Id>(Self::active_row_key());
            active
        });
        crate::a11y::tree(ui.ctx(), focus.id, "Project", active);
        if ui.input(|input| input.pointer.any_released()) {
            self.drag_paths.clear();
        }
        self.reveal = None;
        self.scroll_to = None;
        if let Some(pattern) = &self.speed_search {
            // Speed-search "Search for:" box over the top of the tree.
            let color = if self.speed_search_miss {
                theme.error
            } else {
                theme.text
            };
            let galley = ui.painter().layout_no_wrap(
                format!("Search for: {pattern}"),
                egui::FontId::proportional(12.5),
                color,
            );
            let rect = Rect::from_min_size(
                area.left_top() + Vec2::new(4.0, 2.0),
                galley.size() + Vec2::new(14.0, 8.0),
            );
            ui.painter().rect_filled(rect, 3.0, theme.header_bg);
            ui.painter().rect_stroke(
                rect,
                3.0,
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
            ui.painter()
                .galley(rect.min + Vec2::new(7.0, 4.0), galley, color);
        }
        action
    }

    /// Context menu of the scratches node and of scratch files.
    fn feature_menu(&self, ui: &mut egui::Ui, entry: &Entry, action: &mut Option<TreeAction>) {
        let invoke = |ui: &mut egui::Ui,
                      label: &str,
                      value: Action,
                      action: &mut Option<TreeAction>| {
            if ui.button(label).clicked() {
                *action = Some(
                    if self.selection().len() > 1
                        && matches!(
                            value,
                            Action::ReformatCode | Action::OptimizeImports | Action::CodeCleanup
                        )
                    {
                        TreeAction::InvokeMany(self.selection(), value)
                    } else {
                        TreeAction::Invoke(entry.path.clone(), value)
                    },
                );
                ui.close();
            }
        };
        let selection = self.selection();
        ui.horizontal(|ui| {
            if ui.button("Cut").clicked() {
                *action = Some(TreeAction::Clipboard(selection.clone(), true));
                ui.close();
            }
            if ui.button("Copy").clicked() {
                *action = Some(TreeAction::Clipboard(selection.clone(), false));
                ui.close();
            }
            if ui.button("Paste").clicked() {
                *action = Some(TreeAction::Paste(if entry.is_dir {
                    entry.path.clone()
                } else {
                    entry.path.parent().unwrap_or(&self.root).into()
                }));
                ui.close();
            }
        });
        if selection.len() > 1
            && ui
                .button(format!("Delete {} selected paths…", selection.len()))
                .clicked()
        {
            *action = Some(TreeAction::DeleteMany(selection));
            ui.close();
        }
        invoke(ui, "Find Usages", Action::FindUsages, action);
        for (label, replace) in [("Find in Files…", false), ("Replace in Files…", true)] {
            if ui.button(crate::i18n::tr_static(label)).clicked() {
                *action = Some(TreeAction::FindInDirectory(
                    if entry.is_dir {
                        entry.path.clone()
                    } else {
                        entry.path.parent().unwrap_or(&self.root).into()
                    },
                    replace,
                ));
                ui.close();
            }
        }
        if entry.is_dir && ui.button("Compare Directories…").clicked() {
            *action = Some(TreeAction::CompareFile(entry.path.clone()));
            ui.close();
        }

        ui.menu_button("Analyze", |ui| {
            for (label, value) in [
                ("Inspect Code…", Action::InspectCode),
                ("Code Cleanup…", Action::CodeCleanup),
                ("Run Inspection by Name…", Action::RunInspectionByName),
            ] {
                invoke(ui, label, value, action);
            }
            for kind in [
                "Dependencies",
                "Backward Dependencies",
                "Cyclic Dependencies",
                "Module Dependencies",
                "Data Flow",
                "Locate Duplicates",
            ] {
                if ui.button(crate::i18n::tr_static(kind)).clicked() {
                    *action = Some(TreeAction::Analyze(entry.path.clone(), kind));
                    ui.close();
                }
            }
        });
        ui.menu_button("Refactor", |ui| {
            for (label, value) in [
                ("Rename…", Action::Rename),
                ("Move…", Action::MoveRefactoring),
                ("Copy…", Action::CopyFile),
                ("Safe Delete…", Action::SafeDelete),
                ("Convert to Package", Action::ConvertToPackage),
            ] {
                invoke(ui, label, value, action);
            }
        });
        for (label, value) in [
            ("Add to Favorites / Bookmarks", Action::ToggleBookmark),
            ("Reformat Code", Action::ReformatCode),
            ("Optimize Imports", Action::OptimizeImports),
            ("Reload from Disk", Action::ReloadFromDisk),
            ("Show Diagram", Action::ShowUmlDiagram),
        ] {
            invoke(ui, label, value, action);
        }
        ui.menu_button("Git", |ui| {
            for (label, value) in [
                ("Changes", Action::ToggleGitView),
                ("Commit…", Action::Commit),
                ("Update Project…", Action::UpdateProject),
                ("Annotate", Action::Annotate),
            ] {
                invoke(ui, label, value, action);
            }
        });
        ui.menu_button("Open In", |ui| {
            invoke(ui, "Browser", Action::OpenInBrowser, action);
            if ui.button("Associated application").clicked() {
                *action = Some(TreeAction::OpenAssociated(entry.path.clone()));
                ui.close();
            }
        });
        if !entry.is_dir {
            ui.menu_button("Convert Line Separators", |ui| {
                for ending in ["LF", "CRLF", "CR"] {
                    if ui.button(crate::i18n::tr_static(ending)).clicked() {
                        *action = Some(TreeAction::LineEndings(entry.path.clone(), ending));
                        ui.close();
                    }
                }
            });
            ui.menu_button("File Encoding", |ui| {
                for encoding in ["UTF-8", "UTF-8 BOM", "UTF-16 LE", "UTF-16 BE", "ISO-8859-1"] {
                    if ui.button(crate::i18n::tr_static(encoding)).clicked() {
                        *action = Some(TreeAction::Encoding(entry.path.clone(), encoding));
                        ui.close();
                    }
                }
            });
            if ui.button("Create Gist…").clicked() {
                *action = Some(TreeAction::CreateGist(entry.path.clone()));
                ui.close();
            }
        }
        ui.menu_button("Deployment", |ui| {
            for (label, value) in [
                ("Configuration…", Action::DeploymentConfiguration),
                ("Upload", Action::DeploymentUpload),
                ("Download", Action::DeploymentDownload),
                ("Sync…", Action::DeploymentSync),
            ] {
                invoke(ui, label, value, action);
            }
        });
        ui.separator();
    }

    fn scratch_menu(
        custom_languages: &[(String, String)],
        ui: &mut egui::Ui,
        entry: &Entry,
        is_root: bool,
        action: &mut Option<TreeAction>,
    ) {
        if is_root || entry.is_dir {
            if ui.button("New Scratch File…").clicked() {
                *action = Some(TreeAction::NewScratch);
                ui.close();
            }
        } else {
            if entry.name.ends_with(".py") && ui.button("Run").clicked() {
                *action = Some(TreeAction::Run(entry.path.clone()));
                ui.close();
            }
            if ui.button("Rename…").clicked() {
                *action = Some(TreeAction::Rename(entry.path.clone()));
                ui.close();
            }
            ui.menu_button("Change Language", |ui| {
                let current = entry
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let mut languages: Vec<_> = crate::scratches::LANGUAGES
                    .iter()
                    .map(|(name, ext)| (name.to_string(), ext.to_string()))
                    .collect();
                languages.extend_from_slice(custom_languages);
                languages.sort();
                languages.dedup();
                for (name, ext) in languages {
                    let selected = if ext == "requirements" {
                        crate::editor::Language::from_path(&entry.path)
                            == crate::editor::Language::Requirements
                    } else {
                        current == ext
                            && crate::editor::Language::from_path(&entry.path)
                                != crate::editor::Language::Requirements
                    };
                    if ui.selectable_label(selected, name).clicked() {
                        *action = Some(TreeAction::ChangeScratchLanguage(entry.path.clone(), ext));
                        ui.close();
                    }
                }
            });
            if ui.button("Delete…").clicked() {
                *action = Some(TreeAction::Delete(entry.path.clone()));
                ui.close();
            }
            ui.separator();
            if !entry.is_dir && ui.button("Compare with…").clicked() {
                *action = Some(TreeAction::CompareFile(entry.path.clone()));
                ui.close();
            }
            copy_target_menu(ui, entry, action);
        }
        if ui.button("Reveal in File Manager").clicked() {
            *action = Some(TreeAction::RevealInFiles(entry.path.clone()));
            ui.close();
        }
    }

    /// Uses path spelling and already-cached project configuration only. Do not
    /// stat files or populate directory caches merely to display a description.
    fn row_detail(&self, entry: &Entry, is_root: bool) -> String {
        if entry.is_dir {
            if let Some(kind) = self.kind_of(&entry.path) {
                return kind.label().into();
            }
            if is_root {
                return "Project root".into();
            }
            if self
                .project_configuration
                .modules
                .iter()
                .any(|module| module.content_root == entry.path)
            {
                return "Module content root".into();
            }
            if self.is_stub_library_path(&entry.path)
                || self
                    .library_roots
                    .iter()
                    .any(|root| entry.path.starts_with(root))
            {
                return "Library".into();
            }
            return "Directory".into();
        }
        let language = crate::editor::Language::from_path(&entry.path);
        if language != crate::editor::Language::Other {
            return language.display_name().into();
        }
        entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!(".{extension} file"))
            .unwrap_or_else(|| "File".into())
    }

    /// Paints one row. Uses only the cached `row` and in-memory state: no
    /// filesystem access unless its context menu is open.
    fn row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        style: &RowStyle,
        row: &PaintRow,
        action: &mut Option<TreeAction>,
        focused: bool,
    ) {
        let PaintRow {
            entry,
            depth,
            is_root,
            in_scratches,
            expandable,
            stub_library,
            excluded,
            kind,
            detail,
        } = row;
        let (depth, is_root, in_scratches, expandable, stub_library, excluded, kind) = (
            *depth,
            *is_root,
            *in_scratches,
            *expandable,
            *stub_library,
            *excluded,
            *kind,
        );
        let (indent_width, indent_guides) = (style.indent_width, style.indent_guides);
        let indent = indent_width * depth as f32 + 6.0;
        // Rows are not focus targets themselves: the tree as a whole holds the keyboard.
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().max(100.0), ROW_HEIGHT),
            Sense::hover(),
        );
        let resp = ui.interact(
            rect,
            ui.id().with(("project-row", &entry.path)),
            // The tree is one keyboard focus owner. Rows retain pointer/DnD
            // interaction without inserting extra stops into Tab traversal.
            Sense::CLICK | Sense::DRAG,
        );
        let selected = self.selected.as_deref() == Some(entry.path.as_path())
            || self.selected_paths.contains(&entry.path);
        let expanded = expandable && self.expanded.contains(&entry.path);
        // Screen readers: each painted row is a tree item, and the primary
        // selection is the tree's active descendant (the tree keeps focus).
        crate::a11y::tree_item(
            &resp,
            &entry.name,
            depth + 1,
            expandable.then_some(expanded),
            selected,
        );
        if self.selected.as_deref() == Some(entry.path.as_path()) {
            ui.ctx()
                .data_mut(|data| data.insert_temp(Self::active_row_key(), resp.id));
        }
        if !selected {
            if let Some(color) = self.scope_colors.get(&entry.path) {
                ui.painter()
                    .rect_filled(rect, 0.0, color.gamma_multiply(0.3));
            }
        }
        if selected {
            // The selection is highlighted like the focused list it is.
            ui.painter().rect_filled(
                rect,
                0.0,
                if focused {
                    theme.selection_bg
                } else {
                    theme.selection_inactive_bg
                },
            );
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, theme.hover_bg);
        }
        if self.reveal.as_deref() == Some(entry.path.as_path()) {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        } else if self.scroll_to.as_deref() == Some(entry.path.as_path()) {
            ui.scroll_to_rect(rect, None);
        }
        if indent_guides {
            for level in 0..depth {
                let x = rect.left() + 6.0 + indent_width * level as f32;
                ui.painter().line_segment(
                    [
                        egui::Pos2::new(x, rect.top()),
                        egui::Pos2::new(x, rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, theme.border),
                );
            }
        }
        let mut x = rect.left() + indent;
        let cy = rect.center().y;
        if expandable {
            // Expand/collapse triangle.
            let tri = if expanded {
                [
                    Pos2::new(x, cy - 2.0),
                    Pos2::new(x + 8.0, cy - 2.0),
                    Pos2::new(x + 4.0, cy + 3.0),
                ]
            } else {
                [
                    Pos2::new(x + 1.0, cy - 4.0),
                    Pos2::new(x + 6.0, cy),
                    Pos2::new(x + 1.0, cy + 4.0),
                ]
            };
            ui.painter().add(egui::Shape::convex_polygon(
                tri.to_vec(),
                theme.text_dim,
                Stroke::NONE,
            ));
        }
        x += 14.0;
        // Icon (a marked folder is tinted with its root color).
        let icon_rect = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::splat(13.0));
        paint_icon(
            ui.painter(),
            icon_rect,
            entry,
            is_root,
            theme,
            kind.and_then(|k| {
                if theme.high_contrast {
                    Some(theme.text)
                } else {
                    root_kind_color(k, theme)
                }
            }),
        );
        x += 20.0;
        let mut color = if theme.high_contrast {
            theme.text
        } else if selected && focused {
            Color32::WHITE
        } else if selected && !theme.dark {
            Color32::BLACK
        } else {
            theme.text
        };
        if !selected && !theme.high_contrast && (stub_library || excluded) {
            color = theme.text_dim;
        }
        if !theme.high_contrast {
            if let Some(c) = kind.map(|k| root_kind_color(k, theme).unwrap_or(theme.text_disabled))
            {
                color = c;
            }
            if let Some(st) = self.vcs_status.get(&entry.path) {
                if let Some(c) = crate::git_panel::status_color(*st, theme) {
                    color = c;
                }
            }
        }
        if !selected && !theme.high_contrast && excluded {
            color = color.gamma_multiply(0.55);
        }
        if let Some(percent) = self.coverage_percent.get(&entry.path) {
            let tint = if *percent >= 80.0 {
                theme.success
            } else if *percent >= 50.0 {
                theme.warning
            } else {
                theme.error
            };
            ui.painter().rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.right() - 4.0, rect.top() + 2.0),
                    Pos2::new(rect.right() - 1.0, rect.bottom() - 2.0),
                ),
                0.0,
                tint,
            );
        }
        let label_rect = ui.painter().text(
            Pos2::new(x, cy),
            egui::Align2::LEFT_CENTER,
            format!(
                "{}{}{}{}",
                if self.options.visibility_icons && entry.name.starts_with('_') {
                    "◦ "
                } else {
                    ""
                },
                entry.name,
                self.problem_counts
                    .get(&entry.path)
                    .filter(|count| **count > 0)
                    .map(|count| format!("  [{count}]"))
                    .unwrap_or_default(),
                self.coverage_percent
                    .get(&entry.path)
                    .map(|percent| format!("  {percent:.0}%"))
                    .unwrap_or_default()
            ),
            egui::FontId::proportional(13.0),
            color,
        );

        if self.show_details {
            let left = label_rect.right() + 10.0;
            if !detail.is_empty() && left + 24.0 < rect.right() {
                // Secondary paint has no hit target: selection, drag, keyboard,
                // context-menu authority and the primary row name remain stable.
                let clip = Rect::from_min_max(Pos2::new(left, rect.top()), rect.right_bottom())
                    .intersect(ui.clip_rect());
                ui.painter().with_clip_rect(clip).text(
                    Pos2::new(left, cy),
                    egui::Align2::LEFT_CENTER,
                    detail.as_str(),
                    egui::FontId::proportional(12.0),
                    if selected { color } else { theme.text_dim },
                );
            }
        }

        if resp.drag_started() && !is_root && !stub_library {
            if !self.selected_paths.contains(&entry.path) {
                self.selected_paths.clear();
                self.selected_paths.insert(entry.path.clone());
            }
            self.drag_paths = self.selection();
        }
        if entry.is_dir
            && resp.hovered()
            && !self.drag_paths.is_empty()
            && ui.input(|input| input.pointer.any_released())
        {
            let copy = ui.input(|input| input.modifiers.ctrl || input.modifiers.command);
            *action = Some(TreeAction::Transfer(
                std::mem::take(&mut self.drag_paths),
                entry.path.clone(),
                copy,
            ));
        }
        if resp.clicked() {
            let modifiers = ui.input(|input| input.modifiers);
            if modifiers.shift {
                let rows = self.visible_rows();
                if let (Some(anchor), Some(end)) = (
                    self.selected
                        .as_ref()
                        .and_then(|path| rows.iter().position(|row| &row.path == path)),
                    rows.iter().position(|row| row.path == entry.path),
                ) {
                    for row in &rows[anchor.min(end)..=anchor.max(end)] {
                        if !row.is_root {
                            self.selected_paths.insert(row.path.clone());
                        }
                    }
                }
            } else if modifiers.ctrl || modifiers.command {
                if !self.selected_paths.remove(&entry.path) {
                    self.selected_paths.insert(entry.path.clone());
                }
            } else {
                self.selected_paths.clear();
                self.selected_paths.insert(entry.path.clone());
            }
            self.selected = Some(entry.path.clone());
            self.speed_search = None;
            if expandable
                && (entry.is_dir
                    || ui.input(|input| {
                        input
                            .pointer
                            .interact_pos()
                            .is_some_and(|point| point.x < rect.left() + indent + 14.0)
                    }))
            {
                if expanded {
                    self.expanded.remove(&entry.path);
                } else {
                    self.expanded.insert(entry.path.clone());
                }
                *action = Some(TreeAction::SelectDir(entry.path.clone()));
            } else if self.options.single_click
                && !modifiers.ctrl
                && !modifiers.command
                && !modifiers.shift
            {
                *action = Some(TreeAction::OpenFile(entry.path.clone()));
            }
        }
        if resp.double_clicked() && !entry.is_dir {
            *action = Some(TreeAction::OpenFileDoubleClick(entry.path.clone()));
        }
        let dir_for_new = if entry.is_dir {
            entry.path.clone()
        } else {
            entry
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone())
        };
        // Pointer selection precedes the menu authority, so a freshly opened
        // menu remains attached to that same selected path on later frames.
        if resp.secondary_clicked() {
            if !self.selected_paths.contains(&entry.path) {
                self.selected_paths.clear();
                self.selected_paths.insert(entry.path.clone());
            }
            self.selected = Some(entry.path.clone());
        }
        // The path's stamp is read only while the menu is open (a change
        // closes it); listing changes close it through the tree revision.
        let menu_authority =
            egui::Id::new((&self.root, &entry.path, &self.selected, self.revision));
        crate::a11y::bound_context_menu_for_owner(
            &resp,
            focused,
            selected && focused,
            menu_authority,
        )
        .show(|ui| {
            let Some(stamp) = crate::run_context::open_menu_stamp(ui, &entry.path) else {
                return;
            };
            self.selected = Some(entry.path.clone());
            if !stub_library
                && !in_scratches
                && crate::custom_menus::render(ui, "project.popup", |ui, value| {
                    if crate::custom_menus::action_button(ui, value).clicked() {
                        let owner = ui
                            .data(|data| {
                                data.get_temp::<crate::run_context::ProjectOwner>(
                                    crate::run_context::tree_owner_key(),
                                )
                            })
                            .unwrap_or_else(|| crate::run_context::ProjectOwner {
                                root: self.root.clone(),
                                session: 0,
                            });
                        *action = crate::custom_menus::path_action(&owner, &entry.path, value);
                        ui.close();
                    }
                })
            {
                return;
            }
            crate::external_tools::context_submenu_at(
                ui,
                Some(&entry.path),
                crate::external_tools::MenuPlace::Project,
            );
            if !entry.is_dir && ui.button("Open in Right Split").clicked() {
                *action = Some(TreeAction::OpenInRightSplit(entry.path.clone()));
                ui.close();
            }
            if stub_library {
                ui.label("External library (read-only tree operations)");
                if !entry.is_dir
                    && entry
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "so" || extension == "pyd")
                    && ui.button("Generate Native Stub…").clicked()
                {
                    *action = Some(TreeAction::GenerateNativeStub(entry.path.clone()));
                    ui.close();
                }
                copy_target_menu(ui, entry, action);
                if ui.button("Show in Explorer").clicked() {
                    *action = Some(TreeAction::RevealInFiles(entry.path.clone()));
                    ui.close();
                }
                return;
            }
            if !entry.is_dir
                && entry
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(rustpy_semantic::module::extension_module_name)
                    .is_some()
                && ui.button("Generate stub").clicked()
            {
                *action = Some(TreeAction::GenerateNativeStub(entry.path.clone()));
                ui.close();
            }
            if in_scratches {
                Self::scratch_menu(&self.scratch_languages, ui, entry, is_root, action);
                return;
            }
            self.feature_menu(ui, entry, action);
            ui.menu_button("New", |ui| {
                if ui.button("File").clicked() {
                    *action = Some(TreeAction::NewFromTemplate(
                        dir_for_new.clone(),
                        String::new(),
                    ));
                    ui.close();
                }
                for (group, templates) in &self.new_template_groups {
                    ui.menu_button(group, |ui| {
                        for template in templates {
                            if ui.button(template).clicked() {
                                *action = Some(TreeAction::NewFromTemplate(
                                    dir_for_new.clone(),
                                    template.clone(),
                                ));
                                ui.close();
                            }
                        }
                    });
                }
                // File templates contributed by plugins.
                let plugin_templates = crate::plugin_host::file_template_menu_entries();
                if !plugin_templates.is_empty() {
                    ui.separator();
                }
                for (label, kind) in plugin_templates {
                    if ui.button(label).clicked() {
                        *action = Some(TreeAction::NewFile(dir_for_new.clone(), kind));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Python Package").clicked() {
                    *action = Some(TreeAction::NewPackage(dir_for_new.clone()));
                    ui.close();
                }
                if ui.button("Directory").clicked() {
                    *action = Some(TreeAction::NewDirectory(dir_for_new.clone()));
                    ui.close();
                }
                if ui.button(".gitignore File").clicked() {
                    *action = Some(TreeAction::NewGitignore(dir_for_new.clone()));
                    ui.close();
                }
                if ui.button("Data Source (SQLite)").clicked() {
                    *action = Some(TreeAction::NewDataSource(dir_for_new.clone()));
                    ui.close();
                }
                if ui.button("Scratch File").clicked() {
                    *action = Some(TreeAction::NewScratch);
                    ui.close();
                }
                if ui.button("Edit File Templates…").clicked() {
                    *action = Some(TreeAction::EditFileTemplates);
                    ui.close();
                }
            });
            if entry.is_dir
                || entry
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "py" || extension == "pyw")
            {
                if let Some(stamp) = stamp {
                    let cache_id = egui::Id::new((
                        "run-context-configuration",
                        &self.root,
                        &entry.path,
                        &stamp,
                    ));
                    let configuration = ui
                        .data(|data| data.get_temp::<rustpy_project::RunConfig>(cache_id))
                        .unwrap_or_else(|| {
                            let configuration =
                                crate::run_context::config_for_path(&self.root, &entry.path);
                            ui.data_mut(|data| data.insert_temp(cache_id, configuration.clone()));
                            configuration
                        });
                    for context_action in crate::run_context::ContextAction::ALL {
                        let label = match context_action {
                            crate::run_context::ContextAction::Launch(mode) => {
                                format!("{} '{}'", mode.label(), configuration.name)
                            }
                            _ => context_action.label().to_string(),
                        };
                        if ui.button(label).clicked() {
                            let owner = ui
                                .data(|data| {
                                    data.get_temp::<crate::run_context::ProjectOwner>(
                                        crate::run_context::tree_owner_key(),
                                    )
                                })
                                .unwrap_or_else(|| crate::run_context::ProjectOwner {
                                    root: self.root.clone(),
                                    session: 0,
                                });
                            *action = Some(TreeAction::ContextRun(Box::new(
                                crate::run_context::PathRequest {
                                    owner,
                                    path: entry.path.clone(),
                                    stamp: stamp.clone(),
                                    configuration: configuration.clone(),
                                    action: context_action,
                                },
                            )));
                            ui.close();
                        }
                    }
                }
            }
            if ui.button("Open in Terminal").clicked() {
                *action = Some(TreeAction::OpenInTerminal(dir_for_new.clone()));
                ui.close();
            }
            if entry.is_dir {
                ui.separator();
                if ui.button("Find in Files…").clicked() {
                    *action = Some(TreeAction::FindInDirectory(entry.path.clone(), false));
                    ui.close();
                }
                if ui.button("Replace in Files…").clicked() {
                    *action = Some(TreeAction::FindInDirectory(entry.path.clone(), true));
                    ui.close();
                }
            }
            ui.separator();
            if !is_root {
                if ui.button("Rename…").clicked() {
                    *action = Some(TreeAction::Rename(entry.path.clone()));
                    ui.close();
                }
                if ui.button("Delete…").clicked() {
                    *action = Some(TreeAction::Delete(entry.path.clone()));
                    ui.close();
                }
                if ui.button("Add to .gitignore").clicked() {
                    *action = Some(TreeAction::AddToGitignore(entry.path.clone()));
                    ui.close();
                }
            }
            copy_target_menu(ui, entry, action);
            if ui.button("Reveal in File Manager").clicked() {
                *action = Some(TreeAction::RevealInFiles(entry.path.clone()));
                ui.close();
            }
            ui.menu_button("Local History", |ui| {
                let hint = if entry.is_dir {
                    "Snapshots of the files in this folder, including deleted ones"
                } else {
                    "Snapshots of this file"
                };
                if ui.button("Show History").on_hover_text(hint).clicked() {
                    *action = Some(TreeAction::LocalHistory(entry.path.clone()));
                    ui.close();
                }
                if ui.button("Put Label…").clicked() {
                    *action = Some(TreeAction::LocalHistoryLabel(entry.path.clone()));
                    ui.close();
                }
            });
            if entry.is_dir && !is_root {
                ui.menu_button("Mark Directory as", |ui| {
                    let current = self.kind_of(&entry.path);
                    for kind in RootKind::ALL {
                        let on = current == Some(kind);
                        if ui
                            .selectable_label(on, kind.label())
                            .on_hover_text(root_kind_hint(kind))
                            .clicked()
                        {
                            *action = Some(TreeAction::MarkAs(
                                entry.path.clone(),
                                if on { None } else { Some(kind) },
                            ));
                            ui.close();
                        }
                    }
                    if let Some(kind) = current {
                        ui.separator();
                        if ui.button(format!("Unmark as {}", kind.label())).clicked() {
                            *action = Some(TreeAction::MarkAs(entry.path.clone(), None));
                            ui.close();
                        }
                    }
                });
            }
        });
        resp.on_hover_ui(|ui| {
            ui.label(match kind {
                Some(k) => format!("{}\n{}", entry.path.display(), k.label()),
                None => entry.path.display().to_string(),
            });
        });
    }
}

/// Height of one tree row.
const ROW_HEIGHT: f32 = 22.0;

/// Per-frame presentation settings shared by every row.
struct RowStyle {
    indent_width: f32,
    indent_guides: bool,
}

impl RowStyle {
    fn new(ctx: &egui::Context) -> Self {
        let (indent_width, indent_guides) = crate::custom_menus::tree_indent(ctx);
        Self {
            indent_width,
            indent_guides,
        }
    }
}

/// Keyboard input taken by the focused tree, in the order it was typed.
enum TreeInput {
    Key(TreeKey),
    Text(String),
}

/// Removes this frame's key presses the tree handles and the typed text
/// from the input, so neither the editor nor global shortcuts see them.
fn take_tree_input(ui: &egui::Ui, keymap: &Keymap) -> Vec<TreeInput> {
    ui.input_mut(|i| {
        let mut out = Vec::new();
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } => match TreeKey::from_key(
                *key,
                *modifiers,
                keymap.lookup_key(*key, *physical_key, *modifiers),
            ) {
                Some(k) => {
                    out.push(TreeInput::Key(k));
                    false
                }
                None => true,
            },
            egui::Event::Text(t) => {
                out.push(TreeInput::Text(t.clone()));
                false
            }
            _ => true,
        });
        out
    })
}

/// Name and folder color of a marked directory (None: dimmed, for Excluded).
pub fn root_kind_color(kind: RootKind, theme: &UiTheme) -> Option<Color32> {
    let (dark, light) = match kind {
        RootKind::Sources => ((0x5E, 0x9C, 0xE6), (0x2D, 0x6C, 0xC0)),
        RootKind::Tests => ((0x6E, 0xB8, 0x8A), (0x3C, 0x8C, 0x50)),
        RootKind::Resources => ((0x4C, 0xB8, 0xB0), (0x1F, 0x8A, 0x80)),
        RootKind::Templates => ((0xB0, 0x8C, 0xE0), (0x7A, 0x4F, 0xB0)),
        RootKind::NamespacePackage => ((0x9A, 0xB4, 0xD0), (0x50, 0x6A, 0x88)),
        RootKind::Excluded => return None,
    };
    let (r, g, b) = if theme.dark { dark } else { light };
    Some(Color32::from_rgb(r, g, b))
}

/// What marking a directory does (tooltip in Mark Directory as).
pub fn root_kind_hint(kind: RootKind) -> &'static str {
    match kind {
        RootKind::Sources => "Imports resolve from here; added to PYTHONPATH of runs",
        RootKind::Tests => "Test code: imports resolve from here and Run All Tests runs it",
        RootKind::Resources => "Data files (templates, static files, fixtures)",
        RootKind::Templates => "Jinja2 / Django templates",
        RootKind::NamespacePackage => "An implicit namespace package (PEP 420) without __init__.py",
        RootKind::Excluded => "Left out of search, navigation, refactoring and the TODO list",
    }
}

fn paint_icon(
    p: &egui::Painter,
    r: Rect,
    entry: &Entry,
    is_root: bool,
    theme: &UiTheme,
    tint: Option<Color32>,
) {
    if entry.is_dir {
        let color = if theme.high_contrast {
            theme.text
        } else if let Some(c) = tint {
            c
        } else if is_root {
            Color32::from_rgb(0x59, 0xA8, 0x69)
        } else if theme.dark {
            Color32::from_rgb(0xAF, 0xB1, 0xB3)
        } else {
            Color32::from_rgb(0x6E, 0x6E, 0x6E)
        };
        let body = Rect::from_min_max(Pos2::new(r.left(), r.top() + 3.0), r.max);
        p.rect_filled(body, 1.5, color);
        p.rect_filled(
            Rect::from_min_max(r.min, Pos2::new(r.left() + 6.0, r.top() + 4.0)),
            1.0,
            color,
        );
        return;
    }
    let ext = entry
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (bg, label) = match ext.as_str() {
        "py" | "pyw" => (Color32::from_rgb(0x3E, 0x7C, 0xB8), "py"),
        "pyi" => (Color32::from_rgb(0x8A, 0x6E, 0xC4), "pi"),
        "md" => (Color32::from_rgb(0x4A, 0x6F, 0xA5), "M"),
        "json" | "toml" | "yaml" | "yml" | "ini" | "cfg" => {
            (Color32::from_rgb(0xB0, 0x8A, 0x2E), "{}")
        }
        "txt" | "rst" => (Color32::from_rgb(0x80, 0x80, 0x80), "T"),
        "html" | "htm" => (Color32::from_rgb(0xE3, 0x6F, 0x3B), "<>"),
        "css" | "scss" => (Color32::from_rgb(0x2E, 0x6F, 0xB0), "#"),
        "js" | "ts" => (Color32::from_rgb(0xD8, 0xB2, 0x2A), "js"),
        "sh" => (Color32::from_rgb(0x4E, 0xAA, 0x4E), "$"),
        "lock" => (Color32::from_rgb(0x80, 0x80, 0x80), "L"),
        _ => (
            if theme.dark {
                Color32::from_rgb(0x75, 0x75, 0x75)
            } else {
                Color32::from_rgb(0x9A, 0x9A, 0x9A)
            },
            "",
        ),
    };
    let bg = if theme.high_contrast { theme.text } else { bg };
    p.rect_filled(r, 2.0, bg);
    if !label.is_empty() {
        p.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(8.0),
            if theme.high_contrast {
                theme.panel_bg
            } else {
                Color32::WHITE
            },
        );
    }
}

/// Both project and scratch menus carry the clicked path into the same App routes.
fn copy_target_actions(entry: &Entry) -> Vec<(&'static str, TreeAction)> {
    let mut actions = vec![("Copy Path…", TreeAction::CopyPath(entry.path.clone()))];
    if !entry.is_dir
        && matches!(
            entry.path.extension().and_then(|e| e.to_str()),
            Some("py" | "pyi")
        )
    {
        actions.push((
            "Copy Reference",
            TreeAction::CopyReference(entry.path.clone()),
        ));
    }
    actions
}
fn copy_target_menu(ui: &mut egui::Ui, entry: &Entry, action: &mut Option<TreeAction>) {
    for (label, selected) in copy_target_actions(entry) {
        if ui.button(label).clicked() {
            *action = Some(selected);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_details_use_cached_kinds_and_path_spelling_without_populating_children() {
        let root = PathBuf::from("/cached-details-project");
        let mut tree = ProjectTree::new(root.clone());
        let mut entry = Entry {
            path: root.join("src"),
            name: "src".into(),
            is_dir: true,
        };
        tree.source_roots.insert(entry.path.clone());
        assert_eq!(tree.row_detail(&entry, false), "Sources Root");
        entry.path = root.join("tests");
        tree.test_roots.insert(entry.path.clone());
        assert_eq!(tree.row_detail(&entry, false), "Test Sources Root");
        entry.path = root.join("library");
        tree.library_roots.push(entry.path.clone());
        assert_eq!(tree.row_detail(&entry, false), "Library");
        entry.path = root.join("main.py");
        entry.is_dir = false;
        assert_eq!(tree.row_detail(&entry, false), "Python");
        entry.path = root.join("image.unknown-extension");
        assert_eq!(tree.row_detail(&entry, false), ".unknown-extension file");
        assert!(
            tree.cache.is_empty(),
            "descriptions must not enumerate any directory or file children"
        );
    }
    #[test]
    fn cached_project_row_toggle_changes_only_secondary_paint() {
        let mut tree = ProjectTree::new(PathBuf::from("/cached-details-project"));
        tree.scratches_dir = None;
        let entry = Entry {
            path: tree.root.join("cached.py"),
            name: "cached.py".into(),
            is_dir: false,
        };
        // File-nesting lookup reads the parent catalog, not a directory cache
        // under the file path. Both paints must use this already-cached row.
        tree.cache.insert(tree.root.clone(), vec![entry.clone()]);
        tree.selected = Some(entry.path.clone());
        tree.selected_paths.insert(entry.path.clone());
        let ctx = egui::Context::default();
        let theme = UiTheme::dark();
        let paint = |tree: &mut ProjectTree| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 120.0))),
                    ..Default::default()
                },
                |ui| {
                    let mut action = None;
                    let row = tree.rows()[1].clone();
                    assert_eq!(row.entry.path, entry.path);
                    let style = RowStyle::new(ui.ctx());
                    tree.row(ui, &theme, &style, &row, &mut action, true);
                    assert!(action.is_none());
                },
            )
        };
        let has = |output: &egui::FullOutput, text: &str| {
            output.shapes.iter().any(|shape|
            matches!(&shape.shape,egui::Shape::Text(text_shape) if text_shape.galley.text()==text))
        };
        let shown = paint(&mut tree);
        assert!(has(&shown, "cached.py"));
        assert!(has(&shown, "Python"));
        shown.drop_without_applying_deltas();
        assert_eq!(tree.cache.len(), 1);
        assert_eq!(tree.cache[&tree.root][0].path, entry.path);
        tree.show_details = false;
        let hidden = paint(&mut tree);
        assert!(has(&hidden, "cached.py"));
        assert!(!has(&hidden, "Python"));
        hidden.drop_without_applying_deltas();
        assert_eq!(tree.selected, Some(entry.path.clone()));
        assert!(tree.selected_paths.contains(&entry.path));
        assert_eq!(tree.cache.len(), 1);
        let cached = &tree.cache[&tree.root];
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].path, entry.path);
        assert_eq!(cached[0].name, entry.name);
        assert_eq!(cached[0].is_dir, entry.is_dir);
    }

    #[test]
    fn project_and_scratch_copy_actions_bind_clicked_target_and_file_kind() {
        let entry = Entry {
            path: "/project/é.py".into(),
            name: "é.py".into(),
            is_dir: false,
        };
        assert_eq!(
            copy_target_actions(&entry),
            vec![
                ("Copy Path…", TreeAction::CopyPath(entry.path.clone())),
                (
                    "Copy Reference",
                    TreeAction::CopyReference(entry.path.clone())
                ),
            ]
        );
        for entry in [
            Entry {
                is_dir: true,
                ..entry.clone()
            },
            Entry {
                path: "/project/note.txt".into(),
                name: "note.txt".into(),
                is_dir: false,
            },
        ] {
            assert_eq!(
                copy_target_actions(&entry),
                vec![("Copy Path…", TreeAction::CopyPath(entry.path.clone()))]
            );
        }
    }

    /// `proj/{pkg/{a.py, b.py}, dnd.py, hints.py, main.py, shapes.py}` with
    /// the root expanded and `pkg` collapsed (no scratches node).
    fn project(name: &str) -> (PathBuf, ProjectTree) {
        let root = std::env::temp_dir().join(format!("rustpy-tree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        for f in [
            "pkg/a.py",
            "pkg/b.py",
            "dnd.py",
            "hints.py",
            "main.py",
            "shapes.py",
        ] {
            std::fs::write(root.join(f), "x = 1\n").unwrap();
        }
        let mut tree = ProjectTree::new(root.clone());
        tree.scratches_dir = None;
        (root, tree)
    }

    fn selected(tree: &ProjectTree) -> String {
        tree.selected
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn press(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// Screen readers see a named Tree whose active descendant is the
    /// selected TreeItem row (the tree itself keeps the keyboard focus).
    #[test]
    fn selection_is_the_trees_active_descendant() {
        let (root, mut tree) = project("a11y-active-π");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let frame = |tree: &mut ProjectTree, events| {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    tree.ui(ui, &UiTheme::dark(), &Keymap::default_keymap(), true);
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        };
        tree.selected = Some(root.join("main.py"));
        tree.request_focus();
        frame(&mut tree, vec![]);
        let update = frame(&mut tree, vec![]);
        let tree_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == ProjectTree::focus_id().accesskit_id())
            .expect("tree node")
            .1
            .clone();
        assert_eq!(tree_node.role(), egui::accesskit::Role::Tree);
        assert_eq!(tree_node.label(), Some("Project"));
        let active = tree_node.active_descendant().expect("selected row");
        let row = &update.nodes.iter().find(|(id, _)| *id == active).unwrap().1;
        assert_eq!(row.role(), egui::accesskit::Role::TreeItem);
        assert_eq!(row.label(), Some("main.py"));
        assert!(row.is_selected() == Some(true));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parent_focused_tree_opens_only_selected_current_path_menu() {
        let (root, mut tree) = project("keyboard-context-π");
        let first = root.join("main.py");
        let second = root.join("shapes.py");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let mods = Modifiers::CTRL | Modifiers::ALT;
        let chord = || {
            vec![
                egui::Event::ModifiersChanged(mods),
                egui::Event::Key {
                    key: Key::F10,
                    physical_key: Some(Key::F10),
                    pressed: true,
                    repeat: false,
                    modifiers: mods,
                },
                egui::Event::Key {
                    key: Key::F10,
                    physical_key: Some(Key::F10),
                    pressed: false,
                    repeat: false,
                    modifiers: mods,
                },
            ]
        };
        let frame = |tree: &mut ProjectTree, events, keyboard| {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    tree.ui(
                        ui,
                        &UiTheme::high_contrast(),
                        &Keymap::default_keymap(),
                        keyboard,
                    );
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        };
        tree.selected = Some(first.clone());
        frame(&mut tree, vec![], true);
        frame(&mut tree, vec![], true);
        ctx.memory_mut(|memory| memory.request_focus(ProjectTree::focus_id()));
        frame(&mut tree, chord(), true);
        let update = frame(&mut tree, vec![], true);
        assert!(egui::Popup::is_any_open(&ctx));
        for label in ["Copy Path…", "Copy Reference", "Reveal in File Manager"] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(label)),
                "selected path offers {label}"
            );
        }
        assert_eq!(tree.selected.as_ref(), Some(&first));
        // A retained parent focus is not permission to use a retired selection.
        tree.selected = Some(second.clone());
        frame(&mut tree, vec![], true);
        assert!(!egui::Popup::is_any_open(&ctx));
        assert_eq!(tree.selected.as_ref(), Some(&second));
        ctx.memory_mut(|memory| memory.request_focus(ProjectTree::focus_id()));
        frame(&mut tree, chord(), false);
        assert!(
            !egui::Popup::is_any_open(&ctx),
            "foreign modal keyboard authority cannot open a tree menu"
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "x = 1\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn paint_frame(
        ctx: &egui::Context,
        tree: &mut ProjectTree,
        events: Vec<egui::Event>,
        height: f32,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, height))),
                ..Default::default()
            },
            |ui| {
                tree.ui(ui, &UiTheme::dark(), &Keymap::default_keymap(), true);
            },
        )
    }

    fn stamp_reads() -> usize {
        crate::run_context::STAMP_READS.with(|reads| reads.get())
    }

    fn row_builds() -> usize {
        ROW_BUILDS.with(|builds| builds.get())
    }

    #[test]
    fn painting_an_unchanged_tree_stats_nothing_and_reuses_its_rows() {
        let (root, mut tree) = project("paint-no-stat");
        tree.expanded.insert(root.join("pkg"));
        let ctx = egui::Context::default();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        let (stamps, builds) = (stamp_reads(), row_builds());
        for _ in 0..2 {
            let output = paint_frame(&ctx, &mut tree, vec![], 600.0);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == "a.py")));
            output.drop_without_applying_deltas();
        }
        assert_eq!(
            stamp_reads(),
            stamps,
            "no path stamp is read while painting"
        );
        assert_eq!(
            row_builds(),
            builds,
            "no directory is listed while painting"
        );
        // A watcher-driven invalidation (or an expansion) rebuilds the rows once.
        std::fs::write(root.join("added.py"), "").unwrap();
        tree.invalidate_all();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        assert_eq!(row_builds(), builds + 1);
        assert!(tree.visible_rows().iter().any(|row| row.name == "added.py"));
        tree.expanded.remove(&root.join("pkg"));
        assert!(!tree.visible_rows().iter().any(|row| row.name == "a.py"));
        assert_eq!(row_builds(), builds + 2);
        assert_eq!(stamp_reads(), stamps, "still no stamp without an open menu");
        // A collapsed folder (the root of a large project has a thousand) is
        // shown without being listed.
        tree.invalidate_all();
        assert!(tree
            .visible_rows()
            .iter()
            .any(|row| row.path == root.join("pkg")));
        assert!(
            !tree.cache.contains_key(&root.join("pkg")),
            "a collapsed folder is not listed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_row_menu_closes_when_its_path_changes_on_disk() {
        let (root, mut tree) = project("menu-stamp");
        let file = root.join("main.py");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mods = Modifiers::CTRL | Modifiers::ALT;
        let chord = vec![
            egui::Event::ModifiersChanged(mods),
            egui::Event::Key {
                key: Key::F10,
                physical_key: Some(Key::F10),
                pressed: true,
                repeat: false,
                modifiers: mods,
            },
        ];
        tree.selected = Some(file.clone());
        for _ in 0..2 {
            paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        }
        let stamps = stamp_reads();
        ctx.memory_mut(|memory| memory.request_focus(ProjectTree::focus_id()));
        paint_frame(&ctx, &mut tree, chord, 600.0).drop_without_applying_deltas();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        assert!(egui::Popup::is_any_open(&ctx));
        assert!(stamp_reads() > stamps, "the open menu stamps its path");
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        assert!(
            egui::Popup::is_any_open(&ctx),
            "an unchanged path keeps it open"
        );
        std::fs::write(&file, "x = 2  # changed while the menu is open\n").unwrap();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        paint_frame(&ctx, &mut tree, vec![], 600.0).drop_without_applying_deltas();
        assert!(
            !egui::Popup::is_any_open(&ctx),
            "a changed path closes the menu"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_expanded_tree_paints_only_the_rows_in_view() {
        let root = tempfile::tempdir().unwrap();
        let mut tree = ProjectTree::new(root.path().into());
        tree.scratches_dir = None;
        for d in 0..100 {
            let dir = root.path().join(format!("package_{d:03}"));
            std::fs::create_dir(&dir).unwrap();
            for f in 0..100 {
                std::fs::write(dir.join(format!("module_{f:03}.py")), "").unwrap();
                if f % 10 == 0 {
                    std::fs::write(dir.join(format!("module_{f:03}.pyi")), "").unwrap();
                }
            }
            tree.expanded.insert(dir);
        }
        assert!(tree.visible_rows().len() > 10_000);
        let ctx = egui::Context::default();
        paint_frame(&ctx, &mut tree, vec![], 800.0).drop_without_applying_deltas();
        let builds = row_builds();
        let mut times = Vec::new();
        for _ in 0..15 {
            let start = std::time::Instant::now();
            let output = paint_frame(&ctx, &mut tree, vec![], 800.0);
            times.push(start.elapsed());
            let texts = output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::Shape::Text(_)))
                .count();
            assert!(texts < 400, "only rows in view are painted ({texts} texts)");
            output.drop_without_applying_deltas();
        }
        assert_eq!(row_builds(), builds);
        times.sort();
        let median = times[times.len() / 2];
        assert!(
            median < std::time::Duration::from_millis(16),
            "a frame of a 10k-row tree takes {median:?}"
        );
    }

    #[test]
    fn arrows_walk_the_visible_rows_and_left_right_fold() {
        let (root, mut tree) = project("arrows");
        let names: Vec<String> = tree.visible_rows().into_iter().map(|r| r.name).collect();
        let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            names,
            [
                root_name.as_str(),
                "pkg",
                "dnd.py",
                "hints.py",
                "main.py",
                "shapes.py"
            ]
        );
        // Nothing selected: the first key selects the first row.
        assert_eq!(tree.key(TreeKey::Down), None);
        assert_eq!(tree.selected.as_deref(), Some(root.as_path()));
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "pkg");
        // Right expands a folder, then goes to its first child.
        tree.key(TreeKey::Right);
        assert!(tree.expanded.contains(&root.join("pkg")));
        assert_eq!(selected(&tree), "pkg");
        tree.key(TreeKey::Right);
        assert_eq!(selected(&tree), "a.py");
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "b.py");
        // Left goes to the parent, then collapses it.
        tree.key(TreeKey::Left);
        assert_eq!(selected(&tree), "pkg");
        tree.key(TreeKey::Left);
        assert!(!tree.expanded.contains(&root.join("pkg")));
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "dnd.py");
        tree.key(TreeKey::End);
        assert_eq!(selected(&tree), "shapes.py");
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "shapes.py", "Down stops at the last row");
        tree.key(TreeKey::Up);
        assert_eq!(selected(&tree), "main.py");
        tree.page_rows = 3;
        tree.key(TreeKey::PageUp);
        assert_eq!(selected(&tree), "pkg");
        tree.key(TreeKey::Home);
        assert_eq!(tree.selected.as_deref(), Some(root.as_path()));
        assert_eq!(
            tree.scroll_to.as_deref(),
            Some(root.as_path()),
            "keyboard moves scroll the row into view"
        );
    }

    #[test]
    fn enter_delete_rename_and_escape_act_on_the_selection() {
        let (root, mut tree) = project("actions");
        tree.selected = Some(root.join("hints.py"));
        assert_eq!(
            tree.key(TreeKey::Open),
            Some(TreeAction::OpenFileDoubleClick(root.join("hints.py")))
        );
        assert_eq!(
            tree.key(TreeKey::Delete),
            Some(TreeAction::Delete(root.join("hints.py")))
        );
        assert_eq!(
            tree.key(TreeKey::Rename),
            Some(TreeAction::Rename(root.join("hints.py")))
        );
        assert_eq!(tree.key(TreeKey::Escape), Some(TreeAction::FocusEditor));
        // Enter on a folder expands or collapses it.
        tree.selected = Some(root.join("pkg"));
        assert_eq!(tree.key(TreeKey::Open), None);
        assert!(tree.expanded.contains(&root.join("pkg")));
        tree.key(TreeKey::Open);
        assert!(!tree.expanded.contains(&root.join("pkg")));
        // The project root is neither deleted nor renamed from the keyboard.
        tree.selected = Some(root.clone());
        assert_eq!(tree.key(TreeKey::Delete), None);
        assert_eq!(tree.key(TreeKey::Rename), None);
        assert!(root.join("hints.py").exists());
    }

    #[test]
    fn typing_searches_from_the_selection_and_up_down_step_through_matches() {
        let (root, mut tree) = project("speed");
        tree.selected = Some(root.join("dnd.py"));
        for c in ["s", "h", "a"] {
            tree.type_text(c);
        }
        assert_eq!(tree.speed_search(), Some("sha"));
        assert_eq!(selected(&tree), "shapes.py");
        // No match: the selection stays and the search shows as a miss.
        tree.type_text("x");
        assert_eq!(selected(&tree), "shapes.py");
        assert!(tree.speed_search_miss);
        // Backspace shortens it; emptying it ends the search.
        for _ in 0..4 {
            tree.key(TreeKey::Backspace);
        }
        assert_eq!(tree.speed_search(), None);
        assert_eq!(selected(&tree), "shapes.py");
        // ".py" from the root: Down/Up move between matching rows only.
        tree.selected = Some(root.clone());
        tree.type_text(".py");
        assert_eq!(selected(&tree), "dnd.py");
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "hints.py");
        tree.key(TreeKey::Down);
        assert_eq!(selected(&tree), "main.py");
        tree.key(TreeKey::Up);
        assert_eq!(selected(&tree), "hints.py");
        // Esc closes the search first, then returns to the editor.
        assert_eq!(tree.key(TreeKey::Escape), None);
        assert_eq!(tree.speed_search(), None);
        assert_eq!(tree.key(TreeKey::Escape), Some(TreeAction::FocusEditor));
        // A space alone does not start a search; Enter opens what it found.
        tree.type_text(" ");
        assert_eq!(tree.speed_search(), None);
        tree.type_text("mpy");
        assert_eq!(selected(&tree), "main.py");
        assert_eq!(
            tree.key(TreeKey::Open),
            Some(TreeAction::OpenFileDoubleClick(root.join("main.py")))
        );
        assert_eq!(tree.speed_search(), None);
    }

    #[test]
    fn speed_search_matches_substrings_and_word_starts() {
        assert!(speed_search_matches("shapes.py", "sha"));
        assert!(speed_search_matches("shapes.py", "SHA"));
        assert!(speed_search_matches("main.py", "ain"));
        assert!(speed_search_matches("main.py", "mpy"));
        assert!(speed_search_matches("test_main.py", "tmp"));
        assert!(speed_search_matches("MyClassName.py", "mcn"));
        assert!(speed_search_matches("anything", ""));
        assert!(!speed_search_matches("dnd.py", "sha"));
        assert!(!speed_search_matches("main.py", "mx"));
        assert!(
            !speed_search_matches("main.py", "ap"),
            "the first fragment starts a word"
        );
        assert!(!speed_search_matches("", "a"));
        assert!(speed_search_matches("données.py", "DON"));
    }

    #[test]
    fn chords_the_tree_handles() {
        let none = Modifiers::NONE;
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            TreeKey::from_key(Key::ArrowDown, none, Some(Action::CaretDown)),
            Some(TreeKey::Down)
        );
        assert_eq!(
            TreeKey::from_key(Key::ArrowUp, shift, Some(Action::SelectUp)),
            Some(TreeKey::Up)
        );
        assert_eq!(
            TreeKey::from_key(Key::Enter, none, Some(Action::Enter)),
            Some(TreeKey::Open)
        );
        assert_eq!(
            TreeKey::from_key(Key::Delete, none, Some(Action::Delete)),
            Some(TreeKey::Delete)
        );
        assert_eq!(
            TreeKey::from_key(Key::Delete, alt, Some(Action::SafeDelete)),
            Some(TreeKey::Delete)
        );
        assert_eq!(
            TreeKey::from_key(Key::F6, shift, Some(Action::Rename)),
            Some(TreeKey::Rename)
        );
        assert_eq!(
            TreeKey::from_key(Key::F2, none, Some(Action::NextError)),
            Some(TreeKey::Rename)
        );
        assert_eq!(
            TreeKey::from_key(Key::Escape, none, Some(Action::Escape)),
            Some(TreeKey::Escape)
        );
        // IDE shortcuts and plain letters (typed text) are not tree keys.
        assert_eq!(TreeKey::from_key(Key::S, ctrl, Some(Action::Save)), None);
        assert_eq!(
            TreeKey::from_key(
                Key::ArrowLeft,
                Modifiers {
                    ctrl: true,
                    alt: true,
                    command: true,
                    ..Modifiers::NONE
                },
                Some(Action::NavigateBack)
            ),
            None
        );
        assert_eq!(
            TreeKey::from_key(Key::Enter, shift, Some(Action::StartNewLine)),
            None
        );
        assert_eq!(TreeKey::from_key(Key::A, none, None), None);
    }

    /// One frame of a 600×600 window with the tree in its left 300 px: the
    /// tree's action and the input events left for the rest of the frame.
    fn frame(
        ctx: &egui::Context,
        tree: &mut ProjectTree,
        events: Vec<egui::Event>,
        keyboard: bool,
    ) -> (Option<TreeAction>, Vec<egui::Event>) {
        frame_in(ctx, tree, events, keyboard, true)
    }

    /// `frame` with the window in the foreground (`window_focused`) or not.
    fn frame_in(
        ctx: &egui::Context,
        tree: &mut ProjectTree,
        events: Vec<egui::Event>,
        keyboard: bool,
        window_focused: bool,
    ) -> (Option<TreeAction>, Vec<egui::Event>) {
        let keymap = Keymap::default_keymap();
        let theme = UiTheme::for_name("Dark");
        let input = egui::RawInput {
            events,
            focused: window_focused,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 600.0))),
            ..Default::default()
        };
        let mut out = (None, Vec::new());
        ctx.run_ui(input, |ui| {
            let left = Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 600.0));
            out.0 = ui
                .scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                    tree.ui(ui, &theme, &keymap, keyboard)
                })
                .inner;
            out.1 = ui.input(|i| i.events.clone());
        })
        .drop_without_applying_deltas();
        out
    }

    fn click(ctx: &egui::Context, tree: &mut ProjectTree, at: Pos2) -> Option<TreeAction> {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(
            ctx,
            tree,
            vec![egui::Event::PointerMoved(at), button(true)],
            true,
        );
        frame(ctx, tree, vec![button(false)], true).0
    }

    #[test]
    fn clicking_a_row_selects_it_and_gives_the_tree_the_keyboard() {
        let (root, mut tree) = project("click");
        let ctx = egui::Context::default();
        frame(&ctx, &mut tree, Vec::new(), true);
        // Rows are 22 px high from the top: the root, pkg, dnd.py, …
        assert_eq!(
            click(&ctx, &mut tree, Pos2::new(60.0, 22.0 * 2.5)),
            Some(TreeAction::OpenFile(root.join("dnd.py")))
        );
        frame(&ctx, &mut tree, Vec::new(), true);
        assert!(
            ProjectTree::has_focus(&ctx),
            "the click keeps the keyboard in the tree"
        );
        let (_, left) = frame(
            &ctx,
            &mut tree,
            vec![press(Key::ArrowDown), egui::Event::Text("x".into())],
            true,
        );
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(selected(&tree), "hints.py");
        // The empty space below the rows is the tree too; a click elsewhere takes the keyboard away.
        click(&ctx, &mut tree, Pos2::new(60.0, 500.0));
        assert!(ProjectTree::has_focus(&ctx));
        click(&ctx, &mut tree, Pos2::new(450.0, 300.0));
        assert!(!ProjectTree::has_focus(&ctx));
        let (_, left) = frame(&ctx, &mut tree, vec![press(Key::ArrowDown)], true);
        assert_eq!(left.len(), 1);
        assert_eq!(selected(&tree), "hints.py");
    }

    #[test]
    fn the_focused_tree_takes_its_keys_and_typing_out_of_the_frame() {
        let (root, mut tree) = project("frame");
        tree.selected = Some(root.join("dnd.py"));
        let ctx = egui::Context::default();
        // Without focus the keys stay for the editor and global shortcuts.
        let (_, left) = frame(&ctx, &mut tree, vec![press(Key::ArrowDown)], true);
        assert_eq!(left.len(), 1);
        assert_eq!(selected(&tree), "dnd.py");
        tree.request_focus();
        frame(&ctx, &mut tree, Vec::new(), true);
        assert!(ProjectTree::has_focus(&ctx));
        frame(&ctx, &mut tree, Vec::new(), true);
        let (action, left) = frame(
            &ctx,
            &mut tree,
            vec![press(Key::ArrowDown), press(Key::Delete)],
            true,
        );
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(selected(&tree), "hints.py");
        assert_eq!(action, Some(TreeAction::Delete(root.join("hints.py"))));
        // Typed text is a speed search; it never reaches other widgets.
        let (_, left) = frame(
            &ctx,
            &mut tree,
            vec![egui::Event::Text("sha".into()), press(Key::S)],
            true,
        );
        assert!(
            left.iter().all(|e| !matches!(e, egui::Event::Text(_))),
            "{left:?}"
        );
        assert_eq!(selected(&tree), "shapes.py");
        let (action, _) = frame(&ctx, &mut tree, vec![press(Key::Enter)], true);
        assert_eq!(
            action,
            Some(TreeAction::OpenFileDoubleClick(root.join("shapes.py")))
        );
        assert!(
            ProjectTree::has_focus(&ctx),
            "arrows and Enter do not move egui's focus away"
        );
        // The window going to the background keeps the tree's focus and its speed search.
        frame(&ctx, &mut tree, vec![egui::Event::Text("ma".into())], true);
        assert_eq!(
            (tree.speed_search(), selected(&tree).as_str()),
            (Some("ma"), "main.py")
        );
        frame_in(&ctx, &mut tree, Vec::new(), true, false);
        frame_in(&ctx, &mut tree, Vec::new(), true, false);
        assert_eq!(tree.speed_search(), Some("ma"));
        assert!(ProjectTree::has_focus(&ctx));
        frame(&ctx, &mut tree, vec![press(Key::Escape)], true);
        assert_eq!(tree.speed_search(), None);
        // A dialog owns the keyboard: the tree lets go of it and of the keys.
        let (_, left) = frame(&ctx, &mut tree, vec![press(Key::ArrowUp)], false);
        assert_eq!(left.len(), 1);
        assert!(!ProjectTree::has_focus(&ctx));
        assert_eq!(selected(&tree), "main.py");
    }

    #[test]
    fn tree_holds_the_project_model() {
        let root = PathBuf::from("/w/app");
        let mut m = ProjectModel::default();
        m.mark(&root.join("src"), Some(RootKind::Sources));
        m.mark(&root.join("tests"), Some(RootKind::Tests));
        m.mark(&root.join("assets"), Some(RootKind::Resources));
        m.mark(&root.join("vendor"), Some(RootKind::Excluded));
        m.add_content_root(Path::new("/w/shared"));
        let mut t = ProjectTree::new(root.clone());
        t.set_model(&m);
        assert_eq!(t.model(), m);
        assert_eq!(t.kind_of(&root.join("tests")), Some(RootKind::Tests));
        assert_eq!(t.kind_of(&root.join("other")), None);
        assert_eq!(
            t.import_roots(),
            vec![
                root.clone(),
                PathBuf::from("/w/shared"),
                root.join("src"),
                root.join("tests")
            ]
        );
        // Revealing a file in an attached content root expands that node only.
        t.reveal(Path::new("/w/shared/pkg/mod.py"));
        assert!(
            t.expanded.contains(Path::new("/w/shared/pkg"))
                && t.expanded.contains(Path::new("/w/shared"))
        );
        assert!(!t.expanded.contains(Path::new("/w")));
        let theme = UiTheme::dark();
        assert!(root_kind_color(RootKind::Excluded, &theme).is_none());
        assert_ne!(
            root_kind_color(RootKind::Sources, &theme),
            root_kind_color(RootKind::Tests, &theme)
        );
    }
    #[test]
    fn compare_chord_captures_selected_file_and_refuses_directory() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("π file.py");
        std::fs::write(&file, "x=1\n").unwrap();
        let mut tree = ProjectTree::new(root.path().to_owned());
        tree.selected = Some(file.clone());
        let mods = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            TreeKey::from_key(Key::D, mods, None),
            Some(TreeKey::Compare)
        );
        assert_eq!(
            tree.key(TreeKey::Compare),
            Some(TreeAction::CompareFile(file))
        );
        tree.selected = Some(root.path().to_owned());
        assert_eq!(tree.key(TreeKey::Compare), None);
    }
}

#[cfg(test)]
mod project_view_feature_tests {
    use super::*;

    #[test]
    fn filters_keep_ancestors_and_nesting_does_not_hide_the_only_matching_child() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("src");
        std::fs::create_dir(&directory).unwrap();
        let parent = directory.join("model.py");
        let child = directory.join("model.pyi");
        std::fs::write(&parent, "pass\n").unwrap();
        std::fs::write(&child, "pass\n").unwrap();
        let mut tree = ProjectTree::new(root.path().into());
        tree.scratches_dir = None;
        tree.expanded.insert(directory.clone());
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|row| row.path == parent));
        assert!(!rows.iter().any(|row| row.path == child));
        tree.expanded.insert(parent.clone());
        assert!(tree
            .visible_rows()
            .iter()
            .any(|row| row.path == child && row.depth == 3));
        tree.set_allowed_paths(Some(
            [root.path().into(), directory.clone(), child.clone()]
                .into_iter()
                .collect(),
        ));
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|row| row.path == child));
        assert!(!rows.iter().any(|row| row.path == parent));
        tree.excluded.insert(directory);
        tree.options.show_excluded = false;
        assert_eq!(tree.visible_rows().len(), 1);
    }

    #[test]
    fn compact_and_flat_packages_preserve_resource_siblings_and_all_package_levels() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("__init__.py"), "").unwrap();
        for directory in ["a", "a/b", "a/b/c", "a/assets"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        for file in [
            "a/__init__.py",
            "a/b/__init__.py",
            "a/b/c/__init__.py",
            "a/b/c/code.py",
            "a/assets/icon.txt",
        ] {
            std::fs::write(root.path().join(file), "").unwrap();
        }
        let mut tree = ProjectTree::new(root.path().into());
        tree.scratches_dir = None;
        tree.options.compact_packages = true;
        let rows = tree.visible_rows();
        assert!(
            rows.iter().any(|row| row.name == "a"),
            "resource sibling prevents a.b compaction"
        );
        tree.expanded.insert(root.path().join("a"));
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|row| row.name == "b.c"));
        assert!(rows.iter().any(|row| row.name == "assets"));
        tree.options.flatten_packages = true;
        let rows = tree.visible_rows();
        for name in ["a", "a.b", "a.b.c"] {
            assert!(
                rows.iter().any(|row| row.name == name && row.depth == 1),
                "flattened {name}"
            );
        }
        tree.expanded.insert(root.path().join("a/b/c"));
        assert!(tree.visible_rows().iter().any(|row| row.name == "code.py"));
    }

    #[test]
    fn type_sort_multiselect_and_escape_preferences_use_the_tree_model() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("zdir")).unwrap();
        for name in ["z.py", "a.txt", "b.py"] {
            std::fs::write(root.path().join(name), "").unwrap();
        }
        let mut tree = ProjectTree::new(root.path().into());
        tree.scratches_dir = None;
        tree.options.sort_by_type = true;
        let names: Vec<_> = tree
            .visible_rows()
            .into_iter()
            .skip(1)
            .map(|row| row.name)
            .collect();
        assert_eq!(names, ["zdir", "b.py", "z.py", "a.txt"]);
        tree.selected = Some(root.path().join("z.py"));
        tree.selected_paths = [root.path().join("z.py"), root.path().join("b.py")]
            .into_iter()
            .collect();
        assert!(
            matches!(tree.key(TreeKey::Delete),Some(TreeAction::DeleteMany(paths))if paths.len()==2)
        );
        tree.options.focus_editor_on_escape = false;
        assert_eq!(tree.key(TreeKey::Escape), None);
        tree.options.focus_editor_on_escape = true;
        assert_eq!(tree.key(TreeKey::Escape), Some(TreeAction::FocusEditor));
    }
}

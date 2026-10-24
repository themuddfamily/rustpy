use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_vcs::log_workspace::{Index, Query, RebaseAction, RebasePlan};

fn date_bound(text: &str, end: bool) -> Result<i64, String> {
    if let Ok(seconds) = text.parse::<i64>() {
        return Ok(seconds);
    }
    let mut date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| "Use YYYY-MM-DD or a Unix timestamp".to_string())?;
    if end {
        date = date.succ_opt().ok_or("Date is out of range")?;
    }
    let time = date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .earliest()
        .ok_or("Date has no local midnight")?
        .timestamp();
    Ok(if end { time - 1 } else { time })
}

#[derive(Clone, Default)]
struct LogTab {
    root: Option<PathBuf>,
    name: String,
    pinned: bool,
    query: Query,
    text: String,
    selected: Option<String>,
}
pub struct Workspace {
    saved_preferences: String,
    project_root: PathBuf,
    job_root: PathBuf,
    job_project: PathBuf,
    last_selected: Option<String>,
    scroll_to: Option<String>,
    indexed_root: PathBuf,
    history_path: Option<PathBuf>,
    query: Query,
    index: Option<Index>,
    tabs: Vec<LogTab>,
    active: usize,
    pub regex: bool,
    pub case_sensitive: bool,
    pub highlight: usize,
    pub compact: bool,
    pub long_edges: bool,
    pub tags: bool,
    pub root_names: bool,
    pub relative_dates: bool,
    pub columns: [bool; 4],
    pub collapsed: bool,
    pub branches_visible: bool,
    branch_search: String,
    goto: String,
    focus_goto: bool,
    path: String,
    since: String,
    until: String,
    root_input: String,
    roots: Vec<PathBuf>,
    active_root: Option<PathBuf>,
    compare: String,
    branch_name: String,
    patch_path: String,
    pending: Option<(String, Vec<String>)>,
    plan: Option<RebasePlan>,
    job: Option<rustpy_vcs::GitJob>,
    output: String,
    viewer: Option<(String, String)>,
    pub parent: usize,
    pub group_files: bool,
    issue_pattern: String,
    issue_url: String,
    members: HashSet<String>,
    nonpicked: HashSet<String>,
    my_email: String,
    pub error: String,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            saved_preferences: String::new(),
            project_root: PathBuf::new(),
            job_root: PathBuf::new(),
            job_project: PathBuf::new(),
            last_selected: None,
            scroll_to: None,
            indexed_root: PathBuf::new(),
            history_path: None,
            query: Query::default(),
            index: None,
            tabs: vec![LogTab {
                name: "Log".into(),
                ..Default::default()
            }],
            active: 0,
            regex: false,
            case_sensitive: false,
            highlight: 0,
            compact: false,
            long_edges: true,
            tags: true,
            root_names: false,
            relative_dates: false,
            columns: [true; 4],
            collapsed: false,
            branches_visible: true,
            branch_search: String::new(),
            goto: String::new(),
            focus_goto: false,
            path: String::new(),
            since: String::new(),
            until: String::new(),
            root_input: String::new(),
            roots: Vec::new(),
            active_root: None,
            compare: "HEAD".into(),
            branch_name: String::new(),
            patch_path: String::new(),
            pending: None,
            plan: None,
            job: None,
            output: String::new(),
            viewer: None,
            parent: 0,
            group_files: true,
            issue_pattern: r"[A-Z]+-\d+".into(),
            issue_url: String::new(),
            members: HashSet::new(),
            nonpicked: HashSet::new(),
            my_email: String::new(),
            error: String::new(),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Preferences {
    compact: bool,
    long_edges: bool,
    tags: bool,
    root_names: bool,
    relative_dates: bool,
    columns: [bool; 4],
    collapsed: bool,
    branches_visible: bool,
    group_files: bool,
    issue_pattern: String,
    issue_url: String,
    highlight: usize,
    regex: bool,
    case_sensitive: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self::from_workspace(&Workspace::default())
    }
}
impl Preferences {
    fn from_workspace(w: &Workspace) -> Self {
        Self {
            compact: w.compact,
            long_edges: w.long_edges,
            tags: w.tags,
            root_names: w.root_names,
            relative_dates: w.relative_dates,
            columns: w.columns,
            collapsed: w.collapsed,
            branches_visible: w.branches_visible,
            group_files: w.group_files,
            issue_pattern: w.issue_pattern.clone(),
            issue_url: w.issue_url.clone(),
            highlight: w.highlight,
            regex: w.regex,
            case_sensitive: w.case_sensitive,
        }
    }
    fn apply(self, w: &mut Workspace) {
        w.compact = self.compact;
        w.long_edges = self.long_edges;
        w.tags = self.tags;
        w.root_names = self.root_names;
        w.relative_dates = self.relative_dates;
        w.columns = self.columns;
        w.collapsed = self.collapsed;
        w.branches_visible = self.branches_visible;
        w.group_files = self.group_files;
        w.issue_pattern = self.issue_pattern;
        w.issue_url = self.issue_url;
        w.highlight = self.highlight.min(4);
        w.regex = self.regex;
        w.case_sensitive = self.case_sensitive;
    }
}
fn preferences_path(root: &Path) -> PathBuf {
    let hash = root
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(14695981039346656037u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(1099511628211)
        });
    rustpy_settings::Settings::config_dir()
        .join("git-log")
        .join(format!("{hash:016x}.json"))
}
impl Workspace {
    fn load_preferences(&mut self, root: &Path) {
        if let Ok(bytes) = std::fs::read(preferences_path(root)) {
            if let Ok(preferences) = serde_json::from_slice::<Preferences>(&bytes) {
                preferences.apply(self);
            }
        }
        self.saved_preferences =
            serde_json::to_string(&Preferences::from_workspace(self)).unwrap_or_default();
    }
    fn save_preferences(&mut self) {
        let Ok(text) = serde_json::to_string(&Preferences::from_workspace(self)) else {
            return;
        };
        if text == self.saved_preferences {
            return;
        }
        let path = preferences_path(&self.project_root);
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if std::fs::write(path, &text).is_ok() {
            self.saved_preferences = text;
        }
    }
    pub fn take_scroll(&mut self, id: &str) -> bool {
        if self.scroll_to.as_deref() == Some(id) {
            self.scroll_to = None;
            true
        } else {
            false
        }
    }
    pub fn graph_ready(&self, text: &str) -> bool {
        self.index.is_none() && self.query.text == text
    }
    pub fn reference_color(&self, c: &CommitInfo, label: &str, theme: &UiTheme) -> Color32 {
        if theme.high_contrast {
            return theme.panel_bg;
        }
        if label == "HEAD" {
            theme.warning
        } else if c
            .refs
            .iter()
            .any(|r| r.strip_prefix("refs/tags/") == Some(label))
        {
            Color32::from_rgb(139, 92, 180)
        } else if c
            .refs
            .iter()
            .any(|r| r.strip_prefix("refs/remotes/") == Some(label))
        {
            Color32::from_rgb(74, 111, 74)
        } else {
            Color32::from_rgb(74, 136, 199)
        }
    }
    pub fn changed_files_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        id: &str,
        files: &[CommitFile],
        events: &mut Vec<GitEvent>,
    ) -> Option<PathBuf> {
        #[derive(Default)]
        struct Node {
            dirs: std::collections::BTreeMap<String, Node>,
            files: Vec<CommitFile>,
        }
        fn draw(
            node: &Node,
            ui: &mut egui::Ui,
            workspace: &mut Workspace,
            theme: &UiTheme,
            repo: &GitRepo,
            id: &str,
            events: &mut Vec<GitEvent>,
        ) -> Option<PathBuf> {
            let mut selected = None;
            for (name, child) in &node.dirs {
                egui::CollapsingHeader::new(name)
                    .default_open(true)
                    .show(ui, |ui| {
                        if let Some(path) = draw(child, ui, workspace, theme, repo, id, events) {
                            selected = Some(path);
                        }
                    });
            }
            for file in &node.files {
                let name = if workspace.group_files {
                    file.path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    file.path
                        .strip_prefix(repo.root())
                        .unwrap_or(&file.path)
                        .display()
                        .to_string()
                };
                let response = ui.selectable_label(
                    false,
                    egui::RichText::new(format!(
                        "{name}   +{} −{}",
                        file.additions, file.deletions
                    ))
                    .color(status_color(file.status, theme).unwrap_or(theme.text)),
                );
                response
                    .context_menu(|ui| workspace.file_actions(ui, repo, id, &file.path, events));
                if response.clicked() {
                    selected = Some(file.path.clone());
                }
                if response.double_clicked() {
                    events.push(GitEvent::OpenFile(file.path.clone()));
                }
            }
            selected
        }
        let mut tree = Node::default();
        for file in files {
            let mut node = &mut tree;
            if self.group_files {
                let relative = file.path.strip_prefix(repo.root()).unwrap_or(&file.path);
                if let Some(directory) = relative.parent() {
                    for component in directory.components() {
                        node = node
                            .dirs
                            .entry(component.as_os_str().to_string_lossy().into_owned())
                            .or_default();
                    }
                }
            }
            node.files.push(file.clone());
        }
        draw(&tree, ui, self, theme, repo, id, events)
    }
    pub fn set_all(&mut self, all: bool) {
        self.query.all = all;
        self.query.branches.clear();
    }
    pub fn file_diff(&self, repo: &GitRepo, id: &str, path: &Path) -> String {
        if self.parent == 0 {
            repo.commit_file_diff(id, path).unwrap_or_default()
        } else {
            repo.log_parent_diff(id, self.parent)
                .unwrap_or_default()
                .into_iter()
                .find(|f| f.path == path)
                .map(|f| f.patch)
                .unwrap_or_default()
        }
    }
    pub fn has_query(&self) -> bool {
        self.query != Query::default()
            || self.active_root.is_some()
            || !self.indexed_root.as_os_str().is_empty()
    }
    pub fn reload(&mut self, root: &Path, limit: usize) {
        if self.index.is_some() {
            return;
        }
        self.index = Some(Index::start(
            self.active_root
                .clone()
                .unwrap_or_else(|| root.to_path_buf()),
            self.query.clone(),
            limit,
        ));
    }
    pub fn details_header(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        c: &CommitInfo,
    ) -> Option<Vec<CommitFile>> {
        ui.label(&c.message);
        crate::task_trackers::issue_navigation(ui, &c.message);
        ui.menu_button("Issue Navigation patterns", |ui| {
            ui.label("Issue regex");
            ui.text_edit_singleline(&mut self.issue_pattern);
            ui.label("URL template: $0 is the complete match; $1… are captures");
            ui.text_edit_singleline(&mut self.issue_url);
        });
        if !self.issue_url.is_empty() {
            if let Ok(pattern) = regex::Regex::new(&self.issue_pattern) {
                ui.horizontal_wrapped(|ui| {
                    for captures in pattern.captures_iter(&c.message) {
                        let mut url = String::new();
                        captures.expand(&self.issue_url, &mut url);
                        if url.starts_with("https://") || url.starts_with("http://") {
                            ui.hyperlink_to(captures.get(0).unwrap().as_str(), url);
                        }
                    }
                });
            }
        }
        ui.checkbox(&mut self.group_files, "Group changed files by directory");
        let mut files = None;
        if c.parents.len() > 1 {
            egui::ComboBox::from_id_salt("log-merge-parent")
                .selected_text(format!("Changes from parent {}", self.parent + 1))
                .show_ui(ui, |ui| {
                    for (n, parent) in c.parents.iter().enumerate() {
                        if ui
                            .selectable_value(
                                &mut self.parent,
                                n,
                                format!("Parent {}: {}", n + 1, &parent[..8]),
                            )
                            .changed()
                        {
                            match repo.log_parent_diff(&c.id, n) {
                                Ok(diff) => {
                                    files = Some(
                                        diff.into_iter()
                                            .map(|f| CommitFile {
                                                path: f.path,
                                                status: f.status,
                                                additions: f.additions,
                                                deletions: f.deletions,
                                            })
                                            .collect(),
                                    )
                                }
                                Err(e) => self.error = e.to_string(),
                            }
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Merge parent"));
        }
        files
    }
    pub fn file_actions(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        id: &str,
        path: &Path,
        events: &mut Vec<GitEvent>,
    ) {
        if ui.button("Show history from this revision").clicked() {
            self.query.path = Some(path.to_path_buf());
            self.query.branches = vec![id.to_string()];
            self.reload(repo.root(), 1000);
            ui.close();
        }
        if ui.button("Jump to source").clicked() {
            events.push(GitEvent::OpenFile(path.to_path_buf()));
            ui.close();
        }
        if ui.button("Open repository version").clicked() {
            match repo.file_at_commit(id, path) {
                Ok(text) => {
                    self.viewer = Some((format!("{} @ {}", path.display(), &id[..8]), text))
                }
                Err(e) => self.error = e.to_string(),
            }
            ui.close();
        }
        if ui.button("Show diff").clicked() {
            match repo.commit_file_diff(id, path) {
                Ok(text) => self.viewer = Some(("Commit diff".into(), text)),
                Err(e) => self.error = e.to_string(),
            }
            ui.close();
        }
        if ui.button("Compare with local").clicked() {
            match repo.file_at_commit(id, path).and_then(|old| {
                let current = std::fs::read_to_string(path)?;
                Ok(similar::TextDiff::from_lines(&old, &current)
                    .unified_diff()
                    .header("Repository revision", "Working tree")
                    .to_string())
            }) {
                Ok(text) => self.viewer = Some(("Compare with local".into(), text)),
                Err(e) => self.error = e.to_string(),
            }
            ui.close();
        }
    }
    pub fn matches(&self, c: &CommitInfo, text: &str) -> bool {
        let hay = format!("{} {} {} {}", c.message, c.author, c.email, c.id);
        if text.trim().is_empty() {
            return true;
        }
        if self.regex {
            regex::RegexBuilder::new(text)
                .case_insensitive(!self.case_sensitive)
                .build()
                .map(|r| r.is_match(&hay))
                .unwrap_or(false)
        } else if self.case_sensitive {
            hay.contains(text)
        } else {
            hay.to_lowercase().contains(&text.to_lowercase())
        }
    }
    pub fn color(&self, c: &CommitInfo, theme: &UiTheme) -> Color32 {
        let highlight = match self.highlight {
            1 => !self.my_email.is_empty() && c.email == self.my_email,
            2 => c.parents.len() > 1,
            3 => self.members.contains(&c.id),
            4 => self.nonpicked.contains(&c.id),
            _ => false,
        };
        if highlight {
            theme.accent
        } else {
            theme.text
        }
    }
    pub fn date(&self, c: &CommitInfo) -> String {
        if !self.relative_dates {
            return rustpy_i18n::regional::timestamp_seconds(c.time);
        }
        let seconds = (chrono::Utc::now().timestamp() - c.time).max(0);
        if seconds < 60 {
            rustpy_i18n::tr("just now").into_owned()
        } else if seconds < 3600 {
            rustpy_i18n::tr_fmt("{n}m ago", &[("n", &(seconds / 60).to_string())])
        } else if seconds < 86400 {
            rustpy_i18n::tr_fmt("{n}h ago", &[("n", &(seconds / 3600).to_string())])
        } else {
            rustpy_i18n::tr_fmt("{n}d ago", &[("n", &(seconds / 86400).to_string())])
        }
    }
    pub fn refs(&self, c: &CommitInfo) -> Vec<String> {
        let mut refs = c
            .refs
            .iter()
            .filter(|r| self.tags || !r.starts_with("refs/tags/"))
            .map(|r| {
                r.trim_start_matches("refs/heads/")
                    .trim_start_matches("refs/remotes/")
                    .trim_start_matches("refs/tags/")
                    .to_string()
            })
            .collect::<Vec<_>>();
        if self.compact && refs.len() > 2 {
            let extra = refs.len() - 2;
            refs.truncate(2);
            refs.push(format!("+{extra}"));
        }
        refs
    }
    pub fn extended_context(&mut self, ui: &mut egui::Ui, repo: &GitRepo, c: &CommitInfo) {
        ui.separator();
        if let Some(url) = repo.log_github_url(&c.id) {
            ui.hyperlink_to("Open on GitHub", url);
        }
        for (label, args) in [
            (
                "Cherry-pick without commit",
                vec!["cherry-pick".into(), "--no-commit".into(), c.id.clone()],
            ),
            (
                "Apply changes from commit",
                vec!["cherry-pick".into(), "--no-commit".into(), c.id.clone()],
            ),
            (
                "Reset (keep local changes)",
                vec!["reset".into(), "--keep".into(), c.id.clone()],
            ),
            (
                "Undo commit, keep in changelist",
                vec!["reset".into(), "--mixed".into(), format!("{}^", c.id)],
            ),
        ] {
            let enabled = !label.starts_with("Undo")
                || repo.resolve_log_revision("HEAD").ok().as_deref() == Some(c.id.as_str());
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                self.pending = Some((label.into(), args));
                ui.close();
            }
        }
        ui.menu_button("Rewrite commits…", |ui| {
            for (label, action) in [
                ("Edit commit message", RebaseAction::Reword),
                ("Squash commits…", RebaseAction::Squash),
                ("Fixup…", RebaseAction::Fixup),
                ("Drop commit", RebaseAction::Drop),
                ("Interactively rebase from here", RebaseAction::Pick),
            ] {
                if ui.button(crate::i18n::tr_static(label)).clicked() {
                    let start = if matches!(action, RebaseAction::Squash | RebaseAction::Fixup) {
                        c.parents.first().unwrap_or(&c.id)
                    } else {
                        &c.id
                    };
                    match repo.log_rebase_plan(start) {
                        Ok(mut plan) => {
                            if let Some(entry) =
                                plan.entries.iter_mut().find(|entry| entry.id == c.id)
                            {
                                entry.action = action;
                            }
                            self.plan = Some(plan);
                        }
                        Err(e) => self.error = e.to_string(),
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("New branch here…", |ui| {
            ui.text_edit_singleline(&mut self.branch_name);
            if ui.button("Create branch").clicked() {
                self.pending = Some((
                    "Create branch".into(),
                    vec!["branch".into(), self.branch_name.clone(), c.id.clone()],
                ));
                ui.close();
            }
        });
        ui.menu_button("Compare with branch…", |ui| {
            ui.text_edit_singleline(&mut self.compare);
            if ui.button("Compare").clicked() {
                match repo.diff_commits(&self.compare, &c.id) {
                    Ok(files) => {
                        self.viewer = Some((
                            "Compare revisions".into(),
                            files
                                .iter()
                                .map(|f| f.patch.clone())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ))
                    }
                    Err(e) => self.error = e.to_string(),
                }
                ui.close();
            }
        });
        if ui.button("Show diff with working tree").clicked() {
            match repo.log_diff_local(&c.id) {
                Ok(text) => self.viewer = Some(("Compare with local".into(), text)),
                Err(e) => self.error = e.to_string(),
            }
            ui.close();
        }
        ui.menu_button("Create patch…", |ui| {
            ui.text_edit_singleline(&mut self.patch_path);
            if ui.button("Save patch").clicked() {
                let result = repo.log_patch(&c.id).and_then(|text| {
                    std::fs::write(&self.patch_path, text)?;
                    Ok(())
                });
                if let Err(e) = result {
                    self.error = e.to_string();
                }
                ui.close();
            }
        });
        for (label, text) in [
            ("Copy short revision", &c.short_id),
            ("Copy full hash", &c.id),
            ("Copy commit message", &c.message),
        ] {
            if ui.button(crate::i18n::tr_static(label)).clicked() {
                ui.ctx().copy_text(text.clone());
                ui.close();
            }
        }
        if ui.button("Go to parent").clicked() {
            if let Some(parent) = c.parents.first() {
                self.goto = parent.clone();
                self.focus_goto = true;
            }
            ui.close();
        }
    }
}
impl GitPanel {
    pub fn reveal_log_root(&mut self, root: PathBuf, revision: String) {
        self.log_workspace.active_root = Some(root.clone());
        self.log_workspace.indexed_root = PathBuf::new();
        self.selected_commit =
            GitRepo::discover(&root).and_then(|repo| repo.resolve_log_revision(&revision).ok());
        self.tab = GitTab::Log;
    }
    pub fn cancel_log_project(&mut self) {
        let mut job = self.log_workspace.job.take();
        if let Some(job) = &mut job {
            job.stop();
        }
        let job_root = self.log_workspace.job_root.clone();
        let job_project = self.log_workspace.job_project.clone();
        self.log_workspace = Workspace::default();
        self.log_workspace.job = job;
        self.log_workspace.job_root = job_root;
        self.log_workspace.job_project = job_project;
        self.log.clear();
        self.graph.clear();
        self.selected_commit = None;
        self.selected_file = None;
        self.commit_files.clear();
        self.diff_text.clear();
        self.log_filter = None;
        self.log_query.clear();
    }
    pub fn log_job_label(&self) -> Option<String> {
        self.log_workspace
            .job
            .as_ref()
            .filter(|job| job.is_running())
            .map(|job| job.command_line.clone())
    }
    pub fn log_lifecycle_stamp(&self) -> Option<String> {
        self.log_workspace
            .job
            .as_ref()
            .filter(|job| job.is_running())
            .map(|job| {
                format!(
                    "git-log:{:?}:{}:stopped={}",
                    job.started,
                    job.command_line,
                    job.was_stopped()
                )
            })
    }
    pub fn stop_log_job(&mut self) {
        if let Some(job) = &mut self.log_workspace.job {
            job.stop();
        }
    }
    pub fn log_modal_active(&self) -> bool {
        self.log_workspace.pending.is_some()
            || self.log_workspace.plan.is_some()
            || self.log_workspace.viewer.is_some()
    }
    pub fn poll_log_job(&mut self) -> Vec<GitEvent> {
        let Some(job) = &mut self.log_workspace.job else {
            return Vec::new();
        };
        for line in job.poll() {
            self.log_workspace.output.push_str(&line);
            self.log_workspace.output.push('\n');
        }
        if job.is_running() {
            return Vec::new();
        }
        let status = job.summary_line();
        self.log_workspace.job = None;
        self.log_workspace.indexed_root = PathBuf::new();
        if self.log_workspace.project_root != self.log_workspace.job_project {
            return Vec::new();
        }
        vec![GitEvent::Status(status), GitEvent::FilesChanged]
    }
    pub(super) fn log_workspace_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        original: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        if self.log_workspace.project_root != original.root() {
            self.log_workspace.load_preferences(original.root());
        }
        self.log_workspace.project_root = original.root().to_path_buf();
        let other = self
            .log_workspace
            .active_root
            .as_ref()
            .and_then(|root| GitRepo::discover(root));
        let repo = other.as_ref().unwrap_or(original);
        if !self.log_workspace.roots.iter().any(|r| r == repo.root()) {
            self.log_workspace.roots.push(repo.root().to_path_buf());
        }
        let mut reload = false;
        if self.log_workspace.last_selected != self.selected_commit {
            self.log_workspace.last_selected = self.selected_commit.clone();
            self.log_workspace.scroll_to = self.selected_commit.clone();
            self.log_workspace.parent = 0;
            if let Some(id) = &self.selected_commit {
                if !self.log.iter().any(|c| &c.id == id) && repo.resolve_log_revision(id).is_ok() {
                    self.log_workspace.query = Query {
                        branches: vec![id.clone()],
                        ..Default::default()
                    };
                    self.log_query.clear();
                    self.commit_files = repo.commit_files(id).unwrap_or_default();
                    reload = true;
                }
            }
        }
        if self.log_workspace.query.text != self.log_query
            || self.log_workspace.query.regex != self.log_workspace.regex
            || self.log_workspace.query.case_sensitive != self.log_workspace.case_sensitive
        {
            self.log_workspace.query.text = self.log_query.clone();
            self.log_workspace.query.regex = self.log_workspace.regex;
            self.log_workspace.query.case_sensitive = self.log_workspace.case_sensitive;
            reload = true;
        }
        if self.log_workspace.indexed_root != repo.root() {
            self.log_workspace.indexed_root = repo.root().to_path_buf();
            self.selected_file = None;
            self.diff_text.clear();
            if let Some(id) = &self.selected_commit {
                self.commit_files = repo.commit_files(id).unwrap_or_default();
            }
            reload = true;
        }
        if self.log_workspace.history_path != self.log_filter {
            self.log_workspace.history_path = self.log_filter.clone();
            self.log_workspace.query.path = self.log_filter.clone();
            reload = true;
        }
        events.extend(self.poll_log_job());
        if let Some(job) = &mut self.log_workspace.job {
            if job.is_running() {
                ui.horizontal(|ui| {
                    crate::a11y::spinner(ui);
                    ui.label(&job.command_line);
                    if ui.button("Cancel operation").clicked() {
                        job.stop();
                    }
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        ui.horizontal_wrapped(|ui| {
            let mut select = None;
            let mut close = None;
            for (n, tab) in self.log_workspace.tabs.iter().enumerate() {
                if ui
                    .selectable_label(
                        n == self.log_workspace.active,
                        format!("{}{}", if tab.pinned { "📌 " } else { "" }, tab.name),
                    )
                    .clicked()
                {
                    select = Some(n);
                }
                if self.log_workspace.tabs.len() > 1
                    && !tab.pinned
                    && ui.small_button("×").clicked()
                {
                    close = Some(n);
                }
            }
            if ui.small_button("Open another log tab").clicked() {
                self.log_workspace.tabs.push(LogTab {
                    name: format!("Log {}", self.log_workspace.tabs.len() + 1),
                    ..Default::default()
                });
                select = Some(self.log_workspace.tabs.len() - 1);
            }
            if ui.small_button("Pin tab").clicked() {
                let n = self.log_workspace.active;
                self.log_workspace.tabs[n].pinned = !self.log_workspace.tabs[n].pinned;
            }
            if let Some(n) = select {
                let old = self.log_workspace.active;
                self.log_workspace.tabs[old].query = self.log_workspace.query.clone();
                self.log_workspace.tabs[old].text = self.log_query.clone();
                self.log_workspace.tabs[old].selected = self.selected_commit.clone();
                self.log_workspace.tabs[old].root = self.log_workspace.active_root.clone();
                self.log_workspace.active = n;
                let tab = self.log_workspace.tabs[n].clone();
                self.log_workspace.query = tab.query;
                self.log_query = tab.text;
                self.selected_commit = tab.selected;
                self.log_workspace.active_root = tab.root;
                self.selected_file = None;
                self.diff_text.clear();
                reload = true;
            }
            if let Some(n) = close {
                self.log_workspace.tabs.remove(n);
                self.log_workspace.active = self
                    .log_workspace
                    .active
                    .min(self.log_workspace.tabs.len() - 1);
                let tab = self.log_workspace.tabs[self.log_workspace.active].clone();
                self.log_workspace.query = tab.query;
                self.log_query = tab.text;
                self.selected_commit = tab.selected;
                self.log_workspace.active_root = tab.root;
                reload = true;
            }
            if ui.button("Fetch").clicked() {
                events.push(GitEvent::Run(crate::git_jobs::GitOp::BranchNetwork {
                    root: repo.root().to_path_buf(),
                    label: "Fetch".into(),
                    args: vec![
                        "fetch".into(),
                        "--all".into(),
                        "--prune".into(),
                        "--progress".into(),
                    ],
                }));
            }
            if ui.button("Update").clicked() {
                events.push(GitEvent::Invoke {
                    root: repo.root().to_path_buf(),
                    action: crate::actions::Action::UpdateProject,
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("Branches", |ui| {
                if ui
                    .selectable_label(
                        self.log_workspace.query.branches.is_empty()
                            && !self.log_workspace.query.all,
                        "HEAD",
                    )
                    .clicked()
                {
                    self.log_workspace.query.branches.clear();
                    self.log_workspace.query.all = false;
                    reload = true;
                }
                if ui
                    .checkbox(&mut self.log_workspace.query.all, "All branches")
                    .changed()
                {
                    self.log_workspace.query.branches.clear();
                    reload = true;
                }
                for branch in repo.branches() {
                    let mut checked = self.log_workspace.query.branches.contains(&branch.name);
                    if ui.checkbox(&mut checked, &branch.name).changed() {
                        if checked {
                            self.log_workspace.query.branches.push(branch.name);
                        } else {
                            self.log_workspace
                                .query
                                .branches
                                .retain(|b| b != &branch.name);
                        }
                        self.log_workspace.query.all = false;
                        reload = true;
                    }
                }
            });
            ui.menu_button("User", |ui| {
                if ui.button("Anyone").clicked() {
                    self.log_workspace.query.author.clear();
                    reload = true;
                    ui.close();
                }
                if ui.button("Me").clicked() {
                    self.log_workspace.query.author = "me".into();
                    reload = true;
                    ui.close();
                }
            });
            if ui
                .text_edit_singleline(&mut self.log_workspace.query.author)
                .on_hover_text("Name, email, me, or empty for anyone")
                .lost_focus()
            {
                reload = true;
            }
            ui.menu_button("Date", |ui| {
                let now = chrono::Utc::now().timestamp();
                let local = chrono::Local::now().date_naive();
                let today =
                    date_bound(&local.to_string(), false).unwrap_or(now - now.rem_euclid(86400));
                let yesterday = local
                    .pred_opt()
                    .and_then(|date| date_bound(&date.to_string(), false).ok())
                    .unwrap_or(today - 86400);
                for (label, since, until) in [
                    ("Any", None, None),
                    ("Today", Some(today), None),
                    ("Yesterday", Some(yesterday), Some(today - 1)),
                    ("Last week", Some(now - 604800), None),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        self.log_workspace.query.since = since;
                        self.log_workspace.query.until = until;
                        reload = true;
                        ui.close();
                    }
                }
                ui.label("Custom dates: YYYY-MM-DD or Unix timestamps (inclusive)");
                ui.text_edit_singleline(&mut self.log_workspace.since);
                ui.text_edit_singleline(&mut self.log_workspace.until);
                if ui.button("Apply date range").clicked() {
                    let since = date_bound(&self.log_workspace.since, false);
                    let until = date_bound(&self.log_workspace.until, true);
                    if (self.log_workspace.since.is_empty() || since.is_ok())
                        && (self.log_workspace.until.is_empty() || until.is_ok())
                    {
                        self.log_workspace.query.since = since.ok();
                        self.log_workspace.query.until = until.ok();
                        reload = true;
                    } else {
                        self.log_workspace.error = "Use YYYY-MM-DD or Unix timestamps".into();
                    }
                }
            });
            ui.menu_button("Path / root", |ui| {
                ui.text_edit_singleline(&mut self.log_workspace.path);
                if ui.button("Filter path/folder").clicked() {
                    self.log_workspace.query.path = (!self.log_workspace.path.is_empty())
                        .then(|| PathBuf::from(&self.log_workspace.path));
                    reload = true;
                }
                for root in self.log_workspace.roots.clone() {
                    if ui
                        .selectable_label(root == repo.root(), root.display().to_string())
                        .clicked()
                    {
                        self.log_workspace.active_root = Some(root);
                        reload = true;
                    }
                }
                ui.text_edit_singleline(&mut self.log_workspace.root_input);
                if ui.button("Add repository").clicked() {
                    let path = PathBuf::from(&self.log_workspace.root_input);
                    if let Some(repository) = GitRepo::discover(&path) {
                        self.log_workspace
                            .roots
                            .push(repository.root().to_path_buf());
                    } else {
                        self.log_workspace.error = "No Git repository at that path".into();
                    }
                }
            });
            ui.checkbox(&mut self.log_workspace.regex, "Regex");
            ui.checkbox(&mut self.log_workspace.case_sensitive, "Match case");
            ui.menu_button("View", |ui| {
                for (value, label) in [&mut self.log_workspace.compact]
                    .into_iter()
                    .zip(["Compact references"])
                {
                    ui.checkbox(value, label);
                }
                ui.checkbox(&mut self.log_workspace.long_edges, "Show long edges");
                ui.checkbox(&mut self.log_workspace.tags, "Show tag names");
                ui.checkbox(&mut self.log_workspace.root_names, "Show root names");
                ui.checkbox(&mut self.log_workspace.relative_dates, "Relative dates");
                ui.checkbox(
                    &mut self.log_workspace.branches_visible,
                    "Branches side panel",
                );
                ui.checkbox(
                    &mut self.log_workspace.collapsed,
                    "Collapse linear branches",
                );
                if ui
                    .checkbox(&mut self.log_workspace.query.linear_bek, "Linear Bek")
                    .changed()
                {
                    reload = true;
                }
                for (n, label) in ["Message", "Author", "Date", "Hash"]
                    .into_iter()
                    .enumerate()
                {
                    ui.checkbox(&mut self.log_workspace.columns[n], label);
                }
                ui.label("Intelli-sort: topological order, then commit time");
            });
            egui::ComboBox::from_id_salt("log-highlight")
                .selected_text(
                    [
                        "Highlight: none",
                        "My commits",
                        "Merge commits",
                        "Current branch",
                        "Not cherry-picked",
                    ][self.log_workspace.highlight],
                )
                .show_ui(ui, |ui| {
                    for (n, label) in [
                        "None",
                        "My commits",
                        "Merge commits",
                        "Current branch",
                        "Not cherry-picked",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        if ui
                            .selectable_value(&mut self.log_workspace.highlight, n, label)
                            .changed()
                        {
                            reload = true;
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Highlight commits"));
        });
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.log_workspace.goto)
                    .hint_text("Go to hash / branch / tag (Ctrl+F)"),
            );
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
                response.request_focus();
            }
            if (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Go").clicked()
                || self.log_workspace.focus_goto
            {
                self.log_workspace.focus_goto = false;
                match repo.resolve_log_revision(&self.log_workspace.goto) {
                    Ok(id) => {
                        self.selected_commit = Some(id.clone());
                        self.commit_files = repo.commit_files(&id).unwrap_or_default();
                        if !self.log.iter().any(|c| c.id == id) {
                            self.log_workspace.query = Query {
                                branches: vec![id],
                                ..Default::default()
                            };
                            self.log_query.clear();
                            reload = true;
                        }
                    }
                    Err(e) => self.log_workspace.error = e.to_string(),
                }
            }
            if let Some(id) = &self.selected_commit {
                if ui.button("Go to child").clicked() {
                    if let Some(child) = self.log.iter().find(|c| c.parents.contains(id)) {
                        self.log_workspace.goto = child.id.clone();
                        self.log_workspace.focus_goto = true;
                    }
                }
            }
        });
        if self.log_workspace.regex {
            if let Err(e) = regex::Regex::new(&self.log_query) {
                ui.colored_label(theme.error, e.to_string());
            }
        }
        if reload {
            self.log_workspace.query.highlight = self.log_workspace.highlight;
            self.log_workspace.index = Some(Index::start(
                repo.root().to_path_buf(),
                self.log_workspace.query.clone(),
                self.log_limit,
            ));
        }
        if let Some(index) = &self.log_workspace.index {
            if let Some(result) = index.poll() {
                match result {
                    Ok(page) => {
                        self.log = page.graph.iter().map(|g| g.to_info()).collect();
                        self.graph = page.graph;
                        self.log_workspace.my_email = page.email;
                        self.log_workspace.members = page.current;
                        self.log_workspace.nonpicked = page.nonpicked;
                    }
                    Err(e) => self.log_workspace.error = e,
                }
                self.log_workspace.index = None;
            } else {
                ui.horizontal(|ui| {
                    crate::a11y::spinner(ui);
                    ui.label(format!(
                        "Indexing: {} commits · {:.1}s",
                        index.visited.load(std::sync::atomic::Ordering::Relaxed),
                        index.started.elapsed().as_secs_f32()
                    ));
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(30));
            }
        }
        if self.log_workspace.branches_visible {
            egui::Panel::left("git-log-branches")
                .resizable(true)
                .default_size(150.0)
                .show(ui, |ui| {
                    ui.text_edit_singleline(&mut self.log_workspace.branch_search);
                    for branch in repo.branches().into_iter().filter(|b| {
                        b.name
                            .to_lowercase()
                            .contains(&self.log_workspace.branch_search.to_lowercase())
                    }) {
                        let response = ui.selectable_label(
                            self.log_workspace.query.branches.contains(&branch.name),
                            &branch.name,
                        );
                        if response.clicked() {
                            self.log_workspace.query.branches = vec![branch.name.clone()];
                            self.log_workspace.index = Some(Index::start(
                                repo.root().to_path_buf(),
                                self.log_workspace.query.clone(),
                                self.log_limit,
                            ));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Show branch log").clicked() {
                                self.log_workspace.query.branches = vec![branch.name.clone()];
                                self.log_workspace.index = Some(Index::start(
                                    repo.root().to_path_buf(),
                                    self.log_workspace.query.clone(),
                                    self.log_limit,
                                ));
                                ui.close();
                            }
                        });
                    }
                });
        }
        if !self.log_workspace.error.is_empty() {
            ui.colored_label(theme.error, &self.log_workspace.error);
            if ui.small_button("Dismiss").clicked() {
                self.log_workspace.error.clear();
            }
        }
        self.log_dialogs(ui, repo, events);
        self.all_branches = self.log_workspace.query.all;
        let available = !self.log_modal_active();
        ui.add_enabled_ui(available, |ui| self.log_ui(ui, theme, repo, events));
        self.log_workspace.save_preferences();
    }
    fn log_dialogs(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        if let Some((label, args)) = self.log_workspace.pending.clone() {
            let mut close = false;
            egui::Window::new(&label)
                .collapsible(false)
                .show_modal(ui.ctx(), |ui| {
                    ui.label(format!("git {}", args.join(" ")));
                    ui.label("This operation changes the current repository. Confirm to continue.");
                    if ui
                        .add_enabled(
                            self.log_workspace.job.is_none(),
                            egui::Button::new("Run operation"),
                        )
                        .clicked()
                    {
                        snapshot_working_tree(repo, repo.root(), &format!("Before {label}"), true);
                        match repo.start_log_command(args) {
                            Ok(job) => {
                                self.log_workspace.job = Some(job);
                                self.log_workspace.job_root = repo.root().to_path_buf();
                                self.log_workspace.job_project =
                                    self.log_workspace.project_root.clone();
                            }
                            Err(e) => self.log_workspace.error = e.to_string(),
                        }
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            if close {
                self.log_workspace.pending = None;
            }
        }
        let can_start = self.log_workspace.job.is_none();
        if let Some(plan) = &mut self.log_workspace.plan {
            let mut start = false;
            let mut cancel = false;
            let mut move_row = None;
            egui::Window::new("Interactive Rebase").resizable(true).show_modal(ui.ctx(),|ui|{ui.label("Rewrites current-branch history. Commit or stash changes first. Edit stops for amendments; Continue/Abort remain in Git operations.");ui.checkbox(&mut plan.preserve_merges,"Preserve merges and branch topology (--rebase-merges)");if !plan.preserve_merges {ui.label("Flatten mode omits merge commits and their merge-only resolutions; non-merge commits replay in the chosen order.");}egui::ScrollArea::vertical().max_height(400.0).show(ui,|ui|{for (n,entry) in plan.entries.iter_mut().enumerate(){ui.push_id(n,|ui|{ui.horizontal(|ui|{egui::ComboBox::from_id_salt("action").selected_text(entry.action.label()).show_ui(ui,|ui|{for action in RebaseAction::ALL{ui.selectable_value(&mut entry.action,action,action.label());}}).combo_named(crate::i18n::tr_static("Rebase action"));ui.monospace(&entry.id[..8]);if ui.button("↑").clicked()&&n>0 {move_row=Some((n,n-1));}
if ui.button("↓").clicked(){move_row=Some((n,n+1));}});ui.text_edit_multiline(&mut entry.message);});}});start=ui.add_enabled(can_start,egui::Button::new("Start Rebasing")).clicked();cancel=ui.button("Cancel").clicked();});
            if let Some((a, b)) = move_row {
                if b < plan.entries.len() {
                    plan.entries.swap(a, b);
                }
            }
            if start {
                match repo.start_log_rebase(plan) {
                    Ok(job) => {
                        self.log_workspace.job = Some(job);
                        self.log_workspace.job_root = repo.root().to_path_buf();
                        self.log_workspace.job_project = self.log_workspace.project_root.clone();
                        cancel = true;
                    }
                    Err(e) => self.log_workspace.error = e.to_string(),
                }
            }
            if cancel {
                self.log_workspace.plan = None;
            }
        }
        if let Some((title, text)) = &mut self.log_workspace.viewer {
            let mut open = true;
            egui::Window::new(title.as_str())
                .open(&mut open)
                .resizable(true)
                .show_modeless(ui.ctx(), |ui| {
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(25)
                                .desired_width(800.0)
                                .interactive(false),
                        );
                    });
                });
            if !open {
                self.log_workspace.viewer = None;
            }
        }
        if !self.log_workspace.output.is_empty() {
            ui.collapsing("Log operation output", |ui| {
                ui.label(&self.log_workspace.output);
            });
        }
        let _ = events;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_filter_respects_regex_and_case() {
        let mut workspace = Workspace::default();
        let c = CommitInfo {
            id: "abc".into(),
            short_id: "abc".into(),
            summary: "Fix bug".into(),
            message: "Fix bug\nABC-123".into(),
            author: "Ada".into(),
            email: "ada@x".into(),
            time: 0,
            parents: vec![],
            refs: vec![],
        };
        assert!(workspace.matches(&c, "fix"));
        workspace.case_sensitive = true;
        assert!(!workspace.matches(&c, "fix"));
        workspace.regex = true;
        assert!(workspace.matches(&c, r"ABC-\d+"));
        assert!(!workspace.matches(&c, "["));
    }
}

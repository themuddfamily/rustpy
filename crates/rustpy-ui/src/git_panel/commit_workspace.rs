//! Persistent changelist/shelf workspace, patch preview and commit checks.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
#[path = "task_context.rs"]
mod task_context;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub struct ChangeList {
    pub name: String,
    pub task: String,
    pub paths: HashSet<PathBuf>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Shelf {
    pub name: String,
    pub base: String,
    pub patch: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub lists: Vec<ChangeList>,
    pub active: usize,
    pub track_tasks: bool,
    pub task: String,
    pub shelves: Vec<Shelf>,
    pub history: Vec<String>,
    pub templates: Vec<String>,
    pub author: String,
    pub show_unversioned: bool,
    pub show_ignored: bool,
    pub grouping: usize,
    pub tree: bool,
    pub preview: bool,
    pub subject_limit: usize,
    pub body_limit: usize,
    pub check_todo: bool,
    pub check_spelling: bool,
    pub prepare_before_commit: usize,
    #[serde(skip)]
    pub prepared_signature: Option<u64>,
    #[serde(skip)]
    pub preparation_test: Option<(u64, u64)>,
    #[serde(skip)]
    pub preparation_warnings: bool,
    #[serde(skip)]
    pub preparation: Option<(PathBuf, CheckKind, std::collections::VecDeque<PathBuf>)>,
    pub clear_message_after_commit: bool,
    pub check_conflicts: bool,
    pub require_clean_checks: bool,
    #[serde(skip)]
    pub index_commit: bool,
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub patch: String,
    #[serde(skip)]
    pub patch_file: String,
    #[serde(skip)]
    pub patch_base: String,
    #[serde(skip)]
    pub reverse: bool,
    #[serde(skip)]
    pub strip: usize,
    #[serde(skip)]
    pub preview_valid: Option<String>,
    #[serde(skip)]
    pub shelf_tab: bool,
    #[serde(skip)]
    pub selected_shelf: Option<usize>,
    #[serde(skip)]
    pub notice: String,
    #[serde(skip)]
    pub failure: Option<String>,
    #[serde(skip)]
    pub unsaved: HashSet<PathBuf>,
    #[serde(skip)]
    pub resolutions: Vec<PatchResolution>,
    #[serde(skip)]
    pub results: Vec<String>,
    #[serde(skip)]
    pub rollback: Option<Vec<PathBuf>>,
    #[serde(skip)]
    pub delete_added: bool,
    #[serde(skip)]
    pub expanded: bool,
    #[serde(skip)]
    pub loaded_root: PathBuf,
}
pub struct PatchResolution {
    path: PathBuf,
    base: String,
    ours: Option<String>,
    theirs: Option<String>,
    result: String,
    delete: bool,
}
pub enum Event {
    History(PathBuf),
    Annotate(PathBuf),
    Repository(PathBuf, String),
    CheckFiles(CheckKind),
    NewBranchCommit(String),
    Signing(bool, bool),
}
#[derive(Clone, Copy)]
pub enum CheckKind {
    Reformat,
    Optimize,
    Rearrange,
    Cleanup,
    Analyze,
    Tests,
}
impl Workspace {
    /// Read the same persisted/in-memory changelist authority used by Local Changes.
    pub(crate) fn default_paths_for(
        &self,
        root: &Path,
        changed: &std::collections::BTreeSet<PathBuf>,
    ) -> HashSet<PathBuf> {
        let loaded;
        let workspace = if self.loaded_root == root {
            self
        } else {
            loaded = Self::load(root);
            &loaded
        };
        changed
            .iter()
            .filter(|path| {
                workspace
                    .lists
                    .first()
                    .is_some_and(|list| list.paths.contains(*path))
                    || !workspace
                        .lists
                        .iter()
                        .skip(1)
                        .any(|list| list.paths.contains(*path))
            })
            .cloned()
            .collect()
    }
    pub(super) fn initial() -> Self {
        Self {
            show_unversioned: true,
            preview: true,
            clear_message_after_commit: true,
            subject_limit: 72,
            body_limit: 100,
            ..Default::default()
        }
    }
    fn load(root: &Path) -> Self {
        let path = storage(root);
        let mut value: Self = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| Self {
                show_unversioned: true,
                clear_message_after_commit: true,
                preview: true,
                subject_limit: 72,
                body_limit: 100,
                ..Default::default()
            });
        if value.lists.is_empty() {
            value.lists.push(ChangeList {
                name: "Default".into(),
                task: String::new(),
                paths: HashSet::new(),
            });
        }
        value.active = value.active.min(value.lists.len() - 1);
        value.loaded_root = root.into();
        value.expanded = true;
        value
    }
    fn save(&mut self) {
        if self.loaded_root.as_os_str().is_empty() {
            return;
        }
        let path = storage(&self.loaded_root);
        let result = (|| -> anyhow::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let bytes = serde_json::to_vec_pretty(self)?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, bytes)?;
            std::fs::rename(temporary, path)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.notice = format!("Could not save commit workspace: {error}");
        }
    }
    fn assign(&mut self, paths: &[PathBuf], target: usize) {
        for list in &mut self.lists {
            for path in paths {
                list.paths.remove(path);
            }
        }
        if let Some(list) = self.lists.get_mut(target) {
            list.paths.extend(paths.iter().cloned());
        }
        self.save();
    }
}
fn storage(root: &Path) -> PathBuf {
    // Keep UI state out of tracked project configuration and shared repositories.
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hash);
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rustpy/commit-workspaces")
        .join(format!("{:016x}.json", hash.finish()))
}
/// Split a unified patch into independently applicable hunks, repeating headers.
pub fn hunks(patch: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut header = String::new();
    let mut current = String::new();
    let mut inside = false;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            header.clear();
            inside = false;
        }
        if line.starts_with("@@ ") {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            current = header.clone();
            inside = true;
        }
        if inside {
            current.push_str(line);
        } else {
            header.push_str(line);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}
/// Select only the captured file/hunk. A single-line patch leaves other edits unstaged.
pub fn hunk_at_line(
    patch: &str,
    path: &Path,
    root: &Path,
    line: usize,
    single: bool,
) -> Option<String> {
    let relative = path
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    for hunk in hunks(patch) {
        let lines: Vec<_> = hunk.lines().collect();
        if !lines.iter().any(|text| {
            text.strip_prefix("+++ b/") == Some(relative.as_str())
                || text.strip_prefix("+++ ") == Some(relative.as_str())
        }) {
            continue;
        }
        let index = lines.iter().position(|text| text.starts_with("@@ "))?;
        let fields: Vec<_> = lines[index].split_whitespace().collect();
        let range = fields.get(2)?.strip_prefix('+')?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        let start: usize = start.parse().ok()?;
        let count: usize = count.parse().ok()?;
        let first = start.saturating_sub(1);
        if line < first || line > first + count.saturating_sub(1) {
            continue;
        }
        if !single {
            return Some(hunk);
        }
        let mut current = first;
        let mut body = String::new();
        let mut found = false;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        for text in lines.iter().skip(index + 1) {
            match text.as_bytes().first().copied() {
                Some(b'+') => {
                    if current == line {
                        body.push_str(text);
                        body.push('\n');
                        new_count += 1;
                        found = true;
                    }
                    current += 1;
                }
                Some(b'-') => {
                    old_count += 1;
                    if current == line {
                        body.push_str(text);
                        body.push('\n');
                        found = true;
                    } else {
                        body.push(' ');
                        body.push_str(&text[1..]);
                        body.push('\n');
                        new_count += 1;
                    }
                }
                Some(b' ') => {
                    body.push_str(text);
                    body.push('\n');
                    old_count += 1;
                    new_count += 1;
                    current += 1;
                }
                Some(b'\\') => body.push_str(&format!("{text}\n")),
                _ => {}
            }
        }
        if !found {
            return None;
        }
        let old = fields.get(1)?.trim_start_matches('-').split(',').next()?;
        let mut result = lines[..index].join("\n");
        result.push('\n');
        result.push_str(&format!(
            "@@ -{old},{old_count} +{start},{new_count} @@\n{body}"
        ));
        return Some(result);
    }
    None
}
pub fn detected_base(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("base-commit: "))
        .filter(|id| id.len() >= 7 && id.chars().all(|character| character.is_ascii_hexdigit()))
        .map(str::to_string)
}
/// Rewrite prefixes and optionally reverse a patch; reject traversal before apply.
pub fn prepare_patch(input: &str, strip: usize, reverse: bool) -> Result<String, String> {
    if reverse && input.contains("GIT binary patch") {
        return Err("Binary patches cannot be reversed in the text patch editor".into());
    }
    let mut output = String::new();
    let mut in_hunk = false;
    for line in input.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@ ") {
            in_hunk = true;
        }
        let line = if let Some(path) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
            .filter(|_| !in_hunk)
        {
            let prefix = if line.starts_with("--- ") {
                if reverse {
                    "+++ "
                } else {
                    "--- "
                }
            } else if reverse {
                "--- "
            } else {
                "+++ "
            };
            let (path, stamp) = path.split_once('\t').unwrap_or((path, ""));
            let path = if path == "/dev/null" {
                path.to_string()
            } else {
                let path = path.trim_matches('"');
                let parts: Vec<_> = path.split('/').collect();
                if parts.len() <= strip {
                    return Err("Strip count removes the entire path".into());
                }
                let remainder = &parts[strip..];
                if remainder
                    .iter()
                    .any(|part| part.is_empty() || *part == ".." || *part == ".")
                {
                    return Err("Unsafe patch path".into());
                }
                remainder.join("/")
            };
            format!(
                "{prefix}{path}{}",
                if stamp.is_empty() {
                    String::new()
                } else {
                    format!("\t{stamp}")
                }
            )
        } else if reverse && line.starts_with("@@ ") {
            let mut fields = line.splitn(4, ' ');
            let _ = fields.next();
            let old = fields.next().ok_or("Missing hunk range")?;
            let new = fields.next().ok_or("Missing hunk range")?;
            let tail = fields.next().unwrap_or("@@");
            format!(
                "@@ -{} +{} {tail}",
                new.trim_start_matches('+'),
                old.trim_start_matches('-')
            )
        } else if reverse && line.starts_with('+') {
            format!("-{}", &line[1..])
        } else if reverse && line.starts_with('-') {
            format!("+{}", &line[1..])
        } else {
            line.into()
        };
        output.push_str(&line);
        output.push('\n');
    }
    // Canonical unified patches put old/new headers and removed/added lines
    // in that order. Keep each no-newline marker attached to its own line.
    if reverse {
        let lines: Vec<_> = output.split_inclusive('\n').collect();
        let mut result = String::new();
        let mut i = 0;
        let mut in_hunk = false;
        while i < lines.len() {
            if lines[i].starts_with("diff --git ") {
                in_hunk = false;
                // Extended and unified headers must describe the same old/new
                // sides after reversal (including rewritten path prefixes).
                let headers: Vec<_> = lines[i + 1..]
                    .iter()
                    .take_while(|line| !line.starts_with("@@ ") && !line.starts_with("diff --git "))
                    .copied()
                    .collect();
                let old = headers.iter().find_map(|line| line.strip_prefix("--- "));
                let new = headers.iter().find_map(|line| line.strip_prefix("+++ "));
                if let (Some(old), Some(new)) = (old, new) {
                    let old = old.trim_end().split('\t').next().unwrap();
                    let new = new.trim_end().split('\t').next().unwrap();
                    result.push_str(&format!(
                        "diff --git {} {}\n",
                        if old == "/dev/null" { new } else { old },
                        if new == "/dev/null" { old } else { new }
                    ));
                    i += 1;
                    continue;
                }
            } else if lines[i].starts_with("@@ ") {
                in_hunk = true;
            }
            if !in_hunk
                && lines[i].starts_with("+++ ")
                && lines
                    .get(i + 1)
                    .is_some_and(|line| line.starts_with("--- "))
            {
                result.push_str(lines[i + 1]);
                result.push_str(lines[i]);
                i += 2;
            } else if in_hunk && lines[i].starts_with(['+', '-']) {
                let mut removed = String::new();
                let mut added = String::new();
                while i < lines.len() && lines[i].starts_with(['+', '-']) {
                    let target = if lines[i].starts_with('-') {
                        &mut removed
                    } else {
                        &mut added
                    };
                    target.push_str(lines[i]);
                    i += 1;
                    if lines
                        .get(i)
                        .is_some_and(|line| line.starts_with("\\ No newline"))
                    {
                        target.push_str(lines[i]);
                        i += 1;
                    }
                }
                result.push_str(&removed);
                result.push_str(&added);
            } else {
                result.push_str(lines[i]);
                i += 1;
            }
        }
        output = result;
    }
    Ok(output)
}
impl GitPanel {
    pub(super) fn workspace_change_menu(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        entry: &StatusEntry,
        events: &mut Vec<GitEvent>,
    ) {
        if entry.is_unversioned() && ui.button("Add to Git").clicked() {
            self.commit_workspace.notice = repo
                .workspace_stage(&entry.path)
                .map(|_| "Added to Git".into())
                .unwrap_or_else(|error| error.to_string());
            self.last_refresh = -100.0;
            ui.close();
        }
        if entry.is_unversioned() && ui.button("Ignore").clicked() {
            if let Ok(path) = entry.path.strip_prefix(repo.root()) {
                self.commit_workspace.notice =
                    rustpy_vcs::add_to_gitignore(repo.root(), &path.to_string_lossy())
                        .map(|_| "Added to .gitignore".into())
                        .unwrap_or_else(|error| error.to_string());
                self.last_refresh = -100.0;
            }
            ui.close();
        }
        ui.menu_button("Move to changelist", |ui| {
            for (index, list) in self.commit_workspace.lists.clone().iter().enumerate() {
                if ui.button(&list.name).clicked() {
                    self.commit_workspace
                        .assign(std::slice::from_ref(&entry.path), index);
                    ui.close();
                }
            }
        });
        for (label, event) in [
            ("Show history", Event::History(entry.path.clone())),
            ("Annotate", Event::Annotate(entry.path.clone())),
            (
                "Open repository version",
                Event::Repository(entry.path.clone(), "HEAD".into()),
            ),
        ] {
            if ui.button(crate::i18n::tr_static(label)).clicked() {
                events.push(GitEvent::CommitWorkspace(event));
                ui.close();
            }
        }
        ui.menu_button("Compare with revision / branch", |ui| {
            ui.text_edit_singleline(&mut self.commit_workspace.patch_base);
            if ui.button("Compare").clicked() {
                let revision = if self.commit_workspace.patch_base.trim().is_empty() {
                    "HEAD"
                } else {
                    self.commit_workspace.patch_base.trim()
                };
                match repo.file_at_commit(revision, &entry.path) {
                    Ok(old) => {
                        let current = std::fs::read_to_string(&entry.path).unwrap_or_default();
                        self.local_diff = Some(SideBySide::new(&old, &current));
                        self.diff_text =
                            unified_diff(&old, &current, &entry.path.display().to_string());
                        self.selected_file = Some(entry.path.clone());
                        self.commit_workspace.preview = true;
                    }
                    Err(error) => self.commit_workspace.notice = error.to_string(),
                }
                ui.close();
            }
        });
        if ui.button("Copy revision number").clicked() {
            ui.ctx().copy_text(repo.workspace_revision());
            ui.close();
        }
        if ui.button("Create patch for file").clicked() {
            self.checked = [entry.path.clone()].into();
            self.workspace_create_patch(repo);
            ui.close();
        }
    }
    pub(super) fn workspace_diff_hunks(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        path: &Path,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        if self.commit_workspace.unsaved.contains(path) {
            ui.label("Save this file before selecting its commit hunks");
            return;
        }
        let patch = repo
            .diff_working_tree(Some(path))
            .unwrap_or_default()
            .into_iter()
            .map(|diff| diff.patch)
            .collect::<Vec<_>>()
            .join("\n");
        let staged = hunks(&repo.workspace_patch(true).unwrap_or_default());
        egui::CollapsingHeader::new("Include changes in commit").show(ui, |ui| {
            for (index, hunk) in hunks(&patch).iter().enumerate() {
                let mut included = staged.contains(hunk);
                if ui
                    .checkbox(&mut included, format!("Include hunk {}", index + 1))
                    .changed()
                {
                    let prepared = if included {
                        Ok(hunk.clone())
                    } else {
                        prepare_patch(hunk, 0, true)
                    };
                    match prepared.map_err(anyhow::Error::msg).and_then(|text| {
                        repo.workspace_apply_patch(&text, true, true)
                            .and_then(|_| repo.workspace_apply_patch(&text, true, false))
                    }) {
                        Ok(()) => {
                            self.commit_workspace.index_commit = true;
                            self.refresh(repo, now);
                            events.push(GitEvent::Status(
                                "Updated hunk selection in staged index".into(),
                            ));
                        }
                        Err(error) => events.push(GitEvent::Status(error.to_string())),
                    }
                }
            }
        });
    }
    fn workspace_resolution_ui(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        let Some(resolution) = self.commit_workspace.resolutions.first_mut() else {
            return;
        };
        let mut done = false;
        let mut open = true;
        egui::Window::new(format!(
            "Resolve patch conflict — {}",
            resolution.path.display()
        ))
        .open(&mut open)
        .default_width(1000.0)
        .show_modal(ui.ctx(), |ui| {
            ui.columns(3, |columns| {
                for (column, (title, text)) in columns.iter_mut().zip([
                    ("Base", resolution.base.as_str()),
                    ("Local", resolution.ours.as_deref().unwrap_or("<absent>")),
                    (
                        "Patched base",
                        resolution.theirs.as_deref().unwrap_or("<deleted>"),
                    ),
                ]) {
                    column.label(title);
                    egui::ScrollArea::both()
                        .max_height(180.0)
                        .show(column, |ui| {
                            ui.monospace(text);
                        });
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Accept local").clicked() {
                    resolution.result = resolution.ours.clone().unwrap_or_default();
                    resolution.delete = resolution.ours.is_none();
                }
                if ui.button("Accept patched base").clicked() {
                    resolution.result = resolution.theirs.clone().unwrap_or_default();
                    resolution.delete = resolution.theirs.is_none();
                }
                ui.checkbox(&mut resolution.delete, "Delete resulting file");
            });
            ui.add(
                egui::TextEdit::multiline(&mut resolution.result)
                    .code_editor()
                    .desired_rows(12)
                    .desired_width(f32::INFINITY),
            );
            if ui
                .add_enabled(
                    !self.commit_workspace.unsaved.contains(&resolution.path),
                    egui::Button::new("Apply resolution / next file"),
                )
                .clicked()
            {
                let result = (|| -> anyhow::Result<()> {
                    let _operation = rustpy_vcs::RepositoryOperation::enter(
                        repo.root(),
                        "Resolve patch conflict",
                    )?;
                    if std::fs::read_to_string(&resolution.path).ok() != resolution.ours {
                        anyhow::bail!("Local file changed since preview; check patch again");
                    }
                    history_store::snapshot_paths(
                        &history_store::base_dir(),
                        Some(repo.root()),
                        std::slice::from_ref(&resolution.path),
                        "Before patch resolution",
                        resolution.delete,
                    );
                    if resolution.delete {
                        if resolution.path.exists() {
                            std::fs::remove_file(&resolution.path)?;
                        }
                    } else {
                        if let Some(parent) = resolution.path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&resolution.path, &resolution.result)?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        done = true;
                        events.push(GitEvent::FilesChanged);
                    }
                    Err(error) => {
                        ui.label(error.to_string());
                    }
                }
            }
        });
        if !open {
            self.commit_workspace.resolutions.clear();
        } else if done {
            self.commit_workspace.resolutions.remove(0);
            self.last_refresh = -100.0;
        }
    }
    pub(crate) fn workspace_task(&mut self, root: &Path, task: Option<(String, String)>) {
        self.workspace_root(root);
        if !self.commit_workspace.track_tasks {
            return;
        }
        let Some((id, summary)) = task else {
            return;
        };
        if self.commit_workspace.task == id {
            return;
        }
        let index = self
            .commit_workspace
            .lists
            .iter()
            .position(|list| list.task == id)
            .unwrap_or_else(|| {
                self.commit_workspace.lists.push(ChangeList {
                    name: format!("{id} {summary}"),
                    task: id.clone(),
                    paths: HashSet::new(),
                });
                self.commit_workspace.lists.len() - 1
            });
        self.commit_workspace.task = id;
        self.commit_workspace.active = index;
        self.commit_workspace.save();
    }
    pub(crate) fn workspace_move_to_active(&mut self, path: &Path) {
        let active = self.commit_workspace.active;
        self.commit_workspace.assign(&[path.to_path_buf()], active);
    }
    pub fn commit_environment(&self) -> Result<Vec<(String, String)>, String> {
        let author = self.commit_workspace.author.trim();
        if author.is_empty() {
            return Ok(Vec::new());
        }
        let (name, email) = author
            .rsplit_once('<')
            .ok_or("Author must be Name <email>")?;
        let email = email
            .strip_suffix('>')
            .ok_or("Author must be Name <email>")?
            .trim();
        if name.trim().is_empty() || !email.contains('@') || author.contains(['\n', '\r', '\0']) {
            return Err("Invalid commit author".into());
        }
        Ok(vec![
            ("GIT_AUTHOR_NAME".into(), name.trim().into()),
            ("GIT_AUTHOR_EMAIL".into(), email.into()),
        ])
    }
    /// Close project-owned previews and invalidate pending preparation before
    /// another project can consume the queued paths or apply a patch.
    pub(crate) fn cancel_commit_project(&mut self) {
        self.commit_workspace = Workspace::default();
    }
    pub(crate) fn commit_modal_active(&self) -> bool {
        self.commit_workspace.failure.is_some()
            || self.commit_workspace.rollback.is_some()
            || !self.commit_workspace.resolutions.is_empty()
    }
    pub fn commit_uses_index(&self) -> bool {
        self.commit_workspace.index_commit || rustpy_vcs::policy::current().staging_area
    }
    pub(crate) fn workspace_root(&mut self, root: &Path) {
        if self.commit_workspace.loaded_root != root {
            self.commit_workspace = Workspace::load(root);
        }
    }
    pub(crate) fn workspace_checks(&mut self) -> Result<(), String> {
        let mut results = Vec::new();
        let workspace = &self.commit_workspace;
        let lines: Vec<_> = self.message.lines().collect();
        if lines
            .first()
            .is_some_and(|line| line.chars().count() > workspace.subject_limit.max(1))
        {
            results.push(format!(
                "Subject exceeds {} characters",
                workspace.subject_limit
            ));
        }
        if lines.len() > 1 && !lines[1].is_empty() {
            results.push("Separate subject and body with a blank line".into());
        }
        if lines
            .iter()
            .skip(2)
            .any(|line| line.chars().count() > workspace.body_limit.max(1))
        {
            results.push(format!("Body exceeds {} characters", workspace.body_limit));
        }
        for path in &self.checked {
            if let Ok(text) = std::fs::read_to_string(path) {
                if workspace.check_todo
                    && text
                        .lines()
                        .any(|line| line.contains("TODO") || line.contains("FIXME"))
                {
                    results.push(format!("TODO: {}", path.display()));
                }
                if workspace.check_conflicts
                    && text
                        .lines()
                        .any(|line| line.starts_with("<<<<<<< ") || line == "=======")
                {
                    results.push(format!("Conflict marker: {}", path.display()));
                }
            }
        }
        if let Ok(issues) = self.grammar_issues() {
            results.extend(
                issues
                    .into_iter()
                    .map(|issue| format!("Message: {}", issue.message)),
            );
        }
        if self.commit_workspace.check_spelling {
            static DICTIONARY: std::sync::OnceLock<rustpy_spell::Dictionary> =
                std::sync::OnceLock::new();
            results.extend(
                rustpy_spell::check_plain_text(
                    &self.message,
                    DICTIONARY.get_or_init(rustpy_spell::Dictionary::bundled),
                )
                .into_iter()
                .take(20)
                .map(|typo| format!("Spelling: {}", typo.word)),
            );
        }
        self.commit_workspace.results = results;
        if self.commit_workspace.require_clean_checks && !self.commit_workspace.results.is_empty() {
            return Err("Commit checks found warnings; review results in Local Changes".into());
        }
        Ok(())
    }
    pub(crate) fn workspace_before_commit(&mut self) -> Result<(), String> {
        self.commit_environment()?;
        self.workspace_checks()?;
        let message = self.message.clone();
        if !message.trim().is_empty() {
            self.commit_workspace.history.retain(|old| old != &message);
            self.commit_workspace.history.insert(0, message);
            self.commit_workspace.history.truncate(50);
            self.commit_workspace.save();
        }
        Ok(())
    }
    pub(crate) fn workspace_tools(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        self.workspace_root(repo.root());
        self.workspace_resolution_ui(ui, repo, events);
        if let Some(error) = self.commit_workspace.failure.clone() {
            let mut open = true;
            egui::Window::new("Commit failed").open(&mut open).show_modal(ui.ctx(),|ui|{ui.label(error);ui.label("Your changes and commit message are retained. See the Git Console for hook output.");if ui.button("Dismiss").clicked(){self.commit_workspace.failure=None;}});
            if !open {
                self.commit_workspace.failure = None;
            }
        }
        // Each path has exactly one owner, including changes discovered after refresh.
        let unassigned: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                !self
                    .commit_workspace
                    .lists
                    .iter()
                    .any(|list| list.paths.contains(&entry.path))
            })
            .map(|entry| entry.path.clone())
            .collect();
        if !unassigned.is_empty() {
            let active = self.commit_workspace.active;
            self.commit_workspace.assign(&unassigned, active);
        }
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.commit_workspace.shelf_tab, false, "Changes");
            ui.selectable_value(
                &mut self.commit_workspace.shelf_tab,
                true,
                format!("Shelf ({})", self.commit_workspace.shelves.len()),
            );
            if ui.button("Refresh").clicked() {
                self.refresh(repo, now);
            }
            if ui.button("Expand all").clicked() {
                self.commit_workspace.expanded = true;
            }
            if ui.button("Collapse all").clicked() {
                self.commit_workspace.expanded = false;
            }
            changed |= ui
                .checkbox(&mut self.commit_workspace.tree, "Tree")
                .changed();
            changed |= ui
                .checkbox(&mut self.commit_workspace.preview, "Show diff preview")
                .changed();
            egui::ComboBox::from_id_salt("change-grouping")
                .selected_text(
                    ["Directory", "Module", "Repository"][self.commit_workspace.grouping.min(2)],
                )
                .show_ui(ui, |ui| {
                    for (index, label) in ["Directory", "Module", "Repository"].iter().enumerate() {
                        changed |= ui
                            .selectable_value(&mut self.commit_workspace.grouping, index, *label)
                            .changed();
                    }
                })
                .combo_named(crate::i18n::tr_static("Group changes by"));
        });
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.show_unversioned,
                    "Show unversioned files",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.show_ignored,
                    "Show ignored files",
                )
                .changed();
            if ui.button("Commit all tracked changes").clicked() {
                self.checked = self
                    .entries
                    .iter()
                    .filter(|entry| !entry.is_unversioned() && entry.status != FileStatus::Ignored)
                    .map(|entry| entry.path.clone())
                    .collect();
            }
        });
        if self.commit_workspace.shelf_tab {
            self.workspace_shelf(ui, repo, now, events);
            return;
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("active-changelist")
                .selected_text(&self.commit_workspace.lists[self.commit_workspace.active].name)
                .show_ui(ui, |ui| {
                    for (index, list) in self.commit_workspace.lists.iter().enumerate() {
                        changed |= ui
                            .selectable_value(&mut self.commit_workspace.active, index, &list.name)
                            .changed();
                    }
                })
                .combo_named(crate::i18n::tr_static("Active changelist"));
            ui.text_edit_singleline(&mut self.commit_workspace.name);
            if ui.button("Create list").clicked() {
                let name = self.commit_workspace.name.trim().to_owned();
                if !name.is_empty()
                    && !self
                        .commit_workspace
                        .lists
                        .iter()
                        .any(|list| list.name == name)
                {
                    self.commit_workspace.lists.push(ChangeList {
                        name,
                        task: self.commit_workspace.task.clone(),
                        paths: HashSet::new(),
                    });
                    self.commit_workspace.active = self.commit_workspace.lists.len() - 1;
                    changed = true;
                }
            }
            if ui.button("Rename list").clicked() {
                let name = self.commit_workspace.name.trim().to_owned();
                if !name.is_empty()
                    && !self
                        .commit_workspace
                        .lists
                        .iter()
                        .any(|list| list.name == name)
                {
                    self.commit_workspace.lists[self.commit_workspace.active].name = name;
                    changed = true;
                }
            }
            if ui
                .add_enabled(
                    self.commit_workspace.active != 0,
                    egui::Button::new("Delete list"),
                )
                .clicked()
            {
                let old = self
                    .commit_workspace
                    .lists
                    .remove(self.commit_workspace.active);
                self.commit_workspace.lists[0].paths.extend(old.paths);
                self.commit_workspace.active = 0;
                changed = true;
            }
            if ui.button("Move checked to active").clicked() {
                let paths: Vec<_> = self.checked.iter().cloned().collect();
                let active = self.commit_workspace.active;
                self.commit_workspace.assign(&paths, active);
            }
            if ui.button("Select active list").clicked() {
                self.checked = self.commit_workspace.lists[self.commit_workspace.active]
                    .paths
                    .iter()
                    .filter(|path| self.entries.iter().any(|entry| &entry.path == *path))
                    .cloned()
                    .collect();
            }
        });
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.track_tasks,
                    "Track lists by task",
                )
                .changed();
            ui.text_edit_singleline(&mut self.commit_workspace.task);
            if ui.button("Activate task").clicked() && self.commit_workspace.track_tasks {
                let task = self.commit_workspace.task.trim().to_owned();
                if !task.is_empty() {
                    let index = self
                        .commit_workspace
                        .lists
                        .iter()
                        .position(|list| list.task == task)
                        .unwrap_or_else(|| {
                            self.commit_workspace.lists.push(ChangeList {
                                name: task.clone(),
                                task,
                                paths: HashSet::new(),
                            });
                            self.commit_workspace.lists.len() - 1
                        });
                    self.commit_workspace.active = index;
                    changed = true;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self
                        .checked
                        .iter()
                        .any(|path| self.commit_workspace.unsaved.contains(path)),
                    egui::Button::new("Stage checked"),
                )
                .on_disabled_hover_text("Save checked editor buffers first")
                .clicked()
            {
                let paths: Vec<_> = self.checked.iter().cloned().collect();
                let result = paths.iter().try_for_each(|path| repo.workspace_stage(path));
                self.commit_workspace.notice = result
                    .map(|_| "Changes staged".into())
                    .unwrap_or_else(|e| e.to_string());
                self.refresh(repo, now);
            }
            if ui
                .add_enabled(
                    self.commit_workspace.unsaved.is_empty(),
                    egui::Button::new("Stage all"),
                )
                .clicked()
            {
                for entry in &self.entries {
                    if entry.status != FileStatus::Ignored {
                        if let Err(error) = repo.workspace_stage(&entry.path) {
                            self.commit_workspace.notice = error.to_string();
                            break;
                        }
                    }
                }
                self.commit_workspace.index_commit = true;
                self.refresh(repo, now);
            }
            if ui.button("Unstage checked").clicked() {
                let paths: Vec<_> = self.checked.iter().cloned().collect();
                for path in paths {
                    if let Err(error) = repo.unstage(&path) {
                        self.commit_workspace.notice = error.to_string();
                        break;
                    }
                }
                self.refresh(repo, now);
            }
            ui.checkbox(
                &mut self.commit_workspace.index_commit,
                "Commit staged index (partial commit)",
            );
            if ui.button("Rollback checked…").clicked() {
                self.commit_workspace.rollback = Some(self.checked.iter().cloned().collect());
            }
            if ui.button("Create patch from checked").clicked() {
                self.workspace_create_patch(repo);
            }
            if ui
                .add_enabled(
                    !self
                        .checked
                        .iter()
                        .any(|path| self.commit_workspace.unsaved.contains(path)),
                    egui::Button::new("Shelve checked"),
                )
                .on_disabled_hover_text("Save checked editor buffers first")
                .clicked()
            {
                self.workspace_create_patch(repo);
                if !self.commit_workspace.patch.is_empty() {
                    self.commit_workspace.shelves.push(Shelf {
                        name: if self.commit_workspace.name.trim().is_empty() {
                            "Shelved changes".into()
                        } else {
                            self.commit_workspace.name.clone()
                        },
                        base: repo.workspace_revision(),
                        patch: self.commit_workspace.patch.clone(),
                    });
                    self.commit_workspace.save();
                    self.commit_workspace.notice =
                        "Saved changes to Shelf; confirm rollback below to finish shelving.".into();
                    self.commit_workspace.rollback = Some(self.checked.iter().cloned().collect());
                }
            }
        });
        if let Some(paths) = self.commit_workspace.rollback.clone() {
            ui.group(|ui| {
                ui.label(format!("Discard changes in {} checked files?", paths.len()));
                ui.checkbox(
                    &mut self.commit_workspace.delete_added,
                    "Delete added/unversioned local files",
                );
                if ui
                    .add_enabled(
                        !paths
                            .iter()
                            .any(|path| self.commit_workspace.unsaved.contains(path)),
                        egui::Button::new("Confirm rollback"),
                    )
                    .on_disabled_hover_text("Save or close affected editor buffers first")
                    .clicked()
                {
                    history_store::snapshot_paths(
                        &history_store::base_dir(),
                        Some(repo.root()),
                        &paths,
                        "Before rollback",
                        self.commit_workspace.delete_added,
                    );
                    for path in &paths {
                        let result = if self.commit_workspace.delete_added
                            || repo.head_bytes(path).is_some()
                        {
                            repo.rollback(path)
                        } else {
                            repo.rollback_keeping_new(path)
                        };
                        if let Err(error) = result {
                            self.commit_workspace.notice = error.to_string();
                            break;
                        }
                    }
                    self.commit_workspace.rollback = None;
                    self.refresh(repo, now);
                    events.push(GitEvent::FilesChanged);
                }
                if ui.button("Cancel rollback").clicked() {
                    self.commit_workspace.rollback = None;
                }
            });
        }
        self.workspace_patch_ui(ui, repo, now, events);
        if !self.commit_workspace.notice.is_empty() {
            ui.label(&self.commit_workspace.notice);
        }
        if changed {
            self.commit_workspace.save();
        }
    }
    fn workspace_create_patch(&mut self, repo: &GitRepo) {
        let paths: Vec<_> = self.checked.iter().cloned().collect();
        match repo.workspace_export(&paths) {
            Ok(patch) => {
                self.commit_workspace.patch = patch;
                self.commit_workspace.patch_base = repo.workspace_revision();
                self.commit_workspace.preview_valid = None;
            }
            Err(error) => {
                self.commit_workspace.patch.clear();
                self.commit_workspace.notice = error.to_string();
            }
        }
    }
    pub(super) fn commit_shelves_ui(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        self.workspace_root(repo.root());
        self.workspace_resolution_ui(ui, repo, events);
        self.workspace_shelf(ui, repo, now, events);
        if !self.commit_workspace.notice.is_empty() {
            ui.label(&self.commit_workspace.notice);
        }
    }
    fn workspace_shelf(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        for (index, shelf) in self.commit_workspace.shelves.iter().enumerate() {
            if ui
                .selectable_label(
                    self.commit_workspace.selected_shelf == Some(index),
                    format!("{} · base {}", shelf.name, shelf.base),
                )
                .clicked()
            {
                self.commit_workspace.selected_shelf = Some(index);
                self.commit_workspace.patch = shelf.patch.clone();
                self.commit_workspace.patch_base = shelf.base.clone();
                self.commit_workspace.preview_valid = None;
            }
        }
        if let Some(index) = self
            .commit_workspace
            .selected_shelf
            .filter(|index| *index < self.commit_workspace.shelves.len())
        {
            ui.horizontal(|ui|{ui.text_edit_singleline(&mut self.commit_workspace.name);if ui.button("Rename shelf").clicked()&&!self.commit_workspace.name.trim().is_empty(){self.commit_workspace.shelves[index].name=self.commit_workspace.name.clone();self.commit_workspace.save();}
if ui.button("Remove shelf").clicked(){self.commit_workspace.shelves.remove(index);self.commit_workspace.selected_shelf=None;self.commit_workspace.save();}
if ui.add_enabled(self.commit_workspace.unsaved.is_empty(),egui::Button::new("Unshelve silently")).clicked(){let patch=self.commit_workspace.patch.clone();match repo.workspace_apply_patch(&patch,false,true).and_then(|_|repo.workspace_apply_patch(&patch,false,false)){Ok(())=>{self.commit_workspace.notice="Shelf applied; retained for later use".into();events.push(GitEvent::FilesChanged);self.refresh(repo,now);},Err(error)=>self.commit_workspace.notice=format!("Shelf conflict: {error}. Working files unchanged; resolve using patch preview.")}}});
        }
        self.workspace_patch_ui(ui, repo, now, events);
    }
    fn workspace_patch_ui(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        if !self.commit_workspace.unsaved.is_empty() {
            ui.label("Save editor buffers before applying or staging patches");
            return;
        }
        egui::CollapsingHeader::new("Patch / hunk staging / import & export").show(ui,|ui|{
            ui.horizontal(|ui|{ui.label("Patch file:");ui.text_edit_singleline(&mut self.commit_workspace.patch_file);if ui.button("Read").clicked(){match std::fs::read_to_string(&self.commit_workspace.patch_file){Ok(text)if text.len()<=8*1024*1024=>{self.commit_workspace.patch=text;self.commit_workspace.preview_valid=None;},Ok(_)=>self.commit_workspace.notice="Patch exceeds 8 MiB".into(),Err(error)=>self.commit_workspace.notice=error.to_string()}}
if ui.button("Write").clicked(){self.commit_workspace.notice=std::fs::write(&self.commit_workspace.patch_file,&self.commit_workspace.patch).map(|_|"Patch exported".into()).unwrap_or_else(|e|e.to_string());}});
            ui.horizontal_wrapped(|ui|{if ui.button("Copy patch").clicked(){ui.ctx().copy_text(self.commit_workspace.patch.clone());}
if ui.button("Apply patch from clipboard").clicked(){if let Ok(mut clipboard)=crate::system_clipboard::open(){if let Ok(text)=clipboard.get_text(){if text.len()<=8*1024*1024&&text.contains("@@ "){self.commit_workspace.patch=text;self.commit_workspace.preview_valid=None;}}}}
if ui.button("Import into shelf").clicked()&&!self.commit_workspace.patch.is_empty(){self.commit_workspace.shelves.push(Shelf{name:self.commit_workspace.name.clone(),base:self.commit_workspace.patch_base.clone(),patch:self.commit_workspace.patch.clone()});self.commit_workspace.save();}});
            ui.horizontal(|ui|{ui.label("Base revision:");ui.text_edit_singleline(&mut self.commit_workspace.patch_base);ui.label("Strip leading dirs:");if ui.add(egui::DragValue::new(&mut self.commit_workspace.strip).range(0..=12)).changed(){self.commit_workspace.preview_valid=None;}
if ui.checkbox(&mut self.commit_workspace.reverse,"Reverse").changed(){self.commit_workspace.preview_valid=None;}});
            if ui.add(egui::TextEdit::multiline(&mut self.commit_workspace.patch).code_editor().desired_rows(6).desired_width(f32::INFINITY)).changed(){self.commit_workspace.preview_valid=None;}
            let prepared=prepare_patch(&self.commit_workspace.patch,self.commit_workspace.strip,self.commit_workspace.reverse);
            ui.horizontal(|ui|{if ui.button("Preview / check conflicts").clicked(){match prepared.as_ref().map_err(Clone::clone).and_then(|patch|repo.workspace_apply_patch(patch,false,true).map_err(|e|e.to_string())){Ok(())=>{self.commit_workspace.preview_valid=prepared.clone().ok();self.commit_workspace.notice=if self.commit_workspace.patch_base!=repo.workspace_revision(){"Patch context applies; base differs from HEAD. Review preview before applying.".into()}else{"Patch applies cleanly".into()};},Err(error)=>{self.commit_workspace.preview_valid=None;self.commit_workspace.notice=format!("Patch conflicts: {error}. Edit the patch or resolve source context, then check again.");}}}
if ui.add_enabled(prepared.as_ref().ok()==self.commit_workspace.preview_valid.as_ref()&&self.commit_workspace.preview_valid.is_some(),egui::Button::new("Apply checked patch")).clicked(){if let Ok(patch)=&prepared{match repo.workspace_apply_patch(patch,false,false){Ok(())=>{events.push(GitEvent::FilesChanged);self.commit_workspace.preview_valid=None;self.refresh(repo,now);},Err(error)=>self.commit_workspace.notice=error.to_string()}}}});
            let patches=hunks(&self.commit_workspace.patch);for(index,patch)in patches.iter().enumerate(){ui.horizontal(|ui|{ui.label(format!("Hunk {}",index+1));if ui.button("Include / stage hunk").clicked(){match repo.workspace_apply_patch(patch,true,true).and_then(|_|repo.workspace_apply_patch(patch,true,false)){Ok(())=>{self.commit_workspace.index_commit=true;self.commit_workspace.notice="Hunk staged; commit uses the index".into();self.refresh(repo,now);},Err(error)=>self.commit_workspace.notice=error.to_string()}}
if ui.button("Exclude / unstage hunk").clicked(){match prepare_patch(patch,0,true).map_err(anyhow::Error::msg).and_then(|reverse|repo.workspace_apply_patch(&reverse,true,true).and_then(|_|repo.workspace_apply_patch(&reverse,true,false))){Ok(())=>self.refresh(repo,now),Err(error)=>self.commit_workspace.notice=error.to_string()}}});}
        });
    }
    pub(crate) fn workspace_message_tools(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        ui.horizontal(|ui| {
            ui.label("New branch:");
            ui.text_edit_singleline(&mut self.new_branch_name);
            if ui
                .add_enabled(
                    self.can_commit() && !self.new_branch_name.trim().is_empty(),
                    egui::Button::new("Commit to new branch"),
                )
                .clicked()
            {
                events.push(GitEvent::CommitWorkspace(Event::NewBranchCommit(
                    self.new_branch_name.trim().into(),
                )));
            }
        });
        let policy = rustpy_vcs::policy::current();
        let mut signoff = policy.sign_off;
        let mut sign = policy.sign_commits;
        ui.horizontal(|ui| {
            let changed = ui.checkbox(&mut signoff, "Sign-off").changed()
                | ui.checkbox(&mut sign, "GPG / SSH sign").changed();
            if changed {
                events.push(GitEvent::CommitWorkspace(Event::Signing(signoff, sign)));
            }
            ui.label("Git hooks run before commit; output is available in the Git Console.");
        });
        ui.horizontal(|ui| {
            ui.label("Author:");
            ui.text_edit_singleline(&mut self.commit_workspace.author);
        });
        egui::ComboBox::from_id_salt("commit-message-history")
            .selected_text("Message history (Ctrl+M)")
            .show_ui(ui, |ui| {
                for text in &self.commit_workspace.history {
                    if ui
                        .selectable_label(false, text.lines().next().unwrap_or(""))
                        .clicked()
                    {
                        self.message = text.clone();
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Commit message history"));
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::M)) {
            if let Some(text) = self.commit_workspace.history.first() {
                self.message = text.clone();
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Save message template").clicked() && !self.message.trim().is_empty() {
                self.commit_workspace.templates.push(self.message.clone());
                self.commit_workspace.save();
            }
            egui::ComboBox::from_id_salt("commit-template")
                .selected_text("Templates")
                .show_ui(ui, |ui| {
                    for template in &self.commit_workspace.templates {
                        if ui
                            .selectable_label(false, template.lines().next().unwrap_or(""))
                            .clicked()
                        {
                            self.message = template.clone();
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Commit message templates"));
            if ui.button("Amend without changing message").clicked() {
                self.amend = true;
                self.message = repo.head_message().unwrap_or_default();
            }
        });
        let issues: Vec<_> = self
            .commit_workspace
            .lists
            .iter()
            .filter(|list| !list.task.is_empty())
            .map(|list| list.task.clone())
            .collect();
        if self.message.ends_with('#') {
            for issue in issues {
                if ui.small_button(format!("#{issue}")).clicked() {
                    self.message.push_str(issue.trim_start_matches('#'));
                }
            }
        }
        egui::ComboBox::from_id_salt("prepare-before-commit")
            .selected_text(format!(
                "Before commit: {}",
                [
                    "None",
                    "Reformat",
                    "Rearrange",
                    "Cleanup",
                    "Analyze",
                    "Run tests"
                ][self.commit_workspace.prepare_before_commit.min(5)]
            ))
            .show_ui(ui, |ui| {
                for (index, label) in [
                    "None",
                    "Reformat",
                    "Rearrange",
                    "Cleanup",
                    "Analyze",
                    "Run tests",
                ]
                .iter()
                .enumerate()
                {
                    if ui
                        .selectable_value(
                            &mut self.commit_workspace.prepare_before_commit,
                            index,
                            *label,
                        )
                        .changed()
                    {
                        self.commit_workspace.prepared_signature = None;
                        self.commit_workspace.save();
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Before commit"));
        if self.commit_workspace.preparation.is_some() {
            ui.label("Commit preparation in progress; review each requested dialog / preview");
            if ui.button("Cancel preparation").clicked() {
                self.commit_workspace.preparation = None;
                self.commit_workspace.prepared_signature = None;
            }
        }
        ui.horizontal_wrapped(|ui| {
            for (label, kind) in [
                ("Reformat", CheckKind::Reformat),
                ("Optimize imports", CheckKind::Optimize),
                ("Rearrange", CheckKind::Rearrange),
                ("Cleanup", CheckKind::Cleanup),
                ("Analyze code", CheckKind::Analyze),
                ("Run tests", CheckKind::Tests),
            ] {
                if ui.button(crate::i18n::tr_static(label)).clicked() {
                    events.push(GitEvent::CommitWorkspace(Event::CheckFiles(kind)));
                }
            }
        });
        egui::CollapsingHeader::new("Commit inspections").show(ui, |ui| {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label("Subject margin");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.commit_workspace.subject_limit)
                            .range(20..=200),
                    )
                    .changed();
                ui.label("Body margin");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.commit_workspace.body_limit).range(20..=200),
                    )
                    .changed();
            });
            changed |= ui
                .checkbox(&mut self.commit_workspace.check_todo, "Check TODO / FIXME")
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.check_spelling,
                    "Check commit message spelling",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.clear_message_after_commit,
                    "Clear initial commit message after successful commit",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.check_conflicts,
                    "Analyze conflict markers",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.commit_workspace.require_clean_checks,
                    "Block commit while checks have warnings",
                )
                .changed();
            if ui.button("Run commit checks").clicked() {
                let _ = self.workspace_checks();
                if self.commit_workspace.results.is_empty() {
                    self.commit_workspace.notice = "Commit checks passed".into();
                }
            }
            for result in &self.commit_workspace.results {
                ui.label(result);
            }
            if changed {
                self.commit_workspace.save();
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hunk_headers_and_reversal() {
        let patch="diff --git a/x.py b/x.py\n--- a/x.py\n+++ b/x.py\n@@ -1 +1 @@\n-a\n+b\n@@ -4 +4 @@\n-c\n+d\n";
        let selected = hunks(patch);
        assert_eq!(selected.len(), 2);
        assert!(selected[1].starts_with("diff --git"));
        assert!(!selected[1].contains("-a\n"));
        let reverse = prepare_patch(&selected[0], 0, true).unwrap();
        assert!(reverse.contains("--- b/x.py\n+++ a/x.py"));
        assert!(reverse.contains("-b\n+a\n"));
        assert_eq!(prepare_patch(&reverse, 0, true).unwrap(), selected[0]);
        let directory = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(directory.path()).unwrap();
        let path = directory.path().join("x.py");
        std::fs::write(&path, "a\n").unwrap();
        repo.commit(std::slice::from_ref(&path), "initial", false)
            .unwrap();
        std::fs::write(&path, "b\n").unwrap();
        repo.workspace_apply_patch(&reverse, false, true).unwrap();
        repo.workspace_apply_patch(&reverse, false, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n");
        repo.workspace_apply_patch(&selected[0], false, true)
            .unwrap();
        repo.workspace_apply_patch(&selected[0], false, false)
            .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "b\n");
    }
    #[test]
    fn reverse_keeps_header_like_content_and_no_newline_markers_owned() {
        let patch = "--- a/x.py\n+++ b/x.py\n@@ -1 +1 @@\n--- old text\n\\ No newline at end of file\n+++ new text\n\\ No newline at end of file\n";
        let reverse = prepare_patch(patch, 0, true).unwrap();
        assert_eq!(reverse, "--- b/x.py\n+++ a/x.py\n@@ -1 +1 @@\n-++ new text\n\\ No newline at end of file\n+-- old text\n\\ No newline at end of file\n");
        assert_eq!(prepare_patch(&reverse, 0, true).unwrap(), patch);
    }

    #[test]
    fn traversal_rejected() {
        assert!(prepare_patch("--- a/../../secret\n+++ b/../../secret\n", 1, false).is_err());
    }
    #[test]
    fn single_owner() {
        let mut workspace = Workspace {
            lists: vec![
                ChangeList {
                    name: "a".into(),
                    task: String::new(),
                    paths: [PathBuf::from("a.py")].into(),
                },
                ChangeList {
                    name: "b".into(),
                    task: String::new(),
                    paths: HashSet::new(),
                },
            ],
            ..Default::default()
        };
        workspace.assign(&[PathBuf::from("a.py")], 1);
        assert!(workspace.lists[0].paths.is_empty());
        assert!(workspace.lists[1].paths.contains(Path::new("a.py")));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    const PATCH:&str="diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1,3 +1,3 @@\n one\n-two\n+changed\n three\n";
    #[test]
    fn single_line_and_file_identity_are_preserved() {
        let root = Path::new("/project");
        let selected = hunk_at_line(PATCH, Path::new("/project/a.py"), root, 1, true).unwrap();
        assert!(selected.contains("-two\n+changed"));
        assert!(hunk_at_line(PATCH, Path::new("/project/other.py"), root, 1, true).is_none());
        assert!(hunk_at_line(PATCH, Path::new("/project/a.py"), root, 90, false).is_none());
    }
    #[test]
    fn patch_base_and_binary_reverse_are_explicit() {
        assert_eq!(
            detected_base("base-commit: abcdef1234\n"),
            Some("abcdef1234".into())
        );
        assert!(detected_base("base-commit: not-a-hash").is_none());
        assert!(prepare_patch("GIT binary patch\nliteral 1\n", 0, true).is_err());
    }
}

#[cfg(test)]
mod commit_checks_tests {
    use super::*;
    #[test]
    fn authors_and_message_inspections_are_real_submit_guards() {
        let mut panel = GitPanel::default();
        panel.commit_workspace.author = "Ada <ada@example.org>".into();
        assert_eq!(
            panel.commit_environment().unwrap()[0],
            ("GIT_AUTHOR_NAME".into(), "Ada".into())
        );
        panel.commit_workspace.author = "Ada\nInjected <ada@example.org>".into();
        assert!(panel.commit_environment().is_err());
        panel.commit_workspace.author.clear();
        panel.commit_workspace.subject_limit = 4;
        panel.commit_workspace.require_clean_checks = true;
        panel.message = "Long subject\nNo blank line".into();
        assert!(panel.workspace_before_commit().is_err());
        assert!(panel
            .commit_workspace
            .results
            .iter()
            .any(|message| message.contains("blank line")));
    }
    #[test]
    fn task_lists_and_shelves_round_trip_without_runtime_authority() {
        let mut workspace = Workspace::initial();
        workspace.lists.push(ChangeList {
            name: "Fix #23".into(),
            task: "23".into(),
            paths: [PathBuf::from("src/main.py")].into(),
        });
        workspace.active = 0;
        workspace.shelves.push(Shelf {
            name: "review".into(),
            base: "abcdef12".into(),
            patch: "--- a/x\n+++ b/x\n".into(),
        });
        workspace.index_commit = true;
        workspace.prepared_signature = Some(44);
        let restored: Workspace =
            serde_json::from_str(&serde_json::to_string(&workspace).unwrap()).unwrap();
        assert_eq!(restored.shelves[0].base, "abcdef12");
        assert_eq!(restored.lists[0].task, "23");
        assert!(!restored.index_commit);
        assert!(restored.prepared_signature.is_none());
    }
}

#[cfg(test)]
mod patch_fixture_tests {
    use super::*;
    #[test]
    fn multi_file_selection_never_stages_the_other_file() {
        let patch = include_str!("../fixtures/commit_workspace/two-files.patch");
        let selected = hunk_at_line(
            patch,
            Path::new("/repo/src/b.py"),
            Path::new("/repo"),
            0,
            false,
        )
        .unwrap();
        assert!(!selected.contains("src/a.py"));
        assert!(selected.contains("+new = True"));
    }
    #[test]
    fn mail_base_revision_is_detected() {
        let patch = include_str!("../fixtures/commit_workspace/mail.patch");
        assert_eq!(detected_base(patch).as_deref(), Some("0123456789abcdef"));
    }
}

#[cfg(test)]
mod default_list_fixtures {
    use super::*;
    #[test]
    fn todo_default_changelist_uses_explicit_membership_and_unassigned_changes() {
        let root = PathBuf::from("/project");
        let a = root.join("a.py");
        let b = root.join("b.py");
        let c = root.join("c.py");
        let mut workspace = Workspace::initial();
        workspace.loaded_root = root.clone();
        workspace.lists = vec![
            ChangeList {
                name: "Default".into(),
                task: String::new(),
                paths: HashSet::from([a.clone()]),
            },
            ChangeList {
                name: "Feature".into(),
                task: String::new(),
                paths: HashSet::from([b.clone()]),
            },
        ];
        let changed = std::collections::BTreeSet::from([a.clone(), b.clone(), c.clone()]);
        assert_eq!(
            workspace.default_paths_for(&root, &changed),
            HashSet::from([a.clone(), c.clone()])
        );
        workspace.lists[1].paths.insert(c);
        assert_eq!(
            workspace.default_paths_for(&root, &changed),
            HashSet::from([a])
        );
    }
}

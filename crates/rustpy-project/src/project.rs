//! Shareable project structure: `.rustpy/project.json`, the equivalent of
//! an `.iml` module file (roadmap §4 "Project structure").
//!
//! It lists the extra content roots and the folders marked Sources, Test
//! Sources, Resources, Templates, Namespace Package or Excluded. Paths are
//! written relative to the project directory so the file can be committed and
//! teammates get the same layout; per-user state (open files, breakpoints,
//! tool-window layout) stays in `workspace.json`.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use rustpy_settings::{Workspace, PROJECT_DIR_NAME};

pub const PROJECT_FILE: &str = "project.json";

/// The files of `.rustpy` meant to be shared through version control: the
/// project structure, run configurations and data sources.
pub const SHARED_FILES: [&str; 9] = [
    PROJECT_FILE,
    "runConfigurations.json",
    "dataSources.json",
    "code-style.json",
    "scopes.json",
    "inspectionProfiles/",
    "codeStyles/",
    "runConfigurations/",
    "dictionaries/",
];

/// `.rustpy/.gitignore` written next to a new `project.json`: everything else
/// in `.rustpy` is per-user (window layout, histories, coverage data, tasks)
/// or may hold secrets (deployment passwords), including files added by later
/// versions, so only the shared files are let through.
const DIR_GITIGNORE: &str = "# RustPy: workspace.json, .name, layout, histories, coverage and secrets stay local.\n# Project structure, profiles, styles, shared run configurations and dictionaries can be committed.\n*\n!.gitignore\n!project.json\n!runConfigurations.json\n!dataSources.json\n!code-style.json\n!scopes.json\n!inspectionProfiles/\n!inspectionProfiles/**\n!codeStyles/\n!codeStyles/**\n!runConfigurations/\n!runConfigurations/**\n!dictionaries/\n!dictionaries/**\n";

/// What a directory is marked as (Project view › Mark Directory as).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RootKind {
    Sources,
    Tests,
    Resources,
    Templates,
    NamespacePackage,
    Excluded,
}

impl RootKind {
    pub const ALL: [RootKind; 6] = [
        RootKind::Sources,
        RootKind::Tests,
        RootKind::Resources,
        RootKind::Templates,
        RootKind::NamespacePackage,
        RootKind::Excluded,
    ];

    /// Menu label ("Mark Directory as" entry).
    pub fn label(self) -> &'static str {
        match self {
            RootKind::Sources => "Sources Root",
            RootKind::Tests => "Test Sources Root",
            RootKind::Resources => "Resource Root",
            RootKind::Templates => "Template Folder",
            RootKind::NamespacePackage => "Namespace Package",
            RootKind::Excluded => "Excluded",
        }
    }

    /// Heading for the folders of this kind (Project Structure page).
    pub fn plural(self) -> &'static str {
        match self {
            RootKind::Sources => "Source Folders",
            RootKind::Tests => "Test Source Folders",
            RootKind::Resources => "Resource Folders",
            RootKind::Templates => "Template Folders",
            RootKind::NamespacePackage => "Namespace Packages",
            RootKind::Excluded => "Excluded Folders",
        }
    }

    /// Folders of this kind are import roots (added to module resolution and `PYTHONPATH`).
    pub fn is_import_root(self) -> bool {
        matches!(self, RootKind::Sources | RootKind::Tests)
    }
}

/// Settings inherited by files below one attached content root. Longest root
/// wins, so a nested module may use a different SDK and Python language level.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectModule {
    pub content_root: PathBuf,
    pub sdk: Option<PathBuf>,
    pub language_level: Option<(u32, u32)>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The project structure. In memory every path is absolute; on disk paths
/// inside (or next to) the project directory are relative.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectModel {
    pub frameworks: crate::frameworks::Facet,
    pub sdk: Option<PathBuf>,
    pub language_level: Option<(u32, u32)>,
    pub modules: Vec<ProjectModule>,
    /// Content roots besides the project directory (attached folders, other parts of a monorepo).
    pub content_roots: Vec<PathBuf>,
    pub source_roots: Vec<PathBuf>,
    pub test_roots: Vec<PathBuf>,
    pub resource_roots: Vec<PathBuf>,
    pub template_dirs: Vec<PathBuf>,
    pub namespace_packages: Vec<PathBuf>,
    /// Left out of the file index, search, navigation and refactoring.
    pub excluded: Vec<PathBuf>,
}

impl ProjectModel {
    /// Add ignore hints without changing a user-maintained ignore file.
    pub fn ensure_gitignore(project: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let directory = project.join(PROJECT_DIR_NAME);
        std::fs::create_dir_all(&directory)?;
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join(".gitignore"))
        {
            Ok(mut file) => file.write_all(DIR_GITIGNORE.as_bytes()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Local project display name; folder names remain the fallback.
    pub fn display_name(project: &Path) -> String {
        std::fs::read_to_string(project.join(PROJECT_DIR_NAME).join(".name"))
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && !name.contains(['\n', '\r', '\0']))
            .unwrap_or_else(|| {
                project
                    .file_name()
                    .unwrap_or(project.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
    }

    /// Names cannot contain line breaks; an empty name restores the folder name.
    pub fn save_display_name(project: &Path, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        anyhow::ensure!(
            !name.contains(['\n', '\r', '\0']),
            "Project names must fit on one line"
        );
        let directory = project.join(PROJECT_DIR_NAME);
        let path = directory.join(".name");
        if name.is_empty() {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            Self::ensure_gitignore(project)?;
            let staged = path.with_extension("name.tmp");
            std::fs::write(&staged, format!("{name}\n"))?;
            std::fs::rename(staged, path)?;
        }
        Ok(())
    }

    pub fn path_for(project: &Path) -> PathBuf {
        project.join(PROJECT_DIR_NAME).join(PROJECT_FILE)
    }

    pub fn exists(project: &Path) -> bool {
        Self::path_for(project).is_file()
    }

    /// Reads `.rustpy/project.json`. Without one, the Sources roots and
    /// excluded folders older builds kept in `workspace.json` are used (nothing
    /// is written: `project.json` appears with the first change to the
    /// structure). A file that cannot be read or parsed (a merge conflict in
    /// the shared file) is an error, so the IDE can tell the user instead of
    /// showing an empty structure. Content roots that must not be walked are
    /// left out (see [`content_root_problem`]).
    pub fn try_load(project: &Path) -> Result<ProjectModel, String> {
        Self::try_load_reporting(project).map(|(m, _)| m)
    }

    /// [`ProjectModel::try_load`], also returning why content roots were left out
    /// (one message per root, for the user).
    pub fn try_load_reporting(project: &Path) -> Result<(ProjectModel, Vec<String>), String> {
        let path = Self::path_for(project);
        let mut model = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<ProjectModel>(&bytes)
                .map(|m| m.resolved(project))
                .map_err(|e| format!("{} is not valid: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::from_workspace(project)?,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let problems: Vec<String> = model
            .drop_invalid_content_roots(project)
            .into_iter()
            .map(|(root, why)| format!("{}: {why}", root.display()))
            .collect();
        for p in &problems {
            log::warn!("{}: content root ignored: {p}", path.display());
        }
        Ok((model, problems))
    }

    /// Resolve project metadata from a checked immutable observation. Missing
    /// metadata migrates the supplied workspace rather than rereading the disk.
    pub fn from_snapshot_reporting(
        project: &Path,
        bytes: Option<&[u8]>,
        workspace: &Workspace,
    ) -> Result<(ProjectModel, Vec<String>), String> {
        let mut model = match bytes {
            Some(bytes) => serde_json::from_slice::<ProjectModel>(bytes).map_err(|error| {
                format!(
                    "{} is not valid: {error}",
                    Self::path_for(project).display()
                )
            })?,
            None => ProjectModel {
                source_roots: workspace.source_roots.clone(),
                excluded: workspace.excluded_dirs.clone(),
                ..Default::default()
            },
        }
        .resolved(project);
        let problems: Vec<String> = model
            .drop_invalid_content_roots(project)
            .into_iter()
            .map(|(root, why)| format!("{}: {why}", root.display()))
            .collect();
        for problem in &problems {
            log::warn!("{}: content root ignored: {problem}", project.display());
        }
        Ok((model, problems))
    }

    /// [`ProjectModel::try_load`]; an unreadable file gives an empty structure.
    pub fn load(project: &Path) -> ProjectModel {
        Self::try_load(project).unwrap_or_else(|e| {
            log::warn!("{e}");
            ProjectModel::default()
        })
    }

    /// Whether [`ProjectModel::save`] may replace the file on disk: it is missing
    /// or valid. A file that does not parse (a merge conflict) holds what the
    /// user has to merge by hand and is never overwritten.
    pub fn check_replaceable(project: &Path) -> Result<(), String> {
        let path = Self::path_for(project);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ProjectModel>(&bytes) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!(
                    "{} is not valid ({e}); fix or delete it first, it was left unchanged",
                    path.display()
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    fn from_workspace(project: &Path) -> Result<ProjectModel, String> {
        let ws = Workspace::try_load(project).map_err(|error| error.to_string())?;
        Ok(ProjectModel {
            source_roots: ws.source_roots,
            excluded: ws.excluded_dirs,
            ..Default::default()
        }
        .resolved(project))
    }

    /// Writes `project.json` (relative, sorted paths). The first time it also adds a
    /// `.rustpy/.gitignore` that keeps the per-user files out of version control.
    /// An existing file that cannot be parsed is not replaced (an error).
    pub fn save(&self, project: &Path) -> anyhow::Result<()> {
        Self::check_replaceable(project).map_err(anyhow::Error::msg)?;
        let path = Self::path_for(project);
        let dir = project.join(PROJECT_DIR_NAME);
        std::fs::create_dir_all(&dir)?;
        Self::ensure_gitignore(project)?;
        let stored = self.relative_to(project);
        // Folder edits own only the root fields. Preserve metadata supplied by
        // other project features (modules, SDK and language level) and newer
        // versions instead of silently dropping it on the next folder mark.
        let mut document = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                serde_json::json!({})
            }
            Err(error) => return Err(error.into()),
        };
        let fields = serde_json::to_value(&stored)?;
        let object = document
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("project.json must be an object"))?;
        for (key, value) in fields.as_object().expect("project model is an object") {
            object.insert(key.clone(), value.clone());
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&document)? + "\n")?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        *self == ProjectModel::default()
    }

    pub fn dirs(&self, kind: RootKind) -> &[PathBuf] {
        match kind {
            RootKind::Sources => &self.source_roots,
            RootKind::Tests => &self.test_roots,
            RootKind::Resources => &self.resource_roots,
            RootKind::Templates => &self.template_dirs,
            RootKind::NamespacePackage => &self.namespace_packages,
            RootKind::Excluded => &self.excluded,
        }
    }

    fn dirs_mut(&mut self, kind: RootKind) -> &mut Vec<PathBuf> {
        match kind {
            RootKind::Sources => &mut self.source_roots,
            RootKind::Tests => &mut self.test_roots,
            RootKind::Resources => &mut self.resource_roots,
            RootKind::Templates => &mut self.template_dirs,
            RootKind::NamespacePackage => &mut self.namespace_packages,
            RootKind::Excluded => &mut self.excluded,
        }
    }

    /// How `dir` itself is marked.
    pub fn kind_of(&self, dir: &Path) -> Option<RootKind> {
        RootKind::ALL
            .into_iter()
            .find(|k| self.dirs(*k).iter().any(|d| d == dir))
    }

    /// Marks `dir` as `kind`, replacing any other mark (`None` unmarks it).
    pub fn mark(&mut self, dir: &Path, kind: Option<RootKind>) {
        for k in RootKind::ALL {
            self.dirs_mut(k).retain(|d| d != dir);
        }
        if let Some(k) = kind {
            let list = self.dirs_mut(k);
            list.push(dir.to_path_buf());
            list.sort();
        }
    }

    /// Removes the content roots [`content_root_problem`] rejects (a shared or
    /// hand-edited file); returns each with the reason. Of two overlapping roots
    /// the first in the (sorted) list stays.
    pub fn drop_invalid_content_roots(&mut self, project: &Path) -> Vec<(PathBuf, String)> {
        let mut kept: Vec<PathBuf> = Vec::new();
        let mut dropped = Vec::new();
        for root in std::mem::take(&mut self.content_roots) {
            match content_root_problem(project, &root, &kept) {
                Some(why) => dropped.push((root, why)),
                None => kept.push(root),
            }
        }
        self.content_roots = kept;
        dropped
    }

    /// Attaches a content root; false when it is already one.
    pub fn add_content_root(&mut self, dir: &Path) -> bool {
        if self.content_roots.iter().any(|d| d == dir) {
            return false;
        }
        self.content_roots.push(dir.to_path_buf());
        // Attaching an existing project preserves its SDK/language identity.
        // Folder marks remain in this project's one authoritative root model.
        let attached = Self::try_load(dir).unwrap_or_default();
        self.modules.push(ProjectModule {
            content_root: dir.to_path_buf(),
            sdk: attached.sdk,
            language_level: attached.language_level,
            ..Default::default()
        });
        self.content_roots.sort();
        true
    }

    /// Detaches a content root together with the marks inside it.
    pub fn remove_content_root(&mut self, dir: &Path) {
        self.content_roots.retain(|d| d != dir);
        self.modules
            .retain(|module| !module.content_root.starts_with(dir));
        for k in RootKind::ALL {
            self.dirs_mut(k).retain(|d| !d.starts_with(dir));
        }
    }

    /// Directories imports resolve from (and `PYTHONPATH` lists): the project
    /// directory, the extra content roots, then Sources and Test Sources roots.
    pub fn import_roots(&self, project: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        let project_root = project.to_path_buf();
        for r in std::iter::once(&project_root).chain(
            self.content_roots
                .iter()
                .chain(&self.source_roots)
                .chain(&self.test_roots),
        ) {
            if !self.excluded.iter().any(|excluded| r.starts_with(excluded)) && !roots.contains(r) {
                roots.push(r.clone());
            }
        }
        for package in &self.namespace_packages {
            if self
                .excluded
                .iter()
                .any(|excluded| package.starts_with(excluded))
            {
                continue;
            }
            if let Some(parent) = package.parent() {
                let parent = parent.to_path_buf();
                if !roots.contains(&parent) {
                    roots.push(parent);
                }
            }
        }
        roots
    }

    pub fn module_for(&self, path: &Path) -> Option<&ProjectModule> {
        self.modules
            .iter()
            .filter(|module| path.starts_with(&module.content_root))
            .max_by_key(|module| module.content_root.components().count())
    }

    pub fn sdk_for(&self, path: &Path) -> Option<&Path> {
        self.module_for(path)
            .and_then(|module| module.sdk.as_deref())
            .or(self.sdk.as_deref())
    }

    pub fn language_level_for(&self, path: &Path) -> Option<(u32, u32)> {
        self.module_for(path)
            .and_then(|module| module.language_level)
            .or(self.language_level)
    }

    /// Absolute paths for a model read from disk. Paths written with a path
    /// variable (`$NAME$/rest`) use the IDE's current definition; one that is
    /// not defined stays as written (see [`undefined_path_variables`]).
    fn resolved(mut self, project: &Path) -> ProjectModel {
        let vars = rustpy_settings::path_variables::active();
        let resolve = |project: &Path, stored: &Path| resolve_stored(&vars, project, stored);
        if let Some(sdk) = &mut self.sdk {
            *sdk = resolve(project, sdk);
        }
        for module in &mut self.modules {
            module.content_root = resolve(project, &module.content_root);
            if let Some(sdk) = &mut module.sdk {
                *sdk = resolve(project, sdk);
            }
        }
        for list in self.lists_mut() {
            for p in list.iter_mut() {
                *p = resolve(project, p);
            }
            list.sort();
            list.dedup();
        }
        self
    }

    /// Paths as written to disk: relative to the project when they are inside it or
    /// share a parent directory with it below any path variable's folder, else
    /// `$NAME$/rest` under a path variable, else absolute; `/` separators.
    fn relative_to(&self, project: &Path) -> ProjectModel {
        let vars = rustpy_settings::path_variables::active();
        let relativize = |project: &Path, path: &Path| store_path(&vars, project, path);
        let mut m = self.clone();
        if let Some(sdk) = &mut m.sdk {
            *sdk = relativize(project, sdk);
        }
        for module in &mut m.modules {
            module.content_root = relativize(project, &module.content_root);
            if let Some(sdk) = &mut module.sdk {
                *sdk = relativize(project, sdk);
            }
        }
        for list in m.lists_mut() {
            for p in list.iter_mut() {
                *p = relativize(project, p);
            }
            list.sort();
            list.dedup();
        }
        m
    }

    fn lists_mut(&mut self) -> [&mut Vec<PathBuf>; 7] {
        [
            &mut self.content_roots,
            &mut self.source_roots,
            &mut self.test_roots,
            &mut self.resource_roots,
            &mut self.template_dirs,
            &mut self.namespace_packages,
            &mut self.excluded,
        ]
    }
}

/// Why `root` cannot be a content root of `project` next to the content roots
/// `others` (None when it can). Content roots are walked, searched and
/// refactored with the project, so a root must lie outside the project
/// directory (it is part of it already), must not contain it (`..` or `/`
/// would pull in every sibling folder or the whole disk) and must not overlap
/// another content root. Symbolic links are followed for folders that exist.
pub fn content_root_problem(project: &Path, root: &Path, others: &[PathBuf]) -> Option<String> {
    let real = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (project_real, root_real) = (real(project), real(root));
    let inside = |a: &Path, b: &Path, a_real: &Path, b_real: &Path| {
        a.starts_with(b) || a_real.starts_with(b_real)
    };
    if inside(root, project, &root_real, &project_real) {
        return Some(
            "it is inside the project directory, which is part of the project already".into(),
        );
    }
    if inside(project, root, &project_real, &root_real) {
        return Some("it contains the project directory (every folder next to the project would be searched and refactored)".into());
    }
    others
        .iter()
        .find(|o| {
            *o != root
                && (inside(root, o, &root_real, &real(o)) || inside(o, root, &real(o), &root_real))
        })
        .map(|o| format!("it overlaps the content root {}", o.display()))
}

/// [`resolve`], first replacing a leading defined `$NAME$`. A path that
/// starts with an undefined variable is kept verbatim, so saving the project
/// again does not lose it.
fn resolve_stored(
    vars: &rustpy_settings::path_variables::PathVariables,
    project: &Path,
    stored: &Path,
) -> PathBuf {
    let text = stored.to_string_lossy();
    if rustpy_settings::path_variables::leading_variable(&text).is_some() {
        return match vars.expand_path(&text, Some(project)) {
            Some(path) => resolve(project, &path),
            None => stored.to_path_buf(),
        };
    }
    resolve(project, stored)
}

/// How `path` is written to `project.json`: relative inside the project; for
/// a path outside it, a path variable whose folder is at least as deep as the
/// folder the path shares with the project (`$USER_HOME$/.pyenv/…` rather
/// than `../../.pyenv/…`), else [`relativize`].
fn store_path(
    vars: &rustpy_settings::path_variables::PathVariables,
    project: &Path,
    path: &Path,
) -> PathBuf {
    if path.starts_with(project)
        || rustpy_settings::path_variables::leading_variable(&path.to_string_lossy()).is_some()
    {
        return relativize(project, path);
    }
    if let Some((collapsed, depth)) = vars.collapse(path, None) {
        let shared = project
            .components()
            .zip(path.components())
            .take_while(|(a, b)| a == b)
            .filter(|(a, _)| matches!(a, Component::Normal(_)))
            .count();
        if depth >= shared {
            return PathBuf::from(collapsed);
        }
    }
    relativize(project, path)
}

/// Path variables (`$NAME$`) that `.rustpy/project.json` and the shared run
/// configurations use but the IDE does not define (nor ignores); `other_macro`
/// names `$…$` macros of run configurations that are not path variables.
/// Sorted, without duplicates; unreadable files contribute nothing.
pub fn undefined_path_variables(project: &Path, other_macro: &dyn Fn(&str) -> bool) -> Vec<String> {
    let vars = rustpy_settings::path_variables::active();
    let mut out = Vec::new();
    for file in [
        ProjectModel::path_for(project),
        crate::config::RunConfigStore::path_for(project),
    ] {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        out.extend(vars.undefined_in_json(&value, other_macro));
    }
    out.sort();
    out.dedup();
    out
}

/// `stored` (relative to `project`, or absolute) as an absolute, lexically
/// normalized path. Relative paths are split on `/` and `\` and joined part by
/// part: a Windows verbatim project path (`\\?\C:\…`, from `canonicalize`)
/// does not treat `/` as a separator.
fn resolve(project: &Path, stored: &Path) -> PathBuf {
    if stored.is_absolute() {
        let mut out = PathBuf::new();
        for c in stored.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        return out;
    }
    let mut out = project.to_path_buf();
    for part in stored.to_string_lossy().split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            name => out.push(name),
        }
    }
    out
}

/// `path` relative to `project` (`.` for the project itself, `../x` for a
/// sibling); absolute when the two only share the file-system root.
fn relativize(project: &Path, path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix(project) {
        let s = rest.to_string_lossy().replace('\\', "/");
        return PathBuf::from(if s.is_empty() { ".".to_string() } else { s });
    }
    let p: Vec<Component> = project.components().collect();
    let t: Vec<Component> = path.components().collect();
    let common = p.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let shares_dir = p[..common]
        .iter()
        .any(|c| matches!(c, Component::Normal(_)));
    if !shares_dir {
        return path.to_path_buf();
    }
    let mut parts: Vec<String> = vec!["..".to_string(); p.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    PathBuf::from(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_projects_keep_independent_sdk_and_language_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("app");
        let attached = directory.path().join("service");
        std::fs::create_dir_all(&project).unwrap();
        let child = ProjectModel {
            sdk: Some(directory.path().join("python-service")),
            language_level: Some((3, 10)),
            ..Default::default()
        };
        child.save(&attached).unwrap();
        let mut model = ProjectModel {
            sdk: Some(directory.path().join("python-app")),
            language_level: Some((3, 13)),
            ..Default::default()
        };
        model.add_content_root(&attached);
        model.save(&project).unwrap();
        let loaded = ProjectModel::try_load(&project).unwrap();
        assert_eq!(
            loaded.language_level_for(&attached.join("main.py")),
            Some((3, 10))
        );
        assert_eq!(
            loaded.sdk_for(&attached.join("main.py")),
            child.sdk.as_deref()
        );
        assert_eq!(
            loaded.language_level_for(&project.join("main.py")),
            Some((3, 13))
        );
        assert_eq!(
            loaded.sdk_for(&project.join("main.py")),
            model.sdk.as_deref()
        );
        model.remove_content_root(&attached);
        assert!(model.modules.is_empty());
    }

    #[test]
    fn project_display_names_are_local_and_resettable() {
        let directory = tempfile::tempdir().unwrap();
        let fallback = ProjectModel::display_name(directory.path());
        ProjectModel::save_display_name(directory.path(), " Team workspace ").unwrap();
        assert_eq!(
            ProjectModel::display_name(directory.path()),
            "Team workspace"
        );
        assert!(ProjectModel::save_display_name(directory.path(), "first\nsecond").is_err());
        assert_eq!(
            ProjectModel::display_name(directory.path()),
            "Team workspace"
        );
        ProjectModel::save_display_name(directory.path(), "").unwrap();
        assert_eq!(ProjectModel::display_name(directory.path()), fallback);
    }

    #[test]
    fn folder_edits_preserve_sdk_module_and_future_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = ProjectModel::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let metadata = serde_json::json!({
            "modules": [{"content_root": ".", "name": "web", "sdk": "/opt/python", "language_level": null}],
            "language_level": [3, 13],
            "future": {"enabled": true},
            "source_roots": ["old"]
        });
        std::fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let mut model = ProjectModel::try_load(dir.path()).unwrap();
        model.mark(&dir.path().join("src"), Some(RootKind::Sources));
        model.save(dir.path()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for field in ["modules", "language_level", "future"] {
            assert_eq!(saved[field], metadata[field]);
        }
        assert_eq!(saved["source_roots"], serde_json::json!(["old", "src"]));
        assert!(dir.path().join(".rustpy/.gitignore").is_file());
    }

    #[test]
    fn excluded_sources_and_attached_roots_do_not_enter_pythonpath() {
        let root = Path::new("/work/app");
        let model = ProjectModel {
            content_roots: vec![PathBuf::from("/work/shared")],
            source_roots: vec![root.join("vendor/src"), root.join("src")],
            test_roots: vec![root.join("vendor/tests")],
            excluded: vec![root.join("vendor"), PathBuf::from("/work/shared")],
            ..Default::default()
        };
        assert_eq!(
            model.import_roots(root),
            vec![root.to_path_buf(), root.join("src")]
        );
    }

    #[test]
    fn marks_are_exclusive_and_saved_relative() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut m = ProjectModel::default();
        m.mark(&root.join("src"), Some(RootKind::Sources));
        m.mark(&root.join("tests"), Some(RootKind::Tests));
        m.mark(&root.join("build"), Some(RootKind::Excluded));
        m.mark(&root.join("tests"), Some(RootKind::Resources));
        assert_eq!(
            m.kind_of(&root.join("tests")),
            Some(RootKind::Resources),
            "one mark per folder"
        );
        assert!(m.test_roots.is_empty());
        assert!(m.add_content_root(&dir.path().join("shared")));
        assert!(!m.add_content_root(&dir.path().join("shared")));
        m.mark(&dir.path().join("shared/lib"), Some(RootKind::Sources));
        m.save(&root).unwrap();

        let text = std::fs::read_to_string(ProjectModel::path_for(&root)).unwrap();
        assert!(text.contains("\"src\""), "{text}");
        assert!(
            text.contains("\"../shared\""),
            "siblings are relative: {text}"
        );
        assert!(
            !text.contains(&root.display().to_string()),
            "no absolute paths: {text}"
        );
        let ignore = std::fs::read_to_string(root.join(".rustpy/.gitignore")).unwrap();
        assert!(ignore.contains("!project.json"));
        for shared in SHARED_FILES {
            assert!(
                ignore.lines().any(|l| l == format!("!{shared}")),
                "{shared} stays shareable: {ignore}"
            );
        }

        let loaded = ProjectModel::load(&root);
        assert_eq!(loaded, m);
        assert_eq!(
            loaded.import_roots(&root),
            vec![
                root.clone(),
                dir.path().join("shared"),
                root.join("src"),
                dir.path().join("shared/lib")
            ]
        );
        m.remove_content_root(&dir.path().join("shared"));
        assert_eq!(
            m.source_roots,
            vec![root.join("src")],
            "marks inside a removed content root go too"
        );
        m.mark(&root.join("src"), None);
        assert_eq!(m.kind_of(&root.join("src")), None);
    }

    #[test]
    fn roots_from_workspace_json_are_used_without_writing_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ws = Workspace {
            source_roots: vec![root.join("src")],
            excluded_dirs: vec![root.join("vendor")],
            ..Default::default()
        };
        ws.save(&root).unwrap();
        assert!(!ProjectModel::exists(&root));
        let m = ProjectModel::try_load(&root).unwrap();
        assert_eq!(m.source_roots, vec![root.join("src")]);
        assert_eq!(m.excluded, vec![root.join("vendor")]);
        // Opening an older project adds nothing to its folder (or repository).
        assert!(
            !ProjectModel::exists(&root),
            "no project.json until the structure is changed"
        );
        assert!(!root.join(".rustpy/.gitignore").exists());
        // The first change writes project.json; workspace.json then stops listing the roots.
        m.save(&root).unwrap();
        assert!(ProjectModel::exists(&root));
        let ws = Workspace {
            source_roots: Vec::new(),
            excluded_dirs: Vec::new(),
            ..Workspace::load(&root)
        };
        ws.save(&root).unwrap();
        assert!(!std::fs::read_to_string(Workspace::path_for(&root))
            .unwrap()
            .contains("excluded_dirs"));
        assert_eq!(ProjectModel::load(&root), m);
        // A project without either file has an empty model and writes nothing.
        let empty = tempfile::tempdir().unwrap();
        assert!(ProjectModel::try_load(empty.path()).unwrap().is_empty());
        assert!(!ProjectModel::exists(empty.path()));
    }

    #[test]
    fn relative_paths_resolve_lexically() {
        assert_eq!(
            resolve(Path::new("/w/proj"), Path::new("../shared/./lib")),
            PathBuf::from("/w/shared/lib")
        );
        assert_eq!(
            resolve(Path::new("/w/proj"), Path::new("/abs/dir")),
            PathBuf::from("/abs/dir")
        );
        assert_eq!(
            relativize(Path::new("/w/proj"), Path::new("/w/proj")),
            PathBuf::from(".")
        );
        assert_eq!(
            relativize(Path::new("/w/proj"), Path::new("/w/proj/a/b")),
            PathBuf::from("a/b")
        );
        assert_eq!(
            relativize(Path::new("/w/proj"), Path::new("/w/other/x")),
            PathBuf::from("../other/x")
        );
        assert_eq!(
            relativize(Path::new("/w/proj"), Path::new("/opt/lib")),
            PathBuf::from("/opt/lib"),
            "only the root in common"
        );
        // Files written on Windows (or edited by hand) may use backslashes; parts are joined one by one.
        assert_eq!(
            resolve(Path::new("/w/proj"), Path::new("..\\shared\\lib")),
            PathBuf::from("/w/shared/lib")
        );
        assert_eq!(
            resolve(Path::new("/w/proj"), Path::new("src//pkg/")),
            PathBuf::from("/w/proj/src/pkg")
        );
        assert_eq!(
            resolve(Path::new("/w/proj"), Path::new(".")),
            PathBuf::from("/w/proj")
        );
    }

    #[test]
    fn unreadable_project_files_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".rustpy")).unwrap();
        std::fs::write(
            ProjectModel::path_for(&root),
            "<<<<<<< HEAD\n{\"source_roots\": [\"src\"]}\n=======\n",
        )
        .unwrap();
        let err = ProjectModel::try_load(&root).unwrap_err();
        assert!(err.contains("project.json is not valid"), "{err}");
        assert!(ProjectModel::load(&root).is_empty());
        assert!(
            std::fs::read_to_string(ProjectModel::path_for(&root))
                .unwrap()
                .starts_with("<<<<<<<"),
            "the file is left for the user to fix"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_never_replaced() {
        // Both sides of a merge conflict, as a merge leaves them.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".rustpy")).unwrap();
        let conflicted = "<<<<<<< HEAD\n{\"source_roots\": [\"src\"], \"content_roots\": [\"../a\"]}\n=======\n{\"excluded\": [\"vendor\"], \"content_roots\": [\"../b\"]}\n>>>>>>> theirs\n";
        std::fs::write(ProjectModel::path_for(&root), conflicted).unwrap();
        assert!(ProjectModel::check_replaceable(&root)
            .unwrap_err()
            .contains("left unchanged"));
        let mut m = ProjectModel::default();
        m.mark(&root.join("src"), Some(RootKind::Tests));
        let err = m.save(&root).unwrap_err().to_string();
        assert!(err.contains("not valid"), "{err}");
        assert_eq!(
            std::fs::read_to_string(ProjectModel::path_for(&root)).unwrap(),
            conflicted,
            "both sides of the conflict are kept"
        );
        // Once fixed, saving works again.
        std::fs::write(
            ProjectModel::path_for(&root),
            "{\"source_roots\": [\"src\"]}",
        )
        .unwrap();
        assert!(ProjectModel::check_replaceable(&root).is_ok());
        m.save(&root).unwrap();
        assert_eq!(ProjectModel::load(&root).test_roots, vec![root.join("src")]);
    }

    #[test]
    fn content_roots_that_contain_or_overlap_the_project_are_ignored_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(dir.path()).unwrap();
        let root = base.join("proj");
        for d in ["proj/src", "shared/lib", "other"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::create_dir_all(root.join(".rustpy")).unwrap();
        // A committed (or hand-edited) file: the parent folder, the file-system
        // root, the project itself, a folder inside it and two overlapping roots.
        std::fs::write(ProjectModel::path_for(&root), r#"{"content_roots": ["..", "/", ".", "src", "../shared", "../shared/lib", "../other"]}"#).unwrap();
        let (m, problems) = ProjectModel::try_load_reporting(&root).unwrap();
        assert_eq!(
            m.content_roots,
            vec![base.join("other"), base.join("shared")],
            "{problems:?}"
        );
        assert_eq!(problems.len(), 5, "{problems:?}");
        assert_eq!(
            problems
                .iter()
                .filter(|p| p.contains("contains the project directory"))
                .count(),
            2,
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("overlaps the content root")),
            "{problems:?}"
        );
        assert_eq!(
            ProjectModel::try_load(&root).unwrap(),
            m,
            "every load leaves them out"
        );
        assert_eq!(
            content_root_problem(&root, &base.join("other"), &[base.join("shared")]),
            None
        );
        assert!(content_root_problem(&root, Path::new("/"), &[]).is_some());
        // A symbolic link to an ancestor is caught too.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&base, base.join("other/up")).unwrap();
            assert!(content_root_problem(&root, &base.join("other/up"), &[])
                .unwrap()
                .contains("contains the project"));
        }
    }
}

#[cfg(test)]
mod checked_workspace_migration_tests {
    use super::*;
    #[test]
    fn corrupt_legacy_workspace_cannot_silently_default_project_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken legacy roots").unwrap();
        let error = ProjectModel::try_load_reporting(dir.path()).unwrap_err();
        assert!(error.contains("workspace.json"));
        assert_eq!(std::fs::read(&path).unwrap(), b"broken legacy roots");
        assert!(!ProjectModel::path_for(dir.path()).exists());
    }
}

#[cfg(test)]
mod path_variable_tests {
    use super::*;
    use rustpy_settings::path_variables::{PathVariable, PathVariables};

    #[test]
    fn paths_outside_the_project_round_trip_through_path_variables() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let project = base.join("work").join("app");
        let vars = PathVariables {
            variables: vec![PathVariable {
                name: "PV_SDKS".into(),
                path: base.join("sdks"),
            }],
            ignored: Vec::new(),
        };
        let interpreter = base.join("sdks").join("py311").join("bin").join("python");
        let stored = store_path(&vars, &project, &interpreter);
        assert_eq!(stored, PathBuf::from("$PV_SDKS$/py311/bin/python"));
        assert_eq!(resolve_stored(&vars, &project, &stored), interpreter);
        // A sibling below the variable's folder level stays relative.
        let sibling = base.join("work").join("lib");
        assert_eq!(
            store_path(&vars, &project, &sibling),
            PathBuf::from("../lib")
        );
        // Inside the project: relative as before.
        assert_eq!(
            store_path(&vars, &project, &project.join("src")),
            PathBuf::from("src")
        );
        // Undefined: kept verbatim in both directions.
        let undefined = PathBuf::from("$PV_UNDEFINED$/x");
        assert_eq!(resolve_stored(&vars, &project, &undefined), undefined);
        assert_eq!(store_path(&vars, &project, &undefined), undefined);
    }

    #[test]
    fn undefined_path_variables_survive_load_and_save_and_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("app");
        std::fs::create_dir_all(project.join(PROJECT_DIR_NAME)).unwrap();
        std::fs::write(
            ProjectModel::path_for(&project),
            r#"{"sdk":"$PV_NOT_DEFINED$/bin/python","source_roots":["src"]}"#,
        )
        .unwrap();
        let mut model = ProjectModel::try_load(&project).unwrap();
        assert_eq!(
            model.sdk,
            Some(PathBuf::from("$PV_NOT_DEFINED$/bin/python"))
        );
        assert_eq!(model.source_roots, vec![project.join("src")]);
        model.test_roots.push(project.join("tests"));
        model.save(&project).unwrap();
        let text = std::fs::read_to_string(ProjectModel::path_for(&project)).unwrap();
        assert!(text.contains("$PV_NOT_DEFINED$/bin/python"), "{text}");
        assert_eq!(
            undefined_path_variables(&project, &|_| false),
            vec!["PV_NOT_DEFINED".to_string()]
        );
        assert!(undefined_path_variables(&project, &|name| name == "PV_NOT_DEFINED").is_empty());
    }
}

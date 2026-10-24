//! Selected settings snapshots and three-way conflict previews. This format carries
//! preferences and text configuration only: machine state and credentials never sync.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
const FORMAT: &str = "rustpy-settings-sync";
const LIMIT: u64 = 32 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Selection {
    pub appearance: bool,
    pub editor: bool,
    pub keymaps: bool,
    pub templates: bool,
    pub inspections: bool,
    pub plugins: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            appearance: true,
            editor: true,
            keymaps: true,
            templates: true,
            inspections: true,
            plugins: true,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plugin {
    pub id: String,
    pub version: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: String,
    pub version: u32,
    pub entries: BTreeMap<String, Value>,
    #[serde(default)]
    pub plugins: Vec<Plugin>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            format: FORMAT.into(),
            version: 1,
            entries: BTreeMap::new(),
            plugins: Vec::new(),
        }
    }
}
impl Snapshot {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            text.len() <= LIMIT as usize,
            "Settings snapshot exceeds 32 MiB"
        );
        let value: Self = serde_json::from_str(text)?;
        value.validate()?;
        Ok(value)
    }
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            std::fs::metadata(path)?.len() <= LIMIT,
            "Settings snapshot exceeds 32 MiB"
        );
        Self::parse(&std::fs::read_to_string(path)?)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            serde_json::to_vec(self)?.len() <= LIMIT as usize,
            "Selected settings exceed 32 MiB"
        );
        anyhow::ensure!(
            self.format == FORMAT && self.version == 1,
            "Unsupported settings sync format"
        );
        anyhow::ensure!(
            self.entries.len() <= 4096 && self.plugins.len() <= 512,
            "Too many settings entries"
        );
        for (key, value) in &self.entries {
            anyhow::ensure!(allowed(key), "Unshareable settings entry: {key}");
            anyhow::ensure!(
                serde_json::to_vec(value)?.len() <= 5 * 1024 * 1024,
                "Settings entry exceeds 5 MiB: {key}"
            );
            if key.starts_with("file/") {
                anyhow::ensure!(value.is_string(), "Text settings file expected");
            }
        }
        let mut plugin_ids = BTreeSet::new();
        for plugin in &self.plugins {
            anyhow::ensure!(plugin_ids.insert(&plugin.id), "Duplicate plugin descriptor");
            anyhow::ensure!(
                valid_id(&plugin.id) && plugin.version.len() <= 128,
                "Invalid plugin descriptor"
            );
        }
        Ok(())
    }
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        self.validate()?;
        write_text(path, &serde_json::to_string_pretty(self)?)
    }
}
fn write_text(path: &Path, text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        text.len() <= LIMIT as usize,
        "Settings export exceeds 32 MiB"
    );
    reject_symlinks(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = path.with_extension("json.new");
    reject_symlinks(&staging)?;
    std::fs::write(&staging, text)?;
    std::fs::rename(staging, path)?;
    Ok(())
}
pub fn write_bundle(snapshot: &Snapshot, path: &Path) -> anyhow::Result<()> {
    write_text(path, &bundle(snapshot)?)
}
fn fields(selection: &Selection) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if selection.appearance {
        fields.extend([
            "appearance",
            "notifications",
            "chrome",
            "accessibility",
            "menus",
            "native_ui",
        ]);
    }
    if selection.editor {
        fields.extend([
            "editor",
            "completion",
            "code_style",
            "code_style_schemes",
            "todo",
            "refactoring",
            "docstring_format",
            "semantic_highlighting",
            "disabled_intentions",
        ]);
    }
    if selection.keymaps {
        fields.extend([
            "keymap",
            "custom_keymaps",
            "custom_shortcuts",
            "keymap_overrides",
            "keymap_quick_lists",
            "quick_list",
            "macro_shortcuts",
            "macros",
        ]);
    }
    if selection.inspections {
        fields.push("inspections");
    }
    if selection.plugins {
        fields.push("plugins");
    }
    fields
}
fn allowed(key: &str) -> bool {
    if let Some(field) = key.strip_prefix("settings/") {
        return fields(&Selection::default()).contains(&field);
    }
    if let Some(path) = key.strip_prefix("file/") {
        let relative = Path::new(path);
        if path.contains('\\')
            || path.contains(':')
            || path.chars().any(char::is_control)
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return false;
        }
        let first = path.split('/').next().unwrap_or_default();
        return matches!(first, "colors" | "ui-themes" | "keymaps") && path.ends_with(".json")
            || matches!(
                path,
                "live_templates.json"
                    | "fileTemplates.json"
                    | "structural-templates.json"
                    | "dialog-policies.json"
                    | "new-project-preferences.json"
                    | "new-project-run-templates.json"
            );
    }
    false
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn reject_symlinks(path: &Path) -> anyhow::Result<()> {
    for ancestor in path.ancestors() {
        if let Ok(metadata) = std::fs::symlink_metadata(ancestor) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Settings sync refuses symlink paths: {}",
                ancestor.display()
            );
        }
    }
    Ok(())
}
pub fn capture(
    settings: &crate::Settings,
    config: &Path,
    selection: &Selection,
    plugins: Vec<Plugin>,
    cancel: &impl Fn() -> bool,
) -> anyhow::Result<Snapshot> {
    let mut snapshot = Snapshot::default();
    let object = serde_json::to_value(settings)?;
    for field in fields(selection) {
        if let Some(value) = object.get(field) {
            snapshot.entries.insert(
                format!("settings/{field}"),
                if field == "plugins" {
                    serde_json::to_value(crate::plugins::Settings {
                        disabled_builtins: settings.plugins.disabled_builtins.clone(),
                        vim_enabled: settings.plugins.vim_enabled,
                        ..Default::default()
                    })?
                } else if field == "native_ui" {
                    let mut portable = settings.native_ui.clone();
                    portable.zenity_program = None;
                    portable.kdialog_program = None;
                    serde_json::to_value(portable)?
                } else if field == "menus" {
                    let mut portable = settings.menus.clone();
                    portable.appearance.background.path.clear();
                    serde_json::to_value(portable)?
                } else {
                    value.clone()
                },
            );
        }
    }
    let mut files = Vec::new();
    if selection.appearance {
        if config.join("dialog-policies.json").is_file() {
            files.push(config.join("dialog-policies.json"));
        }
        for name in ["colors", "ui-themes"] {
            collect(config, &config.join(name), 0, &mut files, cancel)?;
        }
    }
    if selection.keymaps {
        collect(config, &config.join("keymaps"), 0, &mut files, cancel)?;
    }
    if selection.templates {
        for name in [
            "live_templates.json",
            "fileTemplates.json",
            "structural-templates.json",
            "new-project-preferences.json",
            "new-project-run-templates.json",
        ] {
            let path = config.join(name);
            if path.is_file() {
                files.push(path);
            }
        }
    }
    let mut bytes = 0;
    for path in files {
        anyhow::ensure!(!cancel(), "Settings capture cancelled");
        reject_symlinks(&path)?;
        let metadata = std::fs::metadata(&path)?;
        anyhow::ensure!(
            metadata.len() <= 5 * 1024 * 1024,
            "Settings file is too large"
        );
        bytes += metadata.len();
        anyhow::ensure!(bytes <= LIMIT, "Selected settings exceed 32 MiB");
        let relative = path
            .strip_prefix(config)?
            .to_string_lossy()
            .replace('\\', "/");
        let key = format!("file/{relative}");
        if allowed(&key) {
            snapshot
                .entries
                .insert(key, Value::String(std::fs::read_to_string(path)?));
        }
    }
    if selection.plugins {
        snapshot.plugins = plugins;
    }
    snapshot.validate()?;
    Ok(snapshot)
}
fn collect(
    root: &Path,
    path: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancel: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(!cancel(), "Settings capture cancelled");
    anyhow::ensure!(
        depth <= 8 && out.len() <= 2048,
        "Settings directory limit exceeded"
    );
    if !path.exists() {
        return Ok(());
    }
    reject_symlinks(path)?;
    if path.is_file() {
        if path.extension().is_some_and(|s| s == "json") {
            out.push(path.into());
        }
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        anyhow::ensure!(
            child.starts_with(root),
            "Settings path escaped configuration directory"
        );
        collect(root, &child, depth + 1, out, cancel)?;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Local,
    Remote,
}
#[derive(Clone, Debug)]
pub struct Change {
    pub key: String,
    pub base: Option<Value>,
    pub local: Option<Value>,
    pub remote: Option<Value>,
    pub conflict: bool,
    pub choice: Choice,
    pub resolved: bool,
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub local: Snapshot,
    pub remote: Snapshot,
    pub changes: Vec<Change>,
}
impl Preview {
    pub fn new(base: &Snapshot, local: Snapshot, remote: Snapshot) -> anyhow::Result<Self> {
        local.validate()?;
        remote.validate()?;
        base.validate()?;
        let mut left = local.entries.clone();
        let mut right = remote.entries.clone();
        let mut old = base.entries.clone();
        left.insert("plugins".into(), serde_json::to_value(&local.plugins)?);
        right.insert("plugins".into(), serde_json::to_value(&remote.plugins)?);
        old.insert("plugins".into(), serde_json::to_value(&base.plugins)?);
        let keys = left
            .keys()
            .chain(right.keys())
            .chain(old.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let changes = keys
            .into_iter()
            .filter_map(|key| {
                let local = left.get(&key).cloned();
                let remote = right.get(&key).cloned();
                let base = old.get(&key).cloned();
                if local == remote {
                    return None;
                }
                let conflict = local != base && remote != base;
                let choice = if remote == base {
                    Choice::Local
                } else {
                    Choice::Remote
                };
                Some(Change {
                    key,
                    base,
                    local,
                    remote,
                    conflict,
                    choice,
                    resolved: !conflict,
                })
            })
            .collect();
        Ok(Self {
            local,
            remote,
            changes,
        })
    }
    pub fn merged(&self) -> anyhow::Result<Snapshot> {
        anyhow::ensure!(
            self.changes.iter().all(|change| change.resolved),
            "Resolve every marked conflict before applying or publishing"
        );
        let mut snapshot = self.local.clone();
        for change in &self.changes {
            let value = match change.choice {
                Choice::Local => &change.local,
                Choice::Remote => &change.remote,
            };
            if change.key == "plugins" {
                snapshot.plugins = value
                    .clone()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
            } else if let Some(value) = value {
                snapshot.entries.insert(change.key.clone(), value.clone());
            } else {
                snapshot.entries.remove(&change.key);
            }
        }
        snapshot.validate()?;
        Ok(snapshot)
    }
}
/// Apply only selected shareable keys into the current Settings. Every other field
/// (sessions, trust, paths, credentials, interpreters, telemetry and update state) survives.
pub fn apply(
    config: &Path,
    snapshot: &Snapshot,
    expected: &Snapshot,
    cancel: &impl Fn() -> bool,
) -> anyhow::Result<Vec<String>> {
    snapshot.validate()?;
    expected.validate()?;
    let settings_path = config.join("settings.json");
    let (mut settings, mut baseline, error) = crate::persist::load_from(&settings_path);
    anyhow::ensure!(
        error.is_none(),
        "Cannot synchronize an invalid local settings file"
    );
    let current = capture(
        &settings,
        config,
        &Selection::default(),
        expected.plugins.clone(),
        cancel,
    )?;
    for key in expected
        .entries
        .keys()
        .chain(snapshot.entries.keys())
        .collect::<BTreeSet<_>>()
    {
        anyhow::ensure!(
            current.entries.get(key) == expected.entries.get(key),
            "Local settings changed since preview: {key}; preview again"
        );
    }
    let mut object = serde_json::to_value(&settings)?;
    let defaults = serde_json::to_value(crate::Settings::default())?;
    let mut files = BTreeMap::<String, Option<String>>::new();
    for key in expected
        .entries
        .keys()
        .chain(snapshot.entries.keys())
        .collect::<BTreeSet<_>>()
    {
        if let Some(field) = key.strip_prefix("settings/") {
            object[field] = snapshot
                .entries
                .get(key)
                .cloned()
                .unwrap_or_else(|| defaults[field].clone());
        } else if let Some(relative) = key.strip_prefix("file/") {
            let destination = config.join(relative);
            reject_symlinks(&destination)?;
            files.insert(
                relative.into(),
                snapshot
                    .entries
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    let local_native_paths = (
        settings.native_ui.zenity_program.clone(),
        settings.native_ui.kdialog_program.clone(),
    );
    let local_plugin_authority = settings.plugins.clone();
    let local_background_path = settings.menus.appearance.background.path.clone();
    settings = serde_json::from_value(object)?;
    (
        settings.native_ui.zenity_program,
        settings.native_ui.kdialog_program,
    ) = local_native_paths;
    settings.menus.appearance.background.path = local_background_path;
    let portable_plugin_preferences = settings.plugins.clone();
    settings.plugins = local_plugin_authority;
    // A remote bundle cannot grant code execution, trust a publisher, weaken
    // signature checks or choose an executable on the receiving machine.
    settings.plugins.disabled_builtins = portable_plugin_preferences.disabled_builtins;
    settings.plugins.vim_enabled = portable_plugin_preferences.vim_enabled;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let backup = config.join("backups").join(format!(
        "settings-sync-{}-{}-{}",
        crate::tasks::now_secs(),
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    reject_symlinks(&backup)?;
    std::fs::create_dir_all(&backup)?;
    if settings_path.is_file() {
        std::fs::copy(&settings_path, backup.join("settings.json"))?;
    }
    let mut originals = Vec::new();
    for relative in files.keys() {
        anyhow::ensure!(!cancel(), "Settings import cancelled");
        let destination = config.join(relative);
        let saved = backup.join(relative);
        let existed = destination.is_file();
        if existed {
            if let Some(parent) = saved.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&destination, &saved)?;
        }
        originals.push((destination, saved, existed));
    }
    let result = (|| -> anyhow::Result<Vec<String>> {
        let mut imported = Vec::new();
        for (relative, text) in &files {
            anyhow::ensure!(!cancel(), "Settings import cancelled");
            let destination = config.join(relative);
            reject_symlinks(&destination)?;
            if let Some(text) = text {
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let staging = destination.with_extension("json.sync-new");
                reject_symlinks(&staging)?;
                std::fs::write(&staging, text)?;
                std::fs::rename(staging, &destination)?;
            } else if destination.is_file() {
                std::fs::remove_file(&destination)?;
            }
            imported.push(relative.clone());
        }
        anyhow::ensure!(!cancel(), "Settings import cancelled");
        crate::persist::save_to(&settings_path, &settings, &mut baseline)?;
        Ok(imported)
    })();
    if result.is_err() {
        for (destination, saved, existed) in originals.into_iter().rev() {
            if existed {
                let _ = std::fs::copy(saved, &destination);
            } else {
                let _ = std::fs::remove_file(&destination);
            }
            let _ = std::fs::remove_file(destination.with_extension("json.sync-new"));
        }
    }
    result
}
/// Export in the existing portable settings bundle format, after merging only
/// shareable settings into a partial object. The selection-only marker makes
/// the normal bundle importer preserve every unselected and machine-local field.
pub fn bundle(snapshot: &Snapshot) -> anyhow::Result<String> {
    snapshot.validate()?;
    let mut settings = serde_json::json!({});
    let mut files = serde_json::Map::new();
    for (key, value) in &snapshot.entries {
        if let Some(field) = key.strip_prefix("settings/") {
            settings[field] = value.clone();
        } else if let Some(path) = key.strip_prefix("file/") {
            files.insert(path.into(), value.clone());
        }
    }
    files.insert(
        "settings.json".into(),
        Value::String(serde_json::to_string_pretty(&settings)?),
    );
    Ok(serde_json::to_string_pretty(
        &serde_json::json!({"format":"rustpy-settings","version":1,"selection_only":true,"files":files,"plugin_choices":snapshot.plugins}),
    )?)
}
pub fn from_bundle(text: &str) -> anyhow::Result<Snapshot> {
    if let Ok(snapshot) = Snapshot::parse(text) {
        return Ok(snapshot);
    }
    anyhow::ensure!(
        text.len() <= LIMIT as usize,
        "Settings bundle exceeds limit"
    );
    let value: Value = serde_json::from_str(text)?;
    anyhow::ensure!(
        value["format"] == "rustpy-settings" && value["version"] == 1,
        "Unsupported settings bundle"
    );
    let mut snapshot = Snapshot::default();
    for (path, content) in value["files"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Bundle files are missing"))?
    {
        if path == "settings.json" {
            let settings: Value = serde_json::from_str(content.as_str().unwrap_or("{}"))?;
            for field in fields(&Selection::default()) {
                if let Some(value) = settings.get(field) {
                    snapshot
                        .entries
                        .insert(format!("settings/{field}"), value.clone());
                }
            }
        } else {
            let key = format!("file/{path}");
            if allowed(&key) && content.is_string() {
                snapshot.entries.insert(key, content.clone());
            }
        }
    }
    if let Some(plugins) = value.get("plugin_choices") {
        snapshot.plugins = serde_json::from_value(plugins.clone())?;
    }
    snapshot.validate()?;
    Ok(snapshot)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_plugin_preferences_cannot_replace_local_execution_authority() {
        let directory = tempfile::tempdir().unwrap();
        let mut local = crate::Settings::default();
        local.plugins.require_signatures = true;
        local
            .plugins
            .runtime_grants
            .insert("local-plugin".into(), "locally-admitted-digest".into());
        local
            .plugins
            .publishers
            .insert("local-publisher".into(), "local-public-key".into());
        local.plugins.scripting_interpreter = "/local/approved-python".into();
        local.menus.appearance.background.path = "/local/chosen-background.png".into();
        std::fs::write(
            directory.path().join("settings.json"),
            serde_json::to_vec(&local).unwrap(),
        )
        .unwrap();
        let expected = capture(
            &local,
            directory.path(),
            &Selection::default(),
            vec![],
            &|| false,
        )
        .unwrap();
        let exported: crate::plugins::Settings =
            serde_json::from_value(expected.entries["settings/plugins"].clone()).unwrap();
        assert!(exported.runtime_grants.is_empty());
        assert!(exported.publishers.is_empty());
        assert!(exported.scripting_interpreter.is_empty());
        let exported_menus: crate::menus::Settings =
            serde_json::from_value(expected.entries["settings/menus"].clone()).unwrap();
        assert!(exported_menus.appearance.background.path.is_empty());
        let mut incoming = crate::plugins::Settings::default();
        incoming
            .runtime_grants
            .insert("remote-plugin".into(), "unadmitted-digest".into());
        incoming
            .publishers
            .insert("remote-publisher".into(), "remote-public-key".into());
        incoming.scripting_interpreter = "/remote/unapproved-python".into();
        incoming.disabled_builtins.insert("database".into());
        incoming.vim_enabled = true;
        let mut hostile = expected.clone();
        hostile.entries.insert(
            "settings/plugins".into(),
            serde_json::to_value(&incoming).unwrap(),
        );
        let mut incoming_menus = crate::menus::Settings::default();
        incoming_menus.appearance.background.path = "/remote/unapproved-background.png".into();
        incoming_menus.appearance.background.opacity = 0.45;
        hostile.entries.insert(
            "settings/menus".into(),
            serde_json::to_value(&incoming_menus).unwrap(),
        );
        apply(directory.path(), &hostile, &expected, &|| false).unwrap();
        let saved: crate::Settings =
            serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(saved.plugins.runtime_grants, local.plugins.runtime_grants);
        assert_eq!(saved.plugins.publishers, local.plugins.publishers);
        assert!(saved.plugins.require_signatures);
        assert_eq!(
            saved.plugins.scripting_interpreter,
            local.plugins.scripting_interpreter
        );
        assert!(!saved.plugins.builtin_enabled("database"));
        assert!(saved.plugins.vim_enabled);
        assert_eq!(
            saved.menus.appearance.background.path,
            local.menus.appearance.background.path
        );
        assert_eq!(saved.menus.appearance.background.opacity, 0.45);
        let mut live = local.clone();
        let remote = crate::Settings {
            plugins: incoming,
            menus: incoming_menus,
            ..Default::default()
        };
        copy_selected_preferences(&mut live, &remote, &Selection::default());
        assert_eq!(
            live.plugins, saved.plugins,
            "in-memory application preserves the same local authorities as disk import"
        );
        assert_eq!(live.menus, saved.menus);
    }
    #[test]
    fn import_is_bound_to_the_selected_configuration_and_preserves_local_fields() {
        let directory = tempfile::tempdir().unwrap();
        let settings = crate::Settings {
            recent_projects: vec![PathBuf::from("/private/work")],
            docker_path: "/machine/docker".into(),
            ..Default::default()
        };
        std::fs::write(
            directory.path().join("settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let expected = capture(
            &settings,
            directory.path(),
            &Selection::default(),
            vec![],
            &|| false,
        )
        .unwrap();
        let mut incoming = expected.clone();
        incoming
            .entries
            .insert("settings/keymap".into(), Value::String("Emacs".into()));
        apply(directory.path(), &incoming, &expected, &|| false).unwrap();
        let saved: crate::Settings =
            serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(saved.keymap, "Emacs");
        assert_eq!(saved.recent_projects, settings.recent_projects);
        assert_eq!(saved.docker_path, "/machine/docker");
    }
    #[test]
    fn cancellation_rolls_back_files_already_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let colors = directory.path().join("colors");
        std::fs::create_dir(&colors).unwrap();
        std::fs::write(colors.join("a.json"), "old-a").unwrap();
        std::fs::write(colors.join("b.json"), "old-b").unwrap();
        let settings = crate::Settings::default();
        std::fs::write(
            directory.path().join("settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let expected = capture(
            &settings,
            directory.path(),
            &Selection::default(),
            vec![],
            &|| false,
        )
        .unwrap();
        let mut incoming = expected.clone();
        incoming.entries.insert(
            "file/colors/a.json".into(),
            Value::String("incoming-a".into()),
        );
        incoming.entries.insert(
            "file/colors/b.json".into(),
            Value::String("incoming-b".into()),
        );
        let cancelled = || {
            std::fs::read_to_string(colors.join("a.json")).is_ok_and(|text| text == "incoming-a")
        };
        assert!(apply(directory.path(), &incoming, &expected, &cancelled).is_err());
        assert_eq!(
            std::fs::read_to_string(colors.join("a.json")).unwrap(),
            "old-a"
        );
        assert_eq!(
            std::fs::read_to_string(colors.join("b.json")).unwrap(),
            "old-b"
        );
    }
    #[test]
    fn applying_shared_fields_keeps_session_credentials_and_machine_state() {
        let mut current = crate::Settings::default();
        current.ai.api_key = "session-only-secret".into();
        current.recent_projects = vec!["/private/project".into()];
        current.docker_path = "/local/docker".into();
        current.default_project_directory = Some("/local/projects".into());
        let mut incoming = crate::Settings {
            default_project_directory: Some("/other-machine/projects".into()),
            ..Default::default()
        };
        incoming.editor.font_size = 19.0;
        incoming.ai.api_key = "must-not-replace".into();
        copy_selected_preferences(&mut current, &incoming, &Selection::default());
        assert_eq!(current.editor.font_size, 19.0);
        assert_eq!(current.ai.api_key, "session-only-secret");
        assert_eq!(current.docker_path, "/local/docker");
        assert_eq!(
            current.default_project_directory,
            Some(PathBuf::from("/local/projects"))
        );
        assert_eq!(
            current.recent_projects,
            vec![PathBuf::from("/private/project")]
        );
    }
    #[test]
    fn portable_bundle_contains_only_selected_fields_and_real_plugin_choices() {
        let mut snapshot = Snapshot::default();
        snapshot
            .entries
            .insert("settings/keymap".into(), Value::String("Emacs".into()));
        snapshot.plugins.push(Plugin {
            id: "example.theme".into(),
            version: "1.0.0".into(),
            enabled: false,
        });
        let text = bundle(&snapshot).unwrap();
        assert!(text.contains("selection_only"));
        assert!(!text.contains("http_proxy"));
        let restored = from_bundle(&text).unwrap();
        assert_eq!(restored.entries, snapshot.entries);
        assert_eq!(restored.plugins, snapshot.plugins);
    }
    #[test]
    fn cancellation_prevents_configuration_capture() {
        assert!(capture(
            &crate::Settings::default(),
            Path::new("/nonexistent/settings-sync-fixture"),
            &Selection::default(),
            vec![],
            &|| true
        )
        .is_err());
    }
    #[test]
    fn credentials_and_sessions_never_enter_snapshots() {
        let mut settings = crate::Settings::default();
        settings.ai.legacy_api_key = "secret-api-key".into();
        settings.recent_projects = vec!["/private/work".into()];
        let snapshot = capture(
            &settings,
            Path::new("/nonexistent/settings-sync-fixture"),
            &Selection::default(),
            vec![],
            &|| false,
        )
        .unwrap();
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("secret-api-key"));
        assert!(!json.contains("private/work"));
        assert!(snapshot.entries.contains_key("settings/keymap"));
    }
    #[test]
    fn both_sides_changed_requires_visible_conflict() {
        let mut base = Snapshot::default();
        base.entries
            .insert("settings/keymap".into(), Value::String("Default".into()));
        let mut local = base.clone();
        local
            .entries
            .insert("settings/keymap".into(), Value::String("Emacs".into()));
        let mut remote = base.clone();
        remote
            .entries
            .insert("settings/keymap".into(), Value::String("Vim".into()));
        let mut preview = Preview::new(&base, local, remote).unwrap();
        assert!(preview.changes[0].conflict);
        assert!(preview.merged().is_err());
        preview.changes[0].choice = Choice::Local;
        preview.changes[0].resolved = true;
        assert_eq!(
            preview.merged().unwrap().entries["settings/keymap"],
            "Emacs"
        );
    }
    #[test]
    fn traversal_and_local_only_keys_are_rejected() {
        let mut snapshot = Snapshot::default();
        for key in [
            "settings/http_proxy",
            "settings/recent_projects",
            "file/../passwords.db",
            "file/plugins/tool/plugin.json",
        ] {
            snapshot.entries.clear();
            snapshot
                .entries
                .insert(key.into(), Value::String("secret".into()));
            assert!(snapshot.validate().is_err());
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Storage {
    #[default]
    Folder,
    File,
    Git,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub storage: Storage,
    pub path: String,
    pub remote: String,
    pub branch: String,
    pub selection: Selection,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            storage: Storage::Folder,
            path: String::new(),
            remote: String::new(),
            branch: "main".into(),
            selection: Selection::default(),
        }
    }
}
impl Preferences {
    pub fn load() -> Self {
        std::fs::read(crate::Settings::system_dir().join("settings-sync/config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some((scheme, rest)) = self.remote.split_once("://") {
            let authority = rest.split('/').next().unwrap_or_default();
            if let Some((user, _)) = authority.rsplit_once('@') {
                anyhow::ensure!(!user.contains(':') && !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https"), "Keep credentials out of the repository URL; use Git credentials or SSH authentication");
            }
            anyhow::ensure!(
                !rest.contains('?') && !rest.contains('#'),
                "Repository URLs cannot carry tokens or query fragments"
            );
        }
        let path = crate::Settings::system_dir().join("settings-sync/config.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_text(&path, &serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
pub fn selected(snapshot: &Snapshot, selection: &Selection) -> Snapshot {
    let mut output = snapshot.clone();
    let keys = fields(selection);
    output.entries.retain(|key, _| {
        if let Some(field) = key.strip_prefix("settings/") {
            keys.contains(&field)
        } else if key.starts_with("file/colors/") || key.starts_with("file/ui-themes/") {
            selection.appearance
        } else if key.starts_with("file/keymaps/") {
            selection.keymaps
        } else {
            selection.templates
        }
    });
    if !selection.plugins {
        output.plugins.clear();
    }
    output
}

/// Copy only the selected preference fields. Cloning the destination first keeps
/// serde-skipped session credentials and every machine-specific field intact.
pub fn copy_selected_preferences(
    destination: &mut crate::Settings,
    source: &crate::Settings,
    selection: &Selection,
) {
    macro_rules! copy {($($field:ident),*)=>{$(destination.$field=source.$field.clone();)*};}
    if selection.appearance {
        copy!(appearance, notifications, chrome, accessibility);
        destination.native_ui.file_chooser = source.native_ui.file_chooser;
        destination.native_ui.linux_provider = source.native_ui.linux_provider;
        let local_background_path = destination.menus.appearance.background.path.clone();
        destination.menus = source.menus.clone();
        destination.menus.appearance.background.path = local_background_path;
    }
    if selection.editor {
        copy!(
            editor,
            completion,
            code_style,
            code_style_schemes,
            todo,
            refactoring,
            docstring_format,
            disabled_intentions
        );
        destination.semantic_highlighting = source.semantic_highlighting;
    }
    if selection.keymaps {
        copy!(
            keymap,
            custom_keymaps,
            custom_shortcuts,
            keymap_overrides,
            keymap_quick_lists,
            quick_list,
            macros,
            macro_shortcuts
        );
    }
    if selection.inspections {
        copy!(inspections);
    }
    if selection.plugins {
        destination.plugins.disabled_builtins = source.plugins.disabled_builtins.clone();
        destination.plugins.vim_enabled = source.plugins.vim_enabled;
    }
}

#[cfg(test)]
mod native_authority_tests {
    use super::*;
    #[test]
    fn chooser_paths_stay_local_through_export_hostile_disk_import_and_live_apply() {
        let folder = tempfile::tempdir().unwrap();
        let mut local = crate::Settings::default();
        local.native_ui.zenity_program = Some("/local/approved/zenity".into());
        local.native_ui.kdialog_program = Some("/local/approved/kdialog".into());
        std::fs::write(
            folder.path().join("settings.json"),
            serde_json::to_vec(&local).unwrap(),
        )
        .unwrap();
        let expected = capture(
            &local,
            folder.path(),
            &Selection::default(),
            Vec::new(),
            &|| false,
        )
        .unwrap();
        let exported: crate::native_ui::Settings =
            serde_json::from_value(expected.entries["settings/native_ui"].clone()).unwrap();
        assert!(exported.zenity_program.is_none() && exported.kdialog_program.is_none());
        let mut incoming = local.clone();
        incoming.native_ui.file_chooser = true;
        incoming.native_ui.zenity_program = Some("/remote/unapproved/program".into());
        incoming.native_ui.kdialog_program = None;
        let mut hostile = expected.clone();
        hostile.entries.insert(
            "settings/native_ui".into(),
            serde_json::to_value(&incoming.native_ui).unwrap(),
        );
        apply(folder.path(), &hostile, &expected, &|| false).unwrap();
        let saved: crate::Settings =
            serde_json::from_slice(&std::fs::read(folder.path().join("settings.json")).unwrap())
                .unwrap();
        assert!(saved.native_ui.file_chooser);
        assert_eq!(
            saved.native_ui.zenity_program,
            local.native_ui.zenity_program
        );
        assert_eq!(
            saved.native_ui.kdialog_program,
            local.native_ui.kdialog_program
        );
        copy_selected_preferences(&mut local, &incoming, &Selection::default());
        assert!(local.native_ui.file_chooser);
        assert_eq!(
            local.native_ui.zenity_program,
            saved.native_ui.zenity_program
        );
        assert_eq!(
            local.native_ui.kdialog_program,
            saved.native_ui.kdialog_program
        );
    }
}

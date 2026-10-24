//! Installed plugins: `<config_dir>/plugins/<id>/`, with the enabled state of
//! each in `<config_dir>/plugins/state.json`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::bundle::{unpack_bundle, MAX_BUNDLE_BYTES};
use crate::contrib::{check_plugin_dir, plugin_program, validate_plugin_dir, Contributions};
use crate::error::{io_err, PluginError};
use crate::manifest::{is_compatible, validate_plugin_id, PluginManifest, MANIFEST_FILE};
use crate::paths::{absolute, safe_relative_path, unique_suffix, MAX_DEPTH};
use crate::version::Version;
use crate::IDE_VERSION;

/// Directory under the config directory that holds the plugins.
pub const PLUGINS_DIR: &str = "plugins";
/// File (inside [`PLUGINS_DIR`]) that records which plugins are disabled.
pub const STATE_FILE: &str = "state.json";

/// Upper bounds for what a plugin directory may contain.
const MAX_FILES: usize = 5_000;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Version-control metadata is never copied into an installed plugin.
const SKIPPED_NAMES: &[&str] = &[".git", ".hg", ".svn", ".DS_Store"];
/// Prefixes of the hidden entries installs create next to the plugins:
/// staging directories, backups of replaced versions and state files being written.
const STAGING_PREFIX: &str = ".staging-";
const BACKUP_PREFIX: &str = ".old-";
const STATE_TMP_PREFIX: &str = ".state-";
/// Leftovers older than this are from an install that crashed (a running
/// install, possibly in another IDE window, finishes within seconds).
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// One directory under `<config_dir>/plugins/`.
#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    /// The directory name, which is the plugin id for every correctly installed plugin.
    pub id: String,
    /// The manifest; for a plugin whose `plugin.json` cannot be read this is a
    /// placeholder with only `id` and `name` set (see `errors`).
    pub manifest: PluginManifest,
    /// The plugin directory (absolute).
    pub dir: PathBuf,
    pub enabled: bool,
    /// Why the plugin cannot be used. A plugin with errors contributes nothing.
    pub errors: Vec<String>,
    /// Non-fatal findings (unknown fields, unknown highlight tags, …).
    pub warnings: Vec<String>,
    contributions: Contributions,
}

impl InstalledPlugin {
    /// Display name (the id when the manifest has none).
    pub fn name(&self) -> &str {
        if self.manifest.name.trim().is_empty() {
            &self.id
        } else {
            &self.manifest.name
        }
    }

    /// True when the plugin loaded without errors.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// True when the plugin is enabled and valid, i.e. it contributes to the IDE.
    pub fn is_active(&self) -> bool {
        self.enabled && self.errors.is_empty()
    }

    /// What this plugin contributes when active (empty for invalid plugins).
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }
}

/// The result of an install or upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub id: String,
    pub version: String,
    /// The version that was replaced, for upgrades.
    pub previous_version: Option<String>,
    /// Where the plugin now lives.
    pub dir: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StateFile {
    /// Plugin id → enabled. Plugins missing here are enabled.
    enabled: BTreeMap<String, bool>,
}

/// The installed plugins of one configuration directory.
#[derive(Clone, Debug)]
pub struct PluginRegistry {
    config_dir: PathBuf,
    plugins: Vec<InstalledPlugin>,
    enabled: BTreeMap<String, bool>,
    signature_policy: Result<rustpy_settings::plugins::Settings, String>,
    errors: Vec<String>,
}

impl PluginRegistry {
    /// Reads every plugin under `<config_dir>/plugins/` and the enabled state.
    /// Never fails: unreadable plugins are listed with `errors`, other
    /// problems are reported by [`errors`](Self::errors).
    pub fn load(config_dir: &Path) -> PluginRegistry {
        let mut registry = PluginRegistry {
            config_dir: absolute(config_dir),
            plugins: Vec::new(),
            enabled: BTreeMap::new(),
            signature_policy: crate::signatures::policy(config_dir),
            errors: Vec::new(),
        };
        registry.reload();
        registry
    }

    /// Re-reads the plugins directory and the state file. Leftovers of an
    /// install that crashed (hidden staging and backup directories older than
    /// an hour) are cleaned up first; an upgrade that crashed half-way, with
    /// the old version moved aside and the new one not yet in place, is rolled back.
    pub fn reload(&mut self) {
        self.errors.clear();
        self.clean_up_leftovers(STALE_AFTER);
        self.enabled = self.read_state();
        self.signature_policy = crate::signatures::policy(&self.config_dir);
        self.plugins = self.scan();
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// `<config_dir>/plugins`.
    pub fn plugins_dir(&self) -> PathBuf {
        self.config_dir.join(PLUGINS_DIR)
    }

    /// Installed plugins, sorted by id.
    pub fn plugins(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    pub fn get(&self, id: &str) -> Option<&InstalledPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Problems not tied to one plugin (unreadable state file or plugins directory).
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.get(id).is_some_and(|p| p.enabled)
    }

    pub fn enable(&mut self, id: &str) -> Result<(), PluginError> {
        self.set_enabled(id, true)
    }

    pub fn disable(&mut self, id: &str) -> Result<(), PluginError> {
        self.set_enabled(id, false)
    }

    /// Enables or disables a plugin and persists the choice in `state.json`.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), PluginError> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginError::NotInstalled(id.to_string()))?;
        plugin.enabled = enabled;
        self.enabled.insert(id.to_string(), enabled);
        self.write_state()
    }

    /// Installs (or upgrades) the plugin in directory `src`: validates it,
    /// then copies it to `<config_dir>/plugins/<id>/`. An installed plugin is
    /// only replaced by a newer version; its enabled state is kept.
    ///
    /// File permissions are not copied: installed files are `0644`, or `0755`
    /// when the source file is executable by its owner (Unix), so a plugin
    /// folder from a shared place cannot bring setuid or world-writable files.
    pub fn install_from_dir(&mut self, src: &Path) -> Result<InstallOutcome, PluginError> {
        let src = absolute(src);
        let meta = fs::metadata(&src).map_err(io_err(&src))?;
        if !meta.is_dir() {
            return Err(PluginError::invalid(format!(
                "{} is not a directory",
                src.display()
            )));
        }
        if let (Ok(real_src), Ok(real_plugins)) =
            (fs::canonicalize(&src), fs::canonicalize(self.plugins_dir()))
        {
            if real_src.starts_with(&real_plugins) {
                return Err(PluginError::invalid(format!(
                    "{} is inside the plugins directory already",
                    src.display()
                )));
            }
        }
        // Fail fast (before copying anything) on an invalid or unwanted plugin.
        let manifest = validate_plugin_dir(&src)?.manifest;
        self.reload();
        self.check_installable(
            &manifest.id,
            &manifest.version,
            manifest.min_ide_version.as_deref(),
        )?;
        let files = plugin_files(&src)?;
        let staging = self.create_staging()?;
        let mut budget = MAX_TOTAL_BYTES;
        for file in &files {
            let target = staging.path.join(&file.rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io_err(parent))?;
            }
            budget -= copy_plugin_file(file, &target, budget)?;
        }
        self.commit(staging, None)
    }

    /// "Install Plugin from Disk…": a plugin directory or a `.rpplugin` bundle file.
    pub fn install_from_path(&mut self, path: &Path) -> Result<InstallOutcome, PluginError> {
        if path.is_dir() {
            self.install_from_dir(path)
        } else {
            self.install_from_bundle(path)
        }
    }

    /// Installs (or upgrades) a plugin from a `.rpplugin` bundle file, which
    /// must be a regular file (a FIFO or a link to a device is refused, not read).
    pub fn install_from_bundle(&mut self, path: &Path) -> Result<InstallOutcome, PluginError> {
        let bytes = read_bundle_file(path)?;
        self.install_bundle_bytes(&bytes)
    }

    /// Installs (or upgrades) a plugin from bundle bytes (e.g. a download).
    pub fn install_bundle_bytes(&mut self, bytes: &[u8]) -> Result<InstallOutcome, PluginError> {
        self.install_bundle_expecting(bytes, None)
    }

    /// Like [`install_bundle_bytes`](Self::install_bundle_bytes); with
    /// `expect = Some((id, version))` the bundle must contain exactly that plugin.
    pub(crate) fn install_bundle_expecting(
        &mut self,
        bytes: &[u8],
        expect: Option<(&str, &str)>,
    ) -> Result<InstallOutcome, PluginError> {
        self.reload();
        let staging = self.create_staging()?;
        unpack_bundle(bytes, &staging.path)?;
        self.commit(staging, expect)
    }

    /// Removes an installed plugin (its directory and its state entry).
    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let plugin = self
            .get(id)
            .ok_or_else(|| PluginError::NotInstalled(id.to_string()))?;
        let dir = plugin.dir.clone();
        // `dir` comes from scanning the plugins directory; make sure of it anyway.
        if dir.parent() != Some(self.plugins_dir().as_path()) {
            return Err(PluginError::invalid(format!(
                "refusing to remove {}",
                dir.display()
            )));
        }
        let is_link = fs::symlink_metadata(&dir)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let removed = if is_link {
            remove_link(&dir)
        } else {
            fs::remove_dir_all(&dir)
        };
        removed.map_err(io_err(&dir))?;
        let had_state = self.enabled.remove(id).is_some();
        self.plugins.retain(|p| p.id != id);
        if had_state {
            self.write_state()?;
        }
        Ok(())
    }

    /// Everything the enabled, valid plugins contribute, in plugin-id order.
    pub fn contributions(&self) -> Contributions {
        let mut all = Contributions::default();
        for plugin in self.plugins.iter().filter(|p| p.is_active()) {
            all.append(&plugin.contributions);
        }
        all
    }

    // ----- internals ---------------------------------------------------------

    fn state_path(&self) -> PathBuf {
        self.plugins_dir().join(STATE_FILE)
    }

    fn read_state(&mut self) -> BTreeMap<String, bool> {
        let path = self.state_path();
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<StateFile>(&text) {
                Ok(state) => state.enabled,
                Err(e) => {
                    self.errors.push(format!(
                        "{}: {e}; every plugin is treated as enabled",
                        path.display()
                    ));
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                self.errors.push(format!("{}: {e}", path.display()));
                BTreeMap::new()
            }
        }
    }

    /// Writes `state.json` atomically (temporary file + rename).
    fn write_state(&self) -> Result<(), PluginError> {
        let dir = self.plugins_dir();
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let path = self.state_path();
        let tmp = dir.join(format!(".state-{}.tmp", unique_suffix()));
        let state = StateFile {
            enabled: self.enabled.clone(),
        };
        let text = serde_json::to_string_pretty(&state).map_err(|source| PluginError::Json {
            what: STATE_FILE.into(),
            source,
        })?;
        fs::write(&tmp, text).map_err(io_err(&tmp))?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            PluginError::Io {
                path: path.clone(),
                source: e,
            }
        })
    }

    fn scan(&mut self) -> Vec<InstalledPlugin> {
        let dir = self.plugins_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                self.errors.push(format!("{}: {e}", dir.display()));
                return Vec::new();
            }
        };
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                self.errors.push(format!(
                    "{}: skipped (the name is not UTF-8)",
                    path.display()
                ));
                continue;
            };
            // Hidden entries are staging/backup directories and temporary files.
            if name.starts_with('.') || !path.is_dir() {
                continue;
            }
            found.push((name, path));
        }
        found.sort();
        found
            .into_iter()
            .map(|(name, path)| self.load_plugin(name, path))
            .collect()
    }

    fn load_plugin(&self, id: String, dir: PathBuf) -> InstalledPlugin {
        let check = check_plugin_dir(&dir);
        let mut errors = check.errors;
        let manifest = check.manifest.unwrap_or_else(|| PluginManifest {
            id: id.clone(),
            name: id.clone(),
            ..PluginManifest::default()
        });
        if errors.is_empty() {
            if manifest.id != id {
                errors.push(format!(
                    "the directory `{id}` holds the plugin `{}`; plugins must live in plugins/<id>/",
                    manifest.id
                ));
            }
            if !manifest.is_compatible_with(IDE_VERSION) {
                errors.push(
                    PluginError::Incompatible {
                        id: manifest.id.clone(),
                        required: manifest.min_ide_version.clone().unwrap_or_default(),
                        current: IDE_VERSION.to_string(),
                    }
                    .to_string(),
                );
            }
        }
        match &self.signature_policy {
            Ok(policy) => {
                if let Err(error) = crate::signatures::verify_directory(&dir, policy) {
                    errors.push(error);
                }
            }
            Err(error) => errors.push(format!("Cannot load publisher trust policy: {error}")),
        }
        let contributions = if errors.is_empty() {
            check.contributions
        } else {
            Contributions::default()
        };
        InstalledPlugin {
            enabled: self.enabled.get(&id).copied().unwrap_or(true),
            id,
            manifest,
            dir: check.dir,
            errors,
            warnings: check.warnings,
            contributions,
        }
    }

    /// Checks compatibility and id collisions; returns the version an install would replace.
    /// Whether plugin `id` in `version` may be installed: the IDE must satisfy
    /// `min_ide_version`, and an installed copy is only replaced by a newer
    /// version (or, when its manifest cannot be read, by any version).
    /// Returns the version an install would replace.
    pub(crate) fn check_installable(
        &self,
        id: &str,
        version: &str,
        min_ide_version: Option<&str>,
    ) -> Result<Option<String>, PluginError> {
        if !is_compatible(min_ide_version, IDE_VERSION) {
            return Err(PluginError::Incompatible {
                id: id.to_string(),
                required: min_ide_version.unwrap_or_default().to_string(),
                current: IDE_VERSION.to_string(),
            });
        }
        let Some(existing) = self.get(id) else {
            return Ok(None);
        };
        let installed = existing.manifest.version.clone();
        match (Version::parse(&installed), Version::parse(version)) {
            (Ok(old), Ok(new)) if existing.manifest.id == id => {
                if new > old {
                    Ok(Some(installed))
                } else {
                    Err(PluginError::AlreadyInstalled {
                        id: id.to_string(),
                        installed,
                        offered: version.to_string(),
                    })
                }
            }
            // Unreadable or misplaced: replacing it repairs the install.
            _ => Ok((!installed.is_empty()).then_some(installed)),
        }
    }

    fn create_staging(&self) -> Result<Staging, PluginError> {
        let dir = self.plugins_dir();
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let suffix = unique_suffix();
        let path = dir.join(format!("{STAGING_PREFIX}{suffix}"));
        fs::create_dir(&path).map_err(io_err(&path))?;
        Ok(Staging {
            path,
            suffix,
            keep: false,
        })
    }

    /// Removes what an interrupted install left behind in the plugins
    /// directory, when it is older than `min_age`: staging directories,
    /// backups of replaced versions and temporary state files are deleted.
    /// A backup is put back only when the upgrade that made it was
    /// interrupted between moving the old version away and moving the new
    /// one in: then its plugin directory is missing and the new version is
    /// still staged (`.staging-<suffix>` next to `.old-<suffix>`, at least
    /// `min_age` old). Any other backup is just not deleted yet: a missing
    /// plugin directory means the plugin was uninstalled. Problems are
    /// reported by [`errors`](Self::errors).
    pub(crate) fn clean_up_leftovers(&mut self, min_age: Duration) {
        let dir = self.plugins_dir();
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        let now = SystemTime::now();
        let old_enough = |meta: &fs::Metadata| {
            meta.modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= min_age)
        };
        let mut leftovers: Vec<(String, PathBuf, fs::Metadata)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let leftover = name.starts_with(STAGING_PREFIX)
                || name.starts_with(BACKUP_PREFIX)
                || (name.starts_with(STATE_TMP_PREFIX) && name.ends_with(".tmp"));
            let path = entry.path();
            if let (true, Ok(meta)) = (leftover, fs::symlink_metadata(&path)) {
                leftovers.push((name, path, meta));
            }
        }
        // The staged new version of the upgrade a backup comes from, if it is still there.
        let staged = |suffix: &str| {
            leftovers
                .iter()
                .find(|(name, ..)| name.strip_prefix(STAGING_PREFIX) == Some(suffix))
                .map(|(_, _, meta)| meta)
        };
        for (name, path, meta) in &leftovers {
            let restore_to = match name.strip_prefix(BACKUP_PREFIX).map(staged) {
                // An upgrade still running (maybe in another IDE window).
                Some(Some(staging)) if !old_enough(staging) => continue,
                // An interrupted upgrade: put the old version back unless the
                // plugin directory was recreated since.
                Some(Some(_)) => backup_plugin_id(path)
                    .map(|id| dir.join(id))
                    .filter(|target| fs::symlink_metadata(target).is_err()),
                _ if !old_enough(meta) => continue,
                _ => None,
            };
            let result = match restore_to {
                Some(target) => fs::rename(path, &target),
                None if meta.file_type().is_symlink() => remove_link(path),
                None if meta.is_dir() => fs::remove_dir_all(path),
                None => fs::remove_file(path),
            };
            if let Err(e) = result {
                self.errors.push(format!("{}: {e}", path.display()));
            }
        }
    }

    /// Validates the staged plugin and moves it into place. Programs of the
    /// plugin's external tools (`$PluginDir$/bin/tool`) are made executable,
    /// so tools work however the plugin arrived (bundles from Windows, packed
    /// by hand, …).
    fn commit(
        &mut self,
        mut staging: Staging,
        expect: Option<(&str, &str)>,
    ) -> Result<InstallOutcome, PluginError> {
        let validated = validate_plugin_dir(&staging.path)?;
        let policy = crate::signatures::policy(&self.config_dir).map_err(PluginError::invalid)?;
        crate::signatures::verify_directory(&staging.path, &policy)
            .map_err(PluginError::invalid)?;
        let manifest = validated.manifest;
        for decl in &manifest.contributes.external_tools {
            if let Some(rel) = plugin_program(decl) {
                let path = staging
                    .path
                    .join(safe_relative_path(&rel).map_err(PluginError::invalid)?);
                make_executable(&path)?;
            }
        }
        if let Some((id, version)) = expect {
            let same_version = matches!(
                (Version::parse(&manifest.version), Version::parse(version)),
                (Ok(a), Ok(b)) if a == b
            );
            if manifest.id != id || !same_version {
                return Err(PluginError::Bundle(format!(
                    "it contains {} {} but the catalog promised {id} {version}",
                    manifest.id, manifest.version
                )));
            }
        }
        let previous_version = self.check_installable(
            &manifest.id,
            &manifest.version,
            manifest.min_ide_version.as_deref(),
        )?;
        // A fresh install starts enabled, even if a stale state entry says
        // otherwise; an upgrade keeps the plugin's state.
        if self.get(&manifest.id).is_none() && self.enabled.remove(&manifest.id).is_some() {
            self.write_state()?;
        }
        let target = self.plugins_dir().join(&manifest.id);
        replace_dir(&mut staging, &target, &self.plugins_dir())?;
        self.reload();
        Ok(InstallOutcome {
            id: manifest.id,
            version: manifest.version,
            previous_version,
            dir: target,
        })
    }
}

/// A temporary directory under the plugins directory (`.staging-<suffix>`),
/// removed unless renamed away or kept to record an interrupted upgrade.
struct Staging {
    path: PathBuf,
    /// The unique part of the name, shared with the backup of the version it replaces.
    suffix: String,
    keep: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Moves the staged plugin to `target`, replacing an existing `target`. The
/// old version is moved aside to `.old-<suffix>` (the staging directory's
/// suffix) and deleted once the new one is in place; while both the backup
/// and the staging directory exist, the swap is unfinished, and a later
/// [`clean_up_leftovers`](PluginRegistry::clean_up_leftovers) puts the old
/// version back. If moving the new version in fails, the old one is put back
/// at once; if even that fails, the staging directory is kept for the cleanup.
fn replace_dir(
    staging: &mut Staging,
    target: &Path,
    plugins_dir: &Path,
) -> Result<(), PluginError> {
    if fs::symlink_metadata(target).is_err() {
        return fs::rename(&staging.path, target).map_err(io_err(target));
    }
    let backup = plugins_dir.join(format!("{BACKUP_PREFIX}{}", staging.suffix));
    fs::rename(target, &backup).map_err(io_err(target))?;
    if let Err(e) = fs::rename(&staging.path, target) {
        if fs::rename(&backup, target).is_err() {
            staging.keep = true;
        }
        return Err(PluginError::Io {
            path: target.to_path_buf(),
            source: e,
        });
    }
    let is_link = fs::symlink_metadata(&backup)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let _ = if is_link {
        remove_link(&backup)
    } else {
        fs::remove_dir_all(&backup)
    };
    Ok(())
}

/// Removes a symbolic link to a directory (not its target).
fn remove_link(path: &Path) -> std::io::Result<()> {
    // Windows directory links are removed like directories, Unix links like files.
    fs::remove_file(path).or_else(|_| fs::remove_dir(path))
}

/// The plugin id in the manifest of a backup directory, when it is a valid id.
fn backup_plugin_id(backup: &Path) -> Option<String> {
    let text = fs::read_to_string(backup.join(MANIFEST_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id = value.get("id")?.as_str()?;
    validate_plugin_id(id).ok().map(|_| id.to_string())
}

/// Reads a bundle file of at most [`MAX_BUNDLE_BYTES`]. Only regular files
/// are opened: opening a FIFO would block and a device (`/dev/zero`) never ends.
pub(crate) fn read_bundle_file(path: &Path) -> Result<Vec<u8>, PluginError> {
    let too_large = || {
        PluginError::Bundle(format!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_BUNDLE_BYTES / (1024 * 1024)
        ))
    };
    let meta = fs::metadata(path).map_err(io_err(path))?;
    if !meta.is_file() {
        return Err(PluginError::Bundle(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > MAX_BUNDLE_BYTES as u64 {
        return Err(too_large());
    }
    let file = fs::File::open(path).map_err(io_err(path))?;
    // The file may have been replaced since `metadata` looked at it.
    if !file.metadata().map_err(io_err(path))?.is_file() {
        return Err(PluginError::Bundle(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BUNDLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err(path))?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Unix permission bits for an installed plugin file: `rwxr-xr-x` for
/// programs, `rw-r--r--` for everything else (never setuid/setgid/sticky or
/// writable by group or others).
pub(crate) const fn installed_mode(executable: bool) -> u32 {
    if executable {
        0o755
    } else {
        0o644
    }
}

/// Creates a new file (never following or replacing anything at `path`) with
/// [`installed_mode`] permissions (narrowed by the umask).
pub(crate) fn create_plugin_file(path: &Path, executable: bool) -> Result<fs::File, PluginError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(installed_mode(executable));
    }
    #[cfg(not(unix))]
    let _ = executable;
    options.open(path).map_err(io_err(path))
}

/// Copies one plugin file to `target` (a new file) without its permissions:
/// see [`installed_mode`]. Copies at most `budget` bytes and returns how many
/// it copied; fails when the source changed into something else than the
/// regular file the directory walk found.
fn copy_plugin_file(file: &PluginFile, target: &Path, budget: u64) -> Result<u64, PluginError> {
    let changed = || {
        PluginError::invalid(format!(
            "`{}` changed while the plugin was being installed",
            file.rel
        ))
    };
    let source = fs::File::open(&file.path).map_err(io_err(&file.path))?;
    let meta = source.metadata().map_err(io_err(&file.path))?;
    if !meta.is_file() {
        return Err(changed());
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if file.inode != Some((meta.dev(), meta.ino())) {
            return Err(changed());
        }
        meta.permissions().mode() & 0o100 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    let mut out = create_plugin_file(target, executable)?;
    let copied = std::io::copy(&mut source.take(budget + 1), &mut out).map_err(io_err(target))?;
    if copied > budget {
        return Err(PluginError::invalid(format!(
            "the plugin is larger than {} MiB",
            MAX_TOTAL_BYTES / (1024 * 1024)
        )));
    }
    Ok(copied)
}

/// `chmod +x` for a staged program: adds the execute bit wherever the file
/// is readable (`rw-r--r--` becomes `rwxr-xr-x`, `rw-------` becomes `rwx------`).
fn make_executable(path: &Path) -> Result<(), PluginError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
        if meta.is_file() {
            let mode = meta.permissions().mode() & 0o777;
            let mode = (mode | ((mode & 0o444) >> 2)) & !0o022;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_err(path))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A regular file of a plugin directory.
pub(crate) struct PluginFile {
    /// `/`-separated path relative to the plugin directory.
    pub rel: String,
    pub path: PathBuf,
    /// Device and inode the walk saw (Unix), to notice a file swapped before copying.
    pub inode: Option<(u64, u64)>,
}

/// Lists the files of a plugin directory (sorted), refusing symbolic links,
/// special files, non-portable names and oversized trees. VCS metadata is skipped.
pub(crate) fn plugin_files(root: &Path) -> Result<Vec<PluginFile>, PluginError> {
    let mut out = Vec::new();
    let mut total = 0u64;
    walk(root, "", 0, &mut out, &mut total)?;
    Ok(out)
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<PluginFile>,
    total: &mut u64,
) -> Result<(), PluginError> {
    if depth > MAX_DEPTH {
        return Err(PluginError::invalid(format!(
            "`{prefix}` is nested more than {MAX_DEPTH} directories deep"
        )));
    }
    let mut entries = fs::read_dir(dir)
        .map_err(io_err(dir))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_err(dir))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(PluginError::invalid(format!(
                "{}: the file name is not UTF-8",
                path.display()
            )));
        };
        if SKIPPED_NAMES.contains(&name) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        safe_relative_path(&rel).map_err(PluginError::invalid)?;
        let file_type = entry.file_type().map_err(io_err(&path))?;
        if file_type.is_symlink() {
            return Err(PluginError::invalid(format!(
                "`{rel}` is a symbolic link; plugins must not contain links"
            )));
        } else if file_type.is_dir() {
            walk(&path, &rel, depth + 1, out, total)?;
        } else if file_type.is_file() {
            // `DirEntry::metadata` does not follow links (and this is no link).
            let meta = entry.metadata().map_err(io_err(&path))?;
            *total += meta.len();
            if *total > MAX_TOTAL_BYTES {
                return Err(PluginError::invalid(format!(
                    "the plugin is larger than {} MiB",
                    MAX_TOTAL_BYTES / (1024 * 1024)
                )));
            }
            #[cfg(unix)]
            let inode = {
                use std::os::unix::fs::MetadataExt;
                Some((meta.dev(), meta.ino()))
            };
            #[cfg(not(unix))]
            let inode = None;
            out.push(PluginFile { rel, path, inode });
            if out.len() > MAX_FILES {
                return Err(PluginError::invalid(format!(
                    "the plugin has more than {MAX_FILES} files"
                )));
            }
        } else {
            return Err(PluginError::invalid(format!(
                "`{rel}` is not a regular file"
            )));
        }
    }
    Ok(())
}

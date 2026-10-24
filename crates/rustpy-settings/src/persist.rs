//! Safe persistence of `settings.json`.
//!
//! * A file that does not parse is copied to `settings.json.invalid-<unix time>`
//!   before anything can overwrite it, and the error is kept for the UI.
//! * Saving merges with what another RustPy process wrote since this one loaded
//!   or last saved the file: a three-way merge against that baseline. Values this
//!   process changed win, all other values keep the file's version, and the
//!   recent / trusted project lists are combined. Two open windows therefore no
//!   longer revert each other's settings or recent projects.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::Settings;

/// Why `settings.json` could not be used when it was loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    pub path: PathBuf,
    pub message: String,
    /// Where the unusable file was copied (None when it could not be copied).
    pub backup: Option<PathBuf>,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is invalid ({}); using default settings",
            self.path.display(),
            self.message
        )?;
        if let Some(b) = &self.backup {
            write!(f, ". Your file was saved as {}", b.display())?;
        }
        Ok(())
    }
}

/// What this process last read from or wrote to a settings file.
#[derive(Clone, Debug, Default)]
pub struct Baseline {
    path: PathBuf,
    /// The settings as this process last knew them, as normalized JSON.
    value: Option<Value>,
    /// Content of an invalid file that was already backed up (not copied twice).
    backed_up: Option<Vec<u8>>,
}

impl Baseline {
    fn of(path: &Path, settings: &Settings) -> Baseline {
        Baseline {
            path: path.to_path_buf(),
            value: normalized(settings).ok(),
            backed_up: None,
        }
    }
}

/// Top-level lists merged entry by entry instead of as one value.
const MERGED_LISTS: &[&str] = &["recent_projects", "trusted_projects", "trusted_locations"];

/// Of [`MERGED_LISTS`], the ones that are sets rather than newest-first
/// histories: a removal (revoked trust) always sticks.
const MERGED_SETS: &[&str] = &["trusted_projects", "trusted_locations"];

/// Serializes through text so numbers compare equal to what a file round trip gives.
fn normalized(settings: &Settings) -> serde_json::Result<Value> {
    serde_json::from_str(&serde_json::to_string(settings)?)
}

/// Reads `path`: the settings, the baseline for later saves, and the problem if the
/// file exists but cannot be used (it is backed up first).
pub fn load_from(path: &Path) -> (Settings, Baseline, Option<LoadError>) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let s = Settings::default_with_flags();
            let base = Baseline::of(path, &s);
            if e.kind() == std::io::ErrorKind::NotFound {
                return (s, base, None);
            }
            log::warn!(
                "cannot read settings file {}: {e}; using defaults",
                path.display()
            );
            return (
                s,
                base,
                Some(LoadError {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                    backup: None,
                }),
            );
        }
    };
    match serde_json::from_slice::<Settings>(&bytes) {
        Ok(s) => {
            let base = Baseline::of(path, &s);
            (s, base, None)
        }
        Err(e) => {
            let backup = backup_invalid(path, &bytes);
            log::warn!(
                "invalid settings file {}: {e}; using defaults (copy kept as {:?})",
                path.display(),
                backup
            );
            let s = Settings::default_with_flags();
            let mut base = Baseline::of(path, &s);
            base.backed_up = Some(bytes);
            (
                s,
                base,
                Some(LoadError {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                    backup,
                }),
            )
        }
    }
}

/// Writes `settings` to `path` (atomically), merging with changes another process
/// made to the file since `baseline`, then moves the baseline to `settings`.
pub fn save_to(path: &Path, settings: &Settings, baseline: &mut Baseline) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ours = normalized(settings)?;
    let mut text: Option<String> = None;
    if let Ok(bytes) = std::fs::read(path) {
        let theirs = serde_json::from_slice::<Settings>(&bytes)
            .ok()
            .and_then(|_| serde_json::from_slice::<Value>(&bytes).ok());
        match theirs {
            Some(theirs) => {
                let base = if baseline.path == path {
                    baseline.value.as_ref()
                } else {
                    None
                };
                if let Some(base) = base.filter(|b| **b != theirs) {
                    text = merged_text(base, &ours, &theirs);
                }
            }
            None => {
                // Unusable file on disk (hand-edited while RustPy runs): keep a copy.
                if baseline.backed_up.as_deref() != Some(bytes.as_slice()) {
                    backup_invalid(path, &bytes);
                }
            }
        }
    }
    let text = match text {
        Some(t) => t,
        None => serde_json::to_string_pretty(settings)?,
    };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    *baseline = Baseline {
        path: path.to_path_buf(),
        value: Some(ours),
        backed_up: None,
    };
    Ok(())
}

fn merged_text(base: &Value, ours: &Value, theirs: &Value) -> Option<String> {
    let merged = merge3(Some(base), Some(ours), Some(theirs), "")?;
    let mut s: Settings = serde_json::from_value(merged).ok()?;
    s.recent_projects.truncate(25);
    log::info!("settings.json changed on disk since it was loaded: merged with the other changes");
    serde_json::to_string_pretty(&s).ok()
}

/// Three-way JSON merge: a value this side (`ours`) changed relative to `base`
/// wins, otherwise the other side's (`theirs`) value is kept. Objects merge key by
/// key; the lists in [`MERGED_LISTS`] merge entry by entry. `None` means absent.
pub fn merge3(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
    key: &str,
) -> Option<Value> {
    if ours == base {
        return theirs.cloned();
    }
    if theirs == base || theirs == ours {
        return ours.cloned();
    }
    match (ours, theirs) {
        (Some(Value::Object(o)), Some(Value::Object(t))) => {
            let empty = Map::new();
            let b = match base {
                Some(Value::Object(b)) => b,
                _ => &empty,
            };
            let mut keys: Vec<&String> = b.keys().chain(o.keys()).chain(t.keys()).collect();
            keys.sort();
            keys.dedup();
            let mut out = Map::new();
            for k in keys {
                if let Some(v) = merge3(b.get(k), o.get(k), t.get(k), k) {
                    out.insert(k.clone(), v);
                }
            }
            Some(Value::Object(out))
        }
        (Some(Value::Array(o)), Some(Value::Array(t))) if MERGED_LISTS.contains(&key) => {
            let b: &[Value] = match base {
                Some(Value::Array(b)) => b,
                _ => &[],
            };
            Some(Value::Array(if MERGED_SETS.contains(&key) {
                merge_set(b, o, t)
            } else {
                merge_list(b, o, t)
            }))
        }
        // Both sides changed the same value: this (newest) save wins.
        _ => ours.cloned(),
    }
}

/// Merges a newest-first list: entries one side added or moved to the front come
/// first (this side's, then the other's), followed by the entries both sides
/// kept, in their old order. An entry either side removed is dropped unless the
/// other side moved it to the front.
pub fn merge_list(base: &[Value], ours: &[Value], theirs: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for v in promoted_prefix(base, ours)
        .iter()
        .chain(promoted_prefix(base, theirs))
    {
        if !out.contains(v) {
            out.push(v.clone());
        }
    }
    for v in base {
        if ours.contains(v) && theirs.contains(v) && !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

/// Merges a set-like list: an entry either side removed stays removed, and
/// entries either side added are kept (this side's order, then the other's
/// additions).
pub fn merge_set(base: &[Value], ours: &[Value], theirs: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for v in ours
        .iter()
        .filter(|v| !base.contains(v) || theirs.contains(v))
        .chain(theirs.iter().filter(|v| !base.contains(v)))
    {
        if !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

/// The entries at the front of `list` that were added or moved up relative to
/// `base`: the shortest prefix after which `list` keeps `base`'s order.
fn promoted_prefix<'a>(base: &[Value], list: &'a [Value]) -> &'a [Value] {
    for n in 0..=list.len() {
        let (head, rest) = list.split_at(n);
        let mut order = base.iter().filter(|v| !head.contains(v));
        if rest.iter().all(|r| order.any(|b| b == r)) {
            return head;
        }
    }
    list
}

/// Copies an unusable settings file next to it as `<name>.invalid-<unix time>`.
fn backup_invalid(path: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".into());
    let mut dest = path.with_file_name(format!("{name}.invalid-{stamp}"));
    let mut n = 1;
    while dest.exists() {
        dest = path.with_file_name(format!("{name}.invalid-{stamp}-{n}"));
        n += 1;
    }
    match std::fs::write(&dest, bytes) {
        Ok(()) => Some(dest),
        Err(e) => {
            log::warn!(
                "cannot back up {} to {}: {e}",
                path.display(),
                dest.display()
            );
            None
        }
    }
}

static BASELINE: Mutex<Option<Baseline>> = Mutex::new(None);
static LOAD_ERROR: Mutex<Option<LoadError>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn load_global(path: &Path) -> Settings {
    let (s, base, err) = load_from(path);
    *lock(&BASELINE) = Some(base);
    *lock(&LOAD_ERROR) = err;
    s
}

pub(crate) fn save_global(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    let mut guard = lock(&BASELINE);
    let base = guard.get_or_insert_with(|| Baseline {
        path: path.to_path_buf(),
        value: None,
        backed_up: None,
    });
    save_to(path, settings, base)
}

pub(crate) fn take_load_error() -> Option<LoadError> {
    lock(&LOAD_ERROR).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustpy-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(v: &[&str]) -> Vec<Value> {
        v.iter().map(|s| json!(s)).collect()
    }

    #[test]
    fn merge3_keeps_both_sides_changes_and_prefers_ours_on_conflict() {
        let base =
            json!({"editor": {"tab_size": 4, "font_size": 13}, "keymap": "Default", "browser": ""});
        let ours = json!({"editor": {"tab_size": 2, "font_size": 13}, "keymap": "Default", "browser": "firefox"});
        let theirs = json!({"editor": {"tab_size": 4, "font_size": 15}, "keymap": "Mine", "browser": "chrome"});
        let m = merge3(Some(&base), Some(&ours), Some(&theirs), "").unwrap();
        assert_eq!(
            m,
            json!({"editor": {"tab_size": 2, "font_size": 15}, "keymap": "Mine", "browser": "firefox"})
        );
    }

    #[test]
    fn merge3_keeps_keys_only_the_other_side_knows() {
        let base = json!({"a": 1});
        let ours = json!({"a": 2});
        let theirs = json!({"a": 1, "future": true});
        assert_eq!(
            merge3(Some(&base), Some(&ours), Some(&theirs), "").unwrap(),
            json!({"a": 2, "future": true})
        );
    }

    #[test]
    fn recent_lists_are_combined_newest_first() {
        let base = paths(&["/a", "/b", "/c"]);
        // This process opened /x, the other one /y.
        let ours = paths(&["/x", "/a", "/b", "/c"]);
        let theirs = paths(&["/y", "/a", "/b", "/c"]);
        assert_eq!(
            merge_list(&base, &ours, &theirs),
            paths(&["/x", "/y", "/a", "/b", "/c"])
        );
        // Reopening an older project moves it up.
        let ours = paths(&["/c", "/x", "/a", "/b"]);
        assert_eq!(
            merge_list(&base, &ours, &theirs),
            paths(&["/c", "/x", "/y", "/a", "/b"])
        );
        // A removal on one side sticks.
        let ours = paths(&["/a", "/c"]);
        assert_eq!(
            merge_list(&base, &ours, &theirs),
            paths(&["/y", "/a", "/c"])
        );
    }

    #[test]
    fn revoked_trust_stays_revoked() {
        let base = paths(&["/p", "/q"]);
        // This process revoked /p; the other one trusted /r (appended).
        let ours = paths(&["/q"]);
        let theirs = paths(&["/p", "/q", "/r"]);
        assert_eq!(merge_set(&base, &ours, &theirs), paths(&["/q", "/r"]));
        assert_eq!(merge_set(&base, &theirs, &ours), paths(&["/q", "/r"]));
        let m = merge3(
            Some(&json!({"trusted_projects": base})),
            Some(&json!({"trusted_projects": ours})),
            Some(&json!({"trusted_projects": theirs})),
            "",
        )
        .unwrap();
        assert_eq!(m, json!({"trusted_projects": ["/q", "/r"]}));
    }

    #[test]
    fn two_processes_do_not_revert_each_other() {
        let dir = tmp_dir("two");
        let path = dir.join("settings.json");
        let mut start = Settings::default_with_flags();
        start.add_recent_project(Path::new("/p/old"));
        let mut b0 = Baseline::default();
        save_to(&path, &start, &mut b0).unwrap();

        let (mut a, mut base_a, err) = load_from(&path);
        assert!(err.is_none());
        let (mut b, mut base_b, _) = load_from(&path);

        a.editor.tab_size = 2;
        a.add_recent_project(Path::new("/p/a"));
        save_to(&path, &a, &mut base_a).unwrap();

        b.appearance.theme = "Light".into();
        b.add_recent_project(Path::new("/p/b"));
        // Trust admission requires an existing canonical directory. Recent
        // project history intentionally also retains unavailable project paths.
        let trusted = dir.join("trusted-project-b");
        std::fs::create_dir(&trusted).unwrap();
        assert!(b.trust_project(&trusted));
        save_to(&path, &b, &mut base_b).unwrap();

        let (merged, _, _) = load_from(&path);
        assert_eq!(merged.editor.tab_size, 2, "A's change survives B's save");
        assert_eq!(merged.appearance.theme, "Light");
        assert_eq!(
            merged.recent_projects,
            vec![
                PathBuf::from("/p/b"),
                PathBuf::from("/p/a"),
                PathBuf::from("/p/old")
            ]
        );
        assert_eq!(
            merged.trusted_projects,
            vec![std::fs::canonicalize(&trusted).unwrap()]
        );

        // A saves again without new changes: B's values are not reverted.
        save_to(&path, &a, &mut base_a).unwrap();
        let (again, _, _) = load_from(&path);
        assert_eq!(again.appearance.theme, "Light");
        assert_eq!(again.editor.tab_size, 2);
        assert_eq!(again.recent_projects.len(), 3);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn invalid_file_is_backed_up_and_reported() {
        let dir = tmp_dir("invalid");
        let path = dir.join("settings.json");
        let broken = "{ \"editor\": { \"tab_size\": 2, }, \"recent_projects\": [\"/p\"] ";
        std::fs::write(&path, broken).unwrap();
        let (s, mut base, err) = load_from(&path);
        let err = err.expect("parse error reported");
        assert_eq!(s.editor.tab_size, 4, "defaults in use");
        let backup = err.backup.clone().expect("backup written");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), broken);
        assert!(err.to_string().contains("invalid"));
        // The first save replaces the file but does not copy it a second time.
        save_to(&path, &s, &mut base).unwrap();
        let backups = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".invalid-"))
            .count();
        assert_eq!(backups, 1);
        assert!(serde_json::from_str::<Settings>(&std::fs::read_to_string(&path).unwrap()).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn file_broken_while_running_is_backed_up_before_save() {
        let dir = tmp_dir("broken-later");
        let path = dir.join("settings.json");
        let s = Settings::default_with_flags();
        let mut base = Baseline::default();
        save_to(&path, &s, &mut base).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        save_to(&path, &s, &mut base).unwrap();
        let backup = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().contains(".invalid-"))
            .expect("backup");
        assert_eq!(
            std::fs::read_to_string(backup.path()).unwrap(),
            "{ not json"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

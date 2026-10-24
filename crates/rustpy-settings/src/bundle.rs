//! File › Manage IDE Settings: export/import the configuration directory as a
//! single JSON bundle (text files only; logs, local history, caches,
//! machine-specific interpreter wrappers and installed plugins are skipped,
//! and so is the password database with its key).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

const FORMAT: &str = "rustpy-settings";
const MAX_FILE: u64 = 5 * 1024 * 1024;
/// Top-level directories of the configuration that are neither exported nor
/// imported. `plugins` holds installed plugins: programs that must stay
/// executable, binary files a text bundle cannot carry, and versions that
/// must not be mixed file by file. Only an install (Settings › Plugins) puts
/// a plugin there.
const SKIP_DIRS: &[&str] = &[
    "log",
    "local_history",
    "caches",
    "interpreters",
    "backups",
    "plugins",
];

/// Whether bundle path `rel` lies in one of the [`SKIP_DIRS`]. Case is
/// ignored: `Plugins/…` is the plugins directory on Windows and macOS.
fn skipped(rel: &Path) -> bool {
    let first = rel
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy());
    first.is_some_and(|first| SKIP_DIRS.iter().any(|d| first.eq_ignore_ascii_case(d)))
}

/// The password database (`rustpy-secrets`): secrets never travel in a bundle,
/// and importing another machine's database would replace this one's with a
/// file its key cannot decrypt. Temporary files of atomic writes are `.<name>…`.
fn is_password_store(rel: &str) -> bool {
    let name = rel.trim_start_matches('.');
    !rel.contains('/') && (name.starts_with("passwords.db") || name.starts_with("passwords.key"))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let ft = e.file_type()?;
        if ft.is_dir() {
            if dir == root && skipped(Path::new(&e.file_name())) {
                continue;
            }
            collect(root, &path, out)?;
        } else if ft.is_file()
            && !is_password_store(&rel)
            && e.metadata().map(|m| m.len() <= MAX_FILE).unwrap_or(false)
        {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((rel, text));
            }
        }
    }
    Ok(())
}

/// Writes the bundle to `out`; returns the exported relative paths.
pub fn export_bundle(config_dir: &Path, out: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    if config_dir.is_dir() {
        collect(config_dir, config_dir, &mut files)?;
    }
    if files.is_empty() {
        bail!("nothing to export in {}", config_dir.display());
    }
    let map: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let doc = serde_json::json!({ "format": FORMAT, "version": 1, "files": map });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("cannot write {}", out.display()))?;
    Ok(files.into_iter().map(|(k, _)| k).collect())
}

/// Rejects absolute paths and `..` components inside a bundle.
fn safe_relative(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(p.to_path_buf())
}

/// Imports a bundle; files that would be overwritten are first copied to
/// `<config>/backups/<timestamp>/`. Returns the imported relative paths.
pub fn import_bundle(config_dir: &Path, bundle: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(bundle)
        .with_context(|| format!("cannot read {}", bundle.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&text).context("not a JSON settings bundle")?;
    if doc["format"].as_str() != Some(FORMAT) {
        bail!("{} is not a RustPy settings bundle", bundle.display());
    }
    if doc["selection_only"].as_bool() == Some(true) {
        let snapshot = crate::settings_sync::from_bundle(&text)?;
        let (settings, _, error) = crate::persist::load_from(&config_dir.join("settings.json"));
        anyhow::ensure!(
            error.is_none(),
            "Cannot import into an invalid settings file"
        );
        let mut expected = crate::settings_sync::capture(
            &settings,
            config_dir,
            &crate::settings_sync::Selection::default(),
            Vec::new(),
            &|| false,
        )?;
        expected
            .entries
            .retain(|key, _| snapshot.entries.contains_key(key));
        return crate::settings_sync::apply(config_dir, &snapshot, &expected, &|| false);
    }
    let files = doc["files"].as_object().context("bundle has no files")?;
    let stamp = crate::tasks::now_secs();
    let backup_root = config_dir.join("backups").join(stamp.to_string());
    // Validate everything before touching the configuration.
    let mut plan = Vec::new();
    for (rel, content) in files {
        let Some(relp) = safe_relative(rel) else {
            bail!("unsafe path in bundle: {rel}")
        };
        // Also when a bundle lists them (made by hand, or exported by a build
        // that did not skip plugins yet).
        if skipped(&relp) || is_password_store(rel) {
            continue;
        }
        if let Some(content) = content.as_str() {
            plan.push((rel.clone(), relp, content));
        }
    }
    let mut imported = Vec::new();
    for (rel, relp, content) in plan {
        let dest = config_dir.join(&relp);
        if dest.is_file() {
            let b = backup_root.join(&relp);
            if let Some(parent) = b.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&dest, &b)?;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, content)?;
        imported.push(rel);
    }
    Ok(imported)
}

/// Moves settings.json (and user keymaps/templates) aside so defaults apply;
/// returns the backup directory.
pub fn reset_to_defaults(config_dir: &Path) -> anyhow::Result<PathBuf> {
    let backup = config_dir.join("backups").join(format!(
        "reset-{}-{}",
        crate::tasks::now_secs(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(config_dir.join("backups"))?;
    std::fs::create_dir(&backup)?;
    let mut moved = Vec::new();
    for name in [
        "settings.json",
        "live_templates.json",
        "fileTemplates.json",
        "structural-templates.json",
        "keymap.json",
        "macros.json",
        "colors",
        "themes",
        "ui-themes",
        "keymaps",
        "templates",
        "codeStyles",
        "dialog-policies.json",
        "new-project-preferences.json",
        "new-project-run-templates.json",
    ] {
        let p = config_dir.join(name);
        if p.exists() {
            if let Err(error) = std::fs::rename(&p, backup.join(name)) {
                for previous in moved.into_iter().rev() {
                    let _ = std::fs::rename(backup.join(previous), config_dir.join(previous));
                }
                return Err(error.into());
            }
            moved.push(name);
        }
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resetting_defaults_backs_up_custom_ui_and_retains_secrets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes/custom.json"), "{}").unwrap();
        std::fs::write(dir.path().join("dialog-policies.json"), "{}").unwrap();
        std::fs::write(dir.path().join("passwords.db"), "secret").unwrap();
        let backup = reset_to_defaults(dir.path()).unwrap();
        assert!(backup.join("themes/custom.json").is_file());
        assert!(backup.join("dialog-policies.json").is_file());
        assert!(!dir.path().join("themes").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("passwords.db")).unwrap(),
            "secret"
        );
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustpy-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn export_skips_logs_and_import_backs_up() {
        let src = tmp("src");
        std::fs::write(src.join("settings.json"), "{\"a\":1}").unwrap();
        std::fs::create_dir_all(src.join("colors")).unwrap();
        std::fs::write(src.join("colors/My.json"), "{}").unwrap();
        std::fs::create_dir_all(src.join("log")).unwrap();
        std::fs::write(src.join("log/rustpy.log"), "noise").unwrap();
        let out = src.join("export.json");
        let files = export_bundle(&src, &out).unwrap();
        assert_eq!(
            files,
            vec!["colors/My.json".to_string(), "settings.json".to_string()]
        );

        let dst = tmp("dst");
        std::fs::write(dst.join("settings.json"), "{\"old\":true}").unwrap();
        let imported = import_bundle(&dst, &out).unwrap();
        assert_eq!(imported.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dst.join("settings.json")).unwrap(),
            "{\"a\":1}"
        );
        let backups: Vec<_> = std::fs::read_dir(dst.join("backups"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path().join("settings.json")).unwrap(),
            "{\"old\":true}"
        );
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn password_database_never_travels() {
        let src = tmp("pw-src");
        std::fs::write(src.join("settings.json"), "{}").unwrap();
        std::fs::write(
            src.join("passwords.db"),
            "{\"format\":\"rustpy-passwords\"}",
        )
        .unwrap();
        std::fs::write(
            src.join("passwords.key"),
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        std::fs::write(src.join(".passwords.db.42.tmp"), "{}").unwrap();
        std::fs::create_dir_all(src.join("certs")).unwrap();
        std::fs::write(src.join("certs/ab.pem"), "-----BEGIN CERTIFICATE-----").unwrap();
        let out = src.join("export.json");
        let files = export_bundle(&src, &out).unwrap();
        assert_eq!(
            files,
            vec!["certs/ab.pem".to_string(), "settings.json".to_string()],
            "accepted certificates are settings; passwords are not"
        );
        // A hand-made bundle with a database is imported without it.
        std::fs::write(&out, "{\"format\":\"rustpy-settings\",\"files\":{\"passwords.db\":\"x\",\"passwords.key\":\"y\",\"a.json\":\"1\"}}").unwrap();
        let dst = tmp("pw-dst");
        assert_eq!(
            import_bundle(&dst, &out).unwrap(),
            vec!["a.json".to_string()]
        );
        assert!(!dst.join("passwords.db").exists() && !dst.join("passwords.key").exists());
        assert!(
            is_password_store("passwords.db")
                && is_password_store(".passwords.key.7.tmp")
                && is_password_store(".passwords.db.42.3.tmp")
        );
        assert!(
            is_password_store("passwords.db.lock"),
            "the lock file of changes from several windows"
        );
        assert!(!is_password_store("certs/passwords.db") && !is_password_store("my-passwords.db"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn rejects_foreign_files_and_path_traversal() {
        // One level down, so `..` lands in a directory only this test uses.
        let d = tmp("bad").join("config");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x.json"), "{\"format\":\"other\"}").unwrap();
        assert!(import_bundle(&d, &d.join("x.json")).is_err());
        std::fs::write(
            d.join("y.json"),
            "{\"format\":\"rustpy-settings\",\"files\":{\"a.json\":\"1\",\"../evil\":\"x\"}}",
        )
        .unwrap();
        assert!(import_bundle(&d, &d.join("y.json")).is_err());
        assert!(
            !d.join("a.json").exists(),
            "nothing is written when any path is unsafe"
        );
        assert!(!d.parent().unwrap().join("evil").exists());
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }

    /// Review finding: bundles carried installed plugins as text files, so a
    /// plugin's program lost its executable bit on import (the tool then
    /// failed with "Permission denied"), binary plugin files were dropped and
    /// an import over an installed plugin mixed two versions file by file.
    /// Plugins are not part of a settings bundle.
    #[test]
    fn plugins_are_neither_exported_nor_imported() {
        let src = tmp("plugins-src");
        std::fs::write(src.join("settings.json"), "{}").unwrap();
        std::fs::create_dir_all(src.join("plugins/com.x/bin")).unwrap();
        std::fs::write(src.join("plugins/com.x/plugin.json"), "{\"id\": \"com.x\"}").unwrap();
        std::fs::write(src.join("plugins/com.x/bin/run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(src.join("plugins/state.json"), "{}").unwrap();
        let out = src.join("export.json");
        assert_eq!(export_bundle(&src, &out).unwrap(), ["settings.json"]);
        assert!(!std::fs::read_to_string(&out).unwrap().contains("plugins/"));

        // A bundle that lists plugin files anyway: they are not written.
        let dst = tmp("plugins-dst");
        std::fs::create_dir_all(dst.join("plugins/com.x")).unwrap();
        std::fs::write(dst.join("plugins/com.x/plugin.json"), "installed").unwrap();
        let files = serde_json::json!({
            "settings.json": "{}",
            "plugins/com.x/plugin.json": "from the bundle",
            "plugins/com.y/plugin.json": "{}",
            "Plugins/com.z/plugin.json": "{}",
        });
        let bundle = dst.join("bundle.json");
        std::fs::write(
            &bundle,
            serde_json::json!({"format": FORMAT, "version": 1, "files": files}).to_string(),
        )
        .unwrap();
        assert_eq!(import_bundle(&dst, &bundle).unwrap(), ["settings.json"]);
        assert_eq!(
            std::fs::read_to_string(dst.join("plugins/com.x/plugin.json")).unwrap(),
            "installed"
        );
        // `Plugins` is checked by entry: on a case-insensitive file system the
        // directory itself aliases the legitimately installed `plugins`.
        assert!(!dst.join("plugins/com.y").exists() && !dst.join("Plugins/com.z").exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn reset_moves_settings_aside() {
        let d = tmp("reset");
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        let b = reset_to_defaults(&d).unwrap();
        assert!(!d.join("settings.json").exists());
        assert!(b.join("settings.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! Custom UI chrome themes. These are independent of editor color schemes.
//! A JSON document contains `name`, `base` and a `colors` object of #RRGGBB
//! overrides. Omitted colors inherit the base; unknown keys are errors.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "custom:";
const MAX_BYTES: u64 = 1024 * 1024;
pub const COLOR_KEYS: &[&str] = &[
    "panel_bg",
    "panel_bg_darker",
    "header_bg",
    "tab_bar_bg",
    "tab_active_bg",
    "tab_hover_bg",
    "tab_underline",
    "border",
    "text",
    "text_dim",
    "text_disabled",
    "accent",
    "selection_bg",
    "selection_inactive_bg",
    "hover_bg",
    "error",
    "warning",
    "success",
    "modified",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Base {
    /// Also accepts the legacy name written by earlier builds.
    #[serde(alias = "Darcula")]
    Dark,
    /// Also accepts the legacy name written by earlier builds.
    #[serde(alias = "IntelliJ Light")]
    Light,
    #[serde(rename = "High Contrast")]
    HighContrast,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub base: Base,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
}

pub fn rgb(value: &str) -> Result<[u8; 3], String> {
    if value.len() != 7
        || !value.starts_with('#')
        || !value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(format!("{value:?}: expected #RRGGBB"));
    }
    Ok([
        u8::from_str_radix(&value[1..3], 16).unwrap(),
        u8::from_str_radix(&value[3..5], 16).unwrap(),
        u8::from_str_radix(&value[5..7], 16).unwrap(),
    ])
}

impl Theme {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err("UI theme exceeds 1 MiB".into());
        }
        let theme: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("Invalid UI theme JSON: {e}"))?;
        theme.validate()?;
        Ok(theme)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.chars().any(char::is_control)
            || self.name.len() > 256
        {
            return Err(
                "Theme name must be nonempty, at most 256 bytes, without control characters".into(),
            );
        }
        for (key, value) in &self.colors {
            if !COLOR_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "Unknown UI color {key:?}; allowed keys: {}",
                    COLOR_KEYS.join(", ")
                ));
            }
            rgb(value).map_err(|e| format!("Color {key:?}: {e}"))?;
        }
        Ok(())
    }
}

fn read(path: &Path) -> Result<Theme, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Theme::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn directory(config: &Path) -> PathBuf {
    config.join("ui-themes")
}

pub fn path(config: &Path, id: &str) -> Result<PathBuf, String> {
    let file = id
        .strip_prefix(PREFIX)
        .ok_or("Not a custom UI theme identifier")?;
    let mut parts = Path::new(file).components();
    if file.is_empty()
        || file.contains(['/', '\\', ':'])
        || !file.ends_with(".json")
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
    {
        return Err("Custom UI theme identifier must contain one .json filename".into());
    }
    Ok(directory(config).join(file))
}

pub fn load(config: &Path, id: &str) -> Result<Theme, String> {
    let p = path(config, id)?;
    let root = dunce::canonicalize(directory(config))
        .map_err(|e| format!("{}: {e}", directory(config).display()))?;
    let resolved = dunce::canonicalize(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    if resolved.parent() != Some(root.as_path()) {
        return Err("UI theme file resolves outside the themes directory".into());
    }
    read(&resolved)
}

/// Validates first, then installs under an opaque unique filename. Display names
/// (including Unicode, slashes or duplicates) never become filesystem paths.
pub fn import(config: &Path, source: &Path) -> Result<String, String> {
    install(config, &read(source)?)
}

pub fn install(config: &Path, theme: &Theme) -> Result<String, String> {
    theme.validate()?;
    let bytes = serde_json::to_vec_pretty(theme).map_err(|e| e.to_string())?;
    let dir = directory(config);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for n in 1..=100_000 {
        let name = format!("theme-{n}.json");
        let file = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&bytes).and_then(|()| f.sync_all()) {
                    let _ = std::fs::remove_file(&file);
                    return Err(e.to_string());
                }
                return Ok(format!("{PREFIX}{name}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Too many installed UI themes".into())
}

/// Includes invalid files so the Appearance page can show actionable errors.
pub fn list(config: &Path) -> Vec<(String, Result<Theme, String>)> {
    let Ok(entries) = std::fs::read_dir(directory(config)) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.ends_with(".json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let id = format!("{PREFIX}{f}");
            let theme = load(config, &id);
            (id, theme)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Theme {
        Theme {
            name: "夜 / same name".into(),
            base: Base::Light,
            colors: [("accent".into(), "#Ab1234".into())].into(),
        }
    }

    #[test]
    fn validates_schema_colors_and_names() {
        assert_eq!(rgb("#Ab1234").unwrap(), [171, 18, 52]);
        for color in ["#fff", "#12345g", "#12345678", "#é1234", "red"] {
            assert!(rgb(color).is_err());
        }
        for json in [
            r##"{"name":"x","base":"Dark","colors":{"acccent":"#123456"}}"##,
            r#"{"name":"x","base":"Dark","editor":{}}"#,
            r#"{"name":"x","base":"Blue"}"#,
            r#"{"name":"","base":"Dark"}"#,
        ] {
            assert!(Theme::parse(json.as_bytes()).is_err(), "{json}");
        }
        assert!(Theme::parse(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
    }

    #[test]
    fn imports_colliding_unicode_names_without_overwriting_and_reloads_edits() {
        let dir = tempfile::tempdir().unwrap();
        let a = install(dir.path(), &sample()).unwrap();
        let b = install(dir.path(), &sample()).unwrap();
        assert_ne!(a, b);
        assert_eq!(load(dir.path(), &a).unwrap(), sample());
        let p = path(dir.path(), &a).unwrap();
        std::fs::write(
            &p,
            r##"{"name":"Edited","base":"Dark","colors":{"text":"#abcdef"}}"##,
        )
        .unwrap();
        assert_eq!(load(dir.path(), &a).unwrap().name, "Edited");
        std::fs::write(&p, "broken").unwrap();
        assert!(load(dir.path(), &a)
            .unwrap_err()
            .contains("Invalid UI theme JSON"));
        assert_eq!(list(dir.path()).len(), 2);
    }

    #[test]
    fn identifiers_cannot_escape_and_failed_imports_do_not_install() {
        let dir = tempfile::tempdir().unwrap();
        for id in [
            "custom:../a.json",
            "custom:/a.json",
            "custom:C:a.json",
            "custom:a\\b.json",
            "custom:.",
            "Dark",
        ] {
            assert!(path(dir.path(), id).is_err(), "{id}");
        }
        let p = dir.path().join("bad.json");
        std::fs::write(&p, "broken").unwrap();
        assert!(import(dir.path(), &p).is_err());
        assert!(!directory(dir.path()).exists());
    }

    #[test]
    fn settings_bundles_preserve_theme_files_and_selection() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let archive = tempfile::NamedTempFile::new().unwrap();
        let id = install(source.path(), &sample()).unwrap();
        let mut settings = crate::Settings::default();
        settings.appearance.theme = id.clone();
        std::fs::write(
            source.path().join("settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        crate::bundle::export_bundle(source.path(), archive.path()).unwrap();
        crate::bundle::import_bundle(target.path(), archive.path()).unwrap();
        let restored: crate::Settings =
            serde_json::from_slice(&std::fs::read(target.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(restored.appearance.theme, id);
        assert_eq!(
            load(target.path(), &restored.appearance.theme).unwrap(),
            sample()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_files_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), serde_json::to_vec(&sample()).unwrap()).unwrap();
        std::fs::create_dir(directory(dir.path())).unwrap();
        std::os::unix::fs::symlink(outside.path(), directory(dir.path()).join("escape.json"))
            .unwrap();
        assert!(load(dir.path(), "custom:escape.json")
            .unwrap_err()
            .contains("outside"));
    }
}

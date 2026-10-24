//! Named editor-macro bindings; recorded steps retain their legacy representation.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MacroShortcut {
    pub name: String,
    pub shortcut: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{persist, Settings};
    #[test]
    fn legacy_macros_load_without_bindings_and_named_binding_persists() {
        let old: Settings =
            serde_json::from_str(r#"{"macros":[["Unicode",["text:π\n","action:EditorLeft"]]]}"#)
                .unwrap();
        assert!(old.macro_shortcuts.is_empty());
        assert_eq!(old.macros[0].1[0], "text:π\n");
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        let (_, mut baseline, error) = persist::load_from(&path);
        assert!(error.is_none());
        let mut settings = old;
        settings.macro_shortcuts.push(MacroShortcut {
            name: "Unicode".into(),
            shortcut: "Ctrl+Alt+Shift+F12".into(),
        });
        persist::save_to(&path, &settings, &mut baseline).unwrap();
        let (loaded, _, error) = persist::load_from(&path);
        assert!(error.is_none());
        assert_eq!(loaded.macros, settings.macros);
        assert_eq!(loaded.macro_shortcuts, settings.macro_shortcuts);
    }
}

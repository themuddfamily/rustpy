//! Opt-in operating-system file panels. Missing providers fall back to the IDE browser.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinuxProvider {
    #[default]
    Auto,
    Zenity,
    Kdialog,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub file_chooser: bool,
    pub linux_provider: LinuxProvider,
    /// Explicit absolute user-selected provider paths; None retains system defaults.
    pub zenity_program: Option<std::path::PathBuf>,
    pub kdialog_program: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_native_settings_keep_system_provider_defaults_and_overrides_roundtrip() {
        let old: Settings =
            serde_json::from_str(r#"{"file_chooser":true,"linux_provider":"zenity"}"#).unwrap();
        assert!(old.zenity_program.is_none() && old.kdialog_program.is_none());
        let settings = Settings {
            zenity_program: Some("/opt/provider λ/zenity".into()),
            ..old
        };
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.zenity_program, settings.zenity_program);
        assert!(restored.file_chooser);
        assert_eq!(restored.linux_provider, LinuxProvider::Zenity);
    }
}

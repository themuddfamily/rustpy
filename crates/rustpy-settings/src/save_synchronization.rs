//! IDE-level automatic saving; defaults preserve explicit-save behavior.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveSynchronizationSettings {
    pub save_on_deactivation: bool,
    pub save_when_idle: bool,
    pub idle_seconds: u64,
    pub backup_before_save: bool,
    pub synchronize_on_frame_activation: bool,
    pub synchronize_on_tab_activation: bool,
}
impl Default for SaveSynchronizationSettings {
    fn default() -> Self {
        Self {
            save_on_deactivation: false,
            save_when_idle: false,
            idle_seconds: 30,
            backup_before_save: false,
            synchronize_on_frame_activation: true,
            synchronize_on_tab_activation: true,
        }
    }
}
impl SaveSynchronizationSettings {
    pub fn idle_delay(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.idle_seconds.clamp(1, 3600))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_default_and_roundtrip_preserve_explicit_options() {
        let old: crate::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            old.save_synchronization,
            SaveSynchronizationSettings::default()
        );
        let settings = SaveSynchronizationSettings {
            save_on_deactivation: true,
            save_when_idle: true,
            idle_seconds: 7,
            backup_before_save: true,
            synchronize_on_frame_activation: false,
            synchronize_on_tab_activation: false,
        };
        let mut original = old;
        original.save_synchronization = settings.clone();
        let loaded: crate::Settings =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(loaded.save_synchronization, settings);
        assert_eq!(
            SaveSynchronizationSettings {
                idle_seconds: 0,
                ..settings.clone()
            }
            .idle_delay()
            .as_secs(),
            1
        );
        assert_eq!(
            SaveSynchronizationSettings {
                idle_seconds: u64::MAX,
                ..settings
            }
            .idle_delay()
            .as_secs(),
            3600
        );
    }
}

//! Platform integration preferences: the renderer and operating-system limit warnings.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformSettings {
    /// Appearance › "Use software rendering": start with a CPU OpenGL implementation
    /// (Mesa llvmpipe on Linux) instead of the GPU driver. Takes effect on restart.
    pub software_rendering: bool,
    /// Warn when the file watcher hits the inotify watch limit ("Don't show again" clears it).
    pub warn_inotify_limit: bool,
}
impl Default for PlatformSettings {
    fn default() -> Self {
        Self {
            software_rendering: false,
            warn_inotify_limit: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_files_keep_hardware_rendering_and_the_warning() {
        let s: PlatformSettings = serde_json::from_str("{}").unwrap();
        assert!(!s.software_rendering);
        assert!(s.warn_inotify_limit);
    }
}

//! Thread Concurrency Visualization preferences, saved as IDE settings.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConcurrencySettings {
    pub threads: bool,
    pub asyncio: bool,
    pub locks: bool,
    pub project_only: bool,
    pub max_events: usize,
    pub max_lanes: usize,
    pub minimum_duration_ms: f64,
    pub follow_live: bool,
}
impl Default for ConcurrencySettings {
    fn default() -> Self {
        Self {
            threads: true,
            asyncio: true,
            locks: true,
            project_only: true,
            max_events: 100_000,
            max_lanes: 256,
            minimum_duration_ms: 0.0,
            follow_live: true,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_enable_bounded_source_correlated_capture() {
        let settings: ConcurrencySettings = serde_json::from_str("{}").unwrap();
        assert!(settings.threads && settings.asyncio && settings.locks && settings.project_only);
        assert_eq!(settings.max_events, 100_000);
        assert_eq!(
            serde_json::from_str::<ConcurrencySettings>(&serde_json::to_string(&settings).unwrap())
                .unwrap(),
            settings
        );
    }
}

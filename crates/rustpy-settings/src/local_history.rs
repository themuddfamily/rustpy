//! Local History is independent of Git and its commit/index settings.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalHistorySettings {
    pub enabled: bool,
    pub retention_days: u16,
    pub max_size_mb: u32,
    pub snapshots_per_file: u16,
}
impl Default for LocalHistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 7,
            max_size_mb: 512,
            snapshots_per_file: 100,
        }
    }
}

//! User-selected prebuilt bundle download preferences.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedIndexesSettings {
    pub url: String,
    pub sha256: String,
    pub max_download_mib: u32,
}
impl Default for SharedIndexesSettings {
    fn default() -> Self {
        Self {
            url: String::new(),
            sha256: String::new(),
            max_download_mib: 64,
        }
    }
}

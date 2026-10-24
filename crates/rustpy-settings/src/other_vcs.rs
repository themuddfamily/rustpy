//! Preferences for bundled Mercurial, SVN and Perforce adapters. No passwords are serialized.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RootSettings {
    pub root: PathBuf,
    pub endpoint: String,
    pub username: String,
    pub client: String,
    pub allow_auth_cache: bool,
    pub svn_ca_file: String,
    pub history_limit: u32,
    pub timeout_seconds: u32,
    pub hg_update_after_pull: bool,
}
impl Default for RootSettings {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            endpoint: String::new(),
            username: String::new(),
            client: String::new(),
            allow_auth_cache: false,
            svn_ca_file: String::new(),
            history_limit: 200,
            timeout_seconds: 300,
            hg_update_after_pull: false,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtherVcsSettings {
    pub roots: Vec<RootSettings>,
}
impl OtherVcsSettings {
    pub fn root(&self, path: &Path) -> RootSettings {
        self.roots
            .iter()
            .find(|entry| entry.root == path)
            .cloned()
            .unwrap_or_else(|| RootSettings {
                root: path.to_path_buf(),
                ..Default::default()
            })
    }
    pub fn put(&mut self, entry: RootSettings) {
        if let Some(existing) = self
            .roots
            .iter_mut()
            .find(|existing| existing.root == entry.root)
        {
            *existing = entry;
        } else {
            self.roots.push(entry);
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn provider_settings_have_no_serialized_secret_fields() {
        let text = serde_json::to_string(&super::OtherVcsSettings::default()).unwrap();
        assert!(!text.contains("password"));
        assert!(!text.contains("token"));
    }
}

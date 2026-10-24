//! IDE-owned executable plugin permissions and publisher authority.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const BUILTINS: &[(&str, &str)] = &[
    ("markdown", "Markdown / reStructuredText"),
    ("terminal", "Terminal"),
    ("git", "Git"),
    ("tfs", "TFS / TFVC adapter (requires configured tf.exe)"),
    ("github", "GitHub / GitLab"),
    ("docker", "Docker"),
    ("database", "Database"),
    ("jupyter", "Jupyter"),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub disabled_builtins: BTreeSet<String>,
    /// Plugin id -> SHA256 of manifest + module; updates require new consent.
    pub runtime_grants: BTreeMap<String, String>,
    /// Publisher id -> base64 Ed25519 verification key. Never project-owned.
    pub publishers: BTreeMap<String, String>,
    pub require_signatures: bool,
    pub vim_enabled: bool,
    pub scripting_interpreter: String,
}
impl Settings {
    pub fn builtin_enabled(&self, id: &str) -> bool {
        !self.disabled_builtins.contains(id)
    }
}

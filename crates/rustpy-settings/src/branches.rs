//! Branch workspace preferences. Identities include repository roots.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BranchPreferences {
    pub favorites: BTreeMap<String, BTreeSet<String>>,
    pub recent: BTreeMap<String, Vec<String>>,
    pub group_prefix: bool,
    pub group_repository: bool,
    pub show_details: bool,
    pub ask_checkout_rebase: bool,
}
impl Default for BranchPreferences {
    fn default() -> Self {
        Self {
            favorites: BTreeMap::new(),
            recent: BTreeMap::new(),
            group_prefix: true,
            group_repository: true,
            show_details: true,
            ask_checkout_rebase: true,
        }
    }
}
impl BranchPreferences {
    pub fn remember(&mut self, root: &str, branch: &str) {
        let recent = self.recent.entry(root.into()).or_default();
        recent.retain(|name| name != branch);
        recent.insert(0, branch.into());
        recent.truncate(20);
    }
    pub fn favorite(&self, root: &str, branch: &str) -> bool {
        self.favorites
            .get(root)
            .is_some_and(|names| names.contains(branch))
    }
    pub fn toggle_favorite(&mut self, root: &str, branch: &str) {
        let names = self.favorites.entry(root.into()).or_default();
        if !names.remove(branch) {
            names.insert(branch.into());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn favorites_are_repository_scoped_and_recents_bounded() {
        let mut p = BranchPreferences::default();
        p.toggle_favorite("/a", "topic");
        assert!(!p.favorite("/b", "topic"));
        for n in 0..30 {
            p.remember("/a", &format!("topic/{n}"));
        }
        p.remember("/a", "topic/29");
        assert_eq!(p.recent["/a"].len(), 20);
        assert_eq!(p.recent["/a"][0], "topic/29");
        assert_eq!(
            serde_json::from_str::<BranchPreferences>(&serde_json::to_string(&p).unwrap()).unwrap(),
            p
        );
    }
}

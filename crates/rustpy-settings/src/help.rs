//! Local-only onboarding preferences and bounded feature usage. No project names,
//! document contents, paths, keypresses or telemetry endpoints are recorded.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub shortcut_hints: bool,
    pub welcome_seen: bool,
    pub next_tip: usize,
    pub counts: BTreeMap<String, u64>,
    #[serde(skip)]
    baseline: BTreeMap<String, u64>,
    #[serde(skip)]
    reset: bool,
}
impl Preferences {
    pub fn load(path: &Path) -> Self {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            return Self::default();
        }
        let mut value: Self = std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        value.counts.retain(|id, _| valid_id(id));
        while value.counts.len() > 2048 {
            value.counts.pop_last();
        }
        value.baseline = value.counts.clone();
        value
    }
    pub fn observe(&mut self, id: &str) {
        if !valid_id(id) || (!self.counts.contains_key(id) && self.counts.len() >= 2048) {
            return;
        }
        let count = self.counts.entry(id.into()).or_default();
        *count = count.saturating_add(1);
    }
    pub fn reset_usage(&mut self) {
        self.counts.clear();
        self.reset = true;
    }
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        let _writer = WRITER.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut output = self.clone();
        if !self.reset {
            output.counts = Self::load(path).counts;
            for (id, count) in &self.counts {
                if output.counts.contains_key(id) || output.counts.len() < 2048 {
                    let increment =
                        count.saturating_sub(self.baseline.get(id).copied().unwrap_or(0));
                    let current = output.counts.entry(id.clone()).or_default();
                    *current = current.saturating_add(increment);
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.new");
        std::fs::write(
            &temporary,
            serde_json::to_vec_pretty(&output).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(temporary, path)?;
        self.counts = output.counts;
        self.baseline = self.counts.clone();
        self.reset = false;
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separate_windows_merge_increments_and_reset_persists() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("help.json");
        let mut first = Preferences::load(&path);
        let mut second = Preferences::load(&path);
        first.observe("GotoAction");
        first.save(&path).unwrap();
        second.observe("GotoAction");
        second.observe("GotoAction");
        second.save(&path).unwrap();
        first.observe("SaveDocument");
        first.save(&path).unwrap();
        let merged = Preferences::load(&path);
        assert_eq!(merged.counts["GotoAction"], 3);
        assert_eq!(merged.counts["SaveDocument"], 1);
        first.reset_usage();
        first.save(&path).unwrap();
        assert!(Preferences::load(&path).counts.is_empty());
        second.observe("GotoAction");
        second.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).counts["GotoAction"], 1);
    }
    #[test]
    fn only_stable_action_ids_are_counted() {
        let mut preferences = Preferences::default();
        preferences.observe("GotoAction");
        preferences.observe("/private/project.py");
        assert_eq!(preferences.counts.len(), 1);
        assert_eq!(preferences.counts["GotoAction"], 1);
    }
    #[test]
    fn counts_are_saturating_and_bounded() {
        let mut preferences = Preferences::default();
        preferences.counts.insert("SaveDocument".into(), u64::MAX);
        preferences.observe("SaveDocument");
        assert_eq!(preferences.counts["SaveDocument"], u64::MAX);
        for index in 0..3000 {
            preferences.observe(&format!("Action{index}"));
        }
        assert_eq!(preferences.counts.len(), 2048);
    }
}

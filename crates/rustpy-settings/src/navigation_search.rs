//! Local-only navigation feedback. Stores no source text or telemetry.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NavigationSearch {
    pub pinned: Vec<PathBuf>,
    pub accepted: BTreeMap<String, u32>,
    pub abbreviations: BTreeMap<String, String>,
    /// Online pairwise linear ranking weights: match, frequency, recency, pin.
    pub weights: [f32; 4],
}

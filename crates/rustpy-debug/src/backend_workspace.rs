//! Suspension-owned Python value views and multi-process debug metadata.
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
pub struct BackendWorkspace {
    pub subprocesses: Vec<Value>,
    pub labels: BTreeMap<String, String>,
    pub inline_watches: BTreeMap<String, (PathBuf, usize)>,
    pub label_input: String,
    pub value_view: Option<(String, String, String)>,
    pub data_view: Option<Value>,
    pub scientific_revision: u64,
    pub scientific_column_offset: usize,
    pub scientific_plots: Vec<Value>,
    pub data_expression: String,
    pub scientific_requested: bool,
    pub renderer: String,
    pub offset: usize,
    pub rows: usize,
    pub columns: usize,
    pub filter: String,
    pub sort_column: usize,
    pub heatmap: bool,
    pub number_precision: usize,
    pub slice: String,
    pub page_counts: HashMap<i64, usize>,
    pub page_more: HashSet<i64>,
    pub dependencies: HashSet<(PathBuf, usize)>,
    pub async_tasks: Option<String>,
    pub system_info: Option<Value>,
    pub pending_view: bool,
    pub force_step: bool,
    pub restore_mute: Option<bool>,
}

impl Default for BackendWorkspace {
    fn default() -> Self {
        Self {
            subprocesses: Vec::new(),
            labels: BTreeMap::new(),
            inline_watches: BTreeMap::new(),
            label_input: String::new(),
            value_view: None,
            data_view: None,
            scientific_revision: 0,
            scientific_column_offset: 0,
            scientific_plots: Vec::new(),
            data_expression: String::new(),
            scientific_requested: false,
            renderer: String::new(),
            offset: 0,
            rows: 100,
            columns: 100,
            filter: String::new(),
            sort_column: 0,
            heatmap: false,
            number_precision: 6,
            slice: String::new(),
            page_counts: HashMap::new(),
            page_more: HashSet::new(),
            dependencies: HashSet::new(),
            async_tasks: None,
            system_info: None,
            pending_view: false,
            force_step: false,
            restore_mute: None,
        }
    }
}

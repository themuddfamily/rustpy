//! Scientific workspace preferences; execution remains owned by consoles/debuggers.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Automatic,
    Scientific,
    Standard,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScientificSettings {
    pub show_plots: bool,
    pub layout: bool,
    pub mode: Mode,
    pub auto_refresh: bool,
    pub refresh_seconds: u64,
    pub page_rows: usize,
    pub page_columns: usize,
    pub number_format: String,
    pub color_map: String,
    pub heatmap: bool,
    pub max_plots: usize,
    pub capture_interactive: bool,
    pub documentation: bool,
    pub variables: bool,
    pub cell_separators: bool,
}
impl Default for ScientificSettings {
    fn default() -> Self {
        Self {
            show_plots: true,
            layout: false,
            mode: Mode::Automatic,
            auto_refresh: false,
            refresh_seconds: 2,
            page_rows: 100,
            page_columns: 50,
            number_format: ".6g".into(),
            color_map: "Viridis".into(),
            heatmap: false,
            max_plots: 64,
            capture_interactive: true,
            documentation: true,
            variables: true,
            cell_separators: true,
        }
    }
}

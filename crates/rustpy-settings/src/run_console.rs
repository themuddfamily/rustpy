//! Run console presentation; process configuration retains per-run overrides.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub buffer_kib: usize,
    pub font_size: f32,
    pub font_family: String,
    pub ansi_palette: Vec<[u8; 3]>,
    pub soft_wrap: bool,
    pub scroll_to_end: bool,
    pub confirm_rerun: bool,
    pub open_new_tab: bool,
    pub close_tabs_on_stop: bool,
    pub show_command_line: bool,
    pub fold_output: bool,
    pub fold_contains: Vec<String>,
    /// Regex captures `path` and optional `line`, or positional groups 1 and 2.
    pub link_filters: Vec<String>,
    pub custom_colors: bool,
    pub foreground: [u8; 3],
    pub stderr: [u8; 3],
    pub system: [u8; 3],
    pub background: [u8; 3],
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            buffer_kib: 4096,
            font_size: 13.0,
            font_family: String::new(),
            ansi_palette: Vec::new(),
            soft_wrap: true,
            scroll_to_end: true,
            confirm_rerun: true,
            open_new_tab: false,
            close_tabs_on_stop: false,
            show_command_line: true,
            fold_output: false,
            fold_contains: vec!["/site-packages/".into()],
            link_filters: Vec::new(),
            custom_colors: false,
            foreground: [220, 220, 220],
            stderr: [255, 100, 100],
            system: [160, 160, 160],
            background: [30, 30, 30],
        }
    }
}

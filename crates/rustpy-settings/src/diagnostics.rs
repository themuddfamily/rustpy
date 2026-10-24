//! Explicit diagnostic tools and local usage-statistics consent.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub collect_usage_statistics: bool,
    pub profiler_program: String,
    pub debugger_program: String,
    pub profile_seconds: u32,
    /// Help › Diagnostic Tools › Debug Log Settings: extra log categories, one
    /// per line (`rustpy_index`, `rustpy_debug:trace`); see [`crate::debug_log`].
    pub debug_log_categories: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            collect_usage_statistics: false,
            profiler_program: "samply".into(),
            debugger_program: if cfg!(windows) {
                "cdb"
            } else if cfg!(target_os = "macos") {
                "lldb"
            } else {
                "gdb"
            }
            .into(),
            profile_seconds: 10,
            debug_log_categories: String::new(),
        }
    }
}

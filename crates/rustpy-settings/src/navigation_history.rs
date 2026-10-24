use serde::{Deserialize, Serialize};
/// Navigation preferences; document history itself is local to its IDE window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NavigationHistorySettings {
    pub restore_tool_windows: bool,
    pub jump_lines: usize,
    pub recent_limit: usize,
}
impl Default for NavigationHistorySettings {
    fn default() -> Self {
        Self {
            restore_tool_windows: true,
            jump_lines: 10,
            recent_limit: 50,
        }
    }
}

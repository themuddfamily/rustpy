use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfilerSettings {
    pub engine: String,
    pub clock: String,
    pub threads: bool,
    pub builtins: bool,
    pub activate_view: bool,
    pub disable_server_reload: bool,
}
impl Default for ProfilerSettings {
    fn default() -> Self {
        Self {
            engine: "cprofile".into(),
            clock: "wall".into(),
            threads: true,
            builtins: true,
            activate_view: true,
            disable_server_reload: true,
        }
    }
}

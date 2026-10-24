//! Python coverage collection and reporting preferences.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoverageSettings {
    pub activate_view: bool,
    pub show_project_view: bool,
    pub branch: bool,
    pub bundled: bool,
    pub executable: String,
    pub pytest_cov: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub sum_tests: bool,
    pub collect_test_folders: bool,
    pub hide_complete: bool,
}
impl Default for CoverageSettings {
    fn default() -> Self {
        Self {
            activate_view: true,
            show_project_view: true,
            branch: true,
            bundled: true,
            executable: String::new(),
            pytest_cov: false,
            include: Vec::new(),
            exclude: Vec::new(),
            sum_tests: false,
            collect_test_folders: true,
            hide_complete: false,
        }
    }
}

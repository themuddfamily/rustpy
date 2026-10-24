use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilePolicy {
    Add,
    #[default]
    Ask,
    DoNotAdd,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub update_cleanup: u8,
    pub update_branch_default: bool,
    pub blame_author: bool,
    pub blame_date: bool,
    pub blame_revision: bool,
    pub blame_lines: bool,
    pub blame_age: bool,
    pub blame_short_names: bool,
    pub blame_ignore_whitespace: bool,
    pub annotate_title: bool,
    pub created: FilePolicy,
    pub removed: FilePolicy,
    pub update_all_roots: bool,
    pub update_submodules: bool,
    pub show_commands: bool,
    pub push_hooks: bool,
    pub update_after_rejected_push: bool,
    pub push_force_with_lease: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            update_cleanup: 0,
            update_branch_default: false,
            blame_author: true,
            blame_date: true,
            blame_revision: false,
            blame_lines: false,
            blame_age: true,
            blame_short_names: false,
            blame_ignore_whitespace: false,
            annotate_title: true,
            created: FilePolicy::Ask,
            removed: FilePolicy::Ask,
            update_all_roots: false,
            update_submodules: true,
            show_commands: true,
            push_hooks: true,
            update_after_rejected_push: false,
            push_force_with_lease: true,
        }
    }
}

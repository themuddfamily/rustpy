use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Highlight {
    #[default]
    Line,
    Word,
    Character,
    Split,
    None,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ignore {
    #[default]
    None,
    Leading,
    Trailing,
    All,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffMergeSettings {
    pub highlight_modified: bool,
    pub whitespace_changes: bool,
    pub unified: bool,
    pub soft_wrap: bool,
    pub highlight: Highlight,
    pub ignore: Ignore,
    pub synchronize: bool,
    pub collapse: bool,
    pub context: usize,
    pub align: bool,
    pub whitespace: bool,
    pub line_numbers: bool,
    pub next_file: bool,
    pub auto_nonconflicting: bool,
    pub enable_nonconflicting: bool,
    pub editor_tabs: bool,
    pub external_files: bool,
    pub external_folders: bool,
    pub diff_program: String,
    pub diff_arguments: String,
    pub merge_program: String,
    pub merge_arguments: String,
}
impl Default for DiffMergeSettings {
    fn default() -> Self {
        Self {
            highlight_modified: true,
            whitespace_changes: true,
            unified: false,
            soft_wrap: false,
            highlight: Highlight::Word,
            ignore: Ignore::None,
            synchronize: true,
            collapse: true,
            context: 3,
            align: true,
            whitespace: false,
            line_numbers: true,
            next_file: false,
            auto_nonconflicting: true,
            enable_nonconflicting: true,
            editor_tabs: false,
            external_files: false,
            external_folders: false,
            diff_program: String::new(),
            diff_arguments: "$LEFT$ $RIGHT$".into(),
            merge_program: String::new(),
            merge_arguments: "$LEFT$ $BASE$ $RIGHT$ $OUTPUT$".into(),
        }
    }
}

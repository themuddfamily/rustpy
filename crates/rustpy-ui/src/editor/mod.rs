//! The code editor: state, commands (pure logic, headless-testable) and the
//! egui view. Roadmap §1.

mod accessibility;
pub mod caret;
pub mod code_vision_view;
pub mod color_preview;
pub mod commands;
pub mod console_code;
pub mod diag_style;
pub mod doc_render;
pub mod emacs;
pub mod file_backups;
pub mod find;
pub mod gutter_actions;
pub mod hard_wrap;
pub(crate) mod ime;
pub mod indent_options;
mod join_lines;
pub(crate) mod language_injections;
pub mod line_commands;
pub mod method_separators;
pub mod navigation_scroll;
mod paste_format;
pub mod paste_indent;
pub(crate) mod preferences;
pub mod python_indent;
pub mod review_comments;
pub mod rich_copy;
pub mod save_actions;
mod scheme_effects;
pub mod session;
mod shared_views;
pub mod smart_end;
pub mod smart_keys;
pub mod soft_wrap;
pub mod state;
pub(crate) mod string_paste;
pub mod structure_moves;
pub(crate) mod svg_preview;
pub mod syntax_support;
#[cfg(test)]
mod tests;
pub mod text_folding;
pub mod textmate;
#[cfg(test)]
mod transform_tests;
pub(crate) mod unicode_edit;
pub(crate) mod unicode_view;
pub mod view;
mod virtual_space_view;
pub mod whitespace_marks;
pub mod word_selection;

pub use state::{Caret, EditOptions, EditorState, Language};

#[cfg(test)]
mod appearance_command_tests;

pub mod data_documents;

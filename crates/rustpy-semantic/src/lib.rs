//! RustPy IDE — semantic model: scopes, bindings, name resolution, module
//! index, typeshed stubs and type inference (roadmap §2.2).

pub mod basic_completion;
pub mod call_help;
pub mod calls;
pub mod classes;
pub mod completion2;
pub mod completion_ctx;
pub mod db;
pub mod deprecation;
pub mod doc_blocks;
pub mod docstrings;
pub mod edit_helpers;
pub mod endpoints;
pub mod enum_authority;
pub mod external_docs;
pub mod features;
pub mod flow;
pub mod hierarchy;
pub mod import_placement;
pub mod imports;
pub mod infer;
pub mod match_support;
pub mod module;
pub mod narrow;
pub mod native_skeletons;
pub mod native_state;
pub mod pytest_fixtures;
pub mod return_hints;
pub mod runtime_docs;
pub mod runtime_types;
pub mod scope;
pub mod static_cond;
mod stub_annotations;
pub mod stub_docs;
pub mod template_context;
pub mod template_jinja;
pub mod types;
pub mod typeshed;
pub mod usages;

pub use db::{Analysis, FileInfo};
pub use infer::{Definition, Inferer};
pub use types::Type;

pub use rustpy_core::{TextRange, TextSize};

/// Clamps an editor offset to the analysed text (the editor may be a keystroke
/// ahead of the analysis copy of a file).
pub fn clamp_offset(file: &db::FileInfo, offset: rustpy_core::TextSize) -> rustpy_core::TextSize {
    offset.min(rustpy_core::TextSize::of(&*file.text))
}

pub mod frameworks;

pub mod framework_templates;

pub mod framework_names;

pub mod cython;

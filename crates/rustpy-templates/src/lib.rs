//! RustPy IDE — live templates, surround-with templates and postfix
//! completion, and File and Code Templates (roadmap §1.7). Pure logic, no UI
//! dependencies.

pub mod file;
pub mod file_live;
pub mod live;
pub mod management;
pub mod postfix;

pub use live::{
    builtin_templates, expand, find, load_user_templates, matching, save_user_templates,
    surround_templates, ExpandContext, Expansion, LiveTemplate, TemplateContext, TemplateVariable,
};
pub use postfix::{
    apply_postfix, expression_before_dot, matching_postfix, postfix_templates, PostfixResult,
    PostfixTemplate,
};
pub use rustpy_core::{TextRange, TextSize};

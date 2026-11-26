//! RustPy IDE — the foundation of the egui front-end: theme and fonts,
//! localization, accessibility, actions and keymaps, text selection and the
//! system clipboard. `rustpy-ui` re-exports these modules under their old paths
//! (`rustpy_ui::theme`, `rustpy_ui::i18n`, …); they depend on no other UI module.

pub mod a11y;
#[doc(hidden)]
pub mod accessibility_policy;
pub mod actions;
pub mod fonts;
pub mod i18n;
#[doc(hidden)]
pub mod menu_snapshot;
#[doc(hidden)]
pub mod system_clipboard;
#[doc(hidden)]
pub mod test_support;
pub mod text_selection;
pub mod theme;
#[doc(hidden)]
pub mod tool_tabs;

#[cfg(test)]
unit_test_isolation!();

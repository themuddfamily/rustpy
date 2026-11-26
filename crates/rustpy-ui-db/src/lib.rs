//! RustPy IDE — the Database tool window (data sources, consoles, schema
//! browsing, result grids) and the diagrams drawn from schemas and models.
//! `rustpy-ui` re-exports these modules under their old paths; they build on
//! `rustpy-ui-base` only and keep the database drivers of `rustpy-db` out of the
//! other UI crates.

// The foundation modules under the paths the panels use (`crate::theme`, …).
use rustpy_ui_base::{a11y, i18n, system_clipboard, theme};

pub mod db_panel;
pub mod diagram_panel;

#[cfg(test)]
rustpy_ui_base::unit_test_isolation!();

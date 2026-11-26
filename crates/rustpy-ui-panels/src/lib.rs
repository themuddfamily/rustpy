//! RustPy IDE — tool window panels that need nothing of the application state
//! (`rustpy_ui::app`): terminal, services, deployment and secure connections, Git
//! jobs, coverage, profiler, concurrency, scientific view, other VCS, endpoints,
//! remote gateway, task trackers, shared index downloads, collaboration and the
//! web/Markdown preview server. `rustpy-ui` re-exports these modules under their
//! old paths; they build on `rustpy-ui-base` only.

// The foundation modules under the paths the panels use (`crate::theme`, …).
use rustpy_ui_base::{a11y, actions, i18n, system_clipboard, text_selection, theme, tool_tabs};

#[doc(hidden)]
pub mod collaboration;
#[doc(hidden)]
pub mod concurrency_panel;
pub mod coverage_panel;
pub mod deploy_ui;
pub mod endpoints_panel;
pub mod git_jobs;
#[doc(hidden)]
pub mod markdown_workspace;
pub mod netsec;
pub mod other_vcs_panel;
pub mod profiler_panel;
#[doc(hidden)]
pub mod remote_gateway;
mod scientific_export;
#[doc(hidden)]
pub mod scientific_panel;
pub mod services_panel;
#[doc(hidden)]
pub mod shared_index_download;
#[doc(hidden)]
pub mod task_trackers;
pub mod terminal_panel;
#[doc(hidden)]
pub mod web_preview_server;

#[cfg(test)]
rustpy_ui_base::unit_test_isolation!();

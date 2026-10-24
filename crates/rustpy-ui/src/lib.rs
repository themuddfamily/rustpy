//! RustPy IDE — egui/eframe front-end (roadmap §0.3, §1, §5).

pub mod a11y;
mod accessibility_announcements;
mod accessibility_palette;
mod accessibility_policy;
pub mod actions;
pub mod ai_panel;
pub mod app;
mod appearance_assets;
pub mod breakpoints;
mod collaboration;
mod concurrency_panel;
pub mod console_panel;
pub mod console_view;
mod console_workspace;
pub mod coverage_panel;
pub mod custom_menus;
pub mod db_panel;
pub mod debug_backend_workspace;
pub mod debug_console;
pub mod debug_editor;
pub mod debug_panel;
pub mod debug_protocol_workspace;
pub mod deploy_ui;
mod diagnostic_tools;
pub mod diagram_panel;
pub mod diff_view;
mod documentation_assets;
mod documentation_view;
pub mod dumb_mode;
pub mod editor;
pub mod endpoints_panel;
mod external_tool_settings;
mod external_tool_workspace;
pub mod external_tools;
pub mod fonts;
pub mod freeze;
pub mod git_jobs;
pub mod git_panel;
mod github_panel;
pub mod hierarchy_panel;
pub mod history_store;
pub mod http_panel;
pub mod i18n;
pub mod input_script;
pub mod launch;
pub mod learn_panel;
pub mod macos_open;
pub mod markdown_view;
mod markdown_workspace;
pub mod merge_tool;
pub mod nav_history;
pub mod netsec;
pub mod notebook_view;
pub mod notifications;
pub mod other_vcs_panel;
pub mod packages_panel;
pub mod parsed_field;
mod performance_trace;
pub mod plugin_host;
pub mod profiler_panel;
pub mod progress_ui;
pub mod project_tree;
pub mod refactor_preview;
mod remote_gateway;
pub mod run_anything;
pub mod run_config_dialog;
pub mod run_context;
pub mod run_panel;
pub mod scratches;
pub mod search_ui;
pub mod services_panel;
mod settings_archive;
mod status_text;
pub mod switcher;
pub mod symbol_index;
mod system_clipboard;
mod system_notifications;
pub mod terminal_panel;
#[cfg(test)]
mod test_isolation;
pub mod test_panel;
pub mod text_selection;
pub mod theme;
mod todo_patterns;
mod tool_tabs;
#[cfg(test)]
mod ui_snapshot;
// Public for the Windows acceptance helper (examples/windows_browser_acceptance.rs).
#[doc(hidden)]
pub mod web_browser_workspace;
mod web_preview_host;
#[doc(hidden)]
pub mod web_preview_server;

/// Run as `GIT_ASKPASS`/`SSH_ASKPASS` by a git process of the IDE: forwards
/// the prompt to the running IDE and returns the exit code (`None`: a normal
/// start). The binary calls it before anything else.
pub fn askpass_helper_main() -> Option<i32> {
    rustpy_vcs::askpass::helper_main()
}

/// Entry point used by the `rustpy` binary for the IDE itself: `args` are the
/// arguments after the program name (paths, `--line`/`--column`/`--wait` and
/// the smoke-test flags; see `rustpy_cli::args`).
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let cli = rustpy_cli::parse(args).map_err(|e| anyhow::anyhow!("{e}"))?;
    match cli.command {
        rustpy_cli::Command::Open(open) => launch::run_ide(open, &cli.global).map(|_| ()),
        rustpy_cli::Command::Diff(d) => merge_tool::run_diff(d).map(|_| ()),
        rustpy_cli::Command::Merge(m) => merge_tool::run_merge(m).map(|_| ()),
        _ => anyhow::bail!("not a command that opens a window"),
    }
}

pub mod stack_trace;

// Early headless process entrypoints used by the desktop launcher.
pub use rustpy_run::redirect_input::{
    helper_main as redirected_input_helper_main, register_host as register_run_host,
};
mod task_trackers;

mod scientific_export;
mod scientific_panel;

mod data_archive;
mod shared_index_download;

mod web_language_protocol;

//! RustPy IDE — process execution, output and execution tools
//! (roadmap §3.2 consoles). Project-owned models remain re-exported for API compatibility.

pub mod ansi;
pub use rustpy_project::config;
pub mod concurrency;
pub mod console_buffer;
pub mod coverage;
pub mod coverage_suites;
pub mod docs;
pub use rustpy_project::interpreter;
pub mod process;
mod process_tree;
pub mod redirect_input;
pub use process_tree::ProcessTree;
pub mod pty_owner;
pub use pty_owner::PtyOwner;
pub mod output_encoding;
pub mod process_owner;
pub mod profile;
pub mod profile_tools;
mod streamed;
pub mod typecollect;
#[cfg(windows)]
mod windows_pty;

pub use coverage::{
    coverage_available, load_json as load_coverage_json, run_with_coverage, CoverageReport,
    CoverageRun, FileCoverage, LineCoverage,
};
pub use process::{OutputChunk, OutputKind, RunSession, SessionStatus};
pub use profile::{
    load_json_lines as load_profile_json_lines, run_with_profiler, CallerRef, FunctionStats,
    ProfileReport, ProfileRun,
};
pub use rustpy_project::config::{RunConfig, RunConfigKind, RunConfigStore};
pub use rustpy_project::configuration_types::LegacyDockerBuildResources;
pub use rustpy_project::interpreter::{
    discover_interpreters, discover_interpreters_with, Interpreter,
};
pub use rustpy_project::interpreter_remote::{path_mapping_script, DockerResources};
pub use streamed::PythonCommand;

pub mod language_server;

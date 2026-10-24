//! RustPy project structure, interpreter discovery and run configuration persistence.

pub mod config;
pub mod configuration_types;
pub mod document_tools;
pub mod env;
pub mod environment_manager;
pub mod files;
pub mod framework_catalog;
pub mod interpreter;
#[cfg(all(test, unix))]
mod interpreter_provider_fixture;
pub mod interpreter_remote;
pub mod manager_layouts;
pub mod native_skeleton;
pub mod probe;
pub mod project;
pub mod project_templates;
pub mod run_workflow;
pub mod schema_store;
pub mod scope;
pub mod setup;
pub mod workspace;

pub use config::{RunConfig, RunConfigKind, RunConfigStore};
pub use interpreter::{discover_interpreters, discover_interpreters_with, Interpreter};
pub use project::{ProjectModel, RootKind};

pub use scope::ProjectScope;
pub use workspace::WorkspaceSnapshot;

pub mod sphinx;

pub mod data_formats;

pub mod data_schemas;
pub mod frameworks;
pub mod manage_py;

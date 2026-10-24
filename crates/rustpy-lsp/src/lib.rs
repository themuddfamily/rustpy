//! RustPy IDE — Language Server Protocol server (`rustpy lsp`, roadmap §8
//! "Command-line launcher").
//!
//! Exposes the IDE's Python intelligence to other editors over stdio:
//! diagnostics from syntax errors and inspections, completion, signature
//! help, hover (Quick Documentation), navigation, find usages, rename,
//! formatting, document/workspace symbols, folding, extend selection, code
//! actions (quick fixes, intentions, optimize imports), semantic tokens,
//! inlay hints and call hierarchy.
//!
//! The analysis is built the way the IDE builds it for a project (workspace
//! folders as content roots, marked Sources Roots, the interpreter's search
//! paths, version and platform) and honours the IDE's settings. Requests run
//! on worker threads and can be cancelled; nothing but protocol messages is
//! ever written to stdout (logs go to stderr and the IDE log file).

mod caps;
mod code_actions;
mod completion;
mod config;
mod diagnostics;
mod docs;
mod formatting;
pub mod gateway;
mod handlers;
mod line_index;
mod paths;
mod project;
mod semantic_tokens;
mod server;
mod snapshot;
mod symbols;
mod syntax;
mod watcher;

#[cfg(test)]
mod tests;

pub use server::{run, run_stdio, ServerOptions};

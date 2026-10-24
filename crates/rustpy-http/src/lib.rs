//! HTTP Client backend for `.http` / `.rest` files (roadmap §6).
//!
//! * [`parser`] turns a request file into [`HttpRequest`]s (`.http` format:
//!   `###` separators, `# @name` directives, headers, bodies, `< file` bodies,
//!   `> {% … %}` response handlers).
//! * [`env`] loads `http-client.env.json` / `http-client.private.env.json`
//!   environments and substitutes `{{variables}}` and `{{$builtins}}`.
//! * [`exec`] runs a request with `ureq` (blocking, or on a background thread).
//! * [`history`] appends executed requests to `.rustpy/http-history.log`.

pub mod env;
pub mod exec;
pub mod history;
pub mod parser;

pub use env::{builtin_variable, list_env_names, load_env, substitute};
pub use exec::{execute, execute_async, HttpResponse};
pub use history::{append_history, history_path, load_history, HistoryEntry};
pub use parser::{file_variables, parse, resolve_request, HttpRequest};

pub mod auth;
pub mod protocols;
pub mod runtime;
pub mod scripts;
pub mod workspace;

//! RustPy IDE — debugger: DAP client speaking to `debugpy` (roadmap §3.3).

pub mod backend_tasks;
pub mod backend_workspace;
mod bootstrap;
pub mod dap;
pub mod model;
mod port;
pub mod protocol;
pub mod session;

pub use model::{
    Breakpoint, ColumnEncoding, CompletionItem, Completions, DebugOptions, ExceptionBreakpoints,
    ExceptionClassBreakpoint, StepInTarget,
};
pub use session::{
    DebugSession, DebugState, EvalResult, LaunchSetup, ReplEntry, ReplItem, Scope, StackFrame,
    Variable, Watch,
};

mod legacy_pydevd;
mod legacy_wire;

pub mod scientific;

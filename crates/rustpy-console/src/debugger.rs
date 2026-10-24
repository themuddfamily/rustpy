//! Console execution in a suspended debugger frame. DAP retains transport,
//! frame selection, cancellation and variable-mutation ownership.
use crate::VALUE_ENGINE_SOURCE;

/// Evaluate using the same trailing-expression execution engine as the REPL.
/// The module is private to this evaluation, so debuggee imports and globals
/// are not polluted by the bundled helper implementation.
pub fn evaluate_in_console(session: &mut rustpy_debug::DebugSession, source: &str) {
    let engine = serde_json::to_string(VALUE_ENGINE_SOURCE).expect("string serialization");
    let code = serde_json::to_string(source).expect("string serialization");
    let wire = format!(
        "(lambda _rp, _rg, _rl: (__import__('builtins').exec({engine}, _rp), _rp['execute']({code}, _rg, _rl))[1])({{}}, globals(), locals())"
    );
    session.evaluate_console_expression(source, &wire);
}

//! Keep the complete inspection/inlay/diagnostic scan bounded after its
//! inspection stage moved out of the semantic crate. This binary has one
//! test, so inference counters are not shared with other tests.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use rustpy_semantic::{features, infer, Analysis};

/// The same ten-branch loop used by semantic's cycle-budget regression.
fn branchy_loop(k: usize, aug: bool) -> String {
    let mut src = String::from("def f(items, eol):\n    out = ''\n    for line in items:\n");
    for i in 0..k {
        src.push_str(&format!(
            "        {} line == '{i}':\n",
            if i == 0 { "if" } else { "elif" }
        ));
        src.push_str(&if aug {
            format!("            out += '{i}'\n")
        } else {
            format!("            out = out + '{i}'\n")
        });
    }
    src.push_str(if aug {
        "        else:\n            out += eol\n"
    } else {
        "        else:\n            out = out + eol\n"
    });
    src.push_str("    return out.upper()\n");
    src
}

#[test]
fn loop_carried_inspection_scans_do_not_recompute_every_order() {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut counts = Vec::new();
        for aug in [false, true] {
            let db = Analysis::for_project(Path::new("/proj"));
            let id = db.set_file_text(Path::new("/proj/m.py"), &branchy_loop(10, aug));
            let file = db.file(id).unwrap();
            let before = infer::DBG_BINDINGS.load(Ordering::Relaxed);
            rustpy_analysis::inspections::inspect(&db, &file, &Default::default());
            features::inlay_hints(&db, &file, true, true);
            features::diagnostics(&db, &file);
            counts.push(infer::DBG_BINDINGS.load(Ordering::Relaxed) - before);
        }
        send.send(counts).unwrap();
    });
    let counts = receive
        .recv_timeout(Duration::from_secs(30))
        .expect("inspection scans exceeded their bounded inference cost");
    for used in counts {
        assert!(
            used < 2_000,
            "{used} binding types computed by inspections, inlay hints and diagnostics"
        );
    }
}

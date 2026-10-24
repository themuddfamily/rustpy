//! Windows acceptance probe for interpreter discovery: the Windows registry,
//! the `py` launcher and WSL distributions. Run on a real Windows host through
//! `qa/windows/run-windows-acceptance.ps1`, which compares this report with what
//! Windows itself reports. Prints one JSON document on stdout.
//!
//! Usage: `windows_acceptance [--distro <name>]...` (default: every registered
//! WSL distribution).

use rustpy_project::interpreter::{discover_interpreters_with, windows_launcher_paths};
use rustpy_project::interpreter_remote::{
    registered_wsl_distributions, ProbeJob, RemoteCandidate, RemoteKind,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn main() {
    let mut distros: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--distro" {
            distros.extend(args.next());
        }
    }

    // Automatic discovery exactly as the IDE runs it with no project open.
    let started = Instant::now();
    let discovered: Vec<Value> = discover_interpreters_with(None, true)
        .into_iter()
        .map(|i| {
            json!({
                "path": i.path,
                "version": i.version,
                "kind": i.kind,
                "prefix": i.prefix,
                "is_venv": i.is_venv,
            })
        })
        .collect();
    let discovery_ms = started.elapsed().as_millis();

    // The parser's view of the launcher's own listing, for diagnosing mismatches.
    let launcher_listing = std::process::Command::new("py.exe")
        .arg("-0p")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let launcher_parsed = launcher_listing
        .as_deref()
        .map(windows_launcher_paths)
        .unwrap_or_default();

    let registrations = registered_wsl_distributions();
    if distros.is_empty() {
        if let Ok(list) = &registrations {
            distros = list.clone();
        }
    }

    // Each WSL distribution through the IDE's remote probe (the same path as
    // Add Interpreter › WSL), one at a time as the dialog does.
    let scratch = std::env::temp_dir().join("rustpy-windows-acceptance");
    let project = scratch.join("project");
    let _ = std::fs::create_dir_all(&project);
    let mut wsl = Vec::new();
    for distro in &distros {
        let candidate = RemoteCandidate {
            kind: RemoteKind::Wsl,
            target: distro.clone(),
            python: "python3".into(),
            project_root: project.clone(),
            docker: None,
            remote_root: None,
        };
        let started = Instant::now();
        let outcome = match ProbeJob::remote(candidate, scratch.join("adapters"), true) {
            Err(e) => Err(e),
            Ok(job) => job
                .receiver
                .recv_timeout(Duration::from_secs(90))
                .map_err(|_| "no result within 90 s".to_string())
                .and_then(|r| r),
        };
        let ms = started.elapsed().as_millis();
        wsl.push(match outcome {
            Ok(m) => json!({
                "distro": distro, "ok": true, "ms": ms,
                "version": m.interpreter.version,
                "path": m.interpreter.path,
                "platform": m.interpreter.platform,
                "prefix": m.interpreter.prefix,
                "sys_path_entries": m.interpreter.sys_path.len(),
            }),
            Err(e) => json!({"distro": distro, "ok": false, "ms": ms, "error": e}),
        });
    }

    let report = json!({
        "discovery_ms": discovery_ms,
        "discovered": discovered,
        "py_launcher_raw": launcher_listing,
        "py_launcher_parsed": launcher_parsed,
        "wsl_registrations": match &registrations {
            Ok(list) => json!(list),
            Err(e) => json!({"error": e}),
        },
        "wsl_probes": wsl,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

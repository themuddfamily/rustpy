//! Verification harness (not committed): runs inspect() (and optionally
//! diagnostics + inlay hints) on a list of files and prints every non-PEP 8
//! problem plus timings.
//!
//! corpus_probe <root> <search1:search2> <filelist> <out> [hints]
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use rustpy_analysis::inspections::{inspect, InspectionOptions};
use rustpy_semantic::db::Analysis;
use rustpy_semantic::module::ModuleResolver;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let search: Vec<PathBuf> = args[2]
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let list = std::fs::read_to_string(&args[3]).unwrap();
    let mut out = std::fs::File::create(&args[4]).unwrap();
    let hints = args.get(5).is_some_and(|a| a == "hints");
    let db = Analysis::new(ModuleResolver::new(vec![root.clone()], search, (3, 12)));
    let mut opts = InspectionOptions::default();
    if let Ok(only) = std::env::var("PROBE_ONLY") {
        opts = InspectionOptions {
            pep8: false,
            unused_imports: false,
            unused_variables: false,
            type_checker: false,
            security: false,
            compatibility: false,
            code_smells: false,
            naming: false,
            missing_docstrings: false,
            incorrect_docstrings: false,
            deprecation: false,
            ..Default::default()
        };
        match only.as_str() {
            "type" => opts.type_checker = true,
            "dep" => opts.deprecation = true,
            "smells" => opts.code_smells = true,
            "unused" => {
                opts.unused_imports = true;
                opts.unused_variables = true;
            }
            "doc" => opts.incorrect_docstrings = true,
            "compat" => opts.compatibility = true,
            "security" => opts.security = true,
            "naming" => opts.naming = true,
            _ => {}
        }
    }
    let mut total_cold = 0.0;
    let mut total_warm = 0.0;
    for line in list.lines() {
        let path = PathBuf::from(line.trim());
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let id = db.set_file_text(&path, &text);
        let file = db.file(id).unwrap();
        let t = Instant::now();
        let b0 = rustpy_semantic::infer::DBG_BINDINGS.load(std::sync::atomic::Ordering::Relaxed);
        let e0 = rustpy_semantic::infer::DBG_EXPRS.load(std::sync::atomic::Ordering::Relaxed);
        let problems = inspect(&db, &file, &opts);
        let cold = t.elapsed().as_secs_f64() * 1000.0;
        let b1 = rustpy_semantic::infer::DBG_BINDINGS.load(std::sync::atomic::Ordering::Relaxed);
        let e1 = rustpy_semantic::infer::DBG_EXPRS.load(std::sync::atomic::Ordering::Relaxed);
        writeln!(out, "COUNT bindings {} exprs {}", b1 - b0, e1 - e0).unwrap();
        // Warm: same text re-registered (new FileInfo, modules cached).
        let id = db.set_file_text(&path, &text);
        let file = db.file(id).unwrap();
        let t = Instant::now();
        let _ = inspect(&db, &file, &opts);
        let warm = t.elapsed().as_secs_f64() * 1000.0;
        total_cold += cold;
        total_warm += warm;
        if hints {
            let t = Instant::now();
            let _ = rustpy_semantic::features::diagnostics(&db, &file);
            let diag = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            let _ = rustpy_semantic::features::inlay_hints(&db, &file, true, true);
            let ih = t.elapsed().as_secs_f64() * 1000.0;
            writeln!(
                out,
                "FILE {} {:.1} {:.1} diag {:.1} hints {:.1}",
                path.display(),
                cold,
                warm,
                diag,
                ih
            )
            .unwrap();
        } else {
            writeln!(out, "FILE {} {:.1} {:.1}", path.display(), cold, warm).unwrap();
        }
        let line_of = |off: usize| text[..off.min(text.len())].matches('\n').count() + 1;
        for p in problems {
            let d = &p.diagnostic;
            let code = d.code.to_string();
            if code.starts_with("PEP8") {
                continue;
            }
            writeln!(
                out,
                "D {}\t{}\t{}\t{}",
                path.display(),
                line_of(usize::from(d.range.start())),
                code,
                d.message.replace('\n', " ")
            )
            .unwrap();
        }
        out.flush().unwrap();
        // An open stdlib source would shadow its typeshed stub for later files.
        db.close_file(&path);
    }
    writeln!(out, "TOTAL cold {:.1} warm {:.1}", total_cold, total_warm).unwrap();
}

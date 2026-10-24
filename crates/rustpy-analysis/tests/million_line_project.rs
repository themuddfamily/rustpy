//! Run once for the whole Performance item. This is a project workload, not a
//! claim that one editor document must eagerly inspect a million-line file.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_core::ProgressReporter;
use rustpy_semantic::{db::CacheLimits, Analysis};
use std::time::Instant;

#[test]
#[ignore = "one-million-line project workload; run explicitly for Performance verification"]
fn million_line_parallel_project_analysis_retains_all_diagnostics_with_a_small_disk_cache() {
    let directory = tempfile::TempDir::with_prefix("rustpy-million-line-project").unwrap();
    let mut source = String::from("from typing import Iterable\n\n");
    for index in 0..192 {
        source.push_str(&format!(
            "\nclass Worker_{index}:\n\
             \x20   value: int = 0\n\
             \x20   def __init__(self, value: int) -> None:\n\
             \x20       self.value = value\n\
             \x20   def apply(self, values: Iterable[int], enabled: bool) -> int:\n\
             \x20       result: int = self.value\n\
             \x20       for item in values:\n\
             \x20           if enabled and item > 0:\n\
             \x20               result += item\n\
             \x20           else:\n\
             \x20               result -= item\n\
             \x20       try:\n\
             \x20           if result < 0:\n\
             \x20               raise ValueError('negative')\n\
             \x20       except ValueError:\n\
             \x20           result = 0\n\
             \x20       finally:\n\
             \x20           result += 1\n\
             \x20       match enabled:\n\
             \x20           case True:\n\
             \x20               result += 2\n\
             \x20           case False:\n\
             \x20               result += 3\n\
             \x20       return result\n\
             \x20   def doubled(self) -> int:\n\
             \x20       return self.value * 2\n\
             \x20   @property\n\
             \x20   def label(self) -> str:\n\
             \x20       return str(self.value)\n\
             \n\
             def evaluate_{index}(value: int) -> int:\n\
             \x20   worker = Worker_{index}(value)\n\
             \x20   values: list[int] = [value, worker.doubled(), 2]\n\
             \x20   result = worker.apply(values, True)\n\
             \x20   if worker.label:\n\
             \x20       return result\n\
             \x20   return 0\n\
             \n\
             accepted_{index}: int = evaluate_{index}({index})\n\
             \n"
        ));
    }
    source.push_str("anchor = missing_λ\n");
    let lines = source.lines().count() * 128;
    assert!(lines >= 1_000_000, "{lines}");
    let actual_code_lines = source
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .count()
        * 128;
    assert!(actual_code_lines >= 900_000, "{actual_code_lines}");
    let mut paths = Vec::new();
    for index in 0..128 {
        let path = directory.path().join(format!("part_{index:03}.py"));
        std::fs::write(&path, &source).unwrap();
        paths.push(path);
    }
    let db = Analysis::for_project(directory.path());
    let limits = CacheLimits {
        max_files: 4,
        max_bytes: 512 * 1024,
    };
    db.set_cache_limits(limits);
    let options = InspectionOptions::default();
    let started = Instant::now();
    let problems =
        inspections::inspect_files(&db, &paths, &options, &ProgressReporter::new()).unwrap();
    assert_eq!(problems.len(), 128);
    let offset = source.find("missing_λ").unwrap();
    for (path, problems) in &problems {
        assert!(paths.contains(path));
        let unresolved: Vec<_> = problems
            .iter()
            .filter(|problem| problem.diagnostic.code == "PyUnresolvedReferences")
            .collect();
        assert_eq!(unresolved.len(), 1, "{}", path.display());
        assert_eq!(usize::from(unresolved[0].diagnostic.range.start()), offset);
        assert_eq!(
            usize::from(unresolved[0].diagnostic.range.len()),
            "missing_λ".len()
        );
    }
    let (files, bytes) = db.cached_disk_files();
    assert!(files <= limits.max_files, "{files}");
    assert!(bytes <= limits.max_bytes, "{bytes}");
    // Evicted files must remain available under their stable path/identity;
    // bounded retention cannot silently turn later inspection into no results.
    let repeat =
        inspections::inspect_files(&db, &paths[..1], &options, &ProgressReporter::new()).unwrap();
    assert_eq!(repeat.len(), 1);
    assert_eq!(
        repeat[0]
            .1
            .iter()
            .filter(|problem| problem.diagnostic.code == "PyUnresolvedReferences")
            .count(),
        1
    );
    eprintln!(
        "{lines} lines ({actual_code_lines} nonblank Python lines) / 128 files: {:?}; retained {files} disk files / {bytes} text bytes",
        started.elapsed()
    );
}

//! Profiler regression fixtures. Execution is deferred to final verification.
use rustpy_run::profile::CallerRef;
use rustpy_run::profile_tools;
use rustpy_run::{FunctionStats, ProfileReport};
fn function(name: &str, line: usize, own: f64, total: f64) -> FunctionStats {
    FunctionStats {
        file: "/project/module.py".into(),
        name: name.into(),
        line,
        tottime: own,
        cumtime: total,
        ncalls: 2,
        primitive_calls: 1,
        ..Default::default()
    }
}
#[test]
fn recursion_is_bounded_and_comparison_matches_source_identity() {
    let mut recursive = function("same", 1, 1.0, 3.0);
    recursive.callers.push(CallerRef {
        file: recursive.file.clone(),
        line: 1,
        name: "same".into(),
        ncalls: 1,
        cumtime: 2.0,
        ..Default::default()
    });
    let report = ProfileReport {
        functions: vec![recursive, function("same", 9, 2.0, 2.0)],
        total_time: 3.0,
        ..Default::default()
    };
    let tree = profile_tools::call_tree(&report, Some(0), 30, 100);
    assert_eq!(tree.len(), 2);
    assert!(tree[1].recursive);
    let baseline = ProfileReport {
        functions: vec![function("same", 1, 0.5, 1.0)],
        ..Default::default()
    };
    let delta = profile_tools::compare(&report, &baseline);
    assert_eq!(delta.len(), 2);
    assert_eq!(
        delta
            .iter()
            .find(|delta| delta.function.line == 1)
            .unwrap()
            .own_delta,
        0.5
    );
    assert_eq!(
        delta
            .iter()
            .find(|delta| delta.function.line == 9)
            .unwrap()
            .calls_delta,
        2
    );
}
#[test]
fn svg_exports_vector_call_graph_with_escaped_labels() {
    let root = tempfile::tempdir().unwrap();
    let report = ProfileReport {
        functions: vec![function("<script>&name", 1, 0.1, 0.1)],
        total_time: 0.1,
        ..Default::default()
    };
    let path = root.path().join("graph.svg");
    profile_tools::export_svg(&report, None, &path).unwrap();
    let svg = std::fs::read_to_string(path).unwrap();
    assert!(svg.contains("&lt;script&gt;&amp;name"));
    assert!(!svg.contains("<script>"));
}
#[test]
fn saved_standard_pstats_roundtrip_through_selected_interpreter() {
    let Ok(python) = which::which(if cfg!(windows) { "python" } else { "python3" }) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let report = ProfileReport {
        functions: vec![function("target", 7, 0.25, 0.5)],
        total_time: 0.25,
        total_calls: 2,
        ..Default::default()
    };
    let path = root.path().join("result.pstat");
    profile_tools::save_report(&report, &path, &python, root.path()).unwrap();
    let imported = profile_tools::import_report(&path, &python, root.path()).unwrap();
    let target = imported
        .functions
        .iter()
        .find(|function| function.name == "target")
        .unwrap();
    assert_eq!(target.ncalls, 2);
    assert_eq!(target.tottime, 0.25);
    assert_eq!(target.cumtime, 0.5);
}
#[test]
fn sampling_and_thread_metadata_remain_distinct_from_call_counts() {
    let source=concat!("##rustpy-profile## {\"file\":\"/p/main.py\",\"line\":1,\"name\":\"run\",\"ncalls\":3}\n","##rustpy-profile## {\"event\":\"summary\",\"engine\":\"vmprof\",\"sampling\":true,\"total_calls\":3,\"threads\":[{\"id\":2,\"name\":\"worker\",\"total_time\":0.2,\"schedules\":4}]}\n");
    let report = rustpy_run::profile::load_json_lines(source).unwrap();
    assert!(report.sampling);
    assert_eq!(report.engine, "vmprof");
    assert_eq!(report.threads[0].name, "worker");
}

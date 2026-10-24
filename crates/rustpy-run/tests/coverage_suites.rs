//! Regression fixtures; do not run during feature implementation.
use rustpy_run::coverage::{CoverageReport, FileCoverage, LineCoverage};
use rustpy_run::coverage_suites;
#[test]
fn complementary_suites_union_branch_arcs_and_do_not_double_count_statements() {
    let file = |executed, missing, executed_arcs, missing_arcs| FileCoverage {
        path: "/project/app.py".into(),
        executed,
        missing,
        executed_arcs,
        missing_arcs,
        partial: vec![1],
        num_branches: 2,
        covered_branches: 1,
        ..Default::default()
    };
    let reports = [
        CoverageReport {
            files: vec![file(vec![1, 2], vec![3], vec![(1, 2)], vec![(1, 3)])],
            branch: true,
            ..Default::default()
        },
        CoverageReport {
            files: vec![file(vec![1, 3], vec![2], vec![(1, 3)], vec![(1, 2)])],
            branch: true,
            ..Default::default()
        },
    ];
    let report = coverage_suites::merge(reports);
    assert_eq!(report.totals(), (3, 3));
    assert_eq!(report.branch_totals(), (2, 2));
    assert_eq!(report.files[0].line_state(1), Some(LineCoverage::Covered));
    assert_eq!(report.total_percent, 100.0);
}
#[test]
fn portable_export_roundtrips_and_html_escapes_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a&b.py");
    std::fs::write(&path, "print('<script>')\n").unwrap();
    let report = CoverageReport {
        files: vec![FileCoverage {
            path,
            executed: vec![1],
            percent: 100.0,
            ..Default::default()
        }],
        total_percent: 100.0,
        ..Default::default()
    };
    let export = root.path().join("suite.json");
    coverage_suites::export_json(&report, &export).unwrap();
    assert_eq!(
        rustpy_run::coverage::load_json(&std::fs::read_to_string(export).unwrap(), root.path())
            .unwrap(),
        report
    );
    let index = coverage_suites::export_html(&report, &root.path().join("html")).unwrap();
    assert!(index.is_file());
    let page = std::fs::read_to_string(index.parent().unwrap().join("file-0.html")).unwrap();
    assert!(page.contains("&lt;script&gt;"));
    assert!(!page.contains("print('<script>')"));
}
#[test]
fn actual_bundled_coverage_runs_without_requiring_an_installed_coverage_package() {
    let Ok(python) = which::which(if cfg!(windows) { "python" } else { "python3" }) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("branch.py");
    std::fs::write(
        &script,
        "def choose(flag):\n    if flag:\n        return 1\n    return 2\nchoose(True)\n",
    )
    .unwrap();
    let mut config = rustpy_project::RunConfig::for_script(&script);
    config.interpreter = Some(python);
    let mut run = rustpy_run::coverage::run_with_coverage_opts(
        9901,
        &config,
        None,
        root.path(),
        &Default::default(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    while run.is_running() && start.elapsed() < std::time::Duration::from_secs(30) {
        run.poll();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let report = run.report().expect("bundled runner produced coverage");
    let file = report.file(&script).unwrap();
    assert!(file.num_branches > 0);
    assert!(file.partial.contains(&2));
    assert!(file.missing.contains(&4));
}

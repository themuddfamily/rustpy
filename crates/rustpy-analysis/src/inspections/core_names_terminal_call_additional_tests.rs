use super::*;

#[test]
fn terminal_member_replacement_and_async_no_return_preserve_reachable_tail() {
    for source in [
        "import sys\nsys.exit = lambda *args: None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        sys.exit(1)\n    return value\n",
        "import sys\nimport sys as runtime\nruntime.exit = lambda *args: None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        sys.exit(1)\n    return value\n",
        "from typing import NoReturn\nasync def stop() -> NoReturn:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n    return value\n",
    ] {
        let names: Vec<_> = found(source, &run(source), "PyUnboundLocalVariable")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["value"], "{source}");
    }
}

#[test]
fn actual_project_sys_returning_exit_does_not_gain_stdlib_terminal_authority() {
    let root = tempfile::TempDir::with_prefix("inspection-terminal-project-sys").unwrap();
    let fake_sys = root.path().join("sys.py");
    let main = root.path().join("main.py");
    std::fs::write(&fake_sys, "def exit(code=0):\n    return code\n").unwrap();
    let source = "import sys\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        sys.exit(1)\n    return value\n";
    std::fs::write(&main, source).unwrap();
    let db = Analysis::for_project(root.path());
    let id = db.set_file_text(&main, source);
    let file = db.file(id).unwrap();
    let resolved = db.module("sys").unwrap();
    assert_eq!(
        resolved.path, fake_sys,
        "fixture must resolve the actual project module"
    );
    let problems = inspect(&db, &file, &InspectionOptions::default());
    let names: Vec<_> = found(source, &problems, "PyUnboundLocalVariable")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["value"]);
    assert_eq!(
        std::fs::read_to_string(fake_sys).unwrap(),
        "def exit(code=0):\n    return code\n"
    );
}

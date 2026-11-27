//! Unit tests never touch the user's configuration or clipboard (see
//! `rustpy_ui_base::test_support`).

rustpy_ui_base::unit_test_isolation!();

#[test]
fn settings_saves_go_to_a_private_directory_by_default() {
    use rustpy_ui_base::test_support::{config_dir, is_unit_test, isolated};
    let dir = rustpy_settings::Settings::config_dir();
    if isolated() {
        assert_eq!(dir, config_dir(std::process::id()));
    }
    let real = dirs::config_dir().map(|d| d.join(rustpy_settings::APP_DIR_NAME));
    assert_ne!(Some(dir), real, "tests must not write the user's settings");
    assert!(is_unit_test(), "the system clipboard stays out of reach");
}

/// Calls that create an owner which outlives its window until it has retired
/// and which sits in a process-wide registry meanwhile: terminal sessions,
/// debugger transports and the cleanup of a cancelled clone.
const PROCESS_WIDE_OWNERS: &[&str] = &[
    "open_command(",
    "terminal_panel.open(",
    "Terminal::spawn",
    "DebugSession::launch(",
    "DebugSession::attach",
    "DapClient::",
    "finish_abandoned_clone(",
];

/// The source ranges of test code in `text`: the whole file for a `*tests.rs`
/// module, otherwise each `#[cfg(test)] mod … { … }` block.
fn test_regions(name: &str, text: &str) -> Vec<std::ops::Range<usize>> {
    let mut regions = Vec::new();
    if name.ends_with("tests.rs") {
        regions.push(0..text.len());
        return regions;
    }
    let mut from = 0;
    while let Some(at) = text[from..].find("#[cfg(test)]") {
        let attribute = from + at;
        from = attribute + 1;
        let rest = text[attribute + "#[cfg(test)]".len()..].trim_start();
        if !rest.starts_with("mod ") && !rest.starts_with("pub(crate) mod ") {
            continue;
        }
        let Some(open) = text[attribute..].find('{').map(|i| attribute + i) else {
            continue;
        };
        if text[attribute..open].contains(';') {
            continue; // `mod name;`: the file itself is scanned
        }
        regions.push(open..matching_brace(text, open));
    }
    regions
}

/// The index just past the brace that closes the one at `open`.
fn matching_brace(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (i, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// Every `fn` body in `range` of `text`.
fn function_bodies(text: &str, range: std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
    let mut bodies = Vec::new();
    let mut from = range.start;
    while let Some(at) = text[from..range.end].find("fn ") {
        let start = from + at;
        from = start + 3;
        let boundary = start == 0 || !text.as_bytes()[start - 1].is_ascii_alphanumeric();
        let Some(open) = text[start..range.end].find(['{', ';']).map(|i| start + i) else {
            continue;
        };
        if boundary && text.as_bytes()[open] == b'{' {
            bodies.push(start..matching_brace(text, open));
        }
    }
    bodies
}

/// All unit tests share one process, and Quit, exit and close preparation in
/// every window count the process-wide owners that are still retiring. A test
/// that creates one therefore runs in its own process; otherwise a parallel
/// Quit test sees a foreign owner, asks for process consent or finds its
/// prepared guard changed, and fails at random.
#[test]
fn tests_that_create_process_wide_owners_run_in_their_own_process() {
    fn visit(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    visit(&src, &mut files);
    files.sort();
    let mut offenders = Vec::new();
    for path in files {
        if path.ends_with("test_isolation.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        for region in test_regions(&name, &text) {
            let bodies = function_bodies(&text, region.clone());
            for marker in PROCESS_WIDE_OWNERS {
                let mut from = region.start;
                while let Some(at) = text[from..region.end].find(marker) {
                    let position = from + at;
                    from = position + marker.len();
                    let isolated = bodies.iter().any(|body| {
                        body.contains(&position) && text[body.clone()].contains("in_own_process(")
                    });
                    if !isolated {
                        let line = text[..position].lines().count();
                        offenders.push(format!(
                            "{}:{line}: {marker}",
                            path.strip_prefix(&src).unwrap().display()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "create these owners only in a function that runs the test in its own process \
         (rustpy_ui_base::test_support::in_own_process):\n{}",
        offenders.join("\n")
    );
}

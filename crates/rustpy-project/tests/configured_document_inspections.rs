//! Actual supplemental document validators. Execute this one ignored case in
//! its own integration-test process with the central resource/retirement owner.
#![cfg(target_os = "linux")]

use rustpy_core::{tasks::TaskId, TaskManager};
use rustpy_project::document_tools::{self, Issue};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn admitted(name: &str, resolved_name: Option<&str>) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")));
    assert!(path.is_absolute() && path.is_file(), "invalid {name}");
    if let Some(program) = resolved_name {
        assert_eq!(
            which::which(program)
                .expect("admitted provider must be on captured PATH")
                .canonicalize()
                .unwrap(),
            path.canonicalize().unwrap(),
            "production PATH must select the explicitly admitted {program}"
        );
    }
    path
}

// document_tools owns its TaskHandles internally. This target has only one
// case, starts with no global jobs, and admits only jobs it synchronously
// requested. On unwind, cancel and drain those exact IDs before TempDir drops.
// TaskManager completion follows run()/supervise_tool_status returning, hence
// all actual provider children have retired before the files are removed.
#[derive(Default)]
struct OwnedValidators {
    ids: HashSet<TaskId>,
}
impl OwnedValidators {
    fn capture(&mut self) {
        self.ids.extend(
            TaskManager::global()
                .running_all()
                .into_iter()
                .filter(|task| task.title == "Checking document syntax")
                .map(|task| task.id),
        );
    }
    fn drained(&self) -> bool {
        self.ids
            .iter()
            .all(|id| !TaskManager::global().is_running(*id))
    }
    fn result(
        &mut self,
        source: &str,
        path: &Path,
        python: &Path,
        dialect: Option<&str>,
        revision: u64,
    ) -> Vec<Issue> {
        let before = document_tools::generation();
        let first =
            document_tools::inspect(source, path, Some(python), dialect, false, true, revision);
        self.capture();
        assert!(
            first.is_empty(),
            "fresh source/revision must not borrow a cached result"
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while document_tools::generation() == before || !self.drained() {
            assert!(
                Instant::now() < deadline,
                "actual validator did not reach terminal completion"
            );
            thread::sleep(Duration::from_millis(10));
        }
        document_tools::inspect(source, path, Some(python), dialect, false, true, revision)
    }
}
impl Drop for OwnedValidators {
    fn drop(&mut self) {
        self.capture();
        for id in &self.ids {
            TaskManager::global().cancel(*id);
        }
        while !self.drained() {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "requires explicit real Node/bash/selected Python with sqlglot; central isolated acceptance"]
fn configured_document_validators_publish_dirty_clean_and_authority_bound_results() {
    assert_eq!(
        std::env::var("RUSTPY_DOCUMENT_INSPECTIONS_ADMITTED").as_deref(),
        Ok("yes")
    );
    let _node = admitted("RUSTPY_INSPECTION_NODE", Some("node"));
    let _bash = admitted("RUSTPY_INSPECTION_BASH", Some("bash"));
    let python = admitted("RUSTPY_INSPECTION_PYTHON", None);
    assert!(
        TaskManager::global().running_all().is_empty(),
        "run this target alone"
    );
    let directory = tempfile::Builder::new()
        .prefix("document-inspections-λ-")
        .tempdir()
        .unwrap();
    let mut owner = OwnedValidators::default(); // drops before directory, including panic
    let cases = [
        (
            "script π.js",
            "const value = ;\n",
            "const value = 42;\n",
            None,
            "JavaScriptSyntax",
            1,
        ),
        (
            "script λ.bash",
            "if true; then\n",
            "if true; then printf '%s' 42; fi\n",
            None,
            "ShellSyntax",
            2, // Bash locates the missing `fi` at the following EOF line.
        ),
        (
            "query π.sql",
            "SELECT (1;\n",
            "SELECT 42;\n",
            Some("sqlite"),
            "SqlDialect",
            1,
        ),
        // This requires the actual selected SDK's sqlglot. Its missing-import
        // empty result fails the positive assertion; no silent prerequisite skip.
        (
            "query λ.sql",
            "SELECT (1;\n",
            "SELECT 42::INTEGER;\n",
            Some("postgresql"),
            "SqlDialect",
            1,
        ),
    ];
    for (index, (name, dirty, clean, dialect, code, expected_line)) in cases.into_iter().enumerate()
    {
        let path = directory.path().join(name);
        let revision = 100 + index as u64 * 10;
        std::fs::write(&path, dirty).unwrap();
        let generation = document_tools::generation();
        assert!(document_tools::inspect(
            dirty,
            &path,
            Some(&python),
            dialect,
            false,
            false,
            revision
        )
        .is_empty());
        assert_eq!(
            document_tools::generation(),
            generation,
            "untrusted request must not launch"
        );
        assert!(TaskManager::global().running_all().is_empty());

        let issues = owner.result(dirty, &path, &python, dialect, revision);
        assert!(
            issues.iter().any(|issue| issue.code == code
                && issue.line == expected_line
                && !issue.message.is_empty()),
            "{name}: actual {code} diagnostic at line {expected_line} absent: {issues:?}"
        );
        assert!(
            document_tools::inspect(dirty, &path, Some(&python), dialect, false, false, revision)
                .is_empty(),
            "trust revocation must hide cached diagnostics"
        );

        // New unsaved content cannot reuse the saved dirty cache or run against
        // different bytes on disk. Completion, rather than an immediate empty
        // return, establishes the negative control.
        assert!(owner
            .result(clean, &path, &python, dialect, revision + 1)
            .is_empty());
        std::fs::write(&path, clean).unwrap();
        assert!(
            owner
                .result(clean, &path, &python, dialect, revision + 2)
                .is_empty(),
            "{name}: corrected source not clean"
        );

        // A new project revision and restored original bytes must produce a
        // fresh real failure rather than borrow the previous clean result.
        std::fs::write(&path, dirty).unwrap();
        assert!(owner
            .result(dirty, &path, &python, dialect, revision + 3)
            .iter()
            .any(|issue| issue.code == code && issue.line == expected_line));
    }
    assert!(owner.drained());
}

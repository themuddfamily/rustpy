//! Explicit valid-source corpus gate; no downloads or interpreter execution.
use std::path::{Path, PathBuf};
fn collect(root: &Path, files: &mut Vec<PathBuf>) {
    for item in std::fs::read_dir(root).expect("read corpus root") {
        let item = item.unwrap();
        let kind = item.file_type().unwrap();
        if kind.is_symlink() {
            continue;
        }
        let path = item.path();
        if kind.is_dir() {
            collect(&path, files);
        } else if kind.is_file()
            && matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("py" | "pyi")
            )
        {
            files.push(path);
        }
    }
}
#[test]
#[ignore = "requires explicitly provisioned, valid-source corpus roots"]
fn declared_valid_corpora_are_lossless_and_error_free() {
    let roots = std::env::var_os("RUSTPY_VALID_CORPUS_DIRS").expect("set valid .py/.pyi roots; intentionally invalid CPython fixtures belong to the existing CPython-oracle gate");
    let mut files = Vec::new();
    for root in std::env::split_paths(&roots) {
        let metadata = std::fs::symlink_metadata(&root).expect("declared corpus root exists");
        assert!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "corpus root must be a real directory: {}",
            root.display()
        );
        let before = files.len();
        collect(&root, &mut files);
        assert!(
            files.len() > before,
            "declared corpus root has no regular .py/.pyi files: {}",
            root.display()
        );
    }
    files.sort();
    files.dedup();
    assert!(!files.is_empty(), "empty corpus is not success");
    let mut failures = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        let Ok(source) = std::str::from_utf8(&bytes) else {
            failures.push(format!(
                "{}: non-UTF8; use source-codec/CPython corpus gate",
                path.display()
            ));
            continue;
        };
        let parsed = rustpy_syntax::parse(source);
        if parsed.syntax_node().text() != source || !parsed.errors().is_empty() {
            failures.push(format!("{}: {:?}", path.display(), parsed.errors()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} failed:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

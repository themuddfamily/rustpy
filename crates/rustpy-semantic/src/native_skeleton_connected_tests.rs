use super::*;
use crate::native_skeletons::{Catalogue, InterpreterBinding, Record};
use crate::native_state::{NativeState, VerifiedNativeIndex};

fn state(path: &Path, name: &str, generation: u64) -> NativeState {
    let interpreter = InterpreterBinding {
        path: PathBuf::from("/trusted/python"),
        sha256: "a".repeat(64),
        version: "3.12.3".into(),
        cache_tag: "cpython-312".into(),
        soabi: "cpython-312-fixture".into(),
    };
    let record = Record {
        protocol: 1,
        module_name: name.into(),
        module_path: path.into(),
        module_sha256: "b".repeat(64),
        interpreter: interpreter.clone(),
        pyi: "def answer() -> int: ...\n".into(),
        unsupported: vec![],
    };
    NativeState {
        catalogue: Arc::new(
            Catalogue::from_records(true, generation, interpreter.clone(), vec![record]).unwrap(),
        ),
        index: VerifiedNativeIndex {
            trusted: true,
            generation,
            interpreter: Some(interpreter),
            modules: [((name.into(), path.into()), "b".repeat(64))]
                .into_iter()
                .collect(),
        },
    }
}

#[test]
fn publication_replaces_cached_any_and_revocation_does_not_mutate_snapshot() {
    let directory = tempfile::TempDir::with_prefix("rustpy-native-catalogue").unwrap();
    let path = directory.path().join("native.so");
    std::fs::write(&path, b"fixture only; never executed").unwrap();
    let db = Analysis::for_project(directory.path());
    assert!(db.module("native").unwrap().text.contains("__getattr__"));
    db.set_native_skeletons(state(&path, "native", 1));
    let generated = db.module("native").unwrap();
    assert!(generated.text.contains("def answer() -> int"));
    assert!(!generated.text.contains("__getattr__"));
    assert!(generated.is_stub);
    let frozen = db
        .snapshot(&[], &rustpy_core::CancellationToken::new())
        .unwrap();
    db.set_native_skeletons(NativeState::default());
    assert!(db.module("native").unwrap().text.contains("__getattr__"));
    assert_eq!(
        frozen.analysis().module("native").unwrap().text,
        generated.text
    );
    assert!(frozen.complete());
}

#[test]
fn supplied_stub_and_typeshed_keep_precedence_over_generated_native_members() {
    let directory = tempfile::TempDir::with_prefix("rustpy-native-stub-precedence").unwrap();
    let path = directory.path().join("native.pyd");
    std::fs::write(&path, b"fixture only; never executed").unwrap();
    std::fs::write(directory.path().join("native.pyi"), "supplied: str\n").unwrap();
    let db = Analysis::for_project(directory.path());
    db.set_native_skeletons(state(&path, "native", 1));
    assert_eq!(&*db.module("native").unwrap().text, "supplied: str\n");
    let math_path = directory.path().join("math.so");
    std::fs::write(&math_path, b"fixture only; never executed").unwrap();
    let db = Analysis::new(ModuleResolver::new(
        vec![],
        vec![directory.path().into()],
        (3, 12),
    ));
    db.set_native_skeletons(state(&math_path, "math", 1));
    let math = db.module("math").unwrap();
    assert!(math.path.to_string_lossy().contains("<typeshed>/math.pyi"));
    assert!(!math.text.contains("def answer() -> int"));
}

#[test]
fn mismatched_current_binary_fingerprint_falls_back_without_claiming_members() {
    let directory = tempfile::TempDir::with_prefix("rustpy-native-stale-catalogue").unwrap();
    let path = directory.path().join("native.so");
    std::fs::write(&path, b"fixture only; never executed").unwrap();
    let db = Analysis::for_project(directory.path());
    let mut stale = state(&path, "native", 1);
    *stale
        .index
        .modules
        .get_mut(&("native".into(), path))
        .unwrap() = "c".repeat(64);
    db.set_native_skeletons(stale);
    assert!(db.module("native").unwrap().text.contains("__getattr__"));
}

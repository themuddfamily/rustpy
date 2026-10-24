//! Docstrings of builtins and compiled standard-library modules read from an
//! interpreter (roadmap §2.10 "Quick Documentation sources"). Typeshed stubs
//! carry no docstrings, and `print`, `str.split` or `math.sqrt` have no
//! Python source to read them from, so `docs_dump.py` collects them with
//! `inspect` in the project interpreter. The dump runs once per interpreter
//! version and platform and is cached as JSON.

use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use rustpy_project::interpreter::Interpreter;

pub const DUMP_SOURCE: &str = include_str!("docs_dump.py");

/// Version of the dump's content: older caches are not used.
const FORMAT: u32 = 1;

/// Interpreters reached through a wrapper (SSH, Docker, WSL) are not run for
/// documentation: the dump would land on the other side, and Docker would
/// start a container.
pub fn supports(interpreter: &Interpreter) -> bool {
    !matches!(interpreter.kind.as_str(), "ssh" | "docker" | "wsl")
}

/// Cache file of the dump for `interpreter`: the standard library's docs
/// depend on the selected executable/build as well as version and platform.
pub fn cache_file_name(interpreter: &Interpreter) -> String {
    let clean = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    interpreter.path.hash(&mut identity);
    interpreter.prefix.hash(&mut identity);
    interpreter.kind.hash(&mut identity);
    interpreter.sys_path.hash(&mut identity);
    if let Ok(metadata) = std::fs::metadata(&interpreter.path) {
        metadata.len().hash(&mut identity);
        metadata.modified().ok().hash(&mut identity);
    }
    format!(
        "v{FORMAT}-python{}-{}-{:016x}.json",
        clean(&interpreter.version),
        clean(&interpreter.platform),
        identity.finish()
    )
}

/// The dump for `interpreter` from `cache_dir`, or a new one (then cached).
/// `None` when the interpreter cannot run the script.
pub fn load_or_dump(interpreter: &Interpreter, cache_dir: &Path) -> Option<String> {
    let name = cache_file_name(interpreter);
    let path = cache_dir.join(&name);
    if let Some(text) = read_dump(&path) {
        if valid_for(&text, interpreter) {
            return Some(text);
        }
    }
    if !supports(interpreter) {
        return None;
    }
    std::fs::create_dir_all(cache_dir).ok()?;
    // Unique per dump: two dumps for one version (interpreters switched back and
    // forth) never write the same file.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = cache_dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()));
    let text = dump_to(&interpreter.path, &tmp)
        .then(|| read_dump(&tmp))
        .flatten()
        .filter(|text| valid_for(text, interpreter));
    match &text {
        Some(_) => {
            let _ = std::fs::rename(&tmp, &path);
        }
        None => {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    text
}

const MAX_DUMP: usize = 32 * 1024 * 1024;
fn read_dump(path: &Path) -> Option<String> {
    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.is_file() || before.len() > MAX_DUMP as u64 {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_DUMP as u64 {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_DUMP as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= MAX_DUMP).then_some(text)
}
fn valid_for(text: &str, interpreter: &Interpreter) -> bool {
    looks_valid(text)
        && serde_json::from_str::<serde_json::Value>(text)
            .is_ok_and(|value| value["version"].as_str() == Some(interpreter.version.as_str()))
}

/// Runs the dump script with the interpreter at `python`, writing the JSON to
/// `out`: isolated mode (nothing from the project is imported), at most a minute.
pub fn dump_to(python: &Path, out: &Path) -> bool {
    let out = out.to_string_lossy();
    rustpy_project::probe::probe_output(
        python,
        &["-I", "-c", DUMP_SOURCE, &out],
        Duration::from_secs(60),
    )
    .is_some()
}

/// A complete dump of the current format (a truncated or older file is redone).
fn looks_valid(text: &str) -> bool {
    text.len() <= MAX_DUMP
        && serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| {
            v["format"].as_u64() == Some(u64::from(FORMAT))
                && v["docs"].as_object().is_some_and(|docs| {
                    docs.len() <= 32768
                        && docs.iter().all(|(name, doc)| {
                            name.len() <= 1024 && doc.as_str().is_some_and(|doc| doc.len() <= 32768)
                        })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn interpreter(kind: &str, version: &str) -> Interpreter {
        Interpreter {
            path: PathBuf::from("/usr/bin/python3"),
            version: version.into(),
            prefix: PathBuf::from("/usr"),
            sys_path: Vec::new(),
            is_venv: false,
            platform: "linux".into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn cache_names_and_remote_interpreters() {
        let first = interpreter("system", "3.12.3");
        assert!(cache_file_name(&first).starts_with("v1-python3.12.3-linux-"));
        let mut second = first.clone();
        second.path = PathBuf::from("/opt/other/python");
        assert_ne!(cache_file_name(&first), cache_file_name(&second));
        assert!(supports(&interpreter("venv", "3.12.3")));
        assert!(!supports(&interpreter("docker", "3.12.3")));
        assert!(!supports(&interpreter("ssh", "3.11.2")));
    }

    #[test]
    fn a_valid_cache_is_used_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut i = interpreter("ssh", "3.12.3");
        i.path = dir.path().join("missing-python");
        let cached = "{\"docs\": {\"builtins.len\": \"Return the number of items in a container.\"}, \"format\": 1, \"version\": \"3.12.3\"}";
        std::fs::write(dir.path().join(cache_file_name(&i)), cached).unwrap();
        assert_eq!(load_or_dump(&i, dir.path()).as_deref(), Some(cached));
        // An outdated or broken cache is not, and a remote interpreter is not run.
        for stale in [
            "{\"docs\": {}, \"format\": 0}",
            "{\"docs\": {\"builtins.len\": \"Return the",
            "{\"format\": 1, \"note\": \"}\"}",
        ] {
            std::fs::write(dir.path().join(cache_file_name(&i)), stale).unwrap();
            assert_eq!(load_or_dump(&i, dir.path()), None, "{stale}");
        }
    }

    #[test]
    fn dumps_with_a_real_interpreter() {
        let Ok(python) = which::which("python3") else {
            return;
        };
        let Some(i) = rustpy_project::interpreter::probe(&python, "system") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let text = load_or_dump(&i, dir.path()).expect("dump");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let docs = &v["docs"];
        assert!(
            docs["builtins.len"]
                .as_str()
                .is_some_and(|d| d.contains("number of items")),
            "{}",
            docs["builtins.len"]
        );
        assert!(docs["builtins.str.split"]
            .as_str()
            .is_some_and(|d| d.contains("substrings")));
        assert!(docs["math.sqrt"]
            .as_str()
            .is_some_and(|d| d.contains("square root")));
        assert!(docs["itertools.chain"].is_string() && docs["time.sleep"].is_string());
        // Cached: the second call reads the file.
        assert!(dir.path().join(cache_file_name(&i)).is_file());
        assert_eq!(load_or_dump(&i, dir.path()).as_deref(), Some(text.as_str()));
    }
    #[test]
    fn cache_rejects_other_version_and_oversized_doc_entries_without_launching_remote() {
        let dir = tempfile::tempdir().unwrap();
        let interpreter = interpreter("ssh", "3.12.3");
        let path = dir.path().join(cache_file_name(&interpreter));
        for value in [
            serde_json::json!({"format":1,"version":"3.11.0","docs":{}}),
            serde_json::json!({"format":1,"version":"3.12.3","docs":{"builtins.len":"x".repeat(32769)}}),
        ] {
            std::fs::write(&path, value.to_string()).unwrap();
            assert_eq!(load_or_dump(&interpreter, dir.path()), None);
        }
    }
}

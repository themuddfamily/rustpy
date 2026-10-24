//! Test-only provisioning isolates executable writers from sibling test spawns.
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_DIR: &str = "RUSTPY_PROVIDER_FIXTURE_CHILD_DIR";
const CHILD_NONCE: &str = "RUSTPY_PROVIDER_FIXTURE_CHILD_NONCE";
const ACK: &str = "RUSTPY_PROVIDER_FIXTURE_ACK ";
const PROVIDER: &str = "#!/bin/sh\nprintf '%s\n' '{\"version\":\"3.12.1\",\"prefix\":\"/external/env\",\"base_prefix\":\"/usr\",\"path\":[],\"platform\":\"linux\"}'\n";
const MALFORMED: &str = "#!/bin/sh\necho malformed\n";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn provisioning_child() {
    let Some(root) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let nonce = std::env::var(CHILD_NONCE).expect("explicit child nonce required");
    let root = std::fs::canonicalize(root).unwrap();
    let mut records = Vec::new();
    for (name, bytes) in [("provider", PROVIDER), ("malformed", MALFORMED)] {
        let path = root.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
        file.sync_all().unwrap();
        let held = file.metadata().unwrap();
        drop(file); // Close this child's writer before chmod, ACK and exit.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let published = std::fs::symlink_metadata(&path).unwrap();
        assert!(published.is_file());
        assert_eq!((held.dev(), held.ino()), (published.dev(), published.ino()));
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        records.push(serde_json::json!({"name": name, "dev": published.dev(),
            "ino": published.ino(), "size": published.len(), "bytes": bytes.as_bytes()}));
    }
    eprintln!(
        "{ACK}{}",
        serde_json::json!({"nonce": nonce,
        "pid": std::process::id(), "files": records})
    );
}

pub(crate) fn provision(root: &Path) {
    let root = std::fs::canonicalize(root).unwrap();
    let nonce = root.file_name().unwrap().to_string_lossy().into_owned();
    let owned = root.join("provisioning-child-environment");
    std::fs::create_dir(&owned).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "interpreter_provider_fixture::provisioning_child",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD_DIR, &root)
        .env(CHILD_NONCE, &nonce)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Preserve the loader search path needed by Cargo's immutable test binary;
    // no user config, display, trust or test-thread override is inherited.
    if let Some(value) = std::env::var_os("LD_LIBRARY_PATH") {
        command.env("LD_LIBRARY_PATH", value);
    }
    for (key, name) in [
        ("RUSTPY_CONFIG_DIR", "config"),
        ("RUSTPY_SYSTEM_DIR", "system"),
        ("RUSTPY_LOG_DIR", "log"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_CACHE_HOME", "xdg-cache"),
        ("XDG_DATA_HOME", "xdg-data"),
        ("XDG_STATE_HOME", "xdg-state"),
        ("XDG_RUNTIME_DIR", "xdg-runtime"),
    ] {
        let directory = owned.join(name);
        std::fs::create_dir(&directory).unwrap();
        command.env(key, directory);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut child = OwnedChild(command.spawn().unwrap());
    let original_pid = child.0.id();
    drop(child.0.stdin.take()); // Actual closed PIPE, not a null-device input.
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "owned provisioning helper timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    let mut errors = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    eprintln!("RUSTPY_PROVIDER_FIXTURE_CHILD pid={original_pid} status={status}\n{output}{errors}");
    assert!(status.success(), "owned provisioning helper failed");
    let records: Vec<_> = errors
        .lines()
        .filter_map(|line| line.strip_prefix(ACK))
        .collect();
    assert_eq!(records.len(), 1, "exactly one actual child ACK required");
    let record: serde_json::Value = serde_json::from_str(records[0]).unwrap();
    assert_eq!(record["nonce"].as_str(), Some(nonce.as_str()));
    assert_eq!(record["pid"].as_u64(), Some(u64::from(original_pid)));
    let files = record["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for (entry, (name, bytes)) in files
        .iter()
        .zip([("provider", PROVIDER), ("malformed", MALFORMED)])
    {
        let path = root.join(name);
        let actual = std::fs::symlink_metadata(&path).unwrap();
        assert!(actual.is_file());
        assert_eq!(entry["name"].as_str(), Some(name));
        assert_eq!(entry["dev"].as_u64(), Some(actual.dev()));
        assert_eq!(entry["ino"].as_u64(), Some(actual.ino()));
        assert_eq!(entry["size"].as_u64(), Some(actual.len()));
        assert_eq!(entry["bytes"], serde_json::json!(bytes.as_bytes()));
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        assert_eq!(actual.permissions().mode() & 0o777, 0o755);
    }
}

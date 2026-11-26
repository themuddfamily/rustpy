//! Support for the unit tests of the UI crates (`rustpy-ui` and the crates split
//! out of it). Each of them invokes [`unit_test_isolation!`](crate::unit_test_isolation)
//! under `cfg(test)`, so before any of its unit tests runs:
//!
//! * an unset `RUSTPY_CONFIG_DIR` points at a per-process directory under the temp
//!   directory (`$TMPDIR/rustpy-ui-tests/config-<pid>`), so every `Settings::save`
//!   and index write of a test stays private. A test, or the caller of `cargo
//!   test`, that sets `RUSTPY_CONFIG_DIR` explicitly keeps it;
//! * the process is marked as a unit-test process, which keeps the system
//!   clipboard out of reach ([`crate::system_clipboard`]).
//!
//! Outside unit tests nothing here runs: the IDE, integration tests and examples
//! are unaffected.
//!
//! It also holds the helpers that unit tests of several UI crates share:
//! [`TestDir`] and [`isolated_unlock_test`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the isolation chose the directory (the caller did not set one).
static ISOLATED: AtomicBool = AtomicBool::new(false);
/// Whether this is a unit-test process of a UI crate.
static UNIT_TEST: AtomicBool = AtomicBool::new(false);

/// Whether this process runs the unit tests of a UI crate.
pub fn is_unit_test() -> bool {
    UNIT_TEST.load(Ordering::SeqCst)
}

/// Whether the isolation chose the configuration directory of this process.
pub fn isolated() -> bool {
    ISOLATED.load(Ordering::SeqCst)
}

/// The shared parent of the per-process configuration directories.
fn root() -> PathBuf {
    std::env::temp_dir().join("rustpy-ui-tests")
}

/// The configuration directory of the test process `pid`.
pub fn config_dir(pid: u32) -> PathBuf {
    root().join(format!("config-{pid}"))
}

/// Removes the directories of test processes that are gone (Linux: no `/proc/<pid>`).
fn remove_stale(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix("config-"))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id()
            && cfg!(target_os = "linux")
            && !Path::new(&format!("/proc/{pid}")).exists()
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Run by [`unit_test_isolation!`](crate::unit_test_isolation) before `main`,
/// while the process is still single-threaded.
pub fn isolate_unit_test_process() {
    UNIT_TEST.store(true, Ordering::SeqCst);
    if std::env::var_os("RUSTPY_CONFIG_DIR").is_some_and(|d| !d.is_empty()) {
        return;
    }
    remove_stale(&root());
    std::env::set_var("RUSTPY_CONFIG_DIR", config_dir(std::process::id()));
    ISOLATED.store(true, Ordering::SeqCst);
}

/// Isolates the unit tests of the invoking crate (see the module documentation).
/// Invoke once, at the crate root, under `#[cfg(test)]`.
#[doc(hidden)]
#[macro_export]
macro_rules! unit_test_isolation {
    () => {
        extern "C" fn __rustpy_ui_isolate_unit_test_process() {
            $crate::test_support::isolate_unit_test_process();
        }
        // A static initializer (what the `ctor` crate does), so no test can save
        // before the isolation is in place.
        #[used]
        #[cfg_attr(
            any(target_os = "linux", target_os = "android", target_os = "freebsd"),
            link_section = ".init_array"
        )]
        #[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
        #[cfg_attr(windows, link_section = ".CRT$XCU")]
        static __RUSTPY_UI_ISOLATE_UNIT_TEST_PROCESS: extern "C" fn() =
            __rustpy_ui_isolate_unit_test_process;
    };
}

/// Runs global unlock-state writers in their own current test executable.
pub fn isolated_unlock_test(selector: &str) -> bool {
    const CHILD: &str = "RUSTPY_UNLOCK_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(selector) {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", selector, "--nocapture"]);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("RUSTPY_")
            || name.starts_with("XDG_")
            || matches!(
                name.as_ref(),
                "DISPLAY" | "WAYLAND_DISPLAY" | "MIR_SOCKET" | "RUST_TEST_THREADS"
            )
        {
            command.env_remove(key);
        }
    }
    for (key, name) in [
        ("RUSTPY_CONFIG_DIR", "config"),
        ("RUSTPY_SYSTEM_DIR", "system"),
        ("RUSTPY_LOG_DIR", "log"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        command.env(key, path);
    }
    let stdout = dir.path().join("stdout.log");
    let stderr = dir.path().join("stderr.log");
    command
        .env(CHILD, selector)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned unlock fixture wait failed: {selector}: {error}");
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned unlock fixture exceeded watchdog: {selector}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let out = std::fs::read_to_string(stdout).unwrap();
    let err = std::fs::read_to_string(stderr).unwrap();
    assert!(
        status.success() && out.contains("1 passed; 0 failed"),
        "owned exact fixture {selector}: {status}\n{out}\n{err}"
    );
    true
}

/// A scratch directory for tests, removed on drop.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(name: &str) -> TestDir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rustpy-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn settings_saves_go_to_a_private_directory_by_default() {
        let dir = rustpy_settings::Settings::config_dir();
        if super::isolated() {
            assert_eq!(dir, super::config_dir(std::process::id()));
        }
        let real = dirs::config_dir().map(|d| d.join(rustpy_settings::APP_DIR_NAME));
        assert_ne!(Some(dir), real, "tests must not write the user's settings");
        assert!(super::is_unit_test());
    }
}

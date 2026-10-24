//! Unit tests never touch the user's configuration: before any test runs, an
//! unset `RUSTPY_CONFIG_DIR` points at a per-process directory under the temp
//! directory (`$TMPDIR/rustpy-ui-tests/config-<pid>`), so every
//! `Settings::save` and index write of a test stays private. A test, or the
//! caller of `cargo test`, that sets `RUSTPY_CONFIG_DIR` explicitly keeps it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the isolation chose the directory (the caller did not set one).
static ISOLATED: AtomicBool = AtomicBool::new(false);

/// The shared parent of the per-process configuration directories.
fn root() -> PathBuf {
    std::env::temp_dir().join("rustpy-ui-tests")
}

/// The configuration directory of the test process `pid`.
pub(crate) fn config_dir(pid: u32) -> PathBuf {
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

extern "C" fn isolate_config_dir() {
    if std::env::var_os("RUSTPY_CONFIG_DIR").is_some_and(|d| !d.is_empty()) {
        return;
    }
    remove_stale(&root());
    // Runs before `main`, while the process is still single-threaded.
    std::env::set_var("RUSTPY_CONFIG_DIR", config_dir(std::process::id()));
    ISOLATED.store(true, Ordering::SeqCst);
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
static ISOLATE_CONFIG_DIR: extern "C" fn() = isolate_config_dir;

#[test]
fn settings_saves_go_to_a_private_directory_by_default() {
    let dir = rustpy_settings::Settings::config_dir();
    if ISOLATED.load(Ordering::SeqCst) {
        assert_eq!(dir, config_dir(std::process::id()));
    }
    let real = dirs::config_dir().map(|d| d.join(rustpy_settings::APP_DIR_NAME));
    assert_ne!(Some(dir), real, "tests must not write the user's settings");
}

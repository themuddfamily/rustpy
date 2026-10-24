//! A killed or crashed IDE must not leave its run processes (and their
//! children) running, re-parented to init. A helper copy of this test binary
//! starts a run through the real run API, the run spawns a grandchild, and the
//! helper is then killed.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rustpy_run::RunSession;

const HELPER: &str = "RUSTPY_ORPHAN_HELPER_OUT";

/// Helper mode only: start `sh -c 'sleep 300 & …; wait'` as a run session,
/// publish the grandchild PID, then keep the session alive until killed.
#[test]
fn orphan_helper_process() {
    let Some(out) = std::env::var_os(HELPER) else {
        return;
    };
    rustpy_core::orphan_guard::install_exit_signal_handlers();
    let out = PathBuf::from(out);
    let script = format!(
        "sleep 300 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; wait",
        out.display()
    );
    let mut session = RunSession::spawn_command(
        1,
        "orphan",
        Path::new("/bin/sh"),
        &["-c".into(), script],
        Path::new("/"),
        &BTreeMap::new(),
    );
    loop {
        session.poll();
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    // A zombie awaiting its (new) parent's reap is no longer running.
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let rest = stat.get(stat.rfind(')')? + 1..)?.trim_start();
            rest.chars().next()
        })
        .is_none_or(|state| !matches!(state, 'Z' | 'X'))
}

fn grandchild_dies_when_helper_gets(signal: i32) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("grandchild.pid");
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "orphan_helper_process",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER, &out)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&out)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            break pid;
        }
        if Instant::now() > deadline || helper.try_wait().unwrap().is_some() {
            let _ = helper.kill();
            let _ = helper.wait();
            panic!("helper did not start its run session");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(alive(pid));
    unsafe { libc::kill(helper.id() as i32, signal) };
    helper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = alive(pid);
    if survived {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(
        !survived,
        "grandchild {pid} of a run survived its IDE (signal {signal})"
    );
}

#[test]
fn run_grandchild_dies_when_ide_is_killed() {
    grandchild_dies_when_helper_gets(libc::SIGKILL);
}

#[test]
fn run_grandchild_dies_when_ide_is_terminated() {
    grandchild_dies_when_helper_gets(libc::SIGTERM);
}

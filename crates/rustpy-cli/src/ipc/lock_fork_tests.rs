//! A real child inherits the locked open-file description before exec closes it.
//! Gate only async-signal-safe syscalls; never sleep or retry the lock probe.
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut descriptors = [-1; 2];
    // SAFETY: valid output array; every returned descriptor becomes owned once.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    // SAFETY: pipe2 succeeded and neither descriptor has another Rust owner.
    unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_completed(worker: &JoinHandle<io::Result<OwnedChild>>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !worker.is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    worker.is_finished()
}

fn child_completed(child: &mut Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) => std::thread::yield_now(),
        }
    }
    false
}

struct GatedSpawn {
    release: Option<OwnedFd>,
    spawned: Option<JoinHandle<io::Result<OwnedChild>>>,
}

impl GatedSpawn {
    fn release(&mut self) {
        if let Some(writer) = self.release.take() {
            let byte = [1u8];
            // SAFETY: a live owned pipe and one readable byte. Send a release
            // byte rather than relying on EOF from unrelated transient forks.
            let _ = unsafe { libc::write(writer.as_raw_fd(), byte.as_ptr().cast(), 1) };
            drop(writer);
        }
    }
}

impl Drop for GatedSpawn {
    fn drop(&mut self) {
        // Release on every assertion/error unwind, then reap only the Child
        // returned by our spawn worker. Never kill an unguarded numeric PID.
        self.release();
        if let Some(worker) = self.spawned.take() {
            // Join only after bounded completion. If a stalled worker is
            // detached, its eventual result still owns cleanup via OwnedChild.
            if spawn_completed(&worker) {
                let _ = worker.join();
            }
        }
    }
}

fn announced_pid(fd: RawFd) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut bytes = [0u8; std::mem::size_of::<libc::pid_t>()];
    let mut count = 0;
    while count < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "pre-exec child did not announce its actual PID"
        );
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; bounded timeout fits the platform integer.
        let ready =
            unsafe { libc::poll(&mut descriptor, 1, remaining.as_millis().min(15_000) as i32) };
        assert!(ready > 0, "pre-exec announcement failed or timed out");
        // SAFETY: writable remainder of the owned byte array and live pipe fd.
        let read =
            unsafe { libc::read(fd, bytes[count..].as_mut_ptr().cast(), bytes.len() - count) };
        assert!(read > 0, "pre-exec announcement pipe closed or failed");
        count += read as usize;
    }
    let pid = libc::pid_t::from_ne_bytes(bytes);
    assert!(pid > 0);
    pid as u32
}

#[test]
fn fork_exec_child() {}

#[test]
fn dropping_instance_lock_releases_it_while_real_child_is_gated_before_exec() {
    let directory = tempfile::tempdir().unwrap();
    let Probe::Free(lock) = probe(directory.path()).unwrap() else {
        panic!("fresh directory")
    };
    assert!(matches!(probe(directory.path()).unwrap(), Probe::Running));
    // CLOEXEC protects exec, not the actual fork-before-exec interval.
    // SAFETY: descriptor is owned and alive throughout this observation.
    let flags = unsafe { libc::fcntl(lock._file._file.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    let (announcement_read, announcement_write) = pipe();
    let (release_read, release_write) = pipe();
    let announce_fd = announcement_write.as_raw_fd();
    let release_fd = release_read.as_raw_fd();
    let inherited_release_writer = release_write.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "ipc::lock_fork_tests::fork_exec_child",
            "--nocapture",
        ])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the child callback performs only close/getpid/write/poll/read, all
    // async-signal-safe. It allocates nothing and touches no mutex/Rust IO.
    unsafe {
        command.pre_exec(move || {
            libc::close(inherited_release_writer);
            let pid = libc::getpid();
            let bytes = pid.to_ne_bytes();
            let mut written = 0;
            while written < bytes.len() {
                let count = libc::write(
                    announce_fd,
                    bytes[written..].as_ptr().cast(),
                    bytes.len() - written,
                );
                if count <= 0 {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                written += count as usize;
            }
            let mut descriptor = libc::pollfd {
                fd: release_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if libc::poll(&mut descriptor, 1, 15_000) <= 0 {
                return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            let mut byte = [0u8; 1];
            if libc::read(release_fd, byte.as_mut_ptr().cast(), 1) != 1 || byte[0] != 1 {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            Ok(())
        });
    }
    let mut gated = GatedSpawn {
        release: Some(release_write),
        spawned: Some(std::thread::spawn(move || {
            // Keep these owned descriptors alive until spawn has completed.
            let result = command.spawn().map(OwnedChild);
            drop(announcement_write);
            drop(release_read);
            result
        })),
    };
    let pid = announced_pid(announcement_read.as_raw_fd());
    let status = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = status[status.rfind(')').unwrap() + 2..]
        .split_whitespace()
        .collect();
    assert_eq!(
        fields[1].parse::<u32>().unwrap(),
        std::process::id(),
        "gate must belong to our actual fork child"
    );
    assert!(fields[0] != "Z", "gated child must remain alive");
    drop(lock);
    // The same immediate single-probe contract as the original regression.
    // This MUST happen while the real child still holds inherited descriptors.
    assert!(
        matches!(probe(directory.path()).unwrap(), Probe::Free(_)),
        "free again, as after a crash; real pre-exec child PID={pid}"
    );
    gated.release();
    let worker = gated.spawned.as_ref().unwrap();
    assert!(
        spawn_completed(worker),
        "released child did not complete exec/spawn"
    );
    let mut child = gated.spawned.take().unwrap().join().unwrap().unwrap();
    assert_eq!(child.0.id(), pid);
    drop(child.0.stdin.take());
    assert!(
        child_completed(&mut child.0),
        "executed child did not exit within15s"
    );
    // try_wait observed and reaped termination, so wait reads its cached status.
    assert!(child.0.wait().unwrap().success());
}

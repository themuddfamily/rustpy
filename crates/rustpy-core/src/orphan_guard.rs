//! Owned process groups must not outlive the IDE when it is killed or crashes
//! (a normal exit already stops them through [`crate::process_tree`]).
//!
//! Three layers, all Unix-only (Windows Jobs are kill-on-close already):
//! - a watchdog process forked on first use (or by [`start`], early, while the
//!   process is still small: the fork keeps every page it shares with the IDE
//!   at that moment alive until the IDE exits) holds the read end of a pipe whose
//!   only writer is this process. Every live owned group is announced on it;
//!   when this process dies for any reason, even SIGKILL, the pipe reaches EOF
//!   and the watchdog sends SIGTERM to the remaining groups, then SIGKILL after
//!   a short grace. It runs in its own session, so terminal signals aimed at the
//!   IDE do not stop it first. This also covers macOS, which has no PDEATHSIG.
//! - [`install_exit_signal_handlers`]: SIGTERM/SIGINT/SIGHUP forward SIGTERM to
//!   the owned groups before the IDE dies with the original signal.
//! - Linux: a group leader spawned from the main thread gets PR_SET_PDEATHSIG,
//!   so the kernel kills it even if the watchdog itself was killed. (PDEATHSIG
//!   follows the forking *thread*, so it is not used for worker-thread spawns.)
//!
//! A group is announced only while its waitable leader pins the PGID, and is
//! withdrawn as soon as the leader is reaped, so a recycled PGID is never hit.

#[cfg(unix)]
pub use unix::{
    install_exit_signal_handlers, register, set_parent_death_signal, start, unregister,
};

#[cfg(not(unix))]
pub fn start() {}
#[cfg(not(unix))]
pub fn register(_group: u32) {}
#[cfg(not(unix))]
pub fn unregister(_group: u32) {}
#[cfg(not(unix))]
pub fn install_exit_signal_handlers() {}
#[cfg(not(unix))]
pub fn set_parent_death_signal(_command: &mut std::process::Command) {}

#[cfg(unix)]
mod unix {
    use std::io::Write;
    use std::os::unix::io::FromRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Mutex;

    /// Groups known to the signal handler (async-signal-safe: atomics only).
    const SLOTS: usize = 1024;
    static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];
    /// Write end of the watchdog pipe; `None` until first use or if it failed.
    static WATCHDOG: Mutex<Option<Option<std::fs::File>>> = Mutex::new(None);

    pub fn register(group: u32) {
        let group = group as i32;
        if group <= 1 {
            return;
        }
        for slot in &GROUPS {
            if slot
                .compare_exchange(0, group, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        announce(b'+', group);
    }
    pub fn unregister(group: u32) {
        let group = group as i32;
        if group <= 1 {
            return;
        }
        for slot in &GROUPS {
            let _ = slot.compare_exchange(group, 0, Ordering::AcqRel, Ordering::Acquire);
        }
        announce(b'-', group);
    }
    /// Forks the watchdog now rather than on the first owned group. A fork
    /// shares the parent's memory copy-on-write, so pages the IDE frees or
    /// rewrites later stay allocated in the watchdog: forked after indexing,
    /// it held hundreds of megabytes of the IDE's old heap.
    pub fn start() {
        let mut watchdog = WATCHDOG.lock().unwrap_or_else(|error| error.into_inner());
        if watchdog.is_none() {
            *watchdog = Some(start_watchdog());
        }
    }
    fn announce(sign: u8, group: i32) {
        let mut watchdog = WATCHDOG.lock().unwrap_or_else(|error| error.into_inner());
        if watchdog.is_none() {
            *watchdog = Some(start_watchdog());
        }
        if let Some(Some(pipe)) = watchdog.as_mut() {
            // One short line per write: atomic below PIPE_BUF.
            let _ = pipe.write_all(format!("{}{group}\n", sign as char).as_bytes());
        }
    }

    fn cloexec_pipe() -> Option<[i32; 2]> {
        let mut fds = [0; 2];
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
        let rc = unsafe {
            let rc = libc::pipe(fds.as_mut_ptr());
            if rc == 0 {
                libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
            }
            rc
        };
        (rc == 0).then_some(fds)
    }

    /// Fork the watchdog. The child of a multi-threaded process may only use
    /// async-signal-safe calls: no allocation, locks or Rust I/O below.
    fn start_watchdog() -> Option<std::fs::File> {
        let [read, write] = cloexec_pipe()?;
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let max_fd = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
            limit.rlim_cur.clamp(256, 65536) as i32
        } else {
            4096
        };
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe {
                libc::close(read);
                libc::close(write);
            }
            return None;
        }
        if pid == 0 {
            unsafe { watchdog(read, max_fd) }
        }
        unsafe { libc::close(read) };
        // SAFETY: `write` is a fresh descriptor owned only by this File.
        Some(unsafe { std::fs::File::from_raw_fd(write) })
    }

    unsafe fn watchdog(read: i32, max_fd: i32) -> ! {
        libc::setsid();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE] {
            libc::signal(signal, libc::SIG_IGN);
        }
        // Keep only the pipe (as stdin): holding the IDE's other descriptors
        // would keep its sockets, PTYs and child pipes open.
        if libc::dup2(read, 0) < 0 {
            libc::_exit(1);
        }
        for fd in 1..max_fd {
            libc::close(fd);
        }
        let mut groups = [0i32; 4096];
        let mut buffer = [0u8; 512];
        let mut value: i32 = 0;
        let mut sign = 0u8;
        loop {
            let count = libc::read(0, buffer.as_mut_ptr().cast(), buffer.len());
            if count < 0 && *errno() == libc::EINTR {
                continue;
            }
            if count <= 0 {
                break;
            }
            for &byte in &buffer[..count as usize] {
                match byte {
                    b'+' | b'-' => {
                        sign = byte;
                        value = 0;
                    }
                    b'0'..=b'9' => {
                        value = value
                            .saturating_mul(10)
                            .saturating_add((byte - b'0') as i32)
                    }
                    b'\n' => {
                        if sign == b'+' {
                            if let Some(slot) = groups.iter_mut().find(|slot| **slot == 0) {
                                *slot = value;
                            }
                        } else if sign == b'-' {
                            for slot in groups.iter_mut().filter(|slot| **slot == value) {
                                *slot = 0;
                            }
                        }
                        sign = 0;
                    }
                    _ => sign = 0,
                }
            }
        }
        // The IDE is gone: terminate, give a short grace, then kill.
        let mut alive = false;
        for &group in groups.iter().filter(|group| **group > 1) {
            alive |= libc::kill(-group, libc::SIGTERM) == 0;
        }
        if alive {
            let grace = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };
            libc::nanosleep(&grace, std::ptr::null_mut());
            for &group in groups.iter().filter(|group| **group > 1) {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        libc::_exit(0)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn errno() -> *mut i32 {
        libc::__errno_location()
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe fn errno() -> *mut i32 {
        libc::__error()
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    unsafe fn errno() -> *mut i32 {
        static mut NONE: i32 = 0;
        std::ptr::addr_of_mut!(NONE)
    }

    extern "C" fn forward_and_die(signal: i32) {
        for slot in &GROUPS {
            let group = slot.load(Ordering::Acquire);
            if group > 1 {
                unsafe { libc::kill(-group, libc::SIGTERM) };
            }
        }
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
    /// For the IDE process: SIGTERM, SIGINT and SIGHUP stop the owned groups
    /// before the process dies with the same signal. Ignored signals stay so.
    pub fn install_exit_signal_handlers() {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut action) != 0
                    || action.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = forward_and_die as extern "C" fn(i32) as usize;
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_flags = libc::SA_RESTART;
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }

    /// Linux: the spawned leader receives SIGKILL when this process dies.
    /// Only for spawns from the main thread (PDEATHSIG tracks the forking
    /// thread, and a worker thread may exit while its child keeps running).
    pub fn set_parent_death_signal(command: &mut std::process::Command) {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = unsafe { libc::getpid() };
            if unsafe { libc::syscall(libc::SYS_gettid) } as i32 != parent {
                return;
            }
            // SAFETY: prctl/getppid/_exit are async-signal-safe.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // The parent may have died before prctl took effect.
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = command;
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::io::AsRawFd;

        fn pipe_fd() -> Option<i32> {
            let watchdog = WATCHDOG.lock().unwrap();
            watchdog.as_ref()?.as_ref().map(|pipe| pipe.as_raw_fd())
        }

        #[test]
        fn start_forks_one_watchdog_that_later_groups_use() {
            start();
            let first = pipe_fd().expect("watchdog started");
            start();
            assert_eq!(pipe_fd(), Some(first));
            // Announcing goes to the same watchdog (no second fork).
            register(i32::MAX as u32);
            unregister(i32::MAX as u32);
            assert_eq!(pipe_fd(), Some(first));
        }
    }
}

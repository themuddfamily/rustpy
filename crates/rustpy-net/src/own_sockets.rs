//! Which process made a connection to a proxy bridge.
//!
//! The SOCKS and HTTP proxy bridges ([`crate::socks`], [`crate::http_bridge`])
//! listen on 127.0.0.1 for the one connection that ureq, in this process,
//! makes to them. Any local program can connect there first, so a connection
//! is only served when its client end is a socket of this process:
//!
//! * Linux and macOS: the addresses (`getsockname`, `getpeername`) of this
//!   process's descriptors (`/proc/self/fd`, `proc_pidinfo(PROC_PIDLISTFDS)`).
//! * Windows: the owning process of the client end in the TCP connection tables
//!   (`GetExtendedTcpTable`).
//!
//! Each check first looks for the bridge's own end of the connection: when it
//! cannot find it, its view of the system is incomplete, and the connection is
//! refused. Platforms without an ownership check also refuse bridge connections:
//! a failure to verify the peer must never expose stored proxy credentials.

use std::net::{SocketAddr, TcpStream};

/// Whether the connection `accepted` (accepted on `local` from `peer`) was
/// made by another process, or its ownership cannot be verified.
pub(crate) fn made_elsewhere(accepted: &TcpStream, local: SocketAddr, peer: SocketAddr) -> bool {
    platform::made_elsewhere(accepted, canonical(local), canonical(peer))
}

/// IPv4-mapped IPv6 addresses (a dual-stack socket's view of 127.0.0.1) as IPv4.
fn canonical(address: SocketAddr) -> SocketAddr {
    SocketAddr::new(address.ip().to_canonical(), address.port())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
    use std::os::fd::{AsRawFd as _, RawFd};

    use super::canonical;

    /// The descriptors open in this process (None when they cannot be listed).
    #[cfg(target_os = "linux")]
    fn descriptors() -> Option<Vec<RawFd>> {
        let entries = std::fs::read_dir("/proc/self/fd").ok()?;
        Some(
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse().ok())
                .collect(),
        )
    }

    /// The sockets open in this process (None when they cannot be listed).
    #[cfg(target_os = "macos")]
    fn descriptors() -> Option<Vec<RawFd>> {
        let pid = libc::c_int::try_from(std::process::id()).ok()?;
        let entry = std::mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: without a buffer, proc_pidinfo returns the size the list needs.
        let needed =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let mut capacity = usize::try_from(needed).ok()? / entry + 64;
        for _ in 0..4 {
            let mut list: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
            let size = libc::c_int::try_from(capacity * entry).ok()?;
            // SAFETY: the buffer has room for `size` bytes of entries.
            let filled = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDLISTFDS,
                    0,
                    list.as_mut_ptr().cast(),
                    size,
                )
            };
            let filled = usize::try_from(filled).ok().filter(|n| *n > 0)? / entry;
            if filled < capacity {
                // SAFETY: proc_pidinfo initialized `filled` whole entries.
                unsafe { list.set_len(filled) };
                return Some(
                    list.iter()
                        .filter(|f| i64::from(f.proc_fdtype) == i64::from(libc::PROX_FDTYPE_SOCKET))
                        .map(|f| f.proc_fd)
                        .collect(),
                );
            }
            // The list filled the buffer: it may have been cut short.
            capacity *= 2;
        }
        None
    }

    /// The local or remote address of socket `fd` (None for anything else).
    fn address(fd: RawFd, remote: bool) -> Option<SocketAddr> {
        // SAFETY: all zeroes is a valid sockaddr_storage.
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len =
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>()).ok()?;
        let ptr = std::ptr::addr_of_mut!(storage).cast::<libc::sockaddr>();
        // SAFETY: `ptr` points to `len` writable bytes. Both calls only read the
        // descriptor and fail for anything but a (connected) socket.
        let status = unsafe {
            if remote {
                libc::getpeername(fd, ptr, &mut len)
            } else {
                libc::getsockname(fd, ptr, &mut len)
            }
        };
        if status != 0 {
            return None;
        }
        match libc::c_int::from(storage.ss_family) {
            libc::AF_INET => {
                // SAFETY: the address family says that the storage holds a sockaddr_in.
                let a = unsafe { &*std::ptr::addr_of!(storage).cast::<libc::sockaddr_in>() };
                Some(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))),
                    u16::from_be(a.sin_port),
                ))
            }
            libc::AF_INET6 => {
                // SAFETY: the address family says that the storage holds a sockaddr_in6.
                let a = unsafe { &*std::ptr::addr_of!(storage).cast::<libc::sockaddr_in6>() };
                Some(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr)),
                    u16::from_be(a.sin6_port),
                ))
            }
            _ => None,
        }
    }

    /// The local and remote address of a connected socket.
    pub(super) fn addresses(fd: RawFd) -> Option<(SocketAddr, SocketAddr)> {
        Some((
            canonical(address(fd, false)?),
            canonical(address(fd, true)?),
        ))
    }

    pub(super) fn made_elsewhere(
        accepted: &TcpStream,
        local: SocketAddr,
        peer: SocketAddr,
    ) -> bool {
        let Some(fds) = descriptors() else {
            return true;
        };
        let own_end = accepted.as_raw_fd();
        if !fds.contains(&own_end) || addresses(own_end) != Some((local, peer)) {
            return true;
        }
        // The client end goes from the peer's address to the listener's.
        !fds.iter()
            .any(|&fd| fd != own_end && addresses(fd) == Some((peer, local)))
    }
}

#[cfg(windows)]
mod platform {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};

    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_CONNECTIONS,
    };

    use super::canonical;

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;

    /// A TCP connection: local and remote address, and the owning process.
    type Connection = (SocketAddr, SocketAddr, u32);

    /// The rows of the TCP connection table of `family`: `dwNumEntries`, then
    /// that many rows of `R` (MIB_TCPTABLE_OWNER_PID / MIB_TCP6TABLE_OWNER_PID).
    fn rows<R: Copy>(family: u32) -> Option<Vec<R>> {
        let offset = std::mem::size_of::<u32>().next_multiple_of(std::mem::align_of::<R>());
        let mut size = 0u32;
        let mut buffer: Vec<u64> = Vec::new();
        for _ in 0..8 {
            // The first call has no buffer: it asks for the size needed.
            let table = if buffer.is_empty() {
                std::ptr::null_mut()
            } else {
                buffer.as_mut_ptr().cast()
            };
            // SAFETY: `table` is null with a `size` of 0, or has room for `size` bytes.
            let status = unsafe {
                GetExtendedTcpTable(
                    table,
                    &mut size,
                    0,
                    family,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                )
            };
            if status == NO_ERROR && !buffer.is_empty() {
                let bytes = buffer.len() * 8;
                let base = buffer.as_ptr().cast::<u8>();
                // SAFETY: the table starts with its row count; every row read lies within the buffer.
                let count = unsafe { std::ptr::read_unaligned(base.cast::<u32>()) } as usize;
                let count = count.min(bytes.saturating_sub(offset) / std::mem::size_of::<R>());
                return Some(
                    (0..count)
                        .map(|i| unsafe {
                            std::ptr::read_unaligned(
                                base.add(offset + i * std::mem::size_of::<R>()).cast::<R>(),
                            )
                        })
                        .collect(),
                );
            }
            if status != ERROR_INSUFFICIENT_BUFFER && status != NO_ERROR {
                return None;
            }
            // Room for connections opened meanwhile.
            buffer = vec![0u64; (size as usize + 4096) / 8 + 1];
            size = u32::try_from(buffer.len() * 8).ok()?;
        }
        None
    }

    /// The TCP connections of the system (IPv4, and IPv6 when it can be read).
    fn connections() -> Option<Vec<Connection>> {
        // Ports are in network byte order in the low 16 bits.
        let port = |p: u32| u16::from_be((p & 0xffff) as u16);
        let v4 = |address: u32, p: u32| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::from(address.to_ne_bytes())), port(p))
        };
        let mut all: Vec<Connection> = rows::<MIB_TCPROW_OWNER_PID>(AF_INET)?
            .iter()
            .map(|r| {
                (
                    v4(r.dwLocalAddr, r.dwLocalPort),
                    v4(r.dwRemoteAddr, r.dwRemotePort),
                    r.dwOwningPid,
                )
            })
            .collect();
        // A dual-stack IPv6 socket reaches 127.0.0.1 as ::ffff:127.0.0.1.
        let v6 = |address: [u8; 16], p: u32| {
            canonical(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::from(address)),
                port(p),
            ))
        };
        if let Some(rows) = rows::<MIB_TCP6ROW_OWNER_PID>(AF_INET6) {
            all.extend(rows.iter().map(|r| {
                (
                    v6(r.ucLocalAddr, r.dwLocalPort),
                    v6(r.ucRemoteAddr, r.dwRemotePort),
                    r.dwOwningPid,
                )
            }));
        }
        Some(all)
    }

    pub(super) fn made_elsewhere(
        _accepted: &TcpStream,
        local: SocketAddr,
        peer: SocketAddr,
    ) -> bool {
        let Some(connections) = connections() else {
            return true;
        };
        let me = std::process::id();
        if !connections
            .iter()
            .any(|&(l, r, pid)| l == local && r == peer && pid == me)
        {
            return true;
        }
        // The client end goes from the peer's address to the listener's.
        !connections
            .iter()
            .any(|&(l, r, pid)| l == peer && r == local && pid == me)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use std::net::{SocketAddr, TcpStream};

    pub(super) fn made_elsewhere(
        _accepted: &TcpStream,
        _local: SocketAddr,
        _peer: SocketAddr,
    ) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn connections_made_in_this_process_are_its_own() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local = listener.local_addr().unwrap();
        let client = TcpStream::connect(local).unwrap();
        let (accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        assert!(!super::made_elsewhere(&accepted, local, peer));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::fd::AsRawFd as _;
            assert_eq!(
                super::platform::addresses(client.as_raw_fd()),
                Some((peer, local))
            );
            assert_eq!(
                super::platform::addresses(accepted.as_raw_fd()),
                Some((local, peer))
            );
            assert_eq!(
                super::platform::addresses(listener.as_raw_fd()),
                None,
                "not connected"
            );
            // The listener's own end is checked first: for a connection it does not
            // belong to, nothing can be told.
            let other = TcpListener::bind("127.0.0.1:0").unwrap();
            let _second = TcpStream::connect(other.local_addr().unwrap()).unwrap();
            let (unrelated, _) = other.accept().unwrap();
            assert!(super::made_elsewhere(&unrelated, local, peer));
        }
    }

    /// Invoked in a separate test process so its TCP descriptor never belongs
    /// to the IDE process, even when both processes have the same OS user.
    #[test]
    fn child_connect() {
        let Ok(address) = std::env::var("RUSTPY_PROXY_TEST_ADDRESS") else {
            return;
        };
        use std::io::Read as _;
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut reply = [0];
        match stream.read(&mut reply) {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("outsider connection was not closed: {other:?}"),
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn other_processes_are_refused_and_do_not_claim_the_bridge() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || crate::socks::accept_one(listener).unwrap());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "own_sockets::tests::child_connect",
                "--exact",
                "--test-threads=1",
            ])
            .env("RUSTPY_PROXY_TEST_ADDRESS", address.to_string())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        // Refusing the outsider leaves the one-shot listener for its real client.
        let client = TcpStream::connect(address).unwrap();
        let accepted = worker.join().unwrap();
        assert_eq!(accepted.peer_addr().unwrap(), client.local_addr().unwrap());
    }
}

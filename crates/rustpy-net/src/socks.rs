//! SOCKS5 (RFC 1928, username/password auth RFC 1929) and SOCKS4/4a client.
//!
//! The bundled `ureq` is built without its SOCKS feature, so SOCKS proxies are
//! plugged in through its resolver hook: [`SocksResolver`] opens the tunnel to
//! the target through the proxy *while resolving*, then returns the address of
//! a one-shot listener on 127.0.0.1 that relays the single connection ureq makes
//! to it. TLS still runs end to end between ureq and the target (the TLS server
//! name comes from the URL, not from the resolved address), so certificate
//! checks are unchanged. Handshake failures surface as resolve errors with a
//! descriptive message.
//!
//! A bridge listener is on loopback, where any local program can connect to
//! it. On Linux, macOS and Windows a bridge only serves connections made by
//! this process (see [`accept_one`]), so another user's program that connects
//! first cannot use the tunnel (or, for HTTP proxies, the credentials).

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::proxy::{ProxyServer, SocksVersion};

/// Time allowed for connecting to the proxy and for the handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a bridge waits for ureq to connect to it.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// The SOCKS5 proxy wants a login and password, and none was offered.
pub(crate) const AUTH_REQUIRED: &str = "the SOCKS5 proxy requires authentication";
/// The SOCKS5 proxy accepts none of the offered methods (e.g. it wants a
/// password and only "no authentication" was offered).
pub(crate) const NO_ACCEPTABLE_METHOD: &str =
    "the SOCKS5 proxy accepts none of the offered authentication methods";

fn err(kind: io::ErrorKind, msg: impl Into<String>) -> io::Error {
    io::Error::new(kind, msg.into())
}

/// Opens a TCP connection to the proxy server.
pub(crate) fn connect_proxy(proxy: &ProxyServer, timeout: Duration) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = (proxy.host.as_str(), proxy.port)
        .to_socket_addrs()
        .map_err(|e| {
            err(
                io::ErrorKind::NotFound,
                format!("cannot resolve the proxy {}: {e}", proxy.address()),
            )
        })?
        .collect();
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => err(
            e.kind(),
            format!("cannot connect to the proxy {}: {e}", proxy.address()),
        ),
        None => err(
            io::ErrorKind::NotFound,
            format!("no address for the proxy {}", proxy.address()),
        ),
    })
}

fn socks5_reply_text(code: u8) -> &'static str {
    match code {
        1 => "general SOCKS server failure",
        2 => "connection not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown error",
    }
}

/// Performs the SOCKS5 handshake on `stream` for a CONNECT to `host:port`.
pub fn socks5_handshake(
    stream: &mut (impl Read + Write),
    proxy: &ProxyServer,
    host: &str,
    port: u16,
) -> io::Result<()> {
    let creds = proxy
        .user
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| (u, proxy.password.as_deref().unwrap_or("")));
    let greeting: &[u8] = if creds.is_some() {
        &[5, 2, 0, 2]
    } else {
        &[5, 1, 0]
    };
    stream.write_all(greeting)?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply)?;
    if reply[0] != 5 {
        return Err(err(
            io::ErrorKind::InvalidData,
            "the proxy does not speak SOCKS5",
        ));
    }
    match (reply[1], creds) {
        (0, _) => {}
        (2, Some((user, pass))) => {
            if user.len() > 255 || pass.len() > 255 {
                return Err(err(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5 login and password are limited to 255 bytes",
                ));
            }
            let mut auth = vec![1u8, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            stream.write_all(&auth)?;
            let mut status = [0u8; 2];
            stream.read_exact(&mut status)?;
            if status[1] != 0 {
                return Err(err(
                    io::ErrorKind::PermissionDenied,
                    "SOCKS5 proxy authentication failed (check the login and password)",
                ));
            }
        }
        (2, None) => return Err(err(io::ErrorKind::PermissionDenied, AUTH_REQUIRED)),
        _ => return Err(err(io::ErrorKind::PermissionDenied, NO_ACCEPTABLE_METHOD)),
    }
    let mut request = vec![5u8, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.is_empty() || host.len() > 255 {
                return Err(err(
                    io::ErrorKind::InvalidInput,
                    format!("host name too long for SOCKS5: {host}"),
                ));
            }
            request.push(3);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request)?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[0] != 5 {
        return Err(err(io::ErrorKind::InvalidData, "invalid SOCKS5 reply"));
    }
    if head[1] != 0 {
        return Err(err(
            io::ErrorKind::ConnectionRefused,
            format!(
                "SOCKS5 proxy cannot connect to {host}:{port}: {}",
                socks5_reply_text(head[1])
            ),
        ));
    }
    // Skip the bound address.
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            len[0] as usize
        }
        _ => {
            return Err(err(
                io::ErrorKind::InvalidData,
                "invalid SOCKS5 reply address",
            ))
        }
    };
    let mut rest = vec![0u8; skip + 2];
    stream.read_exact(&mut rest)?;
    Ok(())
}

/// Performs the SOCKS4 (`V4`, IPv4 resolved here) or SOCKS4a handshake.
pub fn socks4_handshake(
    stream: &mut (impl Read + Write),
    proxy: &ProxyServer,
    host: &str,
    port: u16,
    version: SocksVersion,
) -> io::Result<()> {
    let mut request = vec![4u8, 1];
    request.extend_from_slice(&port.to_be_bytes());
    let ipv4 = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => Some(ip),
        Ok(IpAddr::V6(_)) => {
            return Err(err(
                io::ErrorKind::InvalidInput,
                "SOCKS4 does not support IPv6 addresses",
            ))
        }
        Err(_) if version == SocksVersion::V4 => {
            let ip = (host, port).to_socket_addrs()?.find_map(|a| match a.ip() {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            });
            Some(ip.ok_or_else(|| {
                err(
                    io::ErrorKind::NotFound,
                    format!("{host} has no IPv4 address (needed for SOCKS4)"),
                )
            })?)
        }
        Err(_) => None,
    };
    request.extend_from_slice(&ipv4.unwrap_or(Ipv4Addr::new(0, 0, 0, 1)).octets());
    request.extend_from_slice(proxy.user.as_deref().unwrap_or("").as_bytes());
    request.push(0);
    if ipv4.is_none() {
        request.extend_from_slice(host.as_bytes());
        request.push(0);
    }
    stream.write_all(&request)?;
    let mut reply = [0u8; 8];
    stream.read_exact(&mut reply)?;
    if reply[1] != 0x5a {
        return Err(err(
            io::ErrorKind::ConnectionRefused,
            format!(
                "SOCKS4 proxy cannot connect to {host}:{port} (code {:#x})",
                reply[1]
            ),
        ));
    }
    Ok(())
}

/// Connects to `host:port` through the SOCKS proxy; returns the tunnel.
pub fn connect(
    proxy: &ProxyServer,
    version: SocksVersion,
    host: &str,
    port: u16,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let mut stream = connect_proxy(proxy, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    match version {
        SocksVersion::V5 => socks5_handshake(&mut stream, proxy, host, port)?,
        SocksVersion::V4 | SocksVersion::V4a => {
            socks4_handshake(&mut stream, proxy, host, port, version)?
        }
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(stream)
}

/// Splits ureq's `host:port` (`[v6]:port`) netloc.
pub(crate) fn split_netloc(netloc: &str) -> io::Result<(String, u16)> {
    let bad = || {
        err(
            io::ErrorKind::InvalidInput,
            format!("invalid address {netloc}"),
        )
    };
    let (host, port) = netloc.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_string(), port))
}

/// Copies both directions until either side closes, then closes both.
pub(crate) fn relay(client: TcpStream, upstream: TcpStream) {
    let (Ok(mut client_read), Ok(mut upstream_read)) = (client.try_clone(), upstream.try_clone())
    else {
        return;
    };
    let mut client_write = client;
    let mut upstream_write = upstream;
    let up = std::thread::Builder::new()
        .name("rustpy-socks-up".into())
        .spawn(move || {
            let _ = io::copy(&mut client_read, &mut upstream_write);
            let _ = upstream_write.shutdown(Shutdown::Both);
            let _ = client_read.shutdown(Shutdown::Both);
        });
    let _ = io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(Shutdown::Both);
    let _ = upstream_read.shutdown(Shutdown::Both);
    if let Ok(t) = up {
        let _ = t.join();
    }
}

/// Waits for the one connection ureq makes to a bridge's `listener` (then
/// closes the listener). Connections from other processes are dropped (see
/// [`crate::own_sockets`]). None when nobody connected in time.
pub(crate) fn accept_one(listener: TcpListener) -> Option<TcpStream> {
    listener.set_nonblocking(true).ok()?;
    let local = listener.local_addr().ok()?;
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    let client = loop {
        match listener.accept() {
            Ok((s, peer))
                if peer.ip().is_loopback()
                    && !crate::own_sockets::made_elsewhere(&s, local, peer) =>
            {
                break s
            }
            Ok((_, peer)) => {
                log::warn!(
                    "proxy bridge: refused a connection from {peer}, which another program made"
                );
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    log::debug!("proxy bridge: nobody connected");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return None,
        }
    };
    drop(listener);
    client.set_nonblocking(false).ok()?;
    let _ = client.set_nodelay(true);
    Some(client)
}

/// Accepts the one connection ureq makes to `listener` and relays it to `upstream`.
fn serve_once(listener: TcpListener, upstream: TcpStream) {
    let Some(client) = accept_one(listener) else {
        return;
    };
    let _ = upstream.set_nodelay(true);
    relay(client, upstream);
}

/// ureq resolver that tunnels every connection through a SOCKS proxy.
pub struct SocksResolver {
    proxy: ProxyServer,
    version: SocksVersion,
    timeout: Duration,
}

impl SocksResolver {
    pub fn new(proxy: ProxyServer, version: SocksVersion) -> Self {
        SocksResolver {
            proxy,
            version,
            timeout: HANDSHAKE_TIMEOUT,
        }
    }
}

impl ureq::Resolver for SocksResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let (host, port) = split_netloc(netloc)?;
        let upstream = connect(&self.proxy, self.version, &host, port, self.timeout)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        std::thread::Builder::new()
            .name("rustpy-socks".into())
            .spawn(move || serve_once(listener, upstream))?;
        Ok(vec![addr])
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Minimal SOCKS5 server for tests: optional user/password, connects to the
    /// requested host:port (a domain is resolved locally) and relays. Records the
    /// requested targets.
    pub(crate) fn socks5_server(
        creds: Option<(&'static str, &'static str)>,
    ) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut c) = conn else { continue };
                let seen = seen2.clone();
                std::thread::spawn(move || {
                    let mut head = [0u8; 2];
                    if c.read_exact(&mut head).is_err() {
                        return;
                    }
                    let mut methods = vec![0u8; head[1] as usize];
                    c.read_exact(&mut methods).unwrap();
                    if let Some((user, pass)) = creds {
                        if !methods.contains(&2) {
                            c.write_all(&[5, 0xff]).unwrap();
                            return;
                        }
                        c.write_all(&[5, 2]).unwrap();
                        let mut v = [0u8; 2];
                        c.read_exact(&mut v).unwrap();
                        let mut u = vec![0u8; v[1] as usize];
                        c.read_exact(&mut u).unwrap();
                        let mut pl = [0u8; 1];
                        c.read_exact(&mut pl).unwrap();
                        let mut p = vec![0u8; pl[0] as usize];
                        c.read_exact(&mut p).unwrap();
                        let ok = u == user.as_bytes() && p == pass.as_bytes();
                        c.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                        if !ok {
                            return;
                        }
                    } else {
                        c.write_all(&[5, 0]).unwrap();
                    }
                    let mut req = [0u8; 4];
                    c.read_exact(&mut req).unwrap();
                    let host = match req[3] {
                        1 => {
                            let mut b = [0u8; 4];
                            c.read_exact(&mut b).unwrap();
                            Ipv4Addr::from(b).to_string()
                        }
                        3 => {
                            let mut l = [0u8; 1];
                            c.read_exact(&mut l).unwrap();
                            let mut b = vec![0u8; l[0] as usize];
                            c.read_exact(&mut b).unwrap();
                            String::from_utf8(b).unwrap()
                        }
                        _ => panic!("unexpected address type"),
                    };
                    let mut pb = [0u8; 2];
                    c.read_exact(&mut pb).unwrap();
                    let port = u16::from_be_bytes(pb);
                    seen.lock().unwrap().push(format!("{host}:{port}"));
                    // `*.test` names resolve to loopback here (only the proxy can resolve them).
                    let resolved = if host.ends_with(".test") {
                        "127.0.0.1"
                    } else {
                        host.as_str()
                    };
                    let target = if host == "unreachable.test" {
                        None
                    } else {
                        (resolved, port)
                            .to_socket_addrs()
                            .ok()
                            .and_then(|mut a| a.next())
                            .and_then(|a| TcpStream::connect(a).ok())
                    };
                    match target {
                        Some(t) => {
                            c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
                            relay(c, t);
                        }
                        None => {
                            let _ = c.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]);
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    /// One-request HTTP server answering `body`.
    pub(crate) fn http_server(body: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut b = [0u8; 1];
                    while !buf.ends_with(b"\r\n\r\n") {
                        if c.read(&mut b).unwrap_or(0) == 0 {
                            return;
                        }
                        buf.push(b[0]);
                    }
                    // Read the request body too: closing with unread data would reset
                    // the connection, and the reset can overtake the response.
                    let head = String::from_utf8_lossy(&buf).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0usize);
                    if c.read_exact(&mut vec![0u8; length]).is_err() {
                        return;
                    }
                    let _ = write!(
                        c,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        addr
    }

    #[test]
    fn socks5_bridge_relays_http() {
        let (proxy_addr, seen) = socks5_server(Some(("user", "secret")));
        let web = http_server("hello through socks");
        let proxy = ProxyServer {
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            user: Some("user".into()),
            password: Some("secret".into()),
        };
        let agent = ureq::AgentBuilder::new()
            .resolver(SocksResolver::new(proxy.clone(), SocksVersion::V5))
            .timeout(Duration::from_secs(10))
            .build();
        let resp = agent
            .get(&format!("http://127.0.0.1:{}/x", web.port()))
            .call()
            .unwrap();
        assert_eq!(resp.into_string().unwrap(), "hello through socks");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[format!("127.0.0.1:{}", web.port())]
        );
        // Wrong password: a descriptive error.
        let bad = ProxyServer {
            password: Some("nope".into()),
            ..proxy.clone()
        };
        let agent = ureq::AgentBuilder::new()
            .resolver(SocksResolver::new(bad, SocksVersion::V5))
            .build();
        let e = agent
            .get("http://example.test/")
            .call()
            .unwrap_err()
            .to_string();
        assert!(e.contains("authentication failed"), "{e}");
        // Target unreachable through the proxy.
        let agent = ureq::AgentBuilder::new()
            .resolver(SocksResolver::new(proxy, SocksVersion::V5))
            .build();
        let e = agent
            .get("http://unreachable.test/")
            .call()
            .unwrap_err()
            .to_string();
        assert!(e.contains("host unreachable"), "{e}");
    }

    #[test]
    fn socks5_sends_host_names_to_the_proxy() {
        let (proxy_addr, seen) = socks5_server(None);
        let web = http_server("named");
        let proxy = ProxyServer {
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            user: None,
            password: None,
        };
        let agent = ureq::AgentBuilder::new()
            .resolver(SocksResolver::new(proxy, SocksVersion::V5))
            .build();
        // Only the proxy can resolve `*.test`.
        let resp = agent
            .get(&format!("http://named.test:{}/", web.port()))
            .call()
            .unwrap();
        assert_eq!(resp.into_string().unwrap(), "named");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[format!("named.test:{}", web.port())]
        );
    }

    #[test]
    fn socks5_requires_offered_auth() {
        let (proxy_addr, _) = socks5_server(Some(("u", "p")));
        let proxy = ProxyServer {
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            user: None,
            password: None,
        };
        let e = connect(
            &proxy,
            SocksVersion::V5,
            "example.com",
            80,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(e.to_string().contains("accepts none"), "{e}");
    }

    #[test]
    fn socks4a_request_bytes() {
        // A fake stream: records writes, replays a canned reply.
        struct Fake {
            written: Vec<u8>,
            reply: io::Cursor<Vec<u8>>,
        }
        impl Read for Fake {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reply.read(buf)
            }
        }
        impl Write for Fake {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let proxy = ProxyServer {
            host: "p".into(),
            port: 1080,
            user: Some("bob".into()),
            password: None,
        };
        let mut f = Fake {
            written: Vec::new(),
            reply: io::Cursor::new(vec![0, 0x5a, 0, 0, 0, 0, 0, 0]),
        };
        socks4_handshake(&mut f, &proxy, "example.com", 443, SocksVersion::V4a).unwrap();
        assert_eq!(
            f.written,
            [&[4u8, 1, 1, 187, 0, 0, 0, 1][..], b"bob\0example.com\0"].concat()
        );
        let mut f = Fake {
            written: Vec::new(),
            reply: io::Cursor::new(vec![0, 0x5b, 0, 0, 0, 0, 0, 0]),
        };
        let e = socks4_handshake(&mut f, &proxy, "10.0.0.1", 80, SocksVersion::V4).unwrap_err();
        assert!(e.to_string().contains("cannot connect"), "{e}");
        assert_eq!(&f.written[..8], &[4, 1, 0, 80, 10, 0, 0, 1]);
        // SOCKS5 with an IPv6 target.
        let reply = [&[5u8, 0][..], &[5, 0, 0, 4], &[0u8; 16], &[0, 80]].concat();
        let mut f = Fake {
            written: Vec::new(),
            reply: io::Cursor::new(reply),
        };
        let anon = ProxyServer {
            user: None,
            ..proxy
        };
        socks5_handshake(&mut f, &anon, "::1", 80).unwrap();
        assert_eq!(f.written[..3], [5, 1, 0]);
        assert_eq!(f.written[3..7], [5, 1, 0, 4]);
    }

    /// The other half of [`connections_from_other_processes_are_refused`]: runs
    /// in a child process, connects to the bridge and sends a request.
    #[test]
    fn child_connects_to_bridge() {
        let Ok(port) = std::env::var("RUSTPY_NET_TEST_BRIDGE_PORT") else {
            return;
        };
        let mut c = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
        println!("CONNECTED");
        let _ = c.write_all(b"GET /from-another-process HTTP/1.1\r\nHost: far.test\r\n\r\n");
        let mut rest = Vec::new();
        let _ = c.set_read_timeout(Some(Duration::from_secs(20)));
        let _ = c.read_to_end(&mut rest);
        println!("CLOSED {}", rest.len());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn connections_from_other_processes_are_refused() {
        use std::io::BufRead as _;
        // A proxy that records the request lines it gets and answers 200.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for mut c in proxy_listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                seen2.lock().unwrap().push(
                    String::from_utf8_lossy(&head)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string(),
                );
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        let proxy = ProxyServer {
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            user: Some("me".into()),
            password: Some("pw".into()),
        };
        let bridge = ureq::Resolver::resolve(
            &crate::http_bridge::HttpProxyResolver::new(proxy),
            "far.test:80",
        )
        .unwrap()[0];
        // Another process connects first...
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "socks::tests::child_connects_to_bridge",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RUSTPY_NET_TEST_BRIDGE_PORT", bridge.port().to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("CONNECTED") {
            line.clear();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "the child did not connect"
            );
        }
        // ...and is turned away; this process's connection is served.
        let mut c = TcpStream::connect(bridge).unwrap();
        c.write_all(b"GET /mine HTTP/1.1\r\nHost: far.test\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        c.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 OK") && response.ends_with("ok"),
            "{response}"
        );
        let mut rest = String::new();
        out.read_to_string(&mut rest).unwrap();
        assert!(
            rest.contains("CLOSED 0"),
            "the other process got an answer: {rest}"
        );
        assert!(child.wait().unwrap().success());
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &["GET http://far.test/mine HTTP/1.1".to_string()]
        );
    }

    #[test]
    fn netloc_parsing() {
        assert_eq!(
            split_netloc("example.com:443").unwrap(),
            ("example.com".to_string(), 443)
        );
        assert_eq!(
            split_netloc("[::1]:8080").unwrap(),
            ("::1".to_string(), 8080)
        );
        assert!(split_netloc("nohost").is_err());
        assert!(split_netloc(":80").is_err());
    }
}

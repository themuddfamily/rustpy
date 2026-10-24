//! HTTP proxies that `ureq` cannot drive on its own.
//!
//! ureq 2 sends `Proxy-Authorization` only with `CONNECT` (HTTPS) and never with
//! the absolute-form requests it sends a proxy for `http://` URLs, so an
//! authenticating proxy answers those with 407. Adding the header as a request
//! header would also send it to origin servers: ureq keeps custom headers on a
//! redirect to `https://`, and logs them at debug level. It also cannot use a
//! proxy at an IPv6 address.
//!
//! [`HttpProxyResolver`] works like the SOCKS bridge ([`crate::socks`]): it
//! "resolves" every host to a one-shot listener on 127.0.0.1 and forwards the
//! connection ureq makes to it to the proxy. A connection that starts with a
//! TLS handshake (an `https://` URL, also after a redirect) is tunnelled with
//! `CONNECT`; otherwise the request line is rewritten to absolute form. Either
//! way the credentials are added to what is sent to the proxy, never to the
//! origin.
//!
//! A plain-HTTP connection carries exactly one request: only its head is
//! rewritten. `Connection: close` asks the proxy to close afterwards, and the
//! proxy's response reaches ureq with `Connection: close` as well, so ureq never
//! puts the connection into its pool to send a later request over it (which
//! would reach the proxy unrewritten, without credentials, or meet a connection
//! the proxy already closed). Interim `1xx` responses before it are dropped:
//! ureq would take one for the response.
//!
//! The listener is on loopback, so another local program could connect to it
//! before ureq does. Only origin-form requests (`GET /path`, what ureq sends)
//! are forwarded, and always to the host ureq resolved, so such a connection
//! cannot spend the credentials on a host of its choosing; on Linux, macOS and
//! Windows, connections from other processes are refused
//! ([`crate::socks::accept_one`]).

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use base64::Engine as _;

use crate::proxy::ProxyServer;
use crate::socks::{accept_one, connect_proxy, relay, split_netloc, HANDSHAKE_TIMEOUT};

/// Largest request or response head handled (ureq reads response heads of up to 100 KiB).
const MAX_HEAD: usize = 128 * 1024;
/// First byte of a TLS handshake record (ClientHello).
const TLS_HANDSHAKE: u8 = 0x16;

/// `user:password` for Basic authentication (None without a user).
fn basic_credentials(proxy: &ProxyServer) -> Option<String> {
    let user = proxy.user.as_deref().filter(|u| !u.is_empty())?;
    Some(base64::engine::general_purpose::STANDARD.encode(format!(
        "{user}:{}",
        proxy.password.as_deref().unwrap_or("")
    )))
}

/// `host:port` for request lines (IPv6 in brackets; port 80 implied for `http://`).
fn authority(host: &str, port: u16, default_port: Option<u16>) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if default_port == Some(port) {
        host
    } else {
        format!("{host}:{port}")
    }
}

/// Reads up to the end of a head (`\r\n\r\n`); returns the head and any bytes after it.
pub(crate) fn read_head(stream: &mut impl Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    read_head_after(Vec::new(), stream)
}

/// [`read_head`] for a head that starts with the bytes in `buf`, read before.
fn read_head_after(mut buf: Vec<u8>, stream: &mut impl Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP head too large",
            ));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the end of the HTTP head",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Lower-case name of a header line (`Name: value`).
fn header_name(line: &[u8]) -> String {
    String::from_utf8_lossy(line.split(|b| *b == b':').next().unwrap_or_default())
        .trim()
        .to_ascii_lowercase()
}

/// Rewrites a request head ureq wrote for `host:port` into the form a proxy
/// expects: absolute-form target, the proxy's credentials, `Connection: close`
/// (hop-by-hop headers of the original are dropped). None when it is not an
/// HTTP request head, or when its target is not in origin form (`/path`):
/// ureq sends nothing else here, so such a request comes from another program
/// that connected to the bridge first, and it must not get the credentials
/// for a target of its choosing.
pub(crate) fn rewrite_request_head(
    head: &[u8],
    host: &str,
    port: u16,
    proxy: &ProxyServer,
) -> Option<Vec<u8>> {
    let text = head.strip_suffix(b"\r\n\r\n")?;
    let mut lines = text
        .split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let request_line = std::str::from_utf8(lines.next()?).ok()?;
    let mut parts = request_line.splitn(3, ' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    if !token(method) || !version.starts_with("HTTP/") || !target.starts_with('/') {
        return None;
    }
    let target = format!("http://{}{target}", authority(host, port, Some(80)));
    let mut out = format!("{method} {target} {version}\r\n").into_bytes();
    for line in lines {
        if matches!(
            header_name(line).as_str(),
            "connection" | "proxy-connection" | "proxy-authorization" | "keep-alive"
        ) {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    if let Some(credentials) = basic_credentials(proxy) {
        out.extend_from_slice(format!("Proxy-Authorization: Basic {credentials}\r\n").as_bytes());
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    Some(out)
}

/// Status code of a response head (0 when it has none).
fn status_code(head: &[u8]) -> u16 {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    String::from_utf8_lossy(line)
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

/// The proxy's response head with `Connection: close` (its own connection
/// headers dropped, with their folded continuation lines), so that ureq does
/// not reuse the bridged connection.
pub(crate) fn close_response_head(head: &[u8]) -> Vec<u8> {
    let text = head.strip_suffix(b"\r\n\r\n").unwrap_or(head);
    let mut out = Vec::with_capacity(head.len() + 24);
    let mut dropping = false;
    for (i, line) in text
        .split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .enumerate()
    {
        if i > 0 {
            if !line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
                dropping = matches!(
                    header_name(line).as_str(),
                    "connection" | "proxy-connection" | "keep-alive"
                );
            }
            if dropping {
                continue;
            }
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out
}

/// The response head to pass on to ureq from what the proxy sends: interim
/// `1xx` heads are skipped (ureq knows none, it would take the first one for the
/// response and the real response for its body), and the final head gets
/// [`close_response_head`]. Returns it with the bytes that followed it.
/// A `101 Switching Protocols` (ureq never asks for one) passes unchanged: the
/// connection speaks another protocol after it.
fn final_response_head(upstream: &mut impl Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut pending = Vec::new();
    loop {
        let (head, rest) = read_head_after(pending, upstream)?;
        match status_code(&head) {
            101 => return Ok((head, rest)),
            100..=199 => pending = rest,
            _ => return Ok((close_response_head(&head), rest)),
        }
    }
}

/// Relays the rest of a rewritten plain-HTTP request to the proxy and the
/// response back, its head from [`final_response_head`].
pub(crate) fn relay_one_request(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let (Ok(mut client_read), Ok(mut upstream_write)) = (client.try_clone(), upstream.try_clone())
    else {
        return Ok(());
    };
    let up = std::thread::Builder::new()
        .name("rustpy-http-proxy-up".into())
        .spawn(move || {
            let _ = io::copy(&mut client_read, &mut upstream_write);
            let _ = upstream_write.shutdown(Shutdown::Both);
            let _ = client_read.shutdown(Shutdown::Both);
        })?;
    let (mut upstream_read, mut client_write) = (upstream, client);
    let mut down = || -> io::Result<()> {
        let (head, rest) = final_response_head(&mut upstream_read)?;
        client_write.write_all(&head)?;
        client_write.write_all(&rest)?;
        io::copy(&mut upstream_read, &mut client_write)?;
        Ok(())
    };
    let result = down();
    let _ = client_write.shutdown(Shutdown::Both);
    let _ = upstream_read.shutdown(Shutdown::Both);
    let _ = up.join();
    result
}

/// Opens a `CONNECT` tunnel to `host:port` through the proxy on `stream`.
pub fn connect_tunnel(
    stream: &mut TcpStream,
    proxy: &ProxyServer,
    host: &str,
    port: u16,
) -> io::Result<()> {
    let target = authority(host, port, None);
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(credentials) = basic_credentials(proxy) {
        request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let (head, rest) = read_head(stream)?;
    let status_line =
        String::from_utf8_lossy(head.split(|b| *b == b'\r').next().unwrap_or_default())
            .into_owned();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(io::Error::new(
            if matches!(code, 401 | 403 | 407) {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::ConnectionRefused
            },
            format!(
                "the proxy {} refused the tunnel to {target}: {status_line}",
                proxy.address()
            ),
        ));
    }
    if !rest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the proxy sent data before the TLS handshake",
        ));
    }
    Ok(())
}

/// Forwards the connection ureq made to the bridge through the proxy.
fn forward(
    mut client: TcpStream,
    mut upstream: TcpStream,
    proxy: &ProxyServer,
    host: &str,
    port: u16,
    timeout: Duration,
) -> io::Result<()> {
    client.set_read_timeout(Some(timeout))?;
    upstream.set_read_timeout(Some(timeout))?;
    upstream.set_write_timeout(Some(timeout))?;
    let mut first = [0u8; 1];
    if client.peek(&mut first)? == 0 {
        return Ok(());
    }
    let tunnel = first[0] == TLS_HANDSHAKE;
    if tunnel {
        connect_tunnel(&mut upstream, proxy, host, port)?;
    } else {
        let (head, rest) = read_head(&mut client)?;
        let head = rewrite_request_head(&head, host, port, proxy).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "refused a request that is not an origin-form HTTP request",
            )
        })?;
        upstream.write_all(&head)?;
        upstream.write_all(&rest)?;
    }
    client.set_read_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;
    let _ = upstream.set_nodelay(true);
    if tunnel {
        relay(client, upstream);
        Ok(())
    } else {
        relay_one_request(client, upstream)
    }
}

/// ureq resolver that sends every connection through an HTTP proxy itself
/// (see the module documentation).
pub struct HttpProxyResolver {
    proxy: ProxyServer,
    timeout: Duration,
}

impl HttpProxyResolver {
    pub fn new(proxy: ProxyServer) -> Self {
        HttpProxyResolver {
            proxy,
            timeout: HANDSHAKE_TIMEOUT,
        }
    }
}

impl ureq::Resolver for HttpProxyResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let (host, port) = split_netloc(netloc)?;
        let upstream = connect_proxy(&self.proxy, self.timeout)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let proxy = self.proxy.clone();
        let timeout = self.timeout;
        std::thread::Builder::new()
            .name("rustpy-http-proxy".into())
            .spawn(move || {
                let Some(client) = accept_one(listener) else {
                    return;
                };
                if let Err(e) = forward(client, upstream, &proxy, &host, port, timeout) {
                    log::warn!("HTTP proxy {}: {e}", proxy.address());
                }
            })?;
        Ok(vec![addr])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(user: Option<&str>, password: Option<&str>) -> ProxyServer {
        ProxyServer {
            host: "proxy.corp".into(),
            port: 3128,
            user: user.map(str::to_string),
            password: password.map(str::to_string),
        }
    }

    #[test]
    fn request_heads_are_rewritten_for_the_proxy() {
        let head = b"POST /api/items?x=1 HTTP/1.1\r\nHost: far.test:8080\r\nConnection: keep-alive\r\nProxy-Authorization: Basic stale\r\nContent-Length: 2\r\n\r\n";
        let out =
            rewrite_request_head(head, "far.test", 8080, &proxy(Some("me"), Some("p:w"))).unwrap();
        let out = String::from_utf8(out).unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode("me:p:w");
        assert_eq!(out, format!("POST http://far.test:8080/api/items?x=1 HTTP/1.1\r\nHost: far.test:8080\r\nContent-Length: 2\r\nProxy-Authorization: Basic {auth}\r\nConnection: close\r\n\r\n"));
        // Port 80 is implied, IPv6 hosts are bracketed, no user means no credentials.
        let out = rewrite_request_head(
            b"GET / HTTP/1.1\r\nHost: x\r\n\r\n",
            "fd00::1",
            80,
            &proxy(None, None),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "GET http://[fd00::1]/ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        );
        // Other request forms are refused: ureq only sends origin form, so they come
        // from another program, which must not get the credentials for its own target.
        let refused: [&[u8]; 5] = [
            b"GET http://attacker.example/steal HTTP/1.1\r\nHost: attacker.example\r\n\r\n",
            b"GET http://a.test/ HTTP/1.1\r\n\r\n",
            b"CONNECT attacker.example:443 HTTP/1.1\r\n\r\n",
            b"OPTIONS * HTTP/1.1\r\n\r\n",
            b"G(T / HTTP/1.1\r\n\r\n",
        ];
        for head in refused {
            assert!(
                rewrite_request_head(head, "a.test", 80, &proxy(Some("me"), Some("pw"))).is_none(),
                "{}",
                String::from_utf8_lossy(head)
            );
        }
        // An origin-form request always goes to the host ureq resolved, whatever its Host header says.
        let out = rewrite_request_head(
            b"GET /x HTTP/1.1\r\nHost: attacker.example\r\n\r\n",
            "a.test",
            80,
            &proxy(None, None),
        )
        .unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .starts_with("GET http://a.test/x HTTP/1.1\r\n"));
        // Not HTTP.
        assert!(
            rewrite_request_head(b"\x16\x03\x01garbage\r\n\r\n", "a", 1, &proxy(None, None))
                .is_none()
        );
        assert!(
            rewrite_request_head(b"GET / HTTP/1.1\r\n", "a", 1, &proxy(None, None)).is_none(),
            "incomplete head"
        );
    }

    #[test]
    fn response_heads_say_connection_close() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\nProxy-Connection: keep-alive\r\nX-Folded: a\r\n b\r\n\r\n";
        assert_eq!(String::from_utf8(close_response_head(head)).unwrap(), "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Folded: a\r\n b\r\nConnection: close\r\n\r\n");
        // A folded continuation of a dropped header goes with it.
        let folded =
            b"HTTP/1.1 200 OK\r\nConnection: keep-alive,\r\n Upgrade\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            String::from_utf8(close_response_head(folded)).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        // HTTP/1.0 without any connection header.
        assert_eq!(
            String::from_utf8(close_response_head(b"HTTP/1.0 407 Auth\r\n\r\n")).unwrap(),
            "HTTP/1.0 407 Auth\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(status_code(b"HTTP/1.1 100 Continue\r\n\r\n"), 100);
        assert_eq!(status_code(b"garbage"), 0);
    }

    #[test]
    fn interim_responses_are_dropped() {
        // Interim heads arriving with the final one, or on their own.
        let mut together: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a.css>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
        let (head, rest) = final_response_head(&mut together).unwrap();
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(rest, b"ok");
        assert!(together.is_empty());
        let mut slow = io::Read::chain(
            &b"HTTP/1.1 102 Processing\r\n\r\n"[..],
            &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"[..],
        );
        let (head, rest) = final_response_head(&mut slow).unwrap();
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        assert!(rest.is_empty());
        // A protocol switch passes as it is.
        let mut upgrade: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n\x81\x00";
        let (head, rest) = final_response_head(&mut upgrade).unwrap();
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
        );
        assert_eq!(rest, b"\x81\x00");
        // Only interim heads: the connection closed before the response.
        let mut cut: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";
        assert_eq!(
            final_response_head(&mut cut).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn heads_are_read_up_to_the_blank_line() {
        let mut data: &[u8] = b"HTTP/1.1 200 OK\r\nX: y\r\n\r\nbody";
        let (head, rest) = read_head(&mut data).unwrap();
        assert_eq!(head, b"HTTP/1.1 200 OK\r\nX: y\r\n\r\n");
        assert_eq!(rest, b"body");
        let mut short: &[u8] = b"HTTP/1.1 200";
        assert_eq!(
            read_head(&mut short).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(basic_credentials(&proxy(Some(""), Some("x"))), None);
        assert_eq!(authority("::1", 443, None), "[::1]:443");
    }
}

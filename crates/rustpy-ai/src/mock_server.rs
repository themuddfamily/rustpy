//! A minimal HTTP/1.1 server on `127.0.0.1:0` for end-to-end tests of the
//! client. It serves one scripted [`Reply`] per connection, in order, and
//! records each request.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// A request as received by the server.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names are lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

pub(crate) enum Reply {
    /// `200 text/event-stream` with chunked transfer encoding; every element is
    /// written (and flushed) as its own HTTP chunk.
    Sse(Vec<Vec<u8>>),
    /// A complete response with a `content-length` body.
    Full {
        status: u16,
        content_type: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        body: String,
    },
    /// Like `Sse`, then a `ping` event every 20 ms until a write fails because the
    /// client closed the connection, which is reported on `hung_up`.
    SseUntilHangup {
        chunks: Vec<Vec<u8>>,
        hung_up: mpsc::Sender<()>,
    },
}

pub(crate) struct MockServer {
    pub base_url: String,
    requests: mpsc::Receiver<Recorded>,
}

impl MockServer {
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                if let Err(err) = serve(stream, reply, &tx) {
                    eprintln!("mock server: {err}");
                }
            }
        });
        MockServer {
            base_url,
            requests: rx,
        }
    }

    /// The next recorded request (waits up to 10 s).
    pub fn next_request(&self) -> Recorded {
        self.requests
            .recv_timeout(Duration::from_secs(10))
            .expect("the client sent a request")
    }

    /// Number of further requests recorded so far, without waiting.
    pub fn extra_requests(&self) -> usize {
        self.requests.try_iter().count()
    }
}

/// Splits `data` at the given (ascending) byte offsets.
pub(crate) fn split_at(data: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
    let mut pieces = Vec::new();
    let mut prev = 0;
    for &cut in cuts.iter().chain(std::iter::once(&data.len())) {
        if cut > prev {
            pieces.push(data[prev..cut].to_vec());
            prev = cut;
        }
    }
    pieces
}

fn serve(mut stream: TcpStream, reply: Reply, requests: &mpsc::Sender<Recorded>) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let request = read_request(&stream)?;
    let _ = requests.send(request);
    match reply {
        Reply::Full {
            status,
            content_type,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
                reason(status),
                body.len()
            );
            for (name, value) in headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes())?;
            stream.write_all(body.as_bytes())?;
            stream.flush()?;
        }
        Reply::Sse(chunks) => {
            write_sse_head(&mut stream)?;
            for chunk in chunks {
                write_chunk(&mut stream, &chunk)?;
                thread::sleep(Duration::from_millis(2));
            }
            stream.write_all(b"0\r\n\r\n")?;
            stream.flush()?;
        }
        Reply::SseUntilHangup { chunks, hung_up } => {
            // The client may hang up at any point, even before the first chunk.
            let mut stream_until_hangup = || -> io::Result<()> {
                write_sse_head(&mut stream)?;
                for chunk in &chunks {
                    write_chunk(&mut stream, chunk)?;
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                    write_chunk(&mut stream, b"event: ping\ndata: {\"type\": \"ping\"}\n\n")?;
                }
                Ok(())
            };
            if stream_until_hangup().is_err() {
                let _ = hung_up.send(());
            }
            return Ok(());
        }
    }
    let _ = stream.shutdown(Shutdown::Write);
    // Wait for the client to close its side so the response is never cut short.
    let mut rest = Vec::new();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.read_to_end(&mut rest);
    Ok(())
}

fn read_request(stream: &TcpStream) -> io::Result<Recorded> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_sse_head(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ntransfer-encoding: chunked\r\ncache-control: no-cache\r\nrequest-id: req_mock\r\n\r\n",
    )?;
    stream.flush()
}

fn write_chunk(stream: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    if data.is_empty() {
        return Ok(()); // an empty chunk would end the body
    }
    stream.write_all(format!("{:x}\r\n", data.len()).as_bytes())?;
    stream.write_all(data)?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        529 => "Site Overloaded",
        _ => "Status",
    }
}

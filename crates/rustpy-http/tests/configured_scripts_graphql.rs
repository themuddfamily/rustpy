//! Explicit configured acceptance: actual Node scripts and owned loopback HTTP.
//! No provider is discovered or started by the ordinary workspace suite.
use rustpy_http::{
    history::{append_named_history, load_history},
    parse,
    runtime::{self, Control, Options},
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Debug)]
struct Received {
    head: String,
    body: Value,
}
struct Endpoint {
    base: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Vec<Received>>>,
}
impl Endpoint {
    fn new(requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut received = Vec::new();
            while received.len() < requests && !cancelled.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, address)) => {
                        assert!(address.ip().is_loopback());
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let request = read_request(&mut stream);
                        let (status, body) =
                            if request.head.starts_with("POST /records/42 HTTP/1.1\r\n") {
                                assert_eq!(request.body, json!({"record":"42", "value":"π🙂"}));
                                (
                                    "201 Created",
                                    json!({"id":"42", "value":request.body["value"]}),
                                )
                            } else if request.head.starts_with("POST /graphql HTTP/1.1\r\n") {
                                assert_eq!(request.body, json!({"query":"{ viewer { id } }"}));
                                ("200 OK", json!({"data":{"viewer":{"id":"π42"}}}))
                            } else {
                                panic!("unexpected owned endpoint request: {request:?}");
                            };
                        assert!(request
                            .head
                            .to_ascii_lowercase()
                            .contains("content-type: application/json\r\n"));
                        let body = serde_json::to_vec(&body).unwrap();
                        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Record: 42\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                        stream.write_all(&body).unwrap();
                        received.push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "owned endpoint did not receive {requests} requests"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("owned loopback endpoint failed: {error}"),
                }
            }
            received
        });
        Self {
            base,
            stop,
            worker: Some(worker),
        }
    }
    fn finish(&mut self) -> Vec<Received> {
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("actual HTTP endpoint completed")
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Received {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 65536, "fixture request exceeds 64 KiB");
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .expect("real POST body has a content length");
    assert!(header_end + length <= 65536);
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "request closed before body");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 65536);
    }
    Received {
        head,
        body: serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
    }
}
fn configured_options(root: &Path) -> Options {
    let node = PathBuf::from(
        std::env::var_os("RUSTPY_HTTP_NODE")
            .expect("set RUSTPY_HTTP_NODE to the explicitly admitted absolute Node executable"),
    );
    assert!(
        node.is_absolute() && node.is_file(),
        "admitted Node must exist"
    );
    let mut options = Options::new(root.into());
    options.node = node.to_str().expect("Node executable path is UTF-8").into();
    options.timeout = Duration::from_secs(5);
    options.process_environment.inherit = false;
    options
}
fn request(source: &str) -> rustpy_http::HttpRequest {
    let mut requests = parse(source);
    assert_eq!(requests.len(), 1);
    requests.remove(0)
}

#[test]
#[ignore = "requires RUSTPY_HTTP_NODE; real pre-request/response JS and loopback exchange"]
fn configured_scripts_mutate_wire_request_publish_assertions_and_persist_history() {
    let root = tempfile::tempdir().unwrap();
    let mut endpoint = Endpoint::new(2);
    let source = r#"### Scripted record
# @name scriptedRecord
< {%
client.test('pre environment', () => client.assert(request.variables.get('seed') === 'captured'));
request.variables.set('record', '42');
client.global.set('trace', 'scripted');
client.log('pre π');
%}
POST $BASE/records/{{record}}
Content-Type: application/json
X-Trace: {{trace}}
Authorization: Bearer fixture-secret

{"record":"{{record}}","value":"π🙂"}
> {%
client.test('created', () => client.assert(response.status === 201));
client.test('body', () => client.assert(response.body.id === '42' && response.body.value === 'π🙂'));
client.test('header', () => client.assert(response.headers.valueOf('X-Record') === '42'));
client.global.set('recordId', response.body.id);
client.log('post π');
%}
>>! response.json
"#.replace("$BASE", &endpoint.base);
    let path = root.path().join("scripts.http");
    std::fs::write(&path, &source).unwrap();
    let mut options = configured_options(root.path());
    options.variables.insert("seed".into(), "captured".into());
    let control = Control::default();
    let completed = runtime::execute(request(&source), options.clone(), &control);
    assert!(completed.response.is_success(), "{:?}", completed.response);
    assert_eq!(completed.response.status, 201);
    assert_eq!(completed.response.tests.len(), 4);
    assert!(completed.response.tests.iter().all(|test| test.passed));
    assert_eq!(completed.response.logs, vec!["pre π", "post π"]);
    assert_eq!(
        completed.globals.get("recordId").map(String::as_str),
        Some("42")
    );
    assert_eq!(control.phase(), "Finished");
    assert_eq!(
        std::fs::read(root.path().join("response.json")).unwrap(),
        completed.response.bytes
    );
    append_named_history(
        root.path(),
        &completed.request,
        &completed.response,
        Some("configured JS".into()),
        Some(400),
    )
    .unwrap();
    let history = load_history(root.path(), 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].execution_id, Some(400));
    assert_eq!(history[0].configuration.as_deref(), Some("configured JS"));
    let saved = history[0].response.as_ref().unwrap();
    assert_eq!(saved.tests, completed.response.tests);
    assert_eq!(saved.logs, completed.response.logs);
    let saved_request = history[0].request.as_ref().unwrap();
    assert!(saved_request.pre_script.is_none() && saved_request.handler.is_none());
    assert!(!saved_request
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("authorization")));
    let failed = source.replace("response.status === 201", "response.status === 202");
    let failed = runtime::execute(request(&failed), options, &Control::default());
    assert_eq!(
        failed.response.status, 201,
        "transport success must remain visible"
    );
    assert_eq!(
        failed.response.error.as_deref(),
        Some("Response handler assertions failed")
    );
    assert_eq!(
        failed
            .response
            .tests
            .iter()
            .filter(|test| !test.passed)
            .count(),
        1
    );
    assert!(failed
        .response
        .tests
        .iter()
        .any(|test| test.name == "created" && !test.passed));
    append_named_history(
        root.path(),
        &failed.request,
        &failed.response,
        Some("configured JS failure".into()),
        Some(401),
    )
    .unwrap();
    let history = load_history(root.path(), 10);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].execution_id, Some(401));
    assert_eq!(history[0].error, failed.response.error);
    assert_eq!(
        history[0].response.as_ref().unwrap().tests,
        failed.response.tests
    );
    let captured = endpoint.finish();
    assert_eq!(captured.len(), 2);
    assert!(captured.iter().all(|request| request
        .head
        .to_ascii_lowercase()
        .contains("x-trace: scripted\r\n")));
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
#[ignore = "requires RUSTPY_HTTP_NODE; real GraphQL POST and response JS assertions"]
fn configured_graphql_query_reaches_loopback_endpoint_and_publishes_response() {
    let root = tempfile::tempdir().unwrap();
    let mut endpoint = Endpoint::new(1);
    let source = r#"### GraphQL viewer
GRAPHQL $BASE/graphql

{ viewer { id } }
> {%
client.test('graphql result', () => client.assert(response.status === 200 && response.body.data.viewer.id === 'π42'));
client.global.set('viewer', response.body.data.viewer.id);
%}
"#.replace("$BASE", &endpoint.base);
    let completed = runtime::execute(
        request(&source),
        configured_options(root.path()),
        &Control::default(),
    );
    assert!(completed.response.is_success(), "{:?}", completed.response);
    assert_eq!(completed.request.method, "POST");
    assert_eq!(
        serde_json::from_str::<Value>(completed.request.body.as_deref().unwrap()).unwrap(),
        json!({"query":"{ viewer { id } }"})
    );
    assert_eq!(completed.response.tests.len(), 1);
    assert!(completed.response.tests[0].passed);
    assert_eq!(
        completed.globals.get("viewer").map(String::as_str),
        Some("π42")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&completed.response.body).unwrap(),
        json!({"data":{"viewer":{"id":"π42"}}})
    );
    append_named_history(
        root.path(),
        &completed.request,
        &completed.response,
        Some("configured GraphQL".into()),
        Some(411),
    )
    .unwrap();
    let saved = load_history(root.path(), 1);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].response.as_ref().unwrap().tests,
        completed.response.tests
    );
    assert_eq!(endpoint.finish().len(), 1);
}

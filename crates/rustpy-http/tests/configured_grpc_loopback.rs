//! Explicit installed grpcurl against a real private grpcio HTTP/2 server.
//! Root supplies grpcio/protobuf SDK and grpcurl; no fake executable/protoc.
use rustpy_core::process_tree::ProcessTree;
use rustpy_http::{
    parse,
    runtime::{self, Control, Options},
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const PROTO: &str = "syntax = \"proto3\";\npackage rustpy.acceptance;\nmessage Text { string value = 1; }\nservice Service { rpc Echo(Text) returns (Text); rpc Fail(Text) returns (Text); rpc Wait(Text) returns (Text); }\n";

fn executable(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("required admitted executable {name}")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must be an absolute installed executable"
    );
    path
}

struct Server {
    child: Child,
    owner: ProcessTree,
    root: PathBuf,
    port: u16,
    retired: bool,
}
impl Server {
    fn start(root: &Path, python: &Path) -> Self {
        let log = std::fs::File::create(root.join("server.log")).unwrap();
        let mut command = Command::new(python);
        command
            .arg("-u")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/grpc_acceptance_server.py"),
            )
            .arg(root)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().expect("selected actual Python must start");
        let owner = ProcessTree::new(&mut child).unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("attach actual server owner: {error}");
        });
        let mut server = Self {
            child,
            owner,
            root: root.into(),
            port: 0,
            retired: false,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join("ready.json").is_file() && Instant::now() < deadline {
            assert!(
                server.owner.poll(&mut server.child).unwrap().is_none(),
                "grpcio/protobuf server exited: {}",
                server.log()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready: Value =
            serde_json::from_slice(&std::fs::read(root.join("ready.json")).unwrap_or_else(
                |error| panic!("real grpc server readiness: {error}: {}", server.log()),
            ))
            .unwrap();
        assert_eq!(ready["pid"], server.child.id());
        server.port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
        server
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("server.log")).unwrap_or_default()
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn await_event(&mut self, kind: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.events().iter().any(|event| event["kind"] == kind) && Instant::now() < deadline
        {
            assert!(
                self.owner.poll(&mut self.child).unwrap().is_none(),
                "server exited: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            self.events().iter().any(|event| event["kind"] == kind),
            "actual server never observed {kind}: {}",
            self.log()
        );
    }
    fn finish(&mut self) {
        std::fs::write(self.root.join("stop"), "stop").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.owner.poll(&mut self.child).unwrap() {
                self.retired = true;
                assert!(
                    status.success(),
                    "actual server retirement: {status}: {}",
                    self.log()
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual server must physically retire"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(self.owner.stamp().contains("reaped=true"));
        assert!(self
            .events()
            .iter()
            .any(|event| event["kind"] == "server-retired"));
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if !self.retired {
            self.owner.force(&mut self.child);
        }
    }
}

fn request(port: u16, method: &str) -> rustpy_http::HttpRequest {
    let source = format!("### actual installed gRPC\n# @proto acceptance.proto\n# @import-path .\n# @timeout 10000\nGRPC grpc://127.0.0.1:{port}/rustpy.acceptance.Service/{method}\nx-rustpy-acceptance: actual-grpc-metadata\n\n{{\"value\":\"π🙂 actual\"}}\n");
    parse(&source)
        .into_iter()
        .next()
        .expect("actual GRPC request parser route")
}

#[test]
#[ignore = "requires RUSTPY_GRPC_ADMITTED=1, absolute RUSTPY_TEST_GRPCURL and RUSTPY_PROVIDER_PYTHON with grpcio/protobuf"]
fn configured_grpcurl_descriptor_exchange_status_cancel_and_server_retirement() {
    assert_eq!(std::env::var("RUSTPY_GRPC_ADMITTED").as_deref(), Ok("1"));
    let python = executable("RUSTPY_PROVIDER_PYTHON");
    let grpcurl = executable("RUSTPY_TEST_GRPCURL");
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(root.join("acceptance.proto"), PROTO).unwrap();
    let mut server = Server::start(&root, &python);
    let mut options = Options::new(root.clone());
    options.grpcurl = grpcurl.to_string_lossy().into_owned();
    options.timeout = Duration::from_secs(10);
    let completed = runtime::execute(
        request(server.port, "Echo"),
        options.clone(),
        &Control::default(),
    );
    assert_eq!(completed.response.status, 200, "{:?}", completed.response);
    assert!(completed.response.is_success());
    assert_eq!(completed.response.content_type, "application/grpc+json");
    let body: Value = serde_json::from_str(&completed.response.body).unwrap();
    assert_eq!(body, json!({"value": "echo:π🙂 actual"}));
    server.await_event("Echo");
    let events = server.events();
    let observed = events.iter().find(|event| event["kind"] == "Echo").unwrap();
    assert_eq!(observed["value"], "π🙂 actual");
    assert_eq!(
        observed["metadata"]["x-rustpy-acceptance"],
        "actual-grpc-metadata"
    );

    let completed = runtime::execute(
        request(server.port, "Fail"),
        options.clone(),
        &Control::default(),
    );
    assert_eq!(
        completed.response.status, 500,
        "actual gRPC errors retain existing HTTP adapter status mapping"
    );
    let error = completed
        .response
        .error
        .expect("real gRPC status must be an adapter error");
    assert!(
        error.contains("InvalidArgument") && error.contains("ACTUAL_GRPC_INVALID_ARGUMENT"),
        "{error}"
    );
    server.await_event("Fail");

    let control = Control::default();
    let worker_control = control.clone();
    let waiting = request(server.port, "Wait");
    let worker = std::thread::spawn(move || runtime::execute(waiting, options, &worker_control));
    server.await_event("Wait");
    control.cancel();
    let completed = worker
        .join()
        .expect("actual grpcurl request worker drains after cancellation");
    assert_eq!(completed.response.status, 0);
    assert!(completed
        .response
        .error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
    server.await_event("wait-retired");
    let port = server.port;
    server.finish();
    let rebound = std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("owned gRPC listener must be released after physical server retirement");
    drop(rebound);
}

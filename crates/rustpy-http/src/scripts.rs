//! Trusted request-file JavaScript via Node, with bounded execution and captured APIs.
use crate::runtime::Control;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptTest {
    pub name: String,
    pub passed: bool,
    pub message: String,
}
#[derive(Default, Deserialize)]
pub struct ResultData {
    pub globals: HashMap<String, String>,
    pub variables: HashMap<String, String>,
    pub tests: Vec<ScriptTest>,
    pub logs: Vec<String>,
}
const HARNESS: &str = include_str!("scripts.js");
pub fn run(
    source: &str,
    base: &Path,
    variables: &HashMap<String, String>,
    globals: &HashMap<String, String>,
    response: Option<&crate::HttpResponse>,
    control: &Control,
    node: &str,
) -> Result<ResultData, String> {
    run_with_environment(
        source,
        base,
        variables,
        globals,
        response,
        control,
        node,
        &crate::runtime::ProcessEnvironment::default(),
    )
}
#[allow(clippy::too_many_arguments)]
pub fn run_with_environment(
    source: &str,
    base: &Path,
    variables: &HashMap<String, String>,
    globals: &HashMap<String, String>,
    response: Option<&crate::HttpResponse>,
    control: &Control,
    node: &str,
    environment: &crate::runtime::ProcessEnvironment,
) -> Result<ResultData, String> {
    let source = if let Some(path) = source.strip_prefix("> ") {
        let snapshot = rustpy_core::vfs::Vfs::new()
            .read(
                &base.join(path),
                rustpy_core::vfs::ReadLimits {
                    max_file_bytes: 256 * 1024,
                    max_cached_bytes: 0,
                    max_cached_files: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        String::from_utf8(snapshot.bytes.to_vec()).map_err(|error| error.to_string())?
    } else {
        source.into()
    };
    if source.len() > 256 * 1024 {
        return Err("HTTP script exceeds 256 KiB".into());
    }
    let payload = serde_json::json!({"source":source,"variables":variables,"globals":globals,"response":response.map(|response|serde_json::json!({"status":response.status,"body":serde_json::from_str::<serde_json::Value>(&response.body).unwrap_or_else(|_|response.body.clone().into()),"headers":response.headers}))});
    let mut command = Command::new(node);
    environment.apply(&mut command);
    let mut child = command
        .args(["--max-old-space-size=64", "-e", HARNESS])
        .current_dir(environment.working_dir.as_deref().unwrap_or(base))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot start JavaScript runtime {node}: {error}"))?;
    let input = serde_json::to_vec(&payload).map_err(|error| error.to_string())?;
    if let Err(error) = child.stdin.take().unwrap().write_all(&input) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.to_string());
    }
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut text = Vec::new();
        out.take(1024 * 1024 + 1)
            .read_to_end(&mut text)
            .map(|_| text)
    });
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        err.take(64 * 1024).read_to_string(&mut text).map(|_| text)
    });
    let started = Instant::now();
    let status = loop {
        if control.cancelled() || started.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = output.join();
            let _ = errors.join();
            return Err("HTTP JavaScript cancelled or exceeded 3 seconds".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output.join();
                let _ = errors.join();
                return Err(error.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = output
        .join()
        .map_err(|_| "JavaScript output worker stopped")?
        .map_err(|error| error.to_string())?;
    let errors = errors.join().ok().and_then(Result::ok).unwrap_or_default();
    if !status.success() {
        return Err(format!("HTTP script failed: {errors}"));
    }
    if bytes.len() > 1024 * 1024 {
        return Err("JavaScript output exceeds 1 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid JavaScript result: {error}"))
}

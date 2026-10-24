//! Request history (`<dir>/.rustpy/http-history.log`, one JSON object per line).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::exec::HttpResponse;
use crate::parser::HttpRequest;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix timestamp (seconds).
    pub time: i64,
    pub name: Option<String>,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub elapsed_ms: u64,
    pub size: usize,
    pub error: Option<String>,
    #[serde(default)]
    pub request: Option<HttpRequest>,
    #[serde(default)]
    pub response: Option<HttpResponse>,
    #[serde(default)]
    pub configuration: Option<String>,
    #[serde(default)]
    pub execution_id: Option<u64>,
}

pub fn history_path(dir: &Path) -> PathBuf {
    dir.join(".rustpy").join("http-history.log")
}

/// Appends one entry (ignored for `# @no-log` requests).
pub fn append_history(dir: &Path, req: &HttpRequest, resp: &HttpResponse) -> anyhow::Result<()> {
    append_named_history(dir, req, resp, None, None)
}
static HISTORY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn append_named_history(
    dir: &Path,
    req: &HttpRequest,
    resp: &HttpResponse,
    name: Option<String>,
    id: Option<u64>,
) -> anyhow::Result<()> {
    let _guard = HISTORY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if req.no_log {
        return Ok(());
    }
    let path = history_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut request = req.clone();
    request.headers.retain(|(key, _)| {
        !key.eq_ignore_ascii_case("authorization") && !key.eq_ignore_ascii_case("cookie")
    });
    request.directives.remove("auth");
    request.binary_body = None;
    request.pre_script = None;
    request.handler = None;
    let mut response = resp.clone();
    response
        .headers
        .retain(|(key, _)| !key.eq_ignore_ascii_case("set-cookie"));
    response.bytes.truncate(256 * 1024);
    if response.body.len() > 256 * 1024 {
        let mut end = 256 * 1024;
        while !response.body.is_char_boundary(end) {
            end -= 1;
        }
        response.body.truncate(end);
        response.body.push_str("\n[history body truncated]");
    }
    let entry = HistoryEntry {
        time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        name: req.name.clone(),
        method: req.method.clone(),
        url: req.url.clone(),
        status: resp.status,
        elapsed_ms: resp.elapsed.as_millis() as u64,
        size: resp.size,
        error: resp.error.clone(),
        request: Some(request),
        response: Some(response),
        configuration: name,
        execution_id: id,
    };
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 16 * 1024 * 1024) {
        let tail = load_history(dir, 16);
        let text = tail
            .into_iter()
            .rev()
            .filter_map(|entry| serde_json::to_string(&entry).ok())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{text}\n"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

/// The newest `limit` entries, newest first.
pub fn load_history(dir: &Path, limit: usize) -> Vec<HistoryEntry> {
    let Ok(snapshot) = rustpy_core::vfs::Vfs::new().read(
        &history_path(dir),
        rustpy_core::vfs::ReadLimits {
            max_file_bytes: 32 * 1024 * 1024,
            max_cached_bytes: 0,
            max_cached_files: 0,
        },
    ) else {
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(&snapshot.bytes) else {
        return Vec::new();
    };
    let mut entries: Vec<HistoryEntry> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// Removes the history file.
pub fn clear_history(dir: &Path) -> anyhow::Result<()> {
    let path = history_path(dir);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn history_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let req = HttpRequest {
            name: Some("one".into()),
            method: "GET".into(),
            url: "https://a/b".into(),
            ..Default::default()
        };
        let resp = HttpResponse {
            status: 200,
            status_text: "OK".into(),
            elapsed: Duration::from_millis(12),
            size: 5,
            ..Default::default()
        };
        append_history(dir.path(), &req, &resp).unwrap();
        let req2 = HttpRequest {
            method: "POST".into(),
            url: "https://a/c".into(),
            ..Default::default()
        };
        let resp2 = HttpResponse {
            error: Some("boom".into()),
            ..Default::default()
        };
        append_history(dir.path(), &req2, &resp2).unwrap();
        let skipped = HttpRequest {
            method: "DELETE".into(),
            url: "https://a/d".into(),
            no_log: true,
            ..Default::default()
        };
        append_history(dir.path(), &skipped, &resp).unwrap();

        let all = load_history(dir.path(), 10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].method, "POST");
        assert_eq!(all[0].error.as_deref(), Some("boom"));
        assert_eq!(all[1].name.as_deref(), Some("one"));
        assert_eq!(all[1].status, 200);
        assert_eq!(all[1].elapsed_ms, 12);
        assert_eq!(load_history(dir.path(), 1).len(), 1);
        clear_history(dir.path()).unwrap();
        assert!(load_history(dir.path(), 10).is_empty());
    }
}

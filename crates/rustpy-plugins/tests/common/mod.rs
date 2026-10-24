//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// The example plugin shipped with the crate.
pub fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("solarized-theme")
}

/// Writes `files` (relative path → contents) below `dir`.
pub fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

/// Creates `<root>/<folder>` holding a plugin with the given manifest fields
/// (merged over a minimal valid manifest) and files.
pub fn write_plugin(root: &Path, folder: &str, manifest: Value, files: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(folder);
    fs::create_dir_all(&dir).unwrap();
    let mut full = json!({"id": "com.example.test", "name": "Test Plugin", "version": "1.0.0", "vendor": "Tests"});
    for (k, v) in manifest.as_object().expect("manifest object") {
        full[k] = v.clone();
    }
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&full).unwrap(),
    )
    .unwrap();
    write_files(&dir, files);
    dir
}

/// A keymap file in the IDE's preset format.
pub const KEYMAP: &str = r#"{"name": "Test Keys", "bindings": {"GotoAction": ["Ctrl+Shift+A", "F1"], "Run": ["Ctrl+F5"]}}"#;

/// A minimal TextMate JSON grammar.
pub const GRAMMAR: &str = r#"{"name": "Toy", "scopeName": "source.toy", "patterns": [{"match": "\\btoy\\b", "name": "keyword.toy"}]}"#;

/// A plugin contributing one of everything except schemes/templates (which the example covers).
pub fn write_tool_plugin(root: &Path, folder: &str, id: &str, version: &str) -> PathBuf {
    write_plugin(
        root,
        folder,
        json!({
            "id": id,
            "name": "Tool Kit",
            "version": version,
            "contributes": {
                "keymaps": ["keymaps/test.json"],
                "textmate_grammars": [{"language": "Toy", "extensions": [".toy", "TOYS"], "path": "syntaxes/toy.tmLanguage.json"}],
                "external_tools": [{"name": "Lint", "program": "ruff", "arguments": "check --config $PluginDir$/ruff.toml $FilePath$", "working_dir": "$ProjectFileDir$", "group": "Linters"}],
                "file_templates": [{"name": "Toy file", "extension": "toy", "path": "templates/new.toy"}]
            }
        }),
        &[
            ("keymaps/test.json", KEYMAP),
            ("syntaxes/toy.tmLanguage.json", GRAMMAR),
            ("ruff.toml", "line-length = 100\n"),
            ("templates/new.toy", "# ${NAME}\ntoy\n"),
        ],
    )
}

/// Names in the plugins directory of `config` (sorted).
pub fn plugins_dir_entries(config: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(config.join("plugins"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// One canned response of the mock server.
#[derive(Clone)]
pub struct Route {
    pub path: String,
    pub status: u16,
    pub body: Vec<u8>,
}

impl Route {
    pub fn ok(path: &str, body: impl Into<Vec<u8>>) -> Self {
        Route {
            path: path.into(),
            status: 200,
            body: body.into(),
        }
    }

    pub fn status(path: &str, status: u16) -> Self {
        Route {
            path: path.into(),
            status,
            body: format!("status {status}").into_bytes(),
        }
    }
}

/// A tiny HTTP/1.1 server on 127.0.0.1 serving fixed routes (404 otherwise),
/// one request per connection. It lives until the test process exits.
pub struct MockServer {
    pub base_url: String,
    hits: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
}

impl MockServer {
    pub fn start(routes: Vec<Route>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(routes);
        let (h, p) = (hits.clone(), paths.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (routes, h, p) = (routes.clone(), h.clone(), p.clone());
                thread::spawn(move || serve(stream, &routes, &h, &p));
            }
        });
        MockServer {
            base_url,
            hits,
            paths,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// Number of requests served so far.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    pub fn requested_paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

fn serve(mut stream: TcpStream, routes: &[Route], hits: &AtomicUsize, paths: &Mutex<Vec<String>>) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&request);
    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
    hits.fetch_add(1, Ordering::SeqCst);
    paths.lock().unwrap().push(path.clone());
    let (status, body) = routes
        .iter()
        .find(|r| r.path == path)
        .map(|r| (r.status, r.body.clone()))
        .unwrap_or((404, b"not found".to_vec()));
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// The words of an External Tools command as the IDE reads them (`rustpy-ui`,
/// `external_tools::split_template`): whitespace separates, `"…"` and `'…'`
/// group, there are no escapes. `(word, had quotes)`.
pub fn ide_words(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let (mut quoted, mut started) = (false, false);
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                quoted = true;
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push((std::mem::take(&mut word), quoted));
                    (quoted, started) = (false, false);
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push((word, quoted));
    }
    out
}

/// `text` with the `$Name$` macros of `macros` filled in, in one pass (as the
/// IDE does; unknown macros stay as written).
fn expand_macros(text: &str, macros: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    'scan: while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(after) = rest.strip_prefix("$$") {
            out.push('$');
            rest = after;
            continue;
        }
        for (name, value) in macros {
            if let Some(after) = rest.strip_prefix(&format!("${name}$")) {
                out.push_str(value);
                rest = after;
                continue 'scan;
            }
        }
        out.push('$');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// Runs a contributed tool the way the IDE runs External Tools (`rustpy-ui`,
/// `external_tools::build_invocation`, then the Run tool window): the words
/// of [`ide_words`] with `macros` (`("FilePath", "/p/x.py")`) filled in inside
/// each word, and the program started directly, without a shell. The program
/// is one word when it starts with a quote, or has spaces and is not a file;
/// otherwise it is taken as it is. (rustpy-ui's plugin_host tests run plugin
/// tools through the IDE's own code.)
#[cfg(unix)]
pub fn run_like_the_ide(
    tool: &rustpy_settings::ExternalTool,
    macros: &[(&str, &str)],
    cwd: &Path,
) -> std::process::Output {
    let template = tool.program.trim();
    let whole = expand_macros(template, macros);
    let mut args: Vec<String> = Vec::new();
    let split = template.starts_with(['"', '\''])
        || (!Path::new(&whole).is_file() && whole.split_whitespace().nth(1).is_some());
    let program = if split {
        let mut words = ide_words(template)
            .into_iter()
            .map(|(w, quoted)| (expand_macros(&w, macros), quoted));
        let first = words.next().map(|(w, _)| w).unwrap_or_default();
        args.extend(
            words
                .filter(|(w, quoted)| *quoted || !w.is_empty())
                .map(|(w, _)| w),
        );
        first
    } else {
        whole
    };
    for (word, quoted) in ide_words(&tool.arguments) {
        let value = expand_macros(&word, macros);
        // An unquoted macro that expands to nothing disappears.
        if quoted || !value.is_empty() {
            args.push(value);
        }
    }
    std::process::Command::new(&program)
        .args(&args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"))
}

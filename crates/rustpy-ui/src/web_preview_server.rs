//! Owned loopback static-asset host and browser live-reload protocol.
//! No project file is served without a per-host capability or same-site cookie.
use parking_lot::RwLock;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone)]
enum Override {
    Html(Vec<u8>),
    Markdown(String),
}
struct Shared {
    cancel: AtomicBool,
    revision: AtomicU64,
    reload: AtomicBool,
    overrides: RwLock<BTreeMap<PathBuf, Override>>,
    markdown: RwLock<rustpy_settings::markdown::Settings>,
}
pub struct Server {
    pub id: u64,
    pub root: PathBuf,
    pub address: SocketAddr,
    token: String,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    _capability: tempfile::TempDir,
}
impl Server {
    pub fn start(
        root: &Path,
        reload: bool,
        markdown: rustpy_settings::markdown::Settings,
    ) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        if !root.is_dir() {
            return Err("Preview root must be a directory".into());
        }
        let base = rustpy_settings::Settings::config_dir().join("web-preview");
        std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        let capability = tempfile::Builder::new()
            .prefix("host-")
            .rand_bytes(24)
            .tempdir_in(base)
            .map_err(|e| e.to_string())?;
        // Derive the capability from a second random name inside the 0700
        // temporary directory. The visible parent-directory entry is not a token.
        let private_token = tempfile::Builder::new()
            .prefix("cap-")
            .rand_bytes(24)
            .tempfile_in(capability.path())
            .map_err(|e| e.to_string())?;
        let token = private_token
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        drop(private_token);
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            cancel: AtomicBool::new(false),
            revision: AtomicU64::new(1),
            reload: AtomicBool::new(reload),
            overrides: RwLock::new(BTreeMap::new()),
            markdown: RwLock::new(markdown),
        });
        let state = shared.clone();
        let cwd = root.clone();
        let key = token.clone();
        let thread = std::thread::Builder::new()
            .name("web-preview-host".into())
            .spawn(move || {
                use notify::Watcher;
                let changed = state.clone();
                let mut watcher =
                    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                        if let Ok(event) = event {
                            if !matches!(event.kind, notify::EventKind::Access(_))
                                && event.paths.iter().any(|p| web_asset(p))
                                && changed.reload.load(Ordering::Acquire)
                            {
                                changed.revision.fetch_add(1, Ordering::AcqRel);
                            }
                        }
                    })
                    .ok();
                if let Some(watcher) = &mut watcher {
                    let _ = watcher.watch(&cwd, notify::RecursiveMode::Recursive);
                }
                while !state.cancel.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, peer)) => {
                            if peer.ip().is_loopback() {
                                let _ = serve(&mut stream, &cwd, address, &key, &state);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::park_timeout(Duration::from_millis(20))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            root,
            address,
            token,
            shared,
            thread: Some(thread),
            _capability: capability,
        })
    }
    pub fn url(&self, path: &Path) -> Result<String, String> {
        let path = rustpy_core::vfs::normalize_path(path);
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| "Preview file is outside the captured root")?;
        let mut url = url::Url::parse(&format!("http://{}/{}/", self.address, self.token))
            .map_err(|e| e.to_string())?;
        url.path_segments_mut()
            .map_err(|_| "Invalid preview origin")?
            .pop_if_empty()
            .extend(relative.iter().map(|part| part.to_string_lossy()));
        Ok(url.into())
    }
    pub fn override_html(&self, path: &Path, html: String) {
        let mut overrides = self.shared.overrides.write();
        if self.cancelling() {
            return;
        }
        overrides.insert(
            rustpy_core::vfs::normalize_path(path),
            Override::Html(html.into_bytes()),
        );
        self.bump();
    }
    pub fn override_markdown(&self, path: &Path, source: String) {
        let mut overrides = self.shared.overrides.write();
        if self.cancelling() {
            return;
        }
        overrides.insert(
            rustpy_core::vfs::normalize_path(path),
            Override::Markdown(source),
        );
        self.bump();
    }
    pub fn configure_markdown(&self, settings: &rustpy_settings::markdown::Settings) {
        let mut current = self.shared.markdown.write();
        if *current != *settings {
            *current = settings.clone();
            self.bump();
        }
    }
    pub fn remove_override(&self, path: &Path) {
        self.shared
            .overrides
            .write()
            .remove(&rustpy_core::vfs::normalize_path(path));
    }
    pub fn clear_overrides(&self) {
        self.shared.overrides.write().clear();
    }
    pub fn set_reload(&self, enabled: bool) {
        self.shared.reload.store(enabled, Ordering::Release);
    }
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
    pub fn bump(&self) {
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }
    pub fn note_saved(&self, path: &Path) {
        if self.shared.reload.load(Ordering::Acquire)
            && path.starts_with(&self.root)
            && web_asset(path)
        {
            self.bump();
        }
    }
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        self.clear_overrides();
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
    pub fn cancelling(&self) -> bool {
        self.shared.cancel.load(Ordering::Acquire)
    }
    pub fn running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }
    pub fn reap(&mut self) {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    pub fn stamp(&self) -> String {
        format!(
            "web-preview:{}:{}:{}:cancel={}",
            self.id,
            self.root.display(),
            self.address,
            self.cancelling()
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.cancel();
        self.reap();
    }
}
pub fn web_asset(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "html"
                | "htm"
                | "css"
                | "js"
                | "mjs"
                | "cjs"
                | "json"
                | "svg"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "woff"
                | "woff2"
                | "ttf"
                | "md"
                | "markdown"
        )
    )
}
fn decoded_path(path: &str) -> Result<PathBuf, String> {
    let mut bytes = Vec::new();
    let input = path.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            if i + 2 >= input.len() {
                return Err("Invalid escape".into());
            }
            let high = (input[i + 1] as char)
                .to_digit(16)
                .ok_or("Invalid escape")?;
            let low = (input[i + 2] as char)
                .to_digit(16)
                .ok_or("Invalid escape")?;
            bytes.push((high * 16 + low) as u8);
            i += 3;
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(bytes).map_err(|_| "Invalid UTF-8 path")?;
    if path.contains('\\') || path.chars().any(char::is_control) {
        return Err("Invalid preview path".into());
    }
    let path = PathBuf::from(path);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("Path escapes preview root".into());
    }
    if path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .any(|part| part.starts_with('.'))
    {
        return Err("Private preview path".into());
    }
    Ok(path)
}
fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = decoded_path(path)?;
    let path = root.join(relative);
    let path = if path.is_dir() {
        path.join("index.html")
    } else {
        path
    };
    let path = path.canonicalize().map_err(|_| "Preview file not found")?;
    if !path.starts_with(root) {
        return Err("Symlink escapes preview root".into());
    }
    if path.strip_prefix(root).unwrap().components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with('.'))
    }) {
        return Err("Private preview path".into());
    }
    Ok(path)
}
fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" | "md" | "markdown" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}
fn reload_script(address: SocketAddr, token: &str, revision: u64) -> String {
    format!(
        "<script>(()=>{{let revision={revision};const endpoint='http://{address}/{token}/_rustpy/revision';setInterval(async()=>{{try{{const response=await fetch(endpoint,{{cache:'no-store',credentials:'same-origin'}});if(!response.ok)return;const next=Number(await response.text());if(next!==revision){{revision=next;location.reload()}}}}catch(_){{}}}},500)}})();</script>"
    )
}
fn injected(mut html: String, script: &str) -> String {
    if let Some(index) = html.to_ascii_lowercase().rfind("</body>") {
        html.insert_str(index, script);
    } else {
        html.push_str(script);
    }
    html
}
fn write_response_bytes(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        if cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Preview host stopped",
            ));
        }
        if Instant::now() > deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Preview response deadline",
            ));
        }
        match stream.write(&bytes[..bytes.len().min(16384)]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(2))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn response(
    stream: &mut TcpStream,
    cancel: &AtomicBool,
    status: &str,
    kind: &str,
    body: &[u8],
    cookie: Option<&str>,
    head: bool,
) -> std::io::Result<()> {
    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(cookie) = cookie {
        header.push_str(&format!(
            "Set-Cookie: rustpy_preview_{cookie}={cookie}; HttpOnly; SameSite=Strict; Path=/\r\n"
        ));
    }
    header.push_str("\r\n");
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    write_response_bytes(stream, header.as_bytes(), deadline, cancel)?;
    if !head {
        write_response_bytes(stream, body, deadline, cancel)?;
    }
    Ok(())
}
fn serve(
    stream: &mut TcpStream,
    root: &Path,
    address: SocketAddr,
    token: &str,
    state: &Shared,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(150)))?;
    stream.set_write_timeout(Some(Duration::from_millis(150)))?;
    let mut request = Vec::new();
    let start = Instant::now();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|s| s == b"\r\n\r\n") {
        if request.len() > 16 * 1024
            || start.elapsed() > Duration::from_secs(1)
            || state.cancel.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let Ok(text) = std::str::from_utf8(&request) else {
        return response(
            stream,
            &state.cancel,
            "400 Bad Request",
            "text/plain",
            b"Invalid request",
            None,
            false,
        );
    };
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("");
    let uri = first.next().unwrap_or("");
    let head = method == "HEAD";
    if !matches!(method, "GET" | "HEAD") {
        return response(
            stream,
            &state.cancel,
            "405 Method Not Allowed",
            "text/plain",
            b"Read-only preview",
            None,
            head,
        );
    }
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim()))
        .collect::<BTreeMap<_, _>>();
    if headers.get("host").copied() != Some(address.to_string().as_str()) {
        return response(
            stream,
            &state.cancel,
            "403 Forbidden",
            "text/plain",
            b"Invalid preview Host",
            None,
            head,
        );
    }
    let path = uri.split('?').next().unwrap_or(uri);
    let prefix = format!("/{token}/");
    let capability = path.starts_with(&prefix);
    let cookie = headers.get("cookie").is_some_and(|cookie| {
        cookie
            .split(';')
            .any(|part| part.trim() == format!("rustpy_preview_{token}={token}"))
    });
    if !capability && !cookie {
        return response(
            stream,
            &state.cancel,
            "403 Forbidden",
            "text/plain",
            b"Preview capability required",
            None,
            head,
        );
    }
    if let Some(origin) = headers.get("origin") {
        if *origin != format!("http://{address}") {
            return response(
                stream,
                &state.cancel,
                "403 Forbidden",
                "text/plain",
                b"Cross-origin preview request denied",
                None,
                head,
            );
        }
    }
    let relative = if capability {
        &path[prefix.len()..]
    } else {
        path.strip_prefix('/').unwrap_or(path)
    };
    if relative == "_rustpy/revision" {
        return response(
            stream,
            &state.cancel,
            "200 OK",
            "text/plain",
            state
                .revision
                .load(Ordering::Acquire)
                .to_string()
                .as_bytes(),
            Some(token),
            head,
        );
    }
    let path = match resolve(root, relative) {
        Ok(path) => path,
        Err(error) => {
            return response(
                stream,
                &state.cancel,
                "404 Not Found",
                "text/plain",
                error.as_bytes(),
                Some(token),
                head,
            );
        }
    };
    let override_value = state.overrides.read().get(&path).cloned();
    let mut is_markdown = matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown")
    );
    let mut bytes = match override_value {
        Some(Override::Html(bytes)) => {
            is_markdown = false;
            bytes
        }
        Some(Override::Markdown(source)) => {
            is_markdown = true;
            source.into_bytes()
        }
        None => {
            let metadata = std::fs::metadata(&path)?;
            if metadata.len() > 32 * 1024 * 1024 {
                return response(
                    stream,
                    &state.cancel,
                    "413 Content Too Large",
                    "text/plain",
                    b"Preview asset exceeds 32 MiB",
                    Some(token),
                    head,
                );
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&path)?
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Ok(());
            }
            bytes
        }
    };
    if is_markdown {
        if bytes.len() > 1024 * 1024 {
            return response(
                stream,
                &state.cancel,
                "413 Content Too Large",
                "text/plain",
                b"Markdown preview exceeds 1 MiB",
                Some(token),
                head,
            );
        }
        if let Ok(source) = std::str::from_utf8(&bytes) {
            bytes = crate::markdown_workspace::html_document(
                source,
                None,
                &state.markdown.read(),
                true,
            )
            .into_bytes();
        }
    }
    if mime(&path).starts_with("text/html") {
        if let Ok(html) = String::from_utf8(bytes.clone()) {
            bytes = injected(
                html,
                &reload_script(address, token, state.revision.load(Ordering::Acquire)),
            )
            .into_bytes();
        }
    }
    response(
        stream,
        &state.cancel,
        "200 OK",
        mime(&path),
        &bytes,
        Some(token),
        head,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_releases_unsaved_overrides_and_rejects_late_publication() {
        let root = tempfile::tempdir().unwrap();
        let server = Server {
            id: 1,
            root: root.path().into(),
            address: std::net::SocketAddr::from(([127, 0, 0, 1], 1)),
            token: "unused-no-listener".into(),
            shared: Arc::new(state()),
            thread: None,
            _capability: root,
        };
        let path = server.root.join("private.md");
        server.override_html(&path, "unsaved html".into());
        assert_eq!(server.shared.overrides.read().len(), 1);
        server.cancel();
        assert!(server.shared.overrides.read().is_empty());
        server.override_markdown(&path, "late unsaved text".into());
        server.override_html(&path, "late html".into());
        assert!(server.shared.overrides.read().is_empty());
    }
    #[test]
    fn paths_reject_encoded_traversal_and_private_roots() {
        for path in [
            "../secret",
            "%2e%2e/secret",
            "%2Fetc/passwd",
            ".git/config",
            ".env",
            "a\\b",
            "a%00b",
        ] {
            assert!(decoded_path(path).is_err(), "{path}");
        }
        assert_eq!(
            decoded_path("assets/a%20b.js").unwrap(),
            PathBuf::from("assets/a b.js")
        );
    }
    #[test]
    #[cfg(unix)]
    fn symlink_escape_is_never_served() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"private").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
            assert!(resolve(root.path(), "escape/secret").is_err());
        }
    }
    #[test]
    fn live_reload_is_injected_before_body_close_and_preserves_scripts() {
        let html = injected(
            "<body><script>window.value=42</script></body>".into(),
            "<script>reload()</script>",
        );
        assert!(html.contains("window.value=42"));
        assert!(html.ends_with("<script>reload()</script></body>"));
    }
    fn state() -> Shared {
        Shared {
            cancel: AtomicBool::new(false),
            revision: AtomicU64::new(7),
            reload: AtomicBool::new(true),
            overrides: RwLock::new(BTreeMap::new()),
            markdown: RwLock::new(Default::default()),
        }
    }
    fn exchange(root: &Path, state: &Shared, target: &str, headers: &str) -> String {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let request = format!("GET {target} HTTP/1.1\r\nHost: {address}\r\n{headers}\r\n");
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut result = String::new();
            stream.read_to_string(&mut result).unwrap();
            result
        });
        let (mut stream, _) = listener.accept().unwrap();
        serve(&mut stream, root, address, "cap", state).unwrap();
        drop(stream);
        client.join().unwrap()
    }
    #[test]
    fn real_http_response_serves_html_js_and_root_relative_cookie_assets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("index.html"),
            "<body><script src='/app.js'></script></body>",
        )
        .unwrap();
        std::fs::write(root.path().join("app.js"), "window.answer=42;").unwrap();
        let state = state();
        let response = exchange(root.path(), &state, "/cap/index.html", "");
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("Set-Cookie: rustpy_preview_cap=cap"));
        assert!(response.contains("location.reload()"));
        let js = exchange(
            root.path(),
            &state,
            "/app.js",
            "Cookie: rustpy_preview_cap=cap\r\n",
        );
        assert!(js.contains("text/javascript"));
        assert!(js.ends_with("window.answer=42;"));
        assert!(exchange(root.path(), &state, "/app.js", "").starts_with("HTTP/1.1 403"));
        assert!(exchange(root.path(), &state, "/cap/%2e%2e/secret", "").starts_with("HTTP/1.1 404"));
    }
    #[test]
    fn generated_unicode_url_serves_nested_html_and_relative_asset_on_owned_host() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("folder λ");
        std::fs::create_dir_all(nested.join("assets")).unwrap();
        let html = "<body>actual π<link rel='stylesheet' href='assets/style%20%CF%80.css'></body>";
        std::fs::write(nested.join("index π.html"), html).unwrap();
        std::fs::write(nested.join("assets/style π.css"), "body { color: red; }").unwrap();
        let mut server = Server::start(root.path(), true, Default::default()).unwrap();
        let request = |target: &str| {
            let mut stream =
                TcpStream::connect_timeout(&server.address, Duration::from_secs(2)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                server.address
            )
            .unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let page = url::Url::parse(
            &server
                .url(&server.root.join("folder λ/index π.html"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            page.path(),
            format!("/{}/folder%20%CE%BB/index%20%CF%80.html", server.token)
        );
        let response = request(page.path());
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("actual π"));
        assert!(response.contains("assets/style%20%CF%80.css"));
        assert!(response.contains("location.reload()"));
        let asset = page.join("assets/style%20%CF%80.css").unwrap();
        let response = request(asset.path());
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("Content-Type: text/css"));
        assert!(response.ends_with("body { color: red; }"));
        assert!(
            request("/forged-capability/folder%20%CE%BB/index%20%CF%80.html")
                .starts_with("HTTP/1.1 403")
        );
        assert!(request(&format!("/{}/%2e%2e/secret", server.token)).starts_with("HTTP/1.1 404"));
        server.cancel();
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.running() {
            assert!(
                Instant::now() < deadline,
                "owned preview host did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        server.reap();
        assert!(TcpStream::connect_timeout(&server.address, Duration::from_millis(100)).is_err());
    }
    #[test]
    fn font_assets_keep_correct_types_and_root_traversal_and_size_guards() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let state = state();
        for (extension, mime) in [
            ("woff", "font/woff"),
            ("woff2", "font/woff2"),
            ("ttf", "font/ttf"),
        ] {
            let path = root.join(format!("owned.{extension}"));
            std::fs::write(&path, "owned font route bytes").unwrap();
            assert!(
                web_asset(&path),
                "saved font edits must invalidate the preview"
            );
            let response = exchange(&root, &state, &format!("/cap/owned.{extension}"), "");
            assert!(response.starts_with("HTTP/1.1 200"));
            assert!(response.contains(&format!("Content-Type: {mime}")));
            assert!(response.ends_with("owned font route bytes"));
        }
        let huge = root.join("oversize.woff2");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(32 * 1024 * 1024 + 1)
            .unwrap();
        assert!(exchange(&root, &state, "/cap/oversize.woff2", "").starts_with("HTTP/1.1 413"));
        assert!(exchange(&root, &state, "/cap/%2e%2e/owned.ttf", "").starts_with("HTTP/1.1 404"));
        assert!(exchange(&root, &state, "/unadmitted/owned.woff2", "").starts_with("HTTP/1.1 403"));
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let secret = outside.path().join("outside.ttf");
            std::fs::write(&secret, "must not escape canonical preview root").unwrap();
            std::os::unix::fs::symlink(&secret, root.join("escaped.ttf")).unwrap();
            let response = exchange(&root, &state, "/cap/escaped.ttf", "");
            assert!(response.starts_with("HTTP/1.1 404"));
            assert!(!response.contains("must not escape"));
        }
    }
    #[test]
    fn markdown_http_preview_uses_renderer_and_relative_host_assets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("notes.md"),
            "# Notes\n\n![local](image.png)",
        )
        .unwrap();
        let response = exchange(root.path(), &state(), "/cap/notes.md", "");
        assert!(response.contains("<h1 id=\"notes\">"));
        assert!(response.contains("src=\"image.png\""));
        assert!(!response.contains("<base href=\"file:"));
    }
}

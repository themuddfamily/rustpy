//! Genuine local KaTeX/Mermaid distributions through the production Markdown
//! capability host and selected headless Chrome. CDP only reads actual engine
//! output; no DOM injection, provider stand-in or native IDE window is used.
use super::*;
use rustpy_settings::web_browsers::{DefaultBrowser, Kind, Profile};
use std::{
    collections::BTreeMap,
    io::Read,
    time::{Duration, Instant},
};

const DRIVER: &str = r#"import json,sys,time,urllib.request,websocket
port,origin,method,params=sys.argv[1:]
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
deadline=time.monotonic()+10
while True:
    with opener.open('http://127.0.0.1:'+port+'/json/list',timeout=10) as response:
        pages=json.load(response)
    all_pages=pages
    pages=[page for page in pages if page.get('type')=='page' and page.get('url','').startswith(origin)]
    if pages or time.monotonic()>=deadline:break
    time.sleep(0.05)
assert len(pages)==1, ('expected exactly one owned preview tab',origin,all_pages)
socket=websocket.create_connection(pages[0]['webSocketDebuggerUrl'],timeout=15,suppress_origin=True,http_no_proxy=['127.0.0.1','localhost'])
try:
    request={'id':1,'method':method,'params':json.loads(params)}
    retryable=request['params'].get('expression','').startswith('(()=>')
    deadline=time.monotonic()+15
    socket.send(json.dumps(request))
    while True:
        result=json.loads(socket.recv())
        if result.get('id')==1:
            error=result.get('error',{}).get('message','')
            if retryable and time.monotonic()<deadline and any(text in error for text in ['Cannot find context','Execution context was destroyed','Inspected target navigated']):
                time.sleep(0.05)
                socket.send(json.dumps(request))
                continue
            assert 'error' not in result, result
            assert 'exceptionDetails' not in result.get('result',{}), result
            print('RUSTPY_REAL_CDP='+json.dumps(result['result']),flush=True)
            break
finally:
    socket.close()
"#;

#[derive(serde::Deserialize)]
struct Distribution {
    root: PathBuf,
    version: String,
    files: BTreeMap<String, String>,
}
#[derive(serde::Deserialize)]
struct Assets {
    katex: Distribution,
    mermaid: Distribution,
}
fn digest(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer).unwrap();
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    format!("{:x}", hash.finalize())
}
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"))
}
fn admitted_file(name: &str, hash_name: &str) -> PathBuf {
    let path = PathBuf::from(required(name));
    assert!(path.is_absolute() && path.is_file());
    assert_eq!(
        digest(&path),
        required(hash_name),
        "admitted {name} bytes changed"
    );
    path
}
fn copy_distribution(distribution: &Distribution, destination: &Path) {
    assert!(distribution.root.is_absolute() && distribution.root.is_dir());
    assert!(!distribution.version.trim().is_empty());
    assert!(!distribution.files.is_empty() && distribution.files.len() < 4096);
    let root = distribution.root.canonicalize().unwrap();
    let mut bytes = 0;
    for (name, hash) in &distribution.files {
        let relative = Path::new(name);
        assert!(relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))));
        let input = root.join(relative);
        assert!(!input.is_symlink() && input.is_file());
        assert!(input.canonicalize().unwrap().starts_with(&root));
        assert_eq!(
            digest(&input),
            *hash,
            "genuine distribution file changed: {name}"
        );
        bytes += input.metadata().unwrap().len();
        assert!(bytes <= 128 * 1024 * 1024);
        assert!(input.metadata().unwrap().len() <= 32 * 1024 * 1024);
        let output = destination.join(relative);
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::copy(&input, &output).unwrap();
        assert_eq!(digest(&output), *hash);
    }
}
fn identity(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
fn browser_children(pid: u32) -> BTreeMap<u32, String> {
    let mut owners = BTreeMap::new();
    owners.insert(pid, identity(pid).expect("owned Chrome must be alive"));
    loop {
        let before = owners.len();
        for entry in std::fs::read_dir("/proc").unwrap().filter_map(Result::ok) {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let (_, fields) = stat.rsplit_once(") ").unwrap();
            let fields = fields.split_whitespace().collect::<Vec<_>>();
            if fields[1]
                .parse::<u32>()
                .is_ok_and(|parent| owners.contains_key(&parent))
            {
                owners.insert(pid, fields[19].into());
            }
        }
        if owners.len() == before {
            return owners;
        }
    }
}
struct Fixture {
    app: IdeApp,
    directory: tempfile::TempDir,
    python: PathBuf,
    port: u16,
    origin: String,
}
impl Fixture {
    fn drain(&mut self) {
        let context = egui::Context::default();
        self.app.cancel_web_preview_project();
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        loop {
            self.app.run_panel.poll();
            self.app.web_preview_begin_frame(&context);
            if !self.app.web_preview_pending()
                && self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .all(|s| !s.owns_process())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cdp(&mut self, expression: &str) -> serde_json::Value {
        let id = self.app.next_session_id;
        self.app.next_session_id += 1;
        let args = vec!["-I".into(), "-u".into(), "-c".into(), DRIVER.into(), self.port.to_string(), self.origin.clone(), "Runtime.evaluate".into(), serde_json::json!({"expression":expression,"awaitPromise":true,"returnByValue":true,"timeout":15000}).to_string()];
        let environment = BTreeMap::from([
            ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
            ("no_proxy".into(), "127.0.0.1,localhost".into()),
        ]);
        let driver = RunSession::spawn_command(
            id,
            "Read actual Markdown engine output",
            &self.python,
            &args,
            self.directory.path(),
            &environment,
        );
        // The App owns the driver before an assertion can unwind.
        self.app.run_panel.add_session(driver);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.app.run_panel.poll();
            let driver = self
                .app
                .run_panel
                .sessions
                .iter()
                .find(|s| s.id == id)
                .unwrap();
            if !driver.owns_process() {
                let text = driver.text();
                assert_eq!(
                    driver.status,
                    rustpy_run::SessionStatus::Exited(0),
                    "actual CDP observation failed: {text}"
                );
                let result: serde_json::Value = serde_json::from_str(
                    text.lines()
                        .find_map(|line| line.strip_prefix("RUSTPY_REAL_CDP="))
                        .expect("actual engine protocol response"),
                )
                .unwrap();
                return result["result"]["value"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "actual CDP driver retirement deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.drain();
    }
}

#[test]
#[ignore = "ROOT-admitted genuine KaTeX/Mermaid file manifest, Chrome and websocket-client Python; headless real-engine only"]
fn configured_enhanced_markdown_renders_real_katex_fonts_and_mermaid_svg() {
    assert_eq!(required("RUSTPY_BROWSER_ADMITTED"), "1");
    assert!(std::env::var_os("RUSTPY_TRUST_ALL").is_none());
    for name in [
        "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_STARTER_ADDRESS",
        "DBUS_STARTER_BUS_TYPE",
        "AT_SPI_BUS_ADDRESS",
        "SESSION_MANAGER",
        "DESKTOP_STARTUP_ID",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "headless provider must not inherit ambient desktop authority: {name}"
        );
    }
    let chrome = admitted_file("RUSTPY_BROWSER_CHROME", "RUSTPY_BROWSER_CHROME_SHA256");
    assert_eq!(&std::fs::read(&chrome).unwrap()[..4], b"\x7fELF");
    let python = admitted_file(
        "RUSTPY_BROWSER_TEST_PYTHON",
        "RUSTPY_BROWSER_TEST_PYTHON_SHA256",
    );
    let manifest = admitted_file(
        "RUSTPY_MARKDOWN_ASSETS_MANIFEST",
        "RUSTPY_MARKDOWN_ASSETS_MANIFEST_SHA256",
    );
    let assets: Assets = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    assert!(
        assets.katex.files.contains_key("katex.min.css")
            && assets.katex.files.contains_key("katex.min.js")
    );
    assert!(assets
        .katex
        .files
        .keys()
        .any(|p| p.starts_with("fonts/") && p.ends_with(".woff2")));
    assert!(assets.mermaid.files.contains_key("mermaid.esm.min.mjs"));
    let temporary = PathBuf::from(required("TMPDIR")).canonicalize().unwrap();
    for name in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
        let path = PathBuf::from(required(name));
        assert!(path.is_absolute() && path.starts_with(&temporary));
        std::fs::create_dir_all(&path).unwrap();
        assert!(path.canonicalize().unwrap().starts_with(&temporary));
    }
    let directory = tempfile::Builder::new()
        .prefix("enhanced-markdown-")
        .tempdir_in(&temporary)
        .unwrap();
    copy_distribution(&assets.katex, &directory.path().join("assets/katex"));
    copy_distribution(&assets.mermaid, &directory.path().join("assets/mermaid"));
    let path = directory.path().join("captured π.md");
    std::fs::write(&path, "# Disk content must not render\n").unwrap();
    let profile = directory.path().join("chrome-profile");
    let arguments = rustpy_project::configuration_types::quote_arguments(&[
        "--headless=new".into(),
        "--no-sandbox".into(),
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-extensions".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--disable-breakpad".into(),
        "--disable-crash-reporter".into(),
        "--renderer-process-limit=2".into(),
        "--num-raster-threads=2".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
        format!("--user-data-dir={}", profile.display()),
        "$URL$".into(),
    ]);
    let mut settings = Settings::default();
    settings.markdown.math = true;
    settings.markdown.mermaid = true;
    settings.markdown.plantuml = false;
    settings.markdown.katex_assets = "./assets/katex".into();
    settings.markdown.mermaid_script = "./assets/mermaid/mermaid.esm.min.mjs".into();
    settings.web_browsers.default = DefaultBrowser::Profile("actual-markdown-chrome".into());
    settings.web_browsers.profiles.push(Profile {
        id: "actual-markdown-chrome".into(),
        name: "Actual Markdown Chrome".into(),
        kind: Kind::Chrome,
        program: chrome.to_string_lossy().into_owned(),
        arguments,
        enabled: true,
    });
    let mut app = IdeApp::new_welcome(settings);
    app.open_file(&path);
    let index = app.active.unwrap();
    let source = "# Captured enhanced π\n\nEditor revision42 formula $\\frac{1}{2}+x^2$.\n\n```mermaid\nflowchart LR\n A[\"Owned π 42\"] --> B[\"Current café\"]\n```\n";
    app.tabs[index]
        .state
        .doc
        .set_text(source, &[], &[])
        .unwrap();
    // Guard owns both browser/profile and capability host BEFORE launch/assertions.
    let mut fixture = Fixture {
        app,
        directory,
        python,
        port: 0,
        origin: String::new(),
    };
    fixture.app.safe_mode = true;
    fixture.app.markdown_browser(
        source,
        Some(&path),
        &egui::Context::default(),
        &mut Workspace::default(),
    );
    assert!(fixture.app.run_panel.sessions.is_empty() && !fixture.app.web_preview_pending());
    assert!(fixture.app.settings.trust_project(fixture.directory.path()));
    fixture.app.safe_mode = false;
    fixture.app.markdown_browser(
        source,
        Some(&path),
        &egui::Context::default(),
        &mut Workspace::default(),
    );
    assert_eq!(fixture.app.run_panel.sessions.len(), 1);
    let browser_id = fixture.app.run_panel.sessions[0].id;
    let cfg = fixture.app.run_panel.sessions[0].config.as_ref().unwrap();
    let RunConfigKind::Program { program, args } = &cfg.kind else {
        panic!("actual configured browser Program required");
    };
    assert_eq!(program, &chrome);
    let page = args
        .iter()
        .find(|a| a.starts_with("http://127.0.0.1:"))
        .expect("actual captured capability URL")
        .clone();
    let url = url::Url::parse(&page).unwrap();
    fixture.origin = format!("http://127.0.0.1:{}/", url.port().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        fixture.app.run_panel.poll();
        if let Ok(port) = std::fs::read_to_string(profile.join("DevToolsActivePort")) {
            fixture.port = port.lines().next().unwrap().parse().unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "genuine headless Chrome startup deadline"
        );
        assert!(fixture
            .app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id == browser_id && s.owns_process()));
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = fixture
        .app
        .run_panel
        .sessions
        .iter()
        .find(|s| s.id == browser_id)
        .unwrap()
        .process_id()
        .unwrap();
    assert_eq!(
        std::fs::canonicalize(format!("/proc/{pid}/exe")).unwrap(),
        chrome.canonicalize().unwrap()
    );
    let expression = r#"(()=>{const math=document.querySelector('.math .katex');const svg=document.querySelector('.mermaid svg');const bounds=e=>e?{width:e.getBoundingClientRect().width,height:e.getBoundingClientRect().height}:null;return {url:location.href,heading:document.querySelector('h1')?.textContent,math:bounds(math),mathml:!!math?.querySelector('.katex-mathml math'),tex:math?.querySelector('annotation[encoding="application/x-tex"]')?.textContent,errors:document.querySelectorAll('.katex-error').length,fonts:Array.from(document.fonts).filter(f=>f.status==='loaded').map(f=>f.family),svg:bounds(svg),nodes:svg?.querySelectorAll('g.node').length??0,edgeLengths:svg?Array.from(svg.querySelectorAll('path.flowchart-link')).map(p=>p.getTotalLength()):[],diagram:svg?.textContent}})()"#;
    let deadline = Instant::now() + Duration::from_secs(60);
    let observed = loop {
        let value = fixture.cdp(expression);
        if value["mathml"] == true
            && value["nodes"].as_u64().is_some_and(|n| n >= 2)
            && value["fonts"].as_array().is_some_and(|fonts| {
                fonts
                    .iter()
                    .any(|f| f.as_str().is_some_and(|s| s.contains("KaTeX_Main")))
            })
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "genuine math/diagram assets did not render: {value}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(observed["url"], page);
    assert_eq!(observed["heading"], "Captured enhanced π");
    assert_eq!(observed["tex"], "\\frac{1}{2}+x^2");
    assert_eq!(observed["errors"], 0);
    assert!(
        observed["math"]["width"].as_f64().unwrap() > 20.
            && observed["math"]["height"].as_f64().unwrap() > 10.
    );
    assert!(
        observed["svg"]["width"].as_f64().unwrap() > 50.
            && observed["svg"]["height"].as_f64().unwrap() > 20.
    );
    assert!(
        observed["diagram"].as_str().unwrap().contains("Owned π 42")
            && observed["diagram"]
                .as_str()
                .unwrap()
                .contains("Current café")
    );
    assert!(observed["edgeLengths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_f64().is_some_and(|v| v > 0.)));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Disk content must not render\n"
    );
    let owners = browser_children(pid);
    assert!(owners.len() > 1, "genuine multiprocess engine required");
    fixture.drain();
    assert!(
        owners
            .iter()
            .all(|(pid, start)| identity(*pid).as_ref() != Some(start)),
        "captured real browser identities must physically retire"
    );
}

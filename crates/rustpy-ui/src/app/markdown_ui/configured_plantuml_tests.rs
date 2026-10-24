//! Real admitted PlantUML PicoWeb, current-buffer Markdown and headless Chrome.
//! https://plantuml.com/picoweb and https://plantuml.com/text-encoding define
//! the loopback endpoint and UTF-8 ~h protocol used by production Markdown.
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
    fn cdp(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.app.next_session_id;
        self.app.next_session_id += 1;
        let args = vec![
            "-I".into(),
            "-u".into(),
            "-c".into(),
            DRIVER.into(),
            self.port.to_string(),
            self.origin.clone(),
            method.into(),
            params.to_string(),
        ];
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
                return result;
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

// Observe the actual Java process's listener inode, never a foreign socket at
// the briefly reserved numeric port. This readback does not establish lifetime
// authority: the App already owns the spawned process and its readers.
fn owns_listener(pid: u32, start: &str, port: u16) -> bool {
    if identity(pid).as_deref() != Some(start) {
        return false;
    }
    let sockets = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter_map(|path| {
            path.to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let found = ["tcp", "tcp6"].iter().any(|name| {
        std::fs::read_to_string(format!("/proc/net/{name}"))
            .unwrap()
            .lines()
            .skip(1)
            .any(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                fields.get(3) == Some(&"0A")
                    && fields
                        .get(1)
                        .and_then(|v| v.rsplit(':').next())
                        .and_then(|v| u16::from_str_radix(v, 16).ok())
                        == Some(port)
                    && fields
                        .get(9)
                        .is_some_and(|inode| sockets.iter().any(|s| s.as_str() == *inode))
            })
    });
    found && identity(pid).as_deref() == Some(start)
}

#[test]
#[ignore = "ROOT-admitted actual Java/official PlantUML JAR, Chrome and websocket-client SDK; real loopback engine"]
fn configured_plantuml_current_buffer_renders_real_svg_and_browser_pixels() {
    assert_eq!(required("RUSTPY_BROWSER_ADMITTED"), "1");
    assert_eq!(required("RUSTPY_PLANTUML_ADMITTED"), "1");
    for name in [
        "RUSTPY_TRUST_ALL",
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_STARTER_ADDRESS",
        "DBUS_STARTER_BUS_TYPE",
        "AT_SPI_BUS_ADDRESS",
        "SESSION_MANAGER",
        "DESKTOP_STARTUP_ID",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "ambient authority forbidden: {name}"
        );
    }
    let java = admitted_file("RUSTPY_PLANTUML_JAVA", "RUSTPY_PLANTUML_JAVA_SHA256");
    let jar = admitted_file("RUSTPY_PLANTUML_JAR", "RUSTPY_PLANTUML_JAR_SHA256");
    let chrome = admitted_file("RUSTPY_BROWSER_CHROME", "RUSTPY_BROWSER_CHROME_SHA256");
    let python = admitted_file(
        "RUSTPY_BROWSER_TEST_PYTHON",
        "RUSTPY_BROWSER_TEST_PYTHON_SHA256",
    );
    let temporary = PathBuf::from(required("TMPDIR")).canonicalize().unwrap();
    for name in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
        let path = PathBuf::from(required(name));
        assert!(path.is_absolute() && path.starts_with(&temporary));
        std::fs::create_dir_all(&path).unwrap();
        assert!(path.canonicalize().unwrap().starts_with(&temporary));
    }
    let directory = tempfile::Builder::new()
        .prefix("actual-plantuml-")
        .tempdir_in(&temporary)
        .unwrap();
    let profile = directory.path().join("chrome-profile");
    let java_temp = directory.path().join("java-temp");
    let java_profile = directory.path().join("java-profile");
    for path in [&java_temp, &java_profile] {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let endpoint = format!("http://127.0.0.1:{port}/plantuml/svg");
    let path = directory.path().join("captured π.md");
    std::fs::write(&path, "# Disk must not render\n").unwrap();
    let diagram =
        "@startuml\nAlice -> Bob: Captured café π 42\nBob --> Alice: Current λ result\n@enduml\n";
    let source = format!("# Current PlantUML π\n\n```plantuml\n{diagram}```\n");
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
    settings.markdown.math = false;
    settings.markdown.mermaid = false;
    settings.markdown.plantuml = true;
    settings.markdown.plantuml_server = endpoint.clone();
    settings.web_browsers.default = DefaultBrowser::Profile("actual-plantuml-chrome".into());
    settings.web_browsers.profiles.push(Profile {
        id: "actual-plantuml-chrome".into(),
        name: "Actual PlantUML Chrome".into(),
        kind: Kind::Chrome,
        program: chrome.to_string_lossy().into_owned(),
        arguments,
        enabled: true,
    });
    let mut app = IdeApp::new_welcome(settings);
    app.open_file(&path);
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .doc
        .set_text(&source, &[], &[])
        .unwrap();
    let mut fixture = Fixture {
        app,
        directory,
        python,
        port: 0,
        origin: String::new(),
    };
    fixture.app.safe_mode = true;
    fixture.app.markdown_browser(
        &source,
        Some(&path),
        &egui::Context::default(),
        &mut Workspace::default(),
    );
    assert!(fixture.app.run_panel.sessions.is_empty() && !fixture.app.web_preview_pending());
    let server_id = fixture.app.next_session_id;
    fixture.app.next_session_id += 1;
    let server_args = vec![
        "-Djava.awt.headless=true".into(),
        format!("-Djava.io.tmpdir={}", java_temp.display()),
        format!("-Duser.home={}", java_profile.display()),
        "-XX:-UsePerfData".into(),
        "-XX:ActiveProcessorCount=2".into(),
        "-Xmx256m".into(),
        "-jar".into(),
        jar.to_string_lossy().into_owned(),
        format!("-picoweb:{port}:127.0.0.1"),
    ];
    let server = RunSession::spawn_command(
        server_id,
        "Actual private PlantUML PicoWeb",
        &java,
        &server_args,
        fixture.directory.path(),
        &BTreeMap::new(),
    );
    fixture.app.run_panel.add_session(server);
    let server_pid = fixture
        .app
        .run_panel
        .sessions
        .iter()
        .find(|s| s.id == server_id)
        .unwrap()
        .process_id()
        .expect("real Java process");
    let server_start = identity(server_pid).unwrap();
    assert_eq!(
        std::fs::canonicalize(format!("/proc/{server_pid}/exe")).unwrap(),
        java.canonicalize().unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !owns_listener(server_pid, &server_start, port) {
        fixture.app.run_panel.poll();
        let server = fixture
            .app
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == server_id)
            .unwrap();
        assert!(
            server.owns_process(),
            "actual Java provider failed: {}",
            server.text()
        );
        assert!(Instant::now() < deadline, "actual Java listener deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fixture.app.settings.trust_project(fixture.directory.path()));
    fixture.app.safe_mode = false;
    fixture.app.markdown_browser(
        &source,
        Some(&path),
        &egui::Context::default(),
        &mut Workspace::default(),
    );
    let browser = fixture
        .app
        .run_panel
        .sessions
        .iter()
        .find(|s| s.id != server_id)
        .unwrap();
    let browser_id = browser.id;
    let RunConfigKind::Program { program, args } = &browser.config.as_ref().unwrap().kind else {
        panic!("actual Chrome command");
    };
    assert_eq!(program, &chrome);
    let page = args
        .iter()
        .find(|a| a.starts_with("http://127.0.0.1:"))
        .unwrap()
        .clone();
    let url = url::Url::parse(&page).unwrap();
    fixture.origin = format!("http://127.0.0.1:{}/", url.port().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        fixture.app.run_panel.poll();
        if let Ok(text) = std::fs::read_to_string(profile.join("DevToolsActivePort")) {
            fixture.port = text.lines().next().unwrap().parse().unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "actual Chrome startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
    let expression = r#"(()=>{const i=document.querySelector('img[alt="PlantUML diagram"]');const b=i?.getBoundingClientRect();return {heading:document.querySelector('h1')?.textContent,src:i?.src,complete:i?.complete,width:i?.naturalWidth,height:i?.naturalHeight,clip:b?{x:b.x+scrollX,y:b.y+scrollY,width:b.width,height:b.height,scale:1}:null}})()"#;
    let deadline = Instant::now() + Duration::from_secs(30);
    let observed = loop {
        let response = fixture.cdp(
            "Runtime.evaluate",
            serde_json::json!({"expression":expression,"returnByValue":true}),
        );
        let value = response["result"]["value"].clone();
        if value["complete"] == true && value["width"].as_u64().is_some_and(|w| w > 100) {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "actual PlantUML image failed: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let hex = diagram
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(observed["heading"], "Current PlantUML π");
    assert_eq!(observed["src"], format!("{endpoint}/~h{hex}"));
    assert!(observed["height"].as_u64().unwrap() > 50);
    // Real Chrome pixels from the actual image rectangle, no canvas/DOM writes.
    let screenshot = fixture.cdp(
        "Page.captureScreenshot",
        serde_json::json!({"format":"png","captureBeyondViewport":true,"clip":observed["clip"]}),
    );
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(screenshot["data"].as_str().unwrap())
        .unwrap();
    let pixels = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    assert!(pixels.width() > 100 && pixels.height() > 50);
    let background = *pixels.get_pixel(0, 0);
    assert!(
        pixels
            .pixels()
            .filter(|pixel| **pixel != background && pixel[3] > 0)
            .count()
            > 200,
        "actual diagram rectangle must contain rendered strokes/text"
    );
    // Read the genuine server SVG at exactly the production-selected URL; the
    // browser's image pixels above independently establish actual display.
    let svg_script = "import json,sys,urllib.request; o=urllib.request.build_opener(urllib.request.ProxyHandler({})); r=o.open(sys.argv[1],timeout=15); assert r.headers.get_content_type()=='image/svg+xml'; data=r.read(1048577); assert len(data)<=1048576; print('RUSTPY_PLANTUML_SVG='+json.dumps({'svg':data.decode('utf-8')}),flush=True)";
    let svg_id = fixture.app.next_session_id;
    fixture.app.next_session_id += 1;
    let svg = RunSession::spawn_command(
        svg_id,
        "Read actual PlantUML SVG",
        &fixture.python,
        &[
            "-I".into(),
            "-c".into(),
            svg_script.into(),
            observed["src"].as_str().unwrap().into(),
        ],
        fixture.directory.path(),
        &BTreeMap::new(),
    );
    fixture.app.run_panel.add_session(svg);
    let deadline = Instant::now() + Duration::from_secs(30);
    let svg = loop {
        fixture.app.run_panel.poll();
        let session = fixture
            .app
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == svg_id)
            .unwrap();
        if !session.owns_process() {
            assert_eq!(
                session.status,
                rustpy_run::SessionStatus::Exited(0),
                "{}",
                session.text()
            );
            // RunSession::text includes the command line and lifecycle System
            // chunks. Decode only the owned reader's structured stdout record;
            // never trim arbitrary bytes until malformed XML happens to parse.
            let output = session.text();
            let records = output
                .lines()
                .filter_map(|line| line.strip_prefix("RUSTPY_PLANTUML_SVG="))
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 1, "exactly one genuine SVG response record");
            let payload: serde_json::Value =
                serde_json::from_str(records[0]).expect("genuine SVG response JSON");
            break payload["svg"]
                .as_str()
                .expect("genuine SVG string")
                .to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "real SVG reader retirement deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let document = roxmltree::Document::parse(&svg).expect("genuine valid SVG XML");
    assert_eq!(document.root_element().tag_name().name(), "svg");
    let labels = document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "text")
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        labels.contains("Captured café π 42") && labels.contains("Current λ result"),
        "real SVG must contain captured Unicode diagram labels: {labels}"
    );
    assert!(!svg.contains("Syntax Error"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Disk must not render\n"
    );
    let browser_pid = fixture
        .app
        .run_panel
        .sessions
        .iter()
        .find(|s| s.id == browser_id)
        .unwrap()
        .process_id()
        .unwrap();
    let mut owners = browser_children(browser_pid);
    owners.extend(browser_children(server_pid));
    fixture.drain();
    assert!(
        owners
            .iter()
            .all(|(pid, start)| identity(*pid).as_ref() != Some(start)),
        "all captured Java/browser identities must physically retire"
    );
}

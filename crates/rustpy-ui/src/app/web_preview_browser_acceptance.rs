//! Real external Chrome, Edge and Firefox against the App's production
//! capability preview host, through the App's configured-browser launch path.
//! This does not substitute for native Wry/WebKitGTK engine acceptance.
use super::*;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(30);
/// Chromium DevTools protocol (Chrome, Edge).
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
/// WebDriver BiDi (Firefox no longer ships CDP). Prints the same
/// `RUSTPY_REAL_CDP={"result":{"value":...}}` line as the CDP driver.
const BIDI_DRIVER: &str = r#"import json,sys,time,websocket
port,origin,method,params=sys.argv[1:]
expression=json.loads(params)['expression']
retryable=expression.startswith('(()=>')
def plain(value):
    kind=value.get('type')
    if kind in ('undefined','null'):return None
    if kind in ('string','boolean'):return value['value']
    if kind=='number':
        number=value['value']
        return int(number) if isinstance(number,float) and number.is_integer() else number
    if kind=='array':return [plain(item) for item in value.get('value',[])]
    if kind=='object':return {key if isinstance(key,str) else json.dumps(plain(key)):plain(item) for key,item in value.get('value',[])}
    return {'type':kind}
socket=websocket.create_connection('ws://127.0.0.1:'+port+'/session',timeout=15,suppress_origin=True,http_no_proxy=['127.0.0.1','localhost'])
counter=0
def call(method,params):
    global counter
    counter+=1
    socket.send(json.dumps({'id':counter,'method':method,'params':params}))
    while True:
        response=json.loads(socket.recv())
        if response.get('id')==counter:return response
try:
    created=call('session.new',{'capabilities':{}})
    assert created.get('type')=='success',created
    deadline=time.monotonic()+10
    while True:
        tree=call('browsingContext.getTree',{'maxDepth':0})
        contexts=[context for context in tree['result']['contexts'] if context.get('url','').startswith(origin)]
        if contexts or time.monotonic()>=deadline:break
        time.sleep(0.05)
    assert len(contexts)==1,('expected exactly one owned preview tab',origin,tree)
    deadline=time.monotonic()+15
    while True:
        result=call('script.evaluate',{'expression':expression,'target':{'context':contexts[0]['context']},'awaitPromise':True,'resultOwnership':'none'})
        ok=result.get('type')=='success' and result['result'].get('type')=='success'
        if not ok and retryable and time.monotonic()<deadline:
            time.sleep(0.05)
            continue
        assert ok,result
        print('RUSTPY_REAL_CDP='+json.dumps({'result':{'value':plain(result['result']['result'])}}),flush=True)
        break
finally:
    try:call('session.end',{})
    except Exception:pass
    socket.close()
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Protocol {
    Cdp,
    Bidi,
}
struct Driver<'a> {
    python: &'a Path,
    root: &'a Path,
    protocol: Protocol,
}
impl Driver<'_> {
    fn eval(&self, port: u16, origin: &str, expression: &str) -> serde_json::Value {
        let script = match self.protocol {
            Protocol::Cdp => DRIVER,
            Protocol::Bidi => BIDI_DRIVER,
        };
        let args = vec!["-I".into(), "-u".into(), "-c".into(), script.into(), port.to_string(), origin.into(), "Runtime.evaluate".into(), serde_json::json!({"expression":expression,"awaitPromise":true,"returnByValue":true,"timeout":15000}).to_string()];
        let env = BTreeMap::from([
            ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
            ("no_proxy".into(), "127.0.0.1,localhost".into()),
        ]);
        let mut driver = RunSession::spawn_command(
            9901,
            "Actual browser protocol control",
            self.python,
            &args,
            self.root,
            &env,
        );
        finish(&mut driver);
        assert_eq!(
            driver.status,
            rustpy_run::SessionStatus::Exited(0),
            "{}",
            driver.text()
        );
        let output = driver.text();
        let result: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| line.strip_prefix("RUSTPY_REAL_CDP="))
                .expect("actual browser protocol response"),
        )
        .unwrap();
        result["result"]["value"].clone()
    }
    /// Poll fresh page snapshots: a save reload destroys the old JS context,
    /// so no promise is held across it.
    fn until(
        &self,
        port: u16,
        origin: &str,
        expression: &str,
        done: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + LIMIT;
        loop {
            let value = self.eval(port, origin, expression);
            if done(&value) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "actual DOM did not settle: {value}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn rendered(&self, port: u16, origin: &str, expected: &str) -> serde_json::Value {
        self.until(port, origin, "(()=>{const node=document.getElementById('value');return {text:node?.textContent??null,js:window.actualJavaScript??null,color:node?getComputedStyle(node).color:null,url:location.href,title:document.title}})()", |value| value["text"] == expected && value["js"] == 42)
    }
}
fn arguments(protocol: Protocol, profile: &Path) -> String {
    let args: Vec<String> = match protocol {
        Protocol::Cdp => vec![
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
        ],
        Protocol::Bidi => {
            // Private profile: no first-run tabs, telemetry, updates, proxy,
            // or more than two content processes.
            std::fs::write(
                profile.join("user.js"),
                [
                    ("browser.shell.checkDefaultBrowser", "false"),
                    ("browser.startup.homepage_override.mstone", "\"ignore\""),
                    ("browser.startup.page", "0"),
                    ("browser.aboutwelcome.enabled", "false"),
                    ("datareporting.policy.dataSubmissionEnabled", "false"),
                    ("toolkit.telemetry.reportingpolicy.firstRun", "false"),
                    ("app.update.disabledForTesting", "true"),
                    ("dom.ipc.processCount", "2"),
                    ("network.proxy.type", "0"),
                ]
                .map(|(key, value)| format!("user_pref(\"{key}\", {value});\n"))
                .concat(),
            )
            .unwrap();
            vec![
                "--headless".into(),
                "--no-remote".into(),
                "--profile".into(),
                profile.display().to_string(),
                "--remote-debugging-port=0".into(),
                "$URL$".into(),
            ]
        }
    };
    rustpy_project::configuration_types::quote_arguments(&args)
}
/// The browser's own published protocol endpoint inside its private profile.
fn debug_port(protocol: Protocol, profile: &Path) -> Option<u16> {
    match protocol {
        Protocol::Cdp => std::fs::read_to_string(profile.join("DevToolsActivePort"))
            .ok()?
            .lines()
            .next()?
            .parse()
            .ok(),
        Protocol::Bidi => serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(profile.join("WebDriverBiDiServer.json")).ok()?,
        )
        .ok()?["ws_port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok()),
    }
}
fn await_port(app: &mut IdeApp, session: usize, protocol: Protocol, profile: &Path) -> u16 {
    let deadline = Instant::now() + LIMIT;
    loop {
        app.run_panel.poll();
        if let Some(port) = debug_port(protocol, profile) {
            return port;
        }
        assert!(
            app.run_panel.sessions[session].owns_process() && Instant::now() < deadline,
            "real browser startup: {}",
            app.run_panel.sessions[session].text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn stop_owned_tree(app: &mut IdeApp, session: usize) {
    let pid = app.run_panel.sessions[session].process_id().unwrap();
    let owners = descendants(pid);
    let session = &mut app.run_panel.sessions[session];
    session.stop();
    finish(session);
    for (pid, start) in owners {
        assert!(
            !identity(pid).is_some_and(|(_, _, current)| current == start),
            "owned browser process physically survived Stop: {pid}"
        );
    }
}
/// A plain loopback HTTP origin that records every request line, proving the
/// browser itself fetched an Open URL target.
fn observed_origin() -> (u16, crossbeam_channel::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, receiver) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let line = text.lines().next().unwrap_or_default().to_owned();
            let agent = text
                .lines()
                .find_map(|line| line.strip_prefix("User-Agent: "))
                .unwrap_or_default()
                .to_owned();
            let body = "<!doctype html><meta charset=utf-8><title>open url π</title><p id=value>opened</p>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = sender.send(format!("{line}\t{agent}"));
        }
    });
    (port, receiver)
}
fn required(name: &str) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} required")));
    assert!(
        path.is_absolute() && path.is_file(),
        "{name}: actual absolute provider required"
    );
    path
}
fn identity(pid: u32) -> Option<(u32, u32, String)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = text
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    Some((
        pid,
        fields.get(1)?.parse().ok()?,
        fields.get(19)?.to_string(),
    ))
}
fn descendants(parent: u32) -> BTreeMap<u32, String> {
    let mut owners = BTreeMap::new();
    let (_, _, start) = identity(parent).expect("actual browser identity");
    owners.insert(parent, start);
    let all = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter_map(identity)
        .collect::<Vec<_>>();
    loop {
        let before = owners.len();
        for (pid, ppid, start) in &all {
            if owners.contains_key(ppid) {
                owners.insert(*pid, start.clone());
            }
        }
        if owners.len() == before {
            return owners;
        }
    }
}
fn finish(session: &mut RunSession) {
    let deadline = Instant::now() + LIMIT;
    while session.owns_process() {
        session.poll();
        assert!(
            Instant::now() < deadline,
            "owned browser/control process failed retirement: {} {}",
            session.lifecycle_stamp(),
            session.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    session.poll();
}
fn html(value: &str) -> String {
    format!("<!doctype html><html><head><meta charset='utf-8'><title>actual π {value}</title><link rel='stylesheet' href='style.css'></head><body><p id='value'>{value}</p><a id='next' href='next%20%CF%80.html'>Next π</a><script src='app.js'></script></body></html>")
}

#[test]
#[ignore = "explicit actual Linux Chrome + selected Python websocket-client SDK; central isolated two-core actor only"]
fn configured_app_chrome_renders_navigates_reloads_saved_source_and_retires_owned_tree() {
    acceptance(Kind::Chrome, "RUSTPY_BROWSER_CHROME", Protocol::Cdp);
}
#[test]
#[ignore = "explicit actual Linux Microsoft Edge + selected Python websocket-client SDK; central isolated two-core actor only"]
fn configured_app_edge_renders_navigates_reloads_saved_source_and_retires_owned_tree() {
    acceptance(Kind::Edge, "RUSTPY_BROWSER_EDGE", Protocol::Cdp);
}
#[test]
#[ignore = "explicit actual Linux Firefox (WebDriver BiDi) + selected Python websocket-client SDK; central isolated two-core actor only"]
fn configured_app_firefox_renders_navigates_reloads_saved_source_and_retires_owned_tree() {
    acceptance(Kind::Firefox, "RUSTPY_BROWSER_FIREFOX", Protocol::Bidi);
}

fn current_affinity() -> String {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|line| line.starts_with("Cpus_allowed_list:"))
        .unwrap()
        .to_owned()
}
/// The actor imposes two distinct cores and nice >= 10, inherited by every
/// browser descendant. When the actor runs as root it must also drop
/// CAP_SYS_NICE (e.g. `setpriv --bounding-set=-sys_nice --inh-caps=-sys_nice`):
/// Chromium-based browsers otherwise renice their own browser/GPU processes to
/// -8 and renderers to 0, escaping the reduced priority.
fn admit_central_actor() -> String {
    let affinity = current_affinity();
    let cpus = affinity
        .split_once(':')
        .unwrap()
        .1
        .trim()
        .split(',')
        .flat_map(|part| {
            let (first, last) = part.split_once('-').unwrap_or((part, part));
            first.parse::<u32>().unwrap()..=last.parse::<u32>().unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cpus.len(),
        2,
        "central actor must impose inherited two-core affinity"
    );
    let cores = cpus
        .iter()
        .map(|cpu| {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            (
                std::fs::read_to_string(format!("{base}/physical_package_id")).unwrap(),
                std::fs::read_to_string(format!("{base}/core_id")).unwrap(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        cores.len(),
        2,
        "central actor must use distinct physical cores"
    );
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    assert!(
        stat.rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(16)
            .unwrap()
            .parse::<i32>()
            .unwrap()
            >= 10
    );
    affinity
}

fn acceptance(kind: Kind, variable: &str, protocol: Protocol) {
    assert_eq!(std::env::var("RUSTPY_BROWSER_ADMITTED").as_deref(), Ok("1"));
    let binary = required(variable);
    let python = required("RUSTPY_BROWSER_TEST_PYTHON");
    assert!(python
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pyvenv.cfg")
        .is_file());
    let mut magic = [0; 4];
    std::fs::File::open(&binary)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(
        &magic, b"\x7fELF",
        "browser admission requires genuine Linux binary, not a wrapper"
    );
    let affinity = admit_central_actor();

    // Configured browsers list: the stock entry for this kind must be
    // detected from PATH and belong to the admitted installation.
    let stock = Settings::default()
        .web_browsers
        .profiles
        .into_iter()
        .find(|profile| profile.kind == kind)
        .expect("stock configured browser entry");
    let detected = browser::detect_program(&stock)
        .unwrap_or_else(|| panic!("{} not detected on PATH", stock.program));
    assert_eq!(
        dunce::canonicalize(&detected).unwrap().parent(),
        dunce::canonicalize(&binary).unwrap().parent(),
        "detected {} is not the admitted installation",
        detected.display()
    );

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let path = project.join("index π.html");
    std::fs::write(&path, html("disk π")).unwrap();
    std::fs::write(project.join("next π.html"), html("next π")).unwrap();
    std::fs::write(
        project.join("style.css"),
        "#value { color: rgb(12, 34, 56); }",
    )
    .unwrap();
    std::fs::write(project.join("app.js"), "window.actualJavaScript=6*7;").unwrap();
    let new_profile = |name: &str| {
        let profile = root.path().join(name);
        std::fs::create_dir(&profile).unwrap();
        profile
    };
    let profile = new_profile("private-browser-profile");
    let driver = Driver {
        python: &python,
        root: root.path(),
        protocol,
    };
    let mut settings = Settings::default();
    settings.editor.ensure_newline_at_eof = false;
    settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
    settings.web_browsers.default = DefaultBrowser::Profile("actual-owned-browser".into());
    settings.web_browsers.reload_on_save = true;
    settings.web_browsers.profiles.push(Profile {
        id: "actual-owned-browser".into(),
        name: format!("Actual owned {kind:?}"),
        kind,
        program: binary.to_string_lossy().into_owned(),
        arguments: arguments(protocol, &profile),
        enabled: true,
    });
    // A welcome App intentionally owns no cwd-discovered project. Admission
    // below therefore exercises trust of this captured preview folder alone.
    let mut app = IdeApp::new_welcome(settings);
    app.open_file(&path);
    app.safe_mode = true;
    app.open_current_web_preview(false);
    assert!(
        app.run_panel.sessions.is_empty() && app.web_preview.server.is_none(),
        "untrusted App cannot launch or serve executable preview"
    );
    assert!(app.settings.trust_project(&project));
    app.safe_mode = false;
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .doc
        .set_text(&html("unsaved λ"), &[], &[])
        .unwrap();
    app.open_current_web_preview(false);
    assert_eq!(
        app.run_panel.sessions.len(),
        1,
        "actual configured profile must launch"
    );
    let address = app
        .web_preview
        .server
        .as_ref()
        .expect("production capability host")
        .address;
    let url = app.web_preview.server.as_ref().unwrap().url(&path).unwrap();
    let origin = format!("http://{address}/");
    let port = await_port(&mut app, 0, protocol, &profile);
    let pid = app.run_panel.sessions[0].process_id().unwrap();
    assert_eq!(
        dunce::canonicalize(format!("/proc/{pid}/exe")).unwrap(),
        dunce::canonicalize(&binary).unwrap()
    );
    eprintln!(
        "Actual configured browser launch: {}",
        app.run_panel.sessions[0].text()
    );
    let first = std::panic::catch_unwind(|| driver.rendered(port, &origin, "unsaved λ"));
    let first = match first {
        Ok(value) => value,
        Err(original_panic) => {
            // The App is not drawing frames while the real driver waits.
            // Drain late browser output on failure before unwinding drops its
            // owned process tree; preserve the original assertion and panic.
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                app.run_panel.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
            let browser = &app.run_panel.sessions[0];
            let text = browser.text();
            let start = text.len().saturating_sub(32 * 1024);
            let start = (start..=text.len())
                .find(|offset| text.is_char_boundary(*offset))
                .unwrap();
            eprintln!(
                "First actual browser render failed: status={:?}, owns_process={}, lifecycle={}\nBrowser output tail (at most 32 KiB):\n{}",
                browser.status,
                browser.owns_process(),
                browser.lifecycle_stamp(),
                &text[start..]
            );
            if let Some(server) = &app.web_preview.server {
                eprintln!(
                    "Actual preview host: {}, address={}, root={}, revision={}, running={}, cancelling={}",
                    server.stamp(),
                    server.address,
                    server.root.display(),
                    server.revision(),
                    server.running(),
                    server.cancelling()
                );
            }
            std::panic::resume_unwind(original_panic);
        }
    };
    assert_eq!(first["js"], 42);
    assert_eq!(first["color"], "rgb(12, 34, 56)");
    assert_eq!(first["url"], url);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), html("disk π"));
    driver.eval(
        port,
        &origin,
        "document.getElementById('next').click();true",
    );
    assert_eq!(driver.rendered(port, &origin, "next π")["text"], "next π");
    driver.eval(port, &origin, "history.back();true");
    driver.rendered(port, &origin, "unsaved λ");
    app.tabs[index]
        .state
        .doc
        .set_text(&html("saved 🙂"), &[], &[])
        .unwrap();
    assert!(
        app.save_tab(index),
        "actual checked App save must publish reload"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), html("saved 🙂"));
    assert_eq!(
        driver.rendered(port, &origin, "saved 🙂")["text"],
        "saved 🙂"
    );
    let owners = descendants(pid);
    assert!(
        owners.len() > 1,
        "genuine multiprocess browser tree required"
    );
    for child in owners.keys() {
        let status = std::fs::read_to_string(format!("/proc/{child}/status")).unwrap();
        assert_eq!(
            status
                .lines()
                .find(|line| line.starts_with("Cpus_allowed_list:"))
                .unwrap(),
            affinity
        );
        let stat = std::fs::read_to_string(format!("/proc/{child}/stat")).unwrap();
        assert!(
            stat.rsplit_once(") ")
                .unwrap()
                .1
                .split_whitespace()
                .nth(16)
                .unwrap()
                .parse::<i32>()
                .unwrap()
                >= 10,
            "browser descendants must retain reduced priority: {child} {stat}"
        );
    }
    stop_owned_tree(&mut app, 0);

    // Open in browser for Markdown through a user-added custom browser entry,
    // then reload-on-save of the Markdown source.
    let markdown = project.join("notes π.md");
    let note =
        |heading: &str| format!("# {heading}\n\nSome *emphasis* and [next](next%20π.html).\n");
    std::fs::write(&markdown, note("Markdown disk π")).unwrap();
    let custom_profile = new_profile("private-custom-profile");
    app.settings.web_browsers.profiles.push(Profile {
        id: "custom-owned".into(),
        name: "Custom owned browser".into(),
        kind: Kind::Custom,
        program: binary.to_string_lossy().into_owned(),
        arguments: arguments(protocol, &custom_profile),
        enabled: true,
    });
    app.open_file(&markdown);
    let markdown_tab = app.active.unwrap();
    assert_ne!(markdown_tab, index);
    app.tabs[markdown_tab]
        .state
        .doc
        .set_text(&note("Markdown unsaved λ"), &[], &[])
        .unwrap();
    let session = app.run_panel.sessions.len();
    app.open_web_file(&markdown, Some("custom-owned".into()), false);
    assert_eq!(
        app.run_panel.sessions.len(),
        session + 1,
        "custom entry must launch"
    );
    let port = await_port(&mut app, session, protocol, &custom_profile);
    let markdown_url = app
        .web_preview
        .server
        .as_ref()
        .unwrap()
        .url(&markdown)
        .unwrap();
    let probe = "(()=>({h1:document.querySelector('h1')?.textContent??null,em:document.querySelector('em')?.textContent??null,url:location.href}))()";
    let shown = driver.until(port, &origin, probe, |value| {
        value["h1"] == "Markdown unsaved λ"
    });
    assert_eq!(shown["em"], "emphasis");
    assert_eq!(shown["url"], markdown_url);
    app.tabs[markdown_tab]
        .state
        .doc
        .set_text(&note("Markdown saved 🙂"), &[], &[])
        .unwrap();
    assert!(
        app.save_tab(markdown_tab),
        "Markdown save must publish reload"
    );
    driver.until(port, &origin, probe, |value| {
        value["h1"] == "Markdown saved 🙂"
    });
    stop_owned_tree(&mut app, session);

    // Open URL through the stock configured-list entry: a bare program name
    // resolved by detection, launched with headless arguments, and observed
    // fetching an arbitrary loopback URL whose query looks like an option.
    let (open_port, requests) = observed_origin();
    let open_profile = new_profile("private-open-url-profile");
    let stock_id = stock.id.clone();
    let entry = app
        .settings
        .web_browsers
        .profiles
        .iter_mut()
        .find(|profile| profile.id == stock_id)
        .unwrap();
    assert_eq!(
        entry.program, stock.program,
        "stock entry stays a bare name"
    );
    entry.arguments = arguments(protocol, &open_profile);
    app.settings.web_browsers.default = DefaultBrowser::Profile(stock_id);
    let target = format!("http://127.0.0.1:{open_port}/open%20url?q=a%20b&next=--private");
    let session = app.run_panel.sessions.len();
    app.open_url(&target);
    assert_eq!(
        app.run_panel.sessions.len(),
        session + 1,
        "Open URL must launch the configured default"
    );
    let port = await_port(&mut app, session, protocol, &open_profile);
    let deadline = Instant::now() + LIMIT;
    let request = loop {
        let request = requests
            .recv_deadline(deadline)
            .expect("browser fetched the Open URL target");
        if request.starts_with("GET /open%20url?q=a%20b&next=--private HTTP/1.1\t") {
            break request;
        }
        eprintln!("Other Open URL origin request: {request}");
    };
    eprintln!("Actual Open URL request: {request}");
    let opened = driver.until(
        port,
        &format!("http://127.0.0.1:{open_port}/"),
        "(()=>({text:document.getElementById('value')?.textContent??null,url:location.href,title:document.title}))()",
        |value| value["text"] == "opened",
    );
    assert_eq!(opened["url"], target);
    assert_eq!(opened["title"], "open url π");
    stop_owned_tree(&mut app, session);

    let captured = app.web_preview_stamps();
    let replacement = tempfile::tempdir().unwrap();
    app.preview_server(replacement.path()).unwrap();
    let current = app.web_preview_stamps();
    app.stop_web_preview(&captured);
    assert_eq!(
        app.web_preview_stamps(),
        current,
        "stale Stop cannot cancel replacement source host"
    );
    let server = app.web_preview.server.as_ref().unwrap();
    let revision = server.revision();
    app.web_preview_saved(&path);
    assert_eq!(
        app.web_preview.server.as_ref().unwrap().revision(),
        revision,
        "old source save cannot publish into replacement root"
    );
    app.stop_web_preview(&current);
    let ctx = egui::Context::default();
    let deadline = Instant::now() + LIMIT;
    while app.web_preview_pending() {
        app.web_preview_begin_frame(&ctx);
        assert!(
            Instant::now() < deadline,
            "preview host failed physical retirement"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
}

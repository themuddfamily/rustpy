//! Windows acceptance probe for Web Browsers and Preview: browser detection,
//! "Open in browser" through the real preview host and launch planner, the
//! Open URL action, process-tree Stop, and (with `--gui`) the WebView2
//! built-in preview of the real IDE. Run on a real Windows host through
//! `qa/windows/run-windows-acceptance-2.ps1`, which compares the detection
//! with the registry and checks for leftover browser processes. Prints JSON.
//!
//! Usage: `windows_browser_acceptance --work <empty dir> [--gui <rustpy.exe>]`
//!
//! Browsers run headless with private profiles inside `--work`; nothing is
//! shown on the desktop unless `--gui` is given (the IDE window opens for
//! about 20 seconds and closes itself).

use rustpy_settings::web_browsers::{Kind, Profile};
use rustpy_ui::web_browser_workspace::{
    launch, redirect_message, redirected_by, validated_url, Launch,
};
use rustpy_ui::web_preview_server::Server;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Requests seen by the beacon listener: (request target, User-Agent).
type Seen = Arc<Mutex<Vec<(String, String)>>>;

fn main() {
    let mut work = None;
    let mut gui = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--work" => work = args.next().map(PathBuf::from),
            "--gui" => gui = args.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(work) = work else {
        eprintln!("--work <dir> is required");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&work).expect("work directory");
    // Absolute but not canonical: no `\\?\` prefix in tool arguments.
    let work = std::path::absolute(&work).expect("work directory");
    // The preview host keeps its capability folder under the config dir.
    std::env::set_var("RUSTPY_CONFIG_DIR", work.join("config"));
    std::env::set_var("RUSTPY_SYSTEM_DIR", work.join("system"));

    let seen: Seen = Arc::default();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("beacon listener");
    let beacon = listener.local_addr().unwrap().port();
    {
        let seen = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                std::thread::spawn(move || answer(stream, &seen));
            }
        });
    }

    let project = work.join("web project");
    write_site(&project, beacon);
    let index = project.join("index.html");
    let mut report = serde_json::Map::new();
    report.insert("work".into(), json!(work));
    report.insert("beacon_port".into(), json!(beacon));

    // Detection: what each built-in browser entry resolves to.
    let mut detection = serde_json::Map::new();
    for kind in [Kind::Edge, Kind::Chrome, Kind::Firefox] {
        let profile = Profile::template(kind);
        let resolved = match launch(&profile, "http://127.0.0.1/") {
            Ok(Launch::Program { program, .. }) => json!({
                "configured": profile.program,
                "program": program,
                "absolute": program.is_absolute(),
                "exists": program.is_file(),
                "redirected_by": redirected_by(&program),
            }),
            Ok(Launch::System(url)) => json!({"system": url}),
            Err(e) => json!({"error": e}),
        };
        detection.insert(profile.name.clone(), resolved);
    }
    report.insert("detection".into(), Value::Object(detection.clone()));
    // The System entry hands the URL to the shell (not launched here: it would
    // open a visible window); only its plan is checked.
    report.insert(
        "system_plan".into(),
        json!(format!(
            "{:?}",
            launch(&Profile::template(Kind::System), "http://127.0.0.1:1/a b")
        )),
    );
    report.insert(
        "rejected_urls".into(),
        json!([
            "javascript:alert(1)",
            "https://user:secret@example.org/",
            "https://example.org/\n--flag"
        ]
        .iter()
        .map(|u| validated_url(u).is_err())
        .collect::<Vec<_>>()),
    );

    // Open in browser: the real capability preview host serves the project;
    // every installed browser loads it headless through the launch planner
    // and the Run tool's process session, then Stop ends its process tree.
    let server = match Server::start(&project, false, Default::default()) {
        Ok(server) => server,
        Err(e) => {
            report.insert("preview_server_error".into(), json!(e));
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            return;
        }
    };
    let page = server.url(&index).expect("preview URL");
    report.insert(
        "preview_url_origin".into(),
        json!(format!("http://{}", server.address)),
    );
    let mut runs = Vec::new();
    let mut first_available = None;
    let mut redirected = Vec::new();
    for kind in [Kind::Edge, Kind::Chrome, Kind::Firefox] {
        let name = Profile::template(kind).name;
        let exists = detection[&name]["exists"].as_bool().unwrap_or(false);
        if !exists {
            runs.push(json!({"browser": name, "skipped": "not installed"}));
            continue;
        }
        // An Image File Execution Options Debugger (MSEdgeRedirect) starts
        // instead of the browser: the user's choice, reported as a skip.
        if let Some(debugger) = detection[&name]["redirected_by"].as_str() {
            let reason = redirect_message(&name, debugger);
            runs.push(json!({"browser": name, "skipped": reason, "redirected_by": debugger}));
            redirected.push(reason);
            continue;
        }
        first_available.get_or_insert(kind);
        let tag = name.to_ascii_lowercase();
        let url = format!("{page}?kind={tag}");
        runs.push(open_and_stop(kind, &work, &tag, &url, &seen, |path| {
            path.starts_with("/beacon?") && path.contains(&format!("kind={tag}&"))
        }));
    }
    report.insert("open_in_browser".into(), json!(runs));

    // Open URL: an address typed by the user, with spaces and an option-like
    // query, reaches the browser as one argument and loads unchanged.
    if let Some(kind) = first_available {
        let target = "/open-url?q=a%20b&next=--private&x=%22quoted%22";
        let url = format!("http://127.0.0.1:{beacon}{target}");
        let mut run = open_and_stop(kind, &work, "open-url", &url, &seen, |path| path == target);
        run["expected_target"] = json!(target);
        report.insert("open_url".into(), run);
    } else if !redirected.is_empty() {
        report.insert("open_url_skipped".into(), json!(redirected.join(" ")));
    }
    drop(server);

    // The IDE's own built-in preview (WebView2), opened by the real action.
    if let Some(exe) = gui {
        report.insert("webview2".into(), webview2(&exe, &work, beacon, &seen));
    }
    report.insert(
        "beacon_requests".into(),
        json!(seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>()),
    );
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

/// Launches `url` with a headless private profile of `kind` exactly as the
/// IDE does (launch plan, then a temporary Program run configuration in a
/// launcher `RunSession`), waits for the page's beacon, then stops the session.
fn open_and_stop(
    kind: Kind,
    work: &Path,
    tag: &str,
    url: &str,
    seen: &Seen,
    matches: impl Fn(&str) -> bool,
) -> Value {
    let template = Profile::template(kind);
    let profile_dir = work.join(format!("profile-{tag}"));
    std::fs::create_dir_all(&profile_dir).unwrap();
    let arguments = match kind {
        Kind::Firefox => {
            std::fs::write(
                profile_dir.join("user.js"),
                "user_pref(\"browser.shell.checkDefaultBrowser\", false);\n\
                 user_pref(\"app.update.enabled\", false);\n\
                 user_pref(\"datareporting.policy.dataSubmissionEnabled\", false);\n\
                 user_pref(\"toolkit.telemetry.enabled\", false);\n",
            )
            .unwrap();
            format!(
                "-headless -no-remote -new-instance -profile \"{}\" $URL$",
                profile_dir.display()
            )
        }
        _ => format!(
            "--headless=new --disable-gpu --no-first-run --no-default-browser-check \
             --disable-extensions --disable-component-update --disable-sync \
             --disable-background-networking{} \"--user-data-dir={}\" $URL$",
            // Root on Linux (a development dry run) needs this; never on Windows.
            if cfg!(unix) { " --no-sandbox" } else { "" },
            profile_dir.display()
        ),
    };
    let profile = Profile {
        id: format!("qa-{tag}"),
        name: format!("{} (headless QA)", template.name),
        kind,
        program: template.program.clone(),
        arguments,
        enabled: true,
    };
    let mut out = json!({"browser": template.name, "tag": tag, "profile_dir": profile_dir});
    let (program, args) = match launch(&profile, url) {
        Ok(Launch::Program { program, args }) => (program, args),
        other => {
            out["ok"] = json!(false);
            out["error"] = json!(format!("{other:?}"));
            return out;
        }
    };
    out["program"] = json!(program);
    out["url_is_one_argument"] = json!(args.iter().filter(|a| a.as_str() == url).count() == 1);
    let before = seen.lock().unwrap().len();
    let cfg = rustpy_project::RunConfig {
        name: format!("Browser launcher: {}", profile.name),
        kind: rustpy_project::RunConfigKind::Program { program, args },
        working_dir: Some(work.to_path_buf()),
        temporary: true,
        add_content_roots: false,
        add_source_roots: false,
        ..Default::default()
    };
    let started = Instant::now();
    // The IDE starts browsers as launcher sessions (see web_preview_ui.rs).
    let mut session = rustpy_run::RunSession::spawn_launcher(1, &cfg, work);
    out["pid"] = json!(session.process_id());
    let deadline = started + Duration::from_secs(60);
    let hit = loop {
        session.poll();
        let hit = seen.lock().unwrap()[before..]
            .iter()
            .find(|(path, _)| matches(path))
            .cloned();
        if hit.is_some() || Instant::now() > deadline {
            break hit;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    out["load_ms"] = json!(started.elapsed().as_millis());
    out["loaded"] = json!(hit.is_some());
    if let Some((path, agent)) = &hit {
        out["request"] = json!(path);
        out["user_agent"] = json!(agent);
        out["javascript"] = json!(path.contains("js=42"));
        out["stylesheet"] = json!(path.contains("css=rgb(1%2C%202%2C%203)"));
    }
    out["running_before_stop"] = json!(session.is_running());
    session.stop();
    let stop_deadline = Instant::now() + Duration::from_secs(20);
    while session.is_running() && Instant::now() < stop_deadline {
        session.poll();
        std::thread::sleep(Duration::from_millis(50));
    }
    out["stopped"] = json!(!session.is_running());
    out["status"] = json!(format!("{:?}", session.status));
    let output = session.text();
    out["output_tail"] = json!(output
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>());
    out["ok"] = json!(hit.is_some() && out["stopped"] == json!(true));
    out
}

fn webview2(exe: &Path, work: &Path, beacon: u16, seen: &Seen) -> Value {
    let project = work.join("gui project");
    write_site(&project, beacon);
    let config = work.join("gui-config");
    let system = work.join("gui-system");
    std::fs::create_dir_all(&config).unwrap();
    let screenshot = work.join("webview2-screenshot.png");
    let before = seen.lock().unwrap().len();
    let started = Instant::now();
    let mut command = std::process::Command::new(exe);
    command
        .arg(&project)
        .arg("--open-file")
        .arg(project.join("index.html"))
        .arg("--action")
        .arg("WebPreview.Open")
        .arg("--screenshot")
        .arg(&screenshot)
        .env("RUSTPY_CONFIG_DIR", &config)
        .env("RUSTPY_SYSTEM_DIR", &system)
        .env("RUSTPY_TRUST_ALL", "1")
        .env("RUSTPY_SMOKE_DELAY", "20")
        .env("RUSTPY_SMOKE_DEADLINE", "60")
        .current_dir(&project);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return json!({"ok": false, "error": format!("cannot start {}: {e}", exe.display())})
        }
    };
    let deadline = started + Duration::from_secs(90);
    let mut exited = None;
    let hit = loop {
        let hit = seen.lock().unwrap()[before..]
            .iter()
            .find(|(path, _)| path.starts_with("/beacon?") && path.contains("kind=webview2&"))
            .cloned();
        if exited.is_none() {
            exited = child.try_wait().ok().flatten();
        }
        if hit.is_some() || exited.is_some() || Instant::now() > deadline {
            break hit;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let load_ms = started.elapsed().as_millis();
    // The smoke screenshot closes the IDE by itself; never leave it open.
    let close_deadline = Instant::now() + Duration::from_secs(60);
    while exited.is_none() && Instant::now() < close_deadline {
        exited = child.try_wait().ok().flatten();
        std::thread::sleep(Duration::from_millis(200));
    }
    let killed = if exited.is_none() {
        let _ = child.kill();
        exited = child.wait().ok();
        true
    } else {
        false
    };
    let data = system.join("webview2");
    let beside_exe = exe.with_file_name(format!(
        "{}.WebView2",
        exe.file_name().unwrap().to_string_lossy()
    ));
    let mut out = json!({
        "loaded": hit.is_some(),
        "load_ms": load_ms,
        "exit": exited.map(|s| format!("{s:?}")),
        "killed_after_timeout": killed,
        "screenshot": screenshot.is_file(),
        "user_data_in_system_dir": data.is_dir(),
        "user_data_beside_exe": beside_exe.exists(),
        "log": std::fs::read_to_string(system.join("log").join("rustpy.log"))
            .map(|t| t.lines().rev().take(15).map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default(),
    });
    if let Some((path, agent)) = &hit {
        out["request"] = json!(path);
        out["user_agent"] = json!(agent);
        out["javascript"] = json!(path.contains("js=42"));
        out["stylesheet"] = json!(path.contains("css=rgb(1%2C%202%2C%203)"));
    }
    out["ok"] = json!(hit.is_some() && !killed);
    out
}

fn write_site(project: &Path, beacon: u16) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(
        project.join("index.html"),
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>RustPy Windows browser acceptance</title>\n\
         <link rel=\"stylesheet\" href=\"style.css\"></head>\n\
         <body><p id=\"value\">RustPy preview</p><script src=\"app.js\"></script></body></html>\n",
    )
    .unwrap();
    std::fs::write(
        project.join("style.css"),
        "#value { color: rgb(1, 2, 3); }\n",
    )
    .unwrap();
    // The beacon proves the page's JavaScript ran and its stylesheet (served
    // by the same host) applied; an image request needs no CORS.
    std::fs::write(
        project.join("app.js"),
        format!(
            "window.addEventListener('load', () => {{\n\
             const kind = new URLSearchParams(location.search).get('kind') || 'webview2';\n\
             const color = getComputedStyle(document.getElementById('value')).color;\n\
             new Image().src = 'http://127.0.0.1:{beacon}/beacon?kind=' + encodeURIComponent(kind) +\n\
             '&js=' + (6 * 7) + '&css=' + encodeURIComponent(color) + '&t=' + Date.now();\n\
             }});\n"
        ),
    )
    .unwrap();
}

fn answer(mut stream: TcpStream, seen: &Seen) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < 65536 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.lines();
    let target = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let agent = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default();
    if !target.is_empty() {
        seen.lock().unwrap().push((target, agent));
    }
    let body = "ok";
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
}

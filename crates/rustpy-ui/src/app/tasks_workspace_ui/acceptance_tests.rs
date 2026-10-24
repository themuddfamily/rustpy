//! Explicit private-loopback tracker and real repository acceptance.
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Server {
    url: String,
    requests: mpsc::Receiver<(String, serde_json::Value)>,
    release: Arc<AtomicBool>,
    hold_search: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.release.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn server(fail_post: bool, hold_post: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let root = url.clone();
    let (tx, requests) = mpsc::channel();
    let release = Arc::new(AtomicBool::new(!hold_post));
    let hold_search = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (released, stopped) = (release.clone(), stop.clone());
    let held_search = hold_search.clone();
    let join = std::thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(peer) => peer,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("private tracker accept: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 1024];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0 && bytes.len() < 65536);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 65536);
            while bytes.len() < header_end + length {
                let mut chunk = [0; 1024];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            assert!(
                headers.lines().any(|line| line
                    .eq_ignore_ascii_case("authorization: Bearer private-loopback-task-token")),
                "private tracker requires captured authentication"
            );
            let request = headers.lines().next().unwrap().to_owned();
            let post = request.starts_with("POST ");
            let body = if length == 0 {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
            };
            tx.send((request, body)).unwrap();
            if post || held_search.load(Ordering::Acquire) {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !released.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < deadline,
                        "test must release actual worklog response"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let (status, body) = if post && fail_post {
                (
                    "503 Service Unavailable",
                    "{\"error\":\"private failure\"}".into(),
                )
            } else if post {
                ("200 OK", "{\"id\":\"accepted-worklog-7\"}".into())
            } else {
                ("200 OK", serde_json::json!({"issues":[{"id":"ABC-7","title":"issue π actual","url":format!("{root}/issue/ABC-7")}]}).to_string())
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            // Cancellation can legitimately close the owned response socket.
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Server {
        url,
        requests,
        release,
        hold_search,
        stop,
        join: Some(join),
    }
}

struct Fixture {
    app: IdeApp,
    root: tempfile::TempDir,
    ctx: egui::Context,
    time: f64,
    server: Server,
    tracker: Tracker,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.release.store(true, Ordering::Release);
        self.app.cancel_task_workspace();
        rustpy_secrets::delete(&self.tracker.secret_key()).unwrap();
    }
}
fn fixture(fail_post: bool, hold_post: bool) -> Fixture {
    assert_eq!(
        std::env::var("RUSTPY_TASK_PROVIDER_ACCEPTANCE").as_deref(),
        Ok("1"),
        "explicitly admit private tracker acceptance"
    );
    let server = server(fail_post, hold_post);
    let root = tempfile::Builder::new()
        .prefix("task π ")
        .tempdir()
        .unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    rustpy_secrets::configure(rustpy_secrets::SafeConfig::memory());
    let tracker = Tracker {
        id: format!("private-{}", server.url.rsplit(':').next().unwrap()),
        name: "Private owned tracker".into(),
        kind: TrackerKind::Generic,
        url: server.url.clone(),
        generic: model::GenericRest {
            worklog_path: "worklog/${id}".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    rustpy_secrets::set(&tracker.secret_key(), "private-loopback-task-token").unwrap();
    app.tasks.as_mut().unwrap().preferences.trackers = vec![tracker.clone()];
    app.tasks.as_mut().unwrap().preferences.create_branch = false;
    app.tasks.as_mut().unwrap().preferences.create_changelist = false;
    app.tasks.as_mut().unwrap().preferences.tracking = false;
    app.open_task_dialog();
    if let Dialog::OpenTask(state) = &mut app.dialog {
        state.query = "issue π".into();
        state.clear_context = false;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    Fixture {
        app,
        root,
        ctx,
        time: 0.0,
        server,
        tracker,
    }
}
fn frame(fixture: &mut Fixture, events: Vec<egui::Event>) -> Vec<(String, Rect)> {
    fixture.time += 0.1;
    let output = fixture.ctx.run_ui(
        egui::RawInput {
            time: Some(fixture.time),
            focused: true,
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1100.0))),
            ..Default::default()
        },
        |ui| {
            if let Dialog::OpenTask(state) =
                std::mem::replace(&mut fixture.app.dialog, Dialog::None)
            {
                fixture.app.draw_open_task(ui.ctx(), state);
            }
            fixture.app.draw_tasks_workspace(ui.ctx());
            fixture.app.poll_vcs_mappings(ui.ctx());
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                Rect::from_min_size(text.pos, text.galley.size()),
            )),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
fn click(fixture: &mut Fixture, label: &str) {
    frame(fixture, vec![]);
    let mut labels = frame(fixture, vec![]);
    for _ in 0..8 {
        if labels.iter().any(|(text, _)| text == label) {
            break;
        }
        labels = frame(
            fixture,
            vec![
                egui::Event::PointerMoved(Pos2::new(450.0, 400.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -450.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let point = labels
        .iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("actual task control {label:?} missing: {labels:?}"))
        .1
        .center();
    for pressed in [true, false] {
        frame(
            fixture,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn until(fixture: &mut Fixture, condition: impl Fn(&Fixture) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition(fixture) {
        assert!(
            Instant::now() < deadline,
            "task deadline: {:?}; {}",
            fixture.app.status,
            fixture.app.task_workspace.notice
        );
        frame(fixture, vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn open_actual_issue(fixture: &mut Fixture) {
    click(fixture, "Search tracker");
    until(
        fixture,
        |fixture| matches!(&fixture.app.dialog, Dialog::OpenTask(state) if state.issues_rx.is_none() && state.metadata.len()==1),
    );
    let (request, _) = fixture
        .server
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(
        request.starts_with("GET /issues?q=issue%20%CF%80 "),
        "{request}"
    );
    click(fixture, "ABC-7 issue π actual");
    let task = fixture.app.tasks.as_ref().unwrap().active_task().unwrap();
    assert_eq!(task.remote_id.as_deref(), Some("ABC-7"));
    assert_eq!(task.tracker.as_deref(), Some(fixture.tracker.id.as_str()));
    assert_eq!(
        task.url.as_deref(),
        Some(format!("{}/issue/ABC-7", fixture.server.url).as_str())
    );
    fixture
        .app
        .tasks
        .as_mut()
        .unwrap()
        .active_task_mut()
        .unwrap()
        .work_items
        .push(WorkItem {
            id: 71,
            started: 1_700_000_000,
            seconds: 125,
            description: "actual work π \"quoted\"".into(),
            ..Default::default()
        });
    fixture.app.save_tasks();
    fixture.app.task_workspace_open(false);
}
fn post(fixture: &mut Fixture) {
    click(fixture, "Send time to tracker");
    let (request, body) = fixture
        .server
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(request.starts_with("POST /worklog/ABC-7 "), "{request}");
    assert_eq!(body["issue"], "ABC-7");
    assert_eq!(body["seconds"], 125);
    assert_eq!(body["description"], "actual work π \"quoted\"");
}
fn work(fixture: &Fixture) -> &WorkItem {
    &fixture
        .app
        .tasks
        .as_ref()
        .unwrap()
        .tasks
        .iter()
        .find(|task| task.remote_id.as_deref() == Some("ABC-7"))
        .unwrap()
        .work_items[0]
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1; real private authenticated tracker"]
fn configured_tracker_search_open_and_worklog_publish_actual_http_results() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = fixture(false, false);
    open_actual_issue(&mut fixture);
    post(&mut fixture);
    until(&mut fixture, |fixture| {
        !fixture.app.task_operation_pending()
    });
    assert!(work(&fixture).sent && !work(&fixture).sending);
    assert!(work(&fixture)
        .receipt
        .as_ref()
        .unwrap()
        .contains("accepted-worklog-7"));
    let saved = rustpy_settings::tasks::TaskStore::load(fixture.root.path());
    assert!(
        saved
            .tasks
            .iter()
            .find(|task| task.remote_id.as_deref() == Some("ABC-7"))
            .unwrap()
            .work_items[0]
            .sent
    );
    assert!(
        !std::fs::read_to_string(rustpy_settings::tasks::TaskStore::path_for(
            fixture.root.path()
        ))
        .unwrap()
        .contains("private-loopback-task-token")
    );
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1; actual failed tracker POST"]
fn configured_tracker_failure_retains_unsent_work_without_automatic_retry() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = fixture(true, false);
    open_actual_issue(&mut fixture);
    post(&mut fixture);
    until(&mut fixture, |fixture| {
        !fixture.app.task_operation_pending()
    });
    assert!(!work(&fixture).sent && !work(&fixture).sending && work(&fixture).receipt.is_none());
    assert!(fixture.app.task_workspace.notice.contains("HTTP 503"));
    for _ in 0..5 {
        frame(&mut fixture, vec![]);
    }
    assert!(fixture.server.requests.try_recv().is_err());
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1 with RUSTPY_TRUST_ALL unset; actual in-flight authority changes"]
fn configured_accepted_worklog_cannot_publish_after_tracker_edit_or_trust_revocation() {
    let _serial = SERIAL.lock().unwrap();
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "unset RUSTPY_TRUST_ALL"
    );
    for changed in 0..4 {
        let mut fixture = fixture(false, true);
        open_actual_issue(&mut fixture);
        post(&mut fixture);
        match changed {
            0 => fixture.app.tasks.as_mut().unwrap().preferences.trackers[0]
                .url
                .push_str("/changed"),
            1 => {
                fixture.app.settings.trusted_projects.clear();
                fixture.app.settings.trusted_locations.clear();
            }
            2 => fixture.app.file_session += 1,
            _ => fixture
                .app
                .tasks
                .as_mut()
                .unwrap()
                .active_task_mut()
                .unwrap()
                .work_items[0]
                .description
                .push_str(" changed"),
        }
        let replacement = fixture
            .app
            .tasks
            .as_mut()
            .unwrap()
            .create_local("replacement active task");
        fixture.app.tasks.as_mut().unwrap().active = Some(replacement.clone());
        fixture.server.release.store(true, Ordering::Release);
        until(&mut fixture, |fixture| {
            !fixture.app.task_operation_pending()
        });
        assert!(!work(&fixture).sent && work(&fixture).receipt.is_none());
        assert_eq!(
            work(&fixture).sending,
            changed == 2,
            "only the original live session may retire its request spinner"
        );
        assert_eq!(
            fixture.app.tasks.as_ref().unwrap().active.as_deref(),
            Some(replacement.as_str())
        );
        assert!(fixture
            .app
            .task_workspace
            .notice
            .contains("authority changed"));
    }
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1; actual late tracker completion and replacement-store ID collisions"]
fn configured_late_worklog_success_error_and_cancel_cannot_clear_replacement_request_owner() {
    let _serial = SERIAL.lock().unwrap();
    for outcome in 0..4 {
        let mut fixture = fixture(outcome == 1, true);
        open_actual_issue(&mut fixture);
        post(&mut fixture);
        let original = fixture
            .app
            .task_workspace
            .inflight
            .as_ref()
            .unwrap()
            .request;
        assert_eq!(work(&fixture).sending_owner, Some(original));
        let serialized = serde_json::to_string(fixture.app.tasks.as_ref().unwrap()).unwrap();
        assert!(
            !serialized.contains("sending_owner"),
            "request identity must never be persisted"
        );
        let mut replacement: rustpy_settings::tasks::TaskStore =
            serde_json::from_str(&serialized).unwrap();
        let item = &mut replacement
            .tasks
            .iter_mut()
            .find(|task| task.remote_id.as_deref() == Some("ABC-7"))
            .unwrap()
            .work_items[0];
        assert!(
            !item.sending && item.sending_owner.is_none(),
            "reload does not adopt a live request"
        );
        // The replacement deliberately has identical persisted task/item IDs
        // and content, but its busy state belongs to a different request.
        item.sending = true;
        item.sending_owner = Some(original + 1);
        let expected = item.clone();
        fixture.app.tasks = Some(replacement);
        match outcome {
            2 => fixture.app.stop_task_operation(),
            3 => fixture.app.cancel_task_workspace(),
            _ => {}
        }
        assert_eq!(
            work(&fixture),
            &expected,
            "cancel must not clear a replacement request"
        );
        fixture.server.release.store(true, Ordering::Release);
        until(&mut fixture, |fixture| {
            !fixture.app.task_operation_pending()
                && !TaskManager::global()
                    .running_all()
                    .iter()
                    .any(|task| task.id == original)
        });
        assert_eq!(
            work(&fixture),
            &expected,
            "late outcome {outcome} must preserve replacement busy state and receipt"
        );
    }
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1; actual owned worklog cancellation"]
fn configured_cancel_retires_original_worklog_spinner_without_publishing_late_receipt() {
    let _serial = SERIAL.lock().unwrap();
    for close_workspace in [false, true] {
        let mut fixture = fixture(false, true);
        open_actual_issue(&mut fixture);
        post(&mut fixture);
        let request = fixture
            .app
            .task_workspace
            .inflight
            .as_ref()
            .unwrap()
            .request;
        assert_eq!(work(&fixture).sending_owner, Some(request));
        if close_workspace {
            fixture.app.cancel_task_workspace();
        } else {
            fixture.app.stop_task_operation();
        }
        assert!(!work(&fixture).sending && work(&fixture).sending_owner.is_none());
        fixture.server.release.store(true, Ordering::Release);
        until(&mut fixture, |_| {
            !TaskManager::global()
                .running_all()
                .iter()
                .any(|task| task.id == request)
        });
        assert!(!work(&fixture).sent && work(&fixture).receipt.is_none());
    }
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1 with RUSTPY_TRUST_ALL unset; actual trust admission"]
fn configured_untrusted_task_cannot_start_worklog_post() {
    let _serial = SERIAL.lock().unwrap();
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "unset RUSTPY_TRUST_ALL"
    );
    let mut fixture = fixture(false, false);
    open_actual_issue(&mut fixture);
    fixture.app.settings.trusted_projects.clear();
    fixture.app.settings.trusted_locations.clear();
    click(&mut fixture, "Send time to tracker");
    assert!(!fixture.app.task_operation_pending());
    assert!(fixture.server.requests.try_recv().is_err());
    assert!(!work(&fixture).sent && !work(&fixture).sending);
    assert!(fixture
        .app
        .task_workspace
        .notice
        .contains("Trust the project"));
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1 with RUSTPY_TRUST_ALL unset; actual search trust admission"]
fn configured_untrusted_tracker_search_sends_nothing_then_explicit_trust_allows_search() {
    let _serial = SERIAL.lock().unwrap();
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "unset RUSTPY_TRUST_ALL"
    );
    let mut fixture = fixture(false, false);
    fixture.app.settings.trusted_projects.clear();
    fixture.app.settings.trusted_locations.clear();
    click(&mut fixture, "Search tracker");
    assert!(
        matches!(&fixture.app.dialog, Dialog::OpenTask(state) if state.issues_rx.is_none()
        && state.metadata.is_empty() && state.issues_status.as_ref().unwrap().contains("Trust the project"))
    );
    assert!(fixture.server.requests.try_recv().is_err());
    fixture.app.trust_project_at(fixture.root.path());
    open_actual_issue(&mut fixture);
}

#[test]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1 with RUSTPY_TRUST_ALL unset; held actual HTTP search responses"]
fn configured_tracker_search_cannot_publish_after_trust_or_selected_tracker_changes() {
    let _serial = SERIAL.lock().unwrap();
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "unset RUSTPY_TRUST_ALL"
    );
    for revoke in [false, true] {
        let mut fixture = fixture(false, false);
        fixture.server.hold_search.store(true, Ordering::Release);
        fixture.server.release.store(false, Ordering::Release);
        click(&mut fixture, "Search tracker");
        let (request, _) = fixture
            .server
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(request.starts_with("GET /issues?"));
        if revoke {
            fixture.app.settings.trusted_projects.clear();
            fixture.app.settings.trusted_locations.clear();
        } else {
            // Keep the old tracker configured: merely checking membership is
            // insufficient when the selected tracker changes during a request.
            let mut other = fixture.tracker.clone();
            other.id.push_str("-replacement");
            fixture
                .app
                .tasks
                .as_mut()
                .unwrap()
                .preferences
                .trackers
                .push(other);
            if let Dialog::OpenTask(state) = &mut fixture.app.dialog {
                state.server = 1;
            }
        }
        fixture.server.release.store(true, Ordering::Release);
        until(
            &mut fixture,
            |fixture| matches!(&fixture.app.dialog, Dialog::OpenTask(state) if state.issues_rx.is_none()),
        );
        assert!(
            matches!(&fixture.app.dialog, Dialog::OpenTask(state) if state.issues.is_empty()
            && state.metadata.is_empty() && state.issues_status.as_ref().unwrap().contains("authority changed"))
        );
        assert!(fixture
            .app
            .tasks
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .all(|task| task.remote_id.is_none()));
    }
}

#[test]
#[cfg(unix)]
#[ignore = "requires RUSTPY_TASK_PROVIDER_ACCEPTANCE=1 and real Git; actual task-close commit consumer"]
fn configured_close_task_commits_real_selected_changes_before_closing() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = fixture(false, false);
    open_actual_issue(&mut fixture);
    let repo = GitRepo::init(fixture.root.path()).unwrap();
    repo.run_git(&["config", "user.name", "Private Acceptance"])
        .unwrap();
    repo.run_git(&["config", "user.email", "private@invalid.example"])
        .unwrap();
    repo.run_git(&[
        "config",
        "core.hooksPath",
        fixture.root.path().join(".git/hooks").to_str().unwrap(),
    ])
    .unwrap();
    let path = fixture.root.path().join("owned.txt");
    std::fs::write(&path, "before\n").unwrap();
    repo.commit(std::slice::from_ref(&path), "baseline", false)
        .unwrap();
    fixture.app.project.as_mut().unwrap().git = Some(repo);
    std::fs::write(&path, "after π\n").unwrap();
    let repo = fixture.app.project.as_ref().unwrap().git.as_ref().unwrap();
    fixture.app.git_panel.refresh(repo, 0.0);
    fixture.app.git_panel.checked.clear();
    fixture.app.git_panel.checked.insert(path.clone());
    fixture.app.task_workspace_open(true);
    fixture.app.task_workspace.commit = true;
    let id = fixture.app.tasks.as_ref().unwrap().active.clone().unwrap();
    // A real rejecting Git hook must leave both the task and repository open
    // for review; retry uses the same production close/commit consumer.
    let hook = fixture.root.path().join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf 'PRIVATE_TASK_HOOK_REJECTED\\n' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    click(&mut fixture, "Commit and close task");
    until(&mut fixture, |fixture| {
        fixture.app.vcs_workspace.commit.is_none()
            && fixture.app.git_panel.commit_workspace.failure.is_some()
    });
    assert!(!fixture.app.tasks.as_ref().unwrap().get(&id).unwrap().closed);
    assert_eq!(
        fixture.app.tasks.as_ref().unwrap().active.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(
        fixture
            .app
            .project
            .as_ref()
            .unwrap()
            .git
            .as_ref()
            .unwrap()
            .head_text(&path)
            .as_deref(),
        Some("before\n")
    );
    std::fs::remove_file(hook).unwrap();
    fixture.app.task_workspace_open(true);
    click(&mut fixture, "Commit and close task");
    until(&mut fixture, |fixture| {
        fixture.app.vcs_workspace.commit.is_none()
            && fixture
                .app
                .tasks
                .as_ref()
                .unwrap()
                .get(&id)
                .is_some_and(|task| task.closed)
    });
    let repo = fixture.app.project.as_ref().unwrap().git.as_ref().unwrap();
    assert_eq!(repo.head_text(&path).as_deref(), Some("after π\n"));
    assert!(repo.head_message().unwrap().contains("ABC-7"));
    assert_ne!(
        fixture.app.tasks.as_ref().unwrap().active.as_deref(),
        Some(id.as_str())
    );
}

//! Real processes through App before-launch and trusted-project startup consumers.
use super::*;
use crate::run_context::RunMode;
use rustpy_project::run_workflow::{BeforeLaunch, Task};
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    root: tempfile::TempDir,
    python: PathBuf,
    context: egui::Context,
    time: f64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.cancel_run_workflows();
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.app.poll_run_workflows(&self.context);
            for session in &mut self.app.run_panel.sessions {
                session.poll();
            }
            if !self.app.run_workflows_pending()
                && self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .all(|session| !session.is_running())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn fixture(first: &str, startup: bool, trusted: bool) -> Fixture {
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("set RUSTPY_PROVIDER_PYTHON to the admitted absolute SDK"),
    );
    assert!(python.is_absolute() && python.is_file());
    let sdk = rustpy_project::interpreter::probe_detailed(&python, "workflow-acceptance")
        .expect("selected SDK must pass the production probe")
        .interpreter;
    assert_eq!(sdk.path, python);
    let root = tempfile::Builder::new()
        .prefix("workflow π ")
        .tempdir()
        .unwrap();
    std::fs::create_dir(root.path().join("cwd π")).unwrap();
    std::fs::write(root.path().join("actual_steps.py"), r#"import json, os, pathlib, subprocess, sys, time
root=pathlib.Path(__file__).parent
mode=sys.argv[1]
with (root/'events.jsonl').open('a',encoding='utf-8') as stream:
    stream.write(json.dumps({'mode':mode,'pid':os.getpid(),'python':sys.executable,'cwd':os.getcwd(),'env':os.environ.get('WORKFLOW_CAPTURE')},ensure_ascii=False)+'\n')
if mode=='failure':
    print('ACTUAL_PREREQUISITE_FAILURE',file=sys.stderr)
    sys.exit(7)
if mode=='hold':
    child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(300)'])
    (root/'descendant.pid').write_text(str(child.pid))
    while True: time.sleep(0.02)
"#).unwrap();
    let configuration = |name: &str, mode: &str| RunConfig {
        name: name.into(),
        kind: RunConfigKind::Python {
            script: root.path().join("actual_steps.py"),
        },
        parameters: mode.into(),
        working_dir: Some(root.path().join("cwd π")),
        env: [("WORKFLOW_CAPTURE".into(), format!("captured-{name}-π"))].into(),
        ..Default::default()
    };
    let first = configuration("first", first);
    let mut second = configuration("second", "second");
    second.workflow.before_launch.push(BeforeLaunch {
        enabled: true,
        task: Task::Configuration {
            name: "first".into(),
        },
    });
    let mut main = configuration("main", "main");
    main.workflow.before_launch.push(BeforeLaunch {
        enabled: true,
        task: Task::Configuration {
            name: "second".into(),
        },
    });
    RunConfigStore {
        configs: vec![first, second, main],
        selected: Some("main".into()),
    }
    .save(root.path())
    .unwrap();
    if startup {
        // Preserve the workspace fields already owned by other consumers.
        let mut workspace = Workspace::try_load(root.path()).unwrap();
        workspace.startup_tasks = vec!["main".into()];
        workspace.interpreter = Some(python.clone());
        workspace.save(root.path()).unwrap();
    }
    let mut settings = Settings::default();
    if trusted {
        settings.trust_project(root.path());
    }
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.project.as_mut().unwrap().interpreter_rx = None;
    if startup {
        // A transient interpreter field is not a configured project SDK:
        // trusting the project rediscovers from model/workspace authority.
        // Use the same selection consumer as the interpreter chooser.
        app.select_interpreter(sdk);
        assert_eq!(
            rustpy_project::ProjectModel::load(root.path())
                .sdk
                .as_deref(),
            Some(python.as_path())
        );
        assert_eq!(
            app.effective_workspace().interpreter.as_deref(),
            Some(python.as_path())
        );
    } else {
        app.project.as_mut().unwrap().interpreter = Some(sdk);
    }
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    Fixture {
        app,
        root,
        python,
        context,
        time: 0.0,
    }
}

fn tick(fixture: &mut Fixture, full_frame: bool) {
    if full_frame {
        fixture.time += 0.05;
        let mut native = eframe::Frame::_new_kittest();
        fixture
            .context
            .run_ui(
                egui::RawInput {
                    time: Some(fixture.time),
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                    ..Default::default()
                },
                |ui| eframe::App::ui(&mut fixture.app, ui, &mut native),
            )
            .drop_without_applying_deltas();
    } else {
        fixture.app.poll_run_workflows(&fixture.context);
        for session in &mut fixture.app.run_panel.sessions {
            session.poll();
        }
    }
    std::thread::sleep(Duration::from_millis(10));
}

fn until(fixture: &mut Fixture, full_frame: bool, condition: impl Fn(&Fixture) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition(fixture) {
        assert!(
            Instant::now() < deadline,
            "workflow deadline: status={:?}, pending={:?}",
            fixture.app.status,
            fixture.app.run_workflow_stamps()
        );
        tick(fixture, full_frame);
    }
}

fn launch(fixture: &mut Fixture) {
    let main = fixture
        .app
        .project
        .as_ref()
        .unwrap()
        .run_configs
        .configs
        .iter()
        .find(|cfg| cfg.name == "main")
        .unwrap()
        .clone();
    fixture.app.launch_config(main, RunMode::Run);
}

fn events(fixture: &Fixture) -> Vec<serde_json::Value> {
    std::fs::read_to_string(fixture.root.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_chain(fixture: &Fixture) {
    let events = events(fixture);
    assert_eq!(events.len(), 3, "{events:?}");
    for (event, name) in events.iter().zip(["first", "second", "main"]) {
        assert_eq!(event["mode"], name);
        assert_eq!(event["env"], format!("captured-{name}-π"));
        assert_eq!(
            event["python"].as_str().unwrap(),
            fixture.python.to_str().unwrap()
        );
        assert_eq!(
            PathBuf::from(event["cwd"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            fixture.root.path().join("cwd π").canonicalize().unwrap()
        );
        assert_reaped(event["pid"].as_u64().unwrap());
    }
    assert!(fixture
        .app
        .run_panel
        .sessions
        .iter()
        .any(|session| session.name == "main"
            && session.status == rustpy_run::SessionStatus::Exited(0)));
}

fn assert_reaped(pid: u64) {
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "process {pid} physically retired"
    );
}

#[test]
#[ignore = "requires explicit RUSTPY_PROVIDER_PYTHON; actual App workflow processes"]
fn configured_nested_before_launch_runs_in_order_with_captured_sdk_environment_and_cwd() {
    let mut fixture = fixture("first", false, true);
    launch(&mut fixture);
    until(&mut fixture, false, |fixture| {
        !fixture.app.run_workflows_pending()
            && !fixture.app.run_panel.sessions.is_empty()
            && fixture
                .app
                .run_panel
                .sessions
                .iter()
                .all(|session| !session.is_running())
    });
    assert_chain(&fixture);
}

#[test]
#[ignore = "requires explicit RUSTPY_PROVIDER_PYTHON; actual failed prerequisite"]
fn configured_failed_before_launch_never_starts_following_tasks_or_main() {
    let mut fixture = fixture("failure", false, true);
    launch(&mut fixture);
    until(&mut fixture, false, |fixture| {
        !fixture.app.run_workflows_pending()
    });
    let events = events(&fixture);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["mode"], "failure");
    assert_reaped(events[0]["pid"].as_u64().unwrap());
    assert!(fixture.app.run_panel.sessions.is_empty());
    assert!(fixture
        .app
        .status
        .as_ref()
        .unwrap()
        .0
        .contains("ACTUAL_PREREQUISITE_FAILURE"));
}

#[test]
#[ignore = "requires explicit RUSTPY_PROVIDER_PYTHON; actual prerequisite process-tree cancellation"]
fn configured_cancel_before_launch_reaps_child_and_descendant_without_starting_main() {
    let mut fixture = fixture("hold", false, true);
    launch(&mut fixture);
    until(&mut fixture, false, |fixture| {
        std::fs::read_to_string(fixture.root.path().join("descendant.pid"))
            .is_ok_and(|pid| pid.parse::<u64>().is_ok())
    });
    let records = events(&fixture);
    let pid = records[0]["pid"].as_u64().unwrap();
    let descendant: u64 = std::fs::read_to_string(fixture.root.path().join("descendant.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(PathBuf::from(format!("/proc/{pid}")).exists());
    assert!(PathBuf::from(format!("/proc/{descendant}")).exists());
    let items = fixture.app.before_launch_stop_items();
    assert_eq!(items.len(), 1);
    let crate::run_anything::StopItem::BeforeLaunch(id, _) = items[0] else {
        panic!("owned prerequisite Stop item")
    };
    fixture.app.stop_before_launch(id);
    until(&mut fixture, false, |fixture| {
        !fixture.app.run_workflows_pending()
    });
    assert_eq!(events(&fixture).len(), 1);
    assert!(fixture.app.run_panel.sessions.is_empty());
    assert_reaped(pid);
    assert_reaped(descendant);
}

#[test]
#[ignore = "requires explicit RUSTPY_PROVIDER_PYTHON; actual trusted-project startup App frames"]
fn configured_startup_task_waits_for_trust_then_runs_once_through_actual_app_frames() {
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "unset RUSTPY_TRUST_ALL to exercise actual project trust admission"
    );
    let mut fixture = fixture("first", true, false);
    for _ in 0..8 {
        tick(&mut fixture, true);
    }
    assert!(!fixture.app.startup_tasks_done);
    assert!(!fixture.root.path().join("events.jsonl").exists());
    assert!(fixture.app.run_panel.sessions.is_empty());
    fixture.app.trust_project_at(fixture.root.path());
    fixture.app.dialog = Dialog::None;
    until(&mut fixture, true, |fixture| {
        fixture.app.startup_tasks_done
            && !fixture.app.run_workflows_pending()
            && !fixture.app.run_panel.sessions.is_empty()
            && fixture
                .app
                .run_panel
                .sessions
                .iter()
                .all(|session| !session.is_running())
    });
    assert_eq!(
        fixture
            .app
            .project
            .as_ref()
            .unwrap()
            .interpreter
            .as_ref()
            .unwrap()
            .path,
        fixture.python,
        "startup rediscovery must preserve the explicitly configured project SDK"
    );
    assert_chain(&fixture);
    for _ in 0..8 {
        tick(&mut fixture, true);
    }
    assert_eq!(
        events(&fixture).len(),
        3,
        "startup tasks do not repeat on later frames"
    );
}

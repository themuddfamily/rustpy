//! Installed data tools, invoked through the actual painted App commands.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    root: tempfile::TempDir,
    context: egui::Context,
    time: f64,
    last_click: String,
    completed_sessions: std::collections::BTreeSet<u64>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
    }
}

fn configured(variable: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(variable)
            .unwrap_or_else(|| panic!("set {variable} to the admitted installed tool")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{variable} must name an installed absolute executable"
    );
    path
}

fn fixture(name: &str, source: &str) -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("data tools π ")
        .tempdir()
        .unwrap();
    let path = root.path().join(name);
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.open_data_files();
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    Fixture {
        app,
        root,
        context,
        time: 0.0,
        last_click: String::new(),
        completed_sessions: Default::default(),
    }
}

fn frame(fixture: &mut Fixture, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
    fixture.time += 0.1;
    let output = fixture.context.run_ui(
        egui::RawInput {
            time: Some(fixture.time),
            focused: true,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 1000.0),
            )),
            ..Default::default()
        },
        |ui| fixture.app.data_files_dialog(ui.ctx()),
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}

fn click(fixture: &mut Fixture, label: &str) {
    // Allow the real dialog its initial sizing pass before targeting a button.
    frame(fixture, vec![]);
    let labels = frame(fixture, vec![]);
    let mut bounds = labels
        .iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("actual Data Tools control {label:?} absent: {labels:?}"))
        .1;
    // A real pointer enters before pressing. Reopening a resized window can
    // reposition its controls during the hover pass; target its stable paint.
    let mut stable = false;
    for _ in 0..4 {
        let labels = frame(fixture, vec![egui::Event::PointerMoved(bounds.center())]);
        let next = labels.iter().find(|(text, _)| text == label).unwrap().1;
        if next == bounds {
            stable = true;
            break;
        }
        bounds = next;
    }
    assert!(
        stable,
        "Data Tools control did not settle: {label} {bounds:?}"
    );
    let point = bounds.center();
    fixture.last_click = format!("label={label:?}; bounds={bounds:?}");
    for pressed in [true, false] {
        let labels = frame(
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
        fixture.last_click.push_str(&format!(
            "; pressed={pressed} bounds={:?}",
            labels
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, bounds)| bounds)
        ));
    }
}

fn tools(fixture: &mut Fixture) {
    fixture.app.open_data_files();
    click(fixture, "XML / Shell / VCS tools");
}

fn replace(fixture: &mut Fixture, source: &str) {
    let index = fixture.app.active.unwrap();
    let end = fixture.app.tabs[index].state.doc.len();
    fixture.app.tabs[index].state.apply_text_edits(
        vec![(TextRange::new(TextSize::new(0), end), source.into())],
        "Acceptance unsaved edit",
    );
}

fn session_ids(fixture: &Fixture) -> Vec<u64> {
    fixture
        .app
        .run_panel
        .sessions
        .iter()
        .map(|session| session.id)
        .collect()
}

fn result(fixture: &mut Fixture) -> (rustpy_run::SessionStatus, String) {
    // RunPanel intentionally replaces an unpinned finished tab of the same
    // name. A fresh session identity, rather than vector growth, proves launch.
    let fresh = fixture
        .app
        .run_panel
        .sessions
        .iter()
        .filter(|session| !fixture.completed_sessions.contains(&session.id))
        .map(|session| session.id)
        .collect::<Vec<_>>();
    assert_eq!(
        fresh.len(),
        1,
        "actual tool must launch; status={:?}; {}; current={:?}; data_dialog={}; click={}",
        fixture.app.status,
        fixture.app.data_files.acceptance_details(),
        fixture.app.active.map(|index| (
            fixture.app.tabs[index].view.id,
            fixture.app.tabs[index].state.doc.identity(),
            fixture.app.tabs[index].state.doc.version(),
            fixture.app.tabs[index].state.doc.is_read_only(),
        )),
        matches!(fixture.app.dialog, Dialog::DataFiles),
        fixture.last_click
    );
    let id = fresh[0];
    fixture.completed_sessions.insert(id);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let session = fixture
            .app
            .run_panel
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .unwrap();
        session.poll();
        if !session.is_running() {
            let status = session.status.clone();
            let output = session
                .output
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            fixture.app.poll_external_tool_sessions();
            return (status, output);
        }
        assert!(
            Instant::now() < deadline,
            "installed data tool did not retire"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires RUSTPY_SHELLCHECK absolute installed executable; actual App command"]
fn configured_shellcheck_reports_unsaved_source_and_clean_readonly_control() {
    let _serial = CONFIGURED_PROVIDERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let executable = configured("RUSTPY_SHELLCHECK");
    let clean = "#!/bin/sh\nvalue='hello π'\nprintf '%s\\n' \"$value\"\n";
    let dirty = "#!/bin/sh\nvalue='hello π'\nprintf '%s\\n' $value\n";
    let mut fixture = fixture("check π.sh", clean);
    fixture.app.settings.data_files.shellcheck = executable.to_string_lossy().into();
    replace(&mut fixture, dirty);
    tools(&mut fixture);
    click(&mut fixture, "ShellCheck");
    let (status, output) = result(&mut fixture);
    assert_eq!(status, rustpy_run::SessionStatus::Exited(1), "{output}");
    assert!(
        output.contains("SC2086"),
        "actual unquoted expansion diagnostic: {output}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("check π.sh")).unwrap(),
        dirty,
        "public tool launch saves current unsaved source"
    );
    replace(&mut fixture, clean);
    assert!(fixture.app.save_all());
    let index = fixture.app.active.unwrap();
    fixture.app.tabs[index].state.doc.set_read_only(true);
    tools(&mut fixture);
    click(&mut fixture, "ShellCheck");
    let (status, output) = result(&mut fixture);
    assert_eq!(status, rustpy_run::SessionStatus::Exited(0), "{output}");
    assert!(!output.contains("SC2086"));
    assert!(fixture.app.tabs[index].state.doc.is_read_only());
    assert_eq!(fixture.app.tabs[index].state.doc.text(), clean);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("check π.sh")).unwrap(),
        clean
    );
}

#[test]
#[ignore = "requires RUSTPY_SHFMT absolute installed executable; actual App formatting/publication"]
fn configured_shfmt_formats_unsaved_source_and_refuses_stale_and_readonly_writes() {
    let _serial = CONFIGURED_PROVIDERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let executable = configured("RUSTPY_SHFMT");
    let initial = "#!/bin/sh\necho original\n";
    let dirty = "#!/bin/sh\nif true;then\necho 'π'\nfi\n";
    let formatted = "#!/bin/sh\nif true; then\n\techo 'π'\nfi\n";
    let mut fixture = fixture("format π.sh", initial);
    fixture.app.settings.data_files.shfmt = executable.to_string_lossy().into();
    replace(&mut fixture, dirty);
    tools(&mut fixture);
    click(&mut fixture, "shfmt");
    let (status, output) = result(&mut fixture);
    assert_eq!(status, rustpy_run::SessionStatus::Exited(0), "{output}");
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("format π.sh")).unwrap(),
        formatted
    );
    let index = fixture.app.active.unwrap();
    assert_eq!(
        fixture.app.tabs[index].state.doc.text(),
        formatted,
        "external-tool synchronization publishes actual formatted bytes to editor"
    );
    tools(&mut fixture);
    let prior_sessions = session_ids(&fixture);
    replace(&mut fixture, dirty);
    let version = fixture.app.tabs[index].state.doc.version();
    click(&mut fixture, "shfmt");
    assert_eq!(
        session_ids(&fixture),
        prior_sessions,
        "stale button must not spawn"
    );
    fixture.app.data_format_active();
    assert_eq!(
        session_ids(&fixture),
        prior_sessions,
        "stale general Format must not spawn"
    );
    assert_eq!(fixture.app.tabs[index].state.doc.version(), version);
    assert_eq!(fixture.app.tabs[index].state.doc.text(), dirty);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("format π.sh")).unwrap(),
        formatted
    );
    assert!(fixture.app.save_all());
    tools(&mut fixture);
    let readonly_version = fixture.app.tabs[index].state.doc.version();
    fixture.app.tabs[index].state.doc.set_read_only(true);
    click(&mut fixture, "shfmt");
    assert_eq!(
        session_ids(&fixture),
        prior_sessions,
        "read-only button must not spawn"
    );
    fixture.app.dialog = Dialog::None;
    fixture.app.data_format_active();
    assert_eq!(
        session_ids(&fixture),
        prior_sessions,
        "read-only general Format must not spawn"
    );
    assert!(fixture.app.tabs[index].state.doc.is_read_only());
    assert_eq!(
        fixture.app.tabs[index].state.doc.version(),
        readonly_version
    );
    assert_eq!(fixture.app.tabs[index].state.doc.text(), dirty);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("format π.sh")).unwrap(),
        dirty
    );
}

#[test]
#[ignore = "requires RUSTPY_XMLLINT absolute installed executable; actual App DTD validator"]
fn configured_xmllint_reports_real_dtd_error_then_accepts_clean_unsaved_document() {
    let _serial = CONFIGURED_PROVIDERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let executable = configured("RUSTPY_XMLLINT");
    let clean = "<!DOCTYPE root [<!ELEMENT root (value)><!ELEMENT value (#PCDATA)>]>\n<root><value>π</value></root>\n";
    let dirty = "<!DOCTYPE root [<!ELEMENT root (value)><!ELEMENT value (#PCDATA)>]>\n<root><wrong>π</wrong></root>\n";
    let mut fixture = fixture("validate π.xml", clean);
    fixture.app.settings.data_files.xmllint = executable.to_string_lossy().into();
    replace(&mut fixture, dirty);
    tools(&mut fixture);
    click(&mut fixture, "Validate DTD");
    let (status, output) = result(&mut fixture);
    assert!(
        matches!(status, rustpy_run::SessionStatus::Exited(code) if code != 0),
        "{status:?}: {output}"
    );
    assert!(
        output.contains("validity error") && output.contains("wrong"),
        "real DTD diagnostic: {output}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("validate π.xml")).unwrap(),
        dirty
    );
    // The original dialog is stale after an editor edit; validation must not
    // silently save/run a different source than the one it captured.
    let prior_sessions = session_ids(&fixture);
    replace(&mut fixture, clean);
    click(&mut fixture, "Validate DTD");
    assert_eq!(
        session_ids(&fixture),
        prior_sessions,
        "stale validator must not replace a completed session"
    );
    tools(&mut fixture);
    click(&mut fixture, "Validate DTD");
    let (status, output) = result(&mut fixture);
    assert_eq!(status, rustpy_run::SessionStatus::Exited(0), "{output}");
    assert!(!output.contains("validity error"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("validate π.xml")).unwrap(),
        clean
    );
    assert_eq!(
        fixture.app.tabs[fixture.app.active.unwrap()]
            .state
            .doc
            .text(),
        clean
    );
}

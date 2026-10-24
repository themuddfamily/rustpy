//! Genuine admitted Pyramid/web2py scaffolds through the Frameworks Run action.
use super::*;
use rustpy_project::{
    project_templates::{plan, TemplateOptions},
    setup::ProjectKind,
};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

struct Owned {
    app: IdeApp,
    _root: tempfile::TempDir,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.app.cancel_run_workflows();
        let ctx = egui::Context::default();
        loop {
            for session in &mut self.app.run_panel.sessions {
                session.stop();
            }
            self.app.poll_run_workflows(&ctx);
            self.app.run_panel.poll();
            if !self.app.run_workflows_pending()
                && self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .all(|session| !session.owns_process())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn required(name: &str) -> String {
    std::env::var(name).expect(name)
}
fn admitted_file(name: &str) -> PathBuf {
    let path = PathBuf::from(required(name));
    assert!(path.is_absolute() && path.is_file());
    let actual: String = rustpy_net::x509::sha256(&std::fs::read(&path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        actual,
        required(&format!("{name}_SHA256")),
        "admitted provider bytes changed"
    );
    path
}
fn response(port: u16) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(
        &(Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_millis(200),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut bytes = Vec::new();
    stream.take(65537).read_to_end(&mut bytes)?;
    assert!(bytes.len() <= 65536);
    Ok(String::from_utf8(bytes).unwrap())
}

#[test]
#[ignore = "explicit genuine Pyramid SDK and web2py3 source tree with dependencies; no installation/download by fixture"]
fn configured_pyramid_web2py_scaffolds_serve_unicode_through_framework_actions_and_retire() {
    assert_eq!(required("RUSTPY_LEGACY_FRAMEWORKS_ADMITTED"), "1");
    assert!(!rustpy_settings::trust_all_from_env());
    let python = admitted_file("RUSTPY_LEGACY_FRAMEWORK_PYTHON");
    let framework = PathBuf::from(required("RUSTPY_WEB2PY_FRAMEWORK"));
    assert!(framework.is_absolute() && framework.join("gluon").is_dir());
    // Bind the entry and WSGI implementation to Root's admitted official source.
    assert_eq!(
        admitted_file("RUSTPY_WEB2PY_ENTRY"),
        framework.join("web2py.py")
    );
    assert_eq!(
        admitted_file("RUSTPY_WEB2PY_MAIN"),
        framework.join("gluon/main.py")
    );
    let sdk = rustpy_project::interpreter::probe_detailed(&python, "configured-legacy-frameworks")
        .expect("genuine SDK probe")
        .interpreter;
    assert_eq!(sdk.path, python);
    for (kind, key, entry, default, port_text) in [
        (
            ProjectKind::Pyramid,
            8,
            "app.py",
            "Hello from Pyramid!",
            "6543",
        ),
        (
            ProjectKind::Web2py,
            9,
            "run_web2py.py",
            "Hello from web2py!",
            "8000",
        ),
    ] {
        let root = tempfile::Builder::new()
            .prefix("legacy-framework-λ-")
            .tempdir()
            .unwrap();
        let options = TemplateOptions {
            project_name: "ownedapp".into(),
            web2py_framework: Some(framework.clone()),
            ..Default::default()
        };
        let scaffold = plan(kind, &options).unwrap();
        scaffold.write_new(root.path()).unwrap();
        // Only choose a private listening port; framework bootstrapping remains
        // the actual generated entry and genuine installed vendor implementation.
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let entry = root.path().join(entry);
        let original = std::fs::read_to_string(&entry).unwrap();
        std::fs::write(&entry, original.replace(port_text, &port.to_string())).unwrap();
        let marker = format!(
            "{default} café λ {}",
            root.path().file_name().unwrap().to_string_lossy()
        );
        let content = if key == 8 {
            entry.clone()
        } else {
            root.path()
                .join("applications/ownedapp/controllers/default.py")
        };
        let original = std::fs::read_to_string(&content).unwrap();
        assert!(original.contains(default));
        std::fs::write(&content, original.replace(default, &marker)).unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
        app.project.as_mut().unwrap().interpreter_rx = None;
        app.project.as_mut().unwrap().interpreter = Some(sdk.clone());
        let mut owned = Owned { app, _root: root };
        assert!(owned.app.project_safe_mode());
        owned.app.framework_launch(key);
        assert!(owned.app.run_panel.sessions.is_empty());
        let ctx = egui::Context::default();
        owned.app.handle_action(Action::TrustProject, &ctx);
        assert!(
            !owned.app.project_safe_mode(),
            "actual Trust Project action must admit this captured root: {:?}",
            owned.app.status
        );
        let id = owned.app.next_session_id;
        drop(reservation);
        owned.app.framework_launch(key);
        let deadline = Instant::now() + Duration::from_secs(30);
        let actual = loop {
            owned.app.poll_run_workflows(&ctx);
            owned.app.run_panel.poll();
            if let Ok(reply) = response(port) {
                if reply.contains(&marker) {
                    break reply;
                }
            }
            if Instant::now() >= deadline {
                let sessions = owned
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .map(|session| (session.id, &session.name, &session.status, session.text()))
                    .collect::<Vec<_>>();
                panic!("genuine server did not serve captured content: status={:?}; sessions={sessions:?}", owned.app.status);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(actual.starts_with("HTTP/1.0 200") || actual.starts_with("HTTP/1.1 200"));
        let cfg = owned.app.run_panel.launcher.launched.get(&id).unwrap();
        assert_eq!(cfg.working_dir.as_deref(), Some(owned._root.path()));
        let materialized = cfg.type_options.as_ref().unwrap().materialize(cfg).unwrap();
        assert!(
            matches!(materialized.kind, RunConfigKind::Python { script } if script.file_name() == entry.file_name())
        );
        let session = owned
            .app
            .run_panel
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap();
        assert!(session.command_line.contains(python.to_str().unwrap()));
        assert!(session.owns_process());
        // A new untrusted project must neither inherit execution admission nor
        // retarget the already captured old server's process/configuration.
        let other = tempfile::tempdir().unwrap();
        let mut replacement = IdeApp::new(Settings::default(), Some(other.path().into()));
        let previous = owned
            .app
            .project
            .replace(replacement.project.take().unwrap());
        owned.app.project_trust_identity = replacement.project_trust_identity.take();
        owned.app.safe_mode = true;
        let count = owned.app.run_panel.sessions.len();
        owned.app.framework_launch(key);
        assert_eq!(owned.app.run_panel.sessions.len(), count);
        assert!(response(port).unwrap().contains(&marker));
        assert_eq!(
            owned.app.run_panel.launcher.launched[&id]
                .working_dir
                .as_deref(),
            Some(owned._root.path())
        );
        let session = owned
            .app
            .run_panel
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .unwrap();
        session.stop();
        while owned
            .app
            .run_panel
            .sessions
            .iter()
            .any(|session| session.owns_process())
        {
            owned.app.run_panel.poll();
            assert!(Instant::now() < deadline, "genuine server failed to retire");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(previous);
        assert_eq!(
            std::fs::read_to_string(&content).unwrap(),
            original.replace(default, &marker)
        );
    }
}

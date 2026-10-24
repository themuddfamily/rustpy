//! Root-admitted genuine typescript-language-server; no generated protocol peer.
use super::*;
use std::time::{Duration, Instant};

struct Owned {
    app: IdeApp,
    context: egui::Context,
    _directory: tempfile::TempDir,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.app.stop_web_languages();
        // Keep the project and client owners until the production supervisor
        // acknowledges retirement, including when an assertion unwinds.
        while self.app.web_languages.is_running() {
            self.app.poll_web_languages(&self.context);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Owned {
    fn until(&mut self, description: &str, ready: impl Fn(&IdeApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.app.poll_web_languages(&self.context);
            if ready(&self.app) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "real LSP did not produce {description}; messages={:?}",
                self.app.web_languages.messages
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "requires explicitly admitted absolute Node and installed genuine typescript-language-server JS entry with TypeScript"]
fn configured_typescript_stdio_completion_diagnostics_checked_edit_undo_and_stop() {
    assert_eq!(std::env::var("RUSTPY_REQUIRE_WEB_LSP").as_deref(), Ok("1"));
    let absolute = |name: &str| {
        let path = PathBuf::from(std::env::var_os(name).expect(name));
        assert!(
            path.is_absolute() && path.is_file(),
            "{name} must name an installed absolute file"
        );
        path
    };
    let node = absolute("RUSTPY_WEB_LSP_NODE");
    let entry = absolute("RUSTPY_WEB_LSP_TYPESCRIPT_ENTRY");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owned.ts");
    let source = "const mismatch: number = 'owned diagnostic';\nconst sample = { ownedMember42: 42 };\nsample.\n";
    std::fs::write(&path, source).unwrap();
    std::fs::write(
        directory.path().join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true,\"noEmit\":true},\"files\":[\"owned.ts\"]}",
    )
    .unwrap();
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    settings.web_development.automatic = false;
    settings.web_development.servers.insert(
        "typescript".into(),
        rustpy_settings::web_development::Server {
            executable: node.to_str().unwrap().into(),
            arguments: vec![entry.to_str().unwrap().into(), "--stdio".into()],
            ..Default::default()
        },
    );
    let mut app = IdeApp::new(settings, Some(directory.path().into()));
    app.open_file(&path);
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(TextSize::new(
            (source.len() - 1) as u32,
        ))]);
    let mut owned = Owned {
        app,
        context: egui::Context::default(),
        _directory: directory,
    };
    assert!(owned.app.web_request(Kind::Completion));
    owned.until(
        "initialized server, opened document and member completion",
        |app| {
            app.web_languages
                .servers
                .values()
                .any(|server| server.client.ready && server.open.contains_key(&path))
                && app
                    .completion
                    .items
                    .iter()
                    .any(|item| item.label == "ownedMember42")
        },
    );
    owned.until("actual TypeScript type diagnostic", |app| {
        app.web_languages
            .servers
            .values()
            .flat_map(|server| server.open.values())
            .flat_map(|open| &open.diagnostics)
            .any(|row| row["code"] == 2322 && row["range"]["start"]["line"] == 0)
            || app.web_languages.reported.iter().any(|row| {
                row.snapshot.path == path && row.message.contains("not assignable to type 'number'")
            })
    });
    let position = owned
        .app
        .completion
        .items
        .iter()
        .position(|item| item.label == "ownedMember42")
        .unwrap();
    owned.app.completion.selected = position;
    let received = owned.app.web_languages.completion.as_ref().unwrap();
    let snapshot = received.snapshot.clone();
    let item = received.items[position].clone();
    assert!(owned.app.accept_web_completion());
    owned.until(
        "checked completion edit (including real resolve response)",
        |app| {
            app.tabs[index]
                .state
                .doc
                .text()
                .contains("sample.ownedMember42")
        },
    );
    assert!(owned.app.tabs[index].state.undo());
    assert_eq!(owned.app.tabs[index].state.doc.text(), source);
    // Replay only the actual received item against its original version. Undo
    // restores bytes, not authority to apply an old language-server response.
    owned.app.web_insert_completion(&snapshot, &item);
    assert_eq!(owned.app.tabs[index].state.doc.text(), source);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    owned.app.stop_web_languages();
    owned.until("physical language-server retirement", |app| {
        !app.web_languages.is_running()
    });
    assert!(owned.app.web_languages.stamps().is_empty());
}

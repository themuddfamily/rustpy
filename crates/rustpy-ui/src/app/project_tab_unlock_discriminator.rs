//! Runs in an owned exact-selector child process: this fixture intentionally owns UNLOCK.
use super::*;
fn input(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        },
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::accesskit::TreeUpdate {
    let mut native = eframe::Frame::_new_kittest();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
            ..Default::default()
        },
        |ui| eframe::App::ui(app, ui, &mut native),
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn named(update: &egui::accesskit::TreeUpdate, ctx: &egui::Context) -> bool {
    update.nodes.iter().any(|(id, node)| {
        node.role() == egui::accesskit::Role::Tab
            && node.label().is_some_and(|s| s.contains("π.py"))
            && ctx
                .memory(|m| m.focused())
                .is_some_and(|focus| focus.accesskit_id() == *id)
    })
}
#[test]
fn coordinated_foreign_unlock_waiter_reproduces_real_project_tab_focus_trap() {
    if crate::netsec::isolated_unlock_test("app::project_tab_unlock_discriminator::coordinated_foreign_unlock_waiter_reproduces_real_project_tab_focus_trap") { return; }
    struct UnlockOwner(Option<std::thread::JoinHandle<bool>>);
    impl Drop for UnlockOwner {
        fn drop(&mut self) {
            // Panic paths cannot leave a process-global waiting request active.
            crate::netsec::unlock_answered();
            crate::netsec::clear_unlock_handler();
            if let Some(worker) = self.0.take() {
                let _ = worker.join();
            }
        }
    }
    for waiting in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("π.py");
        std::fs::write(&path, "π = 1\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        frame(
            &mut app,
            &ctx,
            0.2,
            input(
                egui::Key::Num1,
                egui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
        );
        frame(&mut app, &ctx, 0.3, vec![]);
        assert!(ProjectTree::has_focus(&ctx));
        let mut update = frame(
            &mut app,
            &ctx,
            0.4,
            input(egui::Key::Tab, egui::Modifiers::NONE),
        );
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(egui::Id::new("project_panel").with("__resize"))
        );
        let identity = app.tabs[app.active.unwrap()].state.doc.identity();
        let version = app.tabs[app.active.unwrap()].state.doc.version();
        let mut owner = UnlockOwner(None);
        if waiting {
            let (sender, receiver) = std::sync::mpsc::channel();
            crate::netsec::set_unlock_handler(move || {
                let _ = sender.send(());
            });
            owner.0 = Some(std::thread::spawn(|| {
                crate::netsec::wait_for_unlock(std::time::Duration::from_secs(30))
            }));
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(crate::netsec::unlock_wanted());
            update = frame(
                &mut app,
                &ctx,
                0.5,
                input(egui::Key::Tab, egui::Modifiers::NONE),
            );
            // Real provider and real focus, not a forced memory owner or Dialog variant.
            assert!(update
                .nodes
                .iter()
                .any(|(_, n)| n.role() == egui::accesskit::Role::PasswordInput));
            assert!(update
                .nodes
                .iter()
                .any(|(_, n)| n.role() == egui::accesskit::Role::Button
                    && n.label() == Some("Not now")));
            assert!(matches!(app.dialog, Dialog::None));
            crate::netsec::unlock_answered();
            assert!(owner.0.take().unwrap().join().unwrap());
            assert!(!crate::netsec::unlock_wanted());
            // The foreign request has retired; the per-App prompt still owns traversal.
        }
        let mut reached = named(&update, &ctx);
        let mut time = if waiting { 0.5 } else { 0.4 };
        for _ in 0..20 {
            if reached {
                break;
            }
            time += 0.1;
            update = frame(
                &mut app,
                &ctx,
                time,
                input(egui::Key::Tab, egui::Modifiers::NONE),
            );
            reached = named(&update, &ctx);
        }
        assert_eq!(
            reached,
            !waiting,
            "waiting={waiting} focus={:?}",
            ctx.memory(|m| m.focused())
        );
        if waiting {
            assert!(update
                .nodes
                .iter()
                .any(|(_, n)| n.role() == egui::accesskit::Role::PasswordInput));
        }
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.identity(), identity);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.version(), version);
        assert_eq!(std::fs::read(&path).unwrap(), "π = 1\n".as_bytes());
        assert!(owner.0.is_none());
        crate::netsec::clear_unlock_handler();
    }
}

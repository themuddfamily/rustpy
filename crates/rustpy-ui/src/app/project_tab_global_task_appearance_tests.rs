use super::*;

#[test]
#[ignore = "requires isolated global task inventory; observational V7 discriminator"]
fn actual_global_task_appearance_during_project_tab_traversal() {
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
    fn paint(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut native_frame = eframe::Frame::_new_kittest();
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
                ..Default::default()
            },
            |ui| eframe::App::ui(app, ui, &mut native_frame),
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }
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
    let secondary_directory = tempfile::tempdir().unwrap();
    let secondary_path = secondary_directory.path().join("ω.py");
    std::fs::write(&secondary_path, "ω = 2\n").unwrap();
    let mut secondary_settings = Settings::default();
    secondary_settings.trust_project(secondary_directory.path());
    let mut secondary_app =
        IdeApp::new(secondary_settings, Some(secondary_directory.path().into()));
    secondary_app.open_file(&secondary_path);
    let secondary_ctx = egui::Context::default();
    crate::theme::install_fonts(&secondary_ctx);
    secondary_ctx.enable_accesskit();
    let secondary_identity = secondary_app.tabs[secondary_app.active.unwrap()]
        .state
        .doc
        .identity();
    let secondary_version = secondary_app.tabs[secondary_app.active.unwrap()]
        .state
        .doc
        .version();
    // Each Context receives its own real frame and key events. Never request
    // focus or assign memory: the original primary traversal remains intact.
    let mut frame = |app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>| {
        let update = paint(app, ctx, time, events.clone());
        let primary_focus = ctx.memory(|memory| memory.focused());
        let primary_queue = app.focus_editor;
        let secondary_update = paint(&mut secondary_app, &secondary_ctx, time, events);
        let after_focus = ctx.memory(|memory| memory.focused());
        if after_focus != primary_focus || app.focus_editor != primary_queue {
            eprintln!(
                "INTERLEAVED_CONTEXT_FAILURE time={time} primary_before={primary_focus:?} primary_after={after_focus:?} secondary_focus={:?} primary_accesskit_focus={:?} secondary_accesskit_focus={:?}",
                secondary_ctx.memory(|memory| memory.focused()),
                update.focus,
                secondary_update.focus,
            );
        }
        assert_eq!(
            after_focus, primary_focus,
            "secondary paint preserves primary Context focus"
        );
        assert_eq!(
            app.focus_editor, primary_queue,
            "secondary paint preserves primary App focus queue"
        );
        update
    };
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, vec![]);
    let identity = app.tabs[app.active.unwrap()].state.doc.identity();
    let version = app.tabs[app.active.unwrap()].state.doc.version();
    frame(
        &mut app,
        &ctx,
        0.2,
        key(
            egui::Key::Num1,
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
        ),
    );
    frame(&mut app, &ctx, 0.3, vec![]);
    assert!(
        ProjectTree::has_focus(&ctx),
        "real Alt+1 activates the actual Project tree"
    );
    let tree_selection = app.project.as_ref().unwrap().tree.selected.clone();
    struct OwnedTask {
        release: Option<std::sync::mpsc::Sender<()>>,
        handle: Option<rustpy_core::tasks::TaskHandle<()>>,
    }
    impl OwnedTask {
        fn finish(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if let Some(mut handle) = self.handle.take() {
                assert!(
                    handle
                        .wait_timeout(std::time::Duration::from_secs(5))
                        .is_some(),
                    "owned task must finish"
                );
            }
        }
    }
    impl Drop for OwnedTask {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if let Some(mut handle) = self.handle.take() {
                let _ = handle.wait_timeout(std::time::Duration::from_secs(5));
            }
        }
    }
    let manager = rustpy_core::tasks::TaskManager::global();
    let (release, receive) = std::sync::mpsc::channel();
    let handle = manager
        .task("owned V7 traversal inventory discriminator")
        .spawn(move |_| {
            let _ = receive.recv();
            Ok(())
        });
    let owned_id = handle.id();
    let mut owned = OwnedTask {
        release: Some(release),
        handle: Some(handle),
    };
    let mut time = 0.4;
    let mut update = frame(
        &mut app,
        &ctx,
        time,
        key(egui::Key::Tab, egui::Modifiers::NONE),
    );
    assert!(
        !ProjectTree::has_focus(&ctx),
        "ordinary Tab must leave the tree focus owner"
    );
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(egui::Id::new("project_panel").with("__resize")),
        "the first Tab reaches the actual Project resize response"
    );
    assert!(manager.is_running(owned_id));
    assert!(!crate::progress_ui::shown_tasks(&manager.running()).iter().any(|task| task.id == owned_id), "setup requires the real owned task to remain younger than SHOW_AFTER through resize; a slow setup is not evidence");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::progress_ui::shown_tasks(&manager.running())
        .iter()
        .any(|task| task.id == owned_id)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "real SHOW_AFTER visibility admission"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        crate::progress_ui::shown_tasks(&manager.running())
            .first()
            .unwrap()
            .id,
        owned_id,
        "owned task must be the actual newest visible inventory; unrelated tasks are untouched"
    );
    eprintln!(
        "GLOBAL_TASK_APPEARANCE admitted owned={owned_id:?} focused={:?} tasks={:?}",
        ctx.memory(|m| m.focused()),
        manager.running()
    );
    let named_tab_focused = |update: &egui::accesskit::TreeUpdate| {
        update.nodes.iter().any(|(id, node)| {
            node.role() == egui::accesskit::Role::Tab
                && node.label().is_some_and(|label| label.contains("π.py"))
                && ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        })
    };
    let snapshot = |update: &egui::accesskit::TreeUpdate, time: f64, app: &IdeApp| {
        let focus = ctx.memory(|memory| memory.focused());
        let focused_node = update
            .nodes
            .iter()
            .find(|(id, _)| focus.is_some_and(|focus| focus.accesskit_id() == *id));
        let named_tabs: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Tab
                    && node.label().is_some_and(|label| label.contains("π.py"))
            })
            .map(|(id, node)| (*id, node.label().map(str::to_owned)))
            .collect();
        format!(
            "time={time} focus={focus:?} focused_node={:?} named_tabs={named_tabs:?} active_view={:?} queued_editor={} keyboard={} tree_focus={} dialog_none={} spacing={:?} interact_size={:?}",
            focused_node.map(|(id, node)| (*id, node.role(), node.label().map(str::to_owned))),
            app.active
                .and_then(|index| app.tabs.get(index))
                .map(|tab| tab.view.id),
            app.focus_editor,
            app.editor_keyboard(),
            ProjectTree::has_focus(&ctx),
            matches!(app.dialog, Dialog::None),
            ctx.style_of(ctx.theme()).spacing.item_spacing,
            ctx.style_of(ctx.theme()).spacing.interact_size
        )
    };
    let mut focus_snapshots = vec![snapshot(&update, time, &app)];
    for ordinal in 0..20 {
        if named_tab_focused(&update) {
            break;
        }
        time += 0.1;
        update = frame(
            &mut app,
            &ctx,
            time,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        focus_snapshots.push(snapshot(&update, time, &app));
        if ordinal == 0 {
            assert_eq!(
                crate::progress_ui::shown_tasks(&manager.running())
                    .first()
                    .unwrap()
                    .id,
                owned_id,
                "first traversal frame retains admitted actual inventory owner"
            );
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::Button
                        && node.label() == Some("×")),
                "first Tab frame paints actual newly visible task cancel Button"
            );
        }
        eprintln!(
            "GLOBAL_TASK_APPEARANCE ordinal={ordinal} tasks={:?} {}",
            manager.running(),
            focus_snapshots.last().unwrap()
        );
    }
    if !named_tab_focused(&update) {
        for (ordinal, snapshot) in focus_snapshots.iter().enumerate() {
            eprintln!("PROJECT_TAB_FRAME ordinal={ordinal} {snapshot}");
        }
        eprintln!(
            "PROJECT_TAB_FAILURE time={time} focus={:?} accesskit_focus={:?} queued_editor={} editor_keyboard={} active={:?} tree_focus={} dialog_none={}",
            ctx.memory(|memory| memory.focused()),
            update.focus,
            app.focus_editor,
            app.editor_keyboard(),
            app.active,
            ProjectTree::has_focus(&ctx),
            matches!(app.dialog, Dialog::None),
        );
        for (id, node) in &update.nodes {
            eprintln!(
                "PROJECT_TAB_NODE id={id:?} role={:?} label={:?} value={:?}",
                node.role(),
                node.label(),
                node.value()
            );
        }
    }
    assert!(
        named_tab_focused(&update),
        "bounded real Tab traversal reaches the actual named editor tab"
    );
    owned.finish();
    frame(
        &mut app,
        &ctx,
        time + 0.1,
        key(
            egui::Key::F10,
            egui::Modifiers {
                ctrl: true,
                command: true,
                alt: true,
                ..Default::default()
            },
        ),
    );
    let menu = frame(&mut app, &ctx, time + 0.2, vec![]);
    let labels: Vec<_> = menu
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label())
        .collect();
    assert!(
        labels
            .iter()
            .any(|label| label.contains("Close Other Tabs")),
        "actual Tab menu opens: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("New Directory")),
        "the Project menu must not retain the chord: {labels:?}"
    );
    assert_eq!(app.project.as_ref().unwrap().tree.selected, tree_selection);
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.identity(), identity);
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.version(), version);
    assert!(!app.tabs[app.active.unwrap()].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "π = 1\n");
    assert_eq!(
        secondary_app.tabs[secondary_app.active.unwrap()]
            .state
            .doc
            .identity(),
        secondary_identity
    );
    assert_eq!(
        secondary_app.tabs[secondary_app.active.unwrap()]
            .state
            .doc
            .version(),
        secondary_version
    );
    assert!(!secondary_app.tabs[secondary_app.active.unwrap()]
        .state
        .doc
        .can_undo());
    assert_eq!(std::fs::read_to_string(&secondary_path).unwrap(), "ω = 2\n");
}

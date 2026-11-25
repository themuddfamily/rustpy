use super::*;

#[test]
fn actual_two_apps_interleave_project_tab_traversal_without_cross_context_focus() {
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
    #[derive(Debug)]
    struct AltPassProbe {
        time: f64,
        frame: u64,
        pass: u64,
        num1_pressed_events: usize,
        before_fonts: Option<(u64, [f32; 4])>,
        after_fonts: Option<(u64, [f32; 4])>,
        before: (Option<egui::Id>, bool, bool, bool, bool),
        after: (Option<egui::Id>, bool, bool, bool, bool),
    }
    fn font_identity(ctx: &egui::Context) -> (u64, [f32; 4]) {
        use std::hash::{Hash, Hasher};
        let identity = ctx.fonts(|fonts| {
            let defs = fonts.definitions();
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            defs.families.hash(&mut hash);
            for (name, data) in &defs.font_data {
                name.hash(&mut hash);
                (std::sync::Arc::as_ptr(data) as usize).hash(&mut hash);
                data.font.len().hash(&mut hash);
                data.index.hash(&mut hash);
                data.tweak.scale.to_bits().hash(&mut hash);
                data.tweak.y_offset_factor.to_bits().hash(&mut hash);
                data.tweak.y_offset.to_bits().hash(&mut hash);
            }
            hash.finish()
        });
        let style = ctx.style_of(ctx.theme());
        let sizes = [
            egui::TextStyle::Body,
            egui::TextStyle::Button,
            egui::TextStyle::Small,
            egui::TextStyle::Monospace,
        ]
        .map(|style_name| {
            style
                .text_styles
                .get(&style_name)
                .map(|font| font.size)
                .unwrap_or(0.0)
        });
        (identity, sizes)
    }
    fn paint(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (egui::accesskit::TreeUpdate, Vec<AltPassProbe>) {
        let mut native_frame = eframe::Frame::_new_kittest();
        let tracing = time == 0.2 || time == 0.3;
        let mut passes = Vec::new();
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
                ..Default::default()
            },
            |ui| {
                let before_fonts = tracing.then(|| font_identity(ctx));
                let before = (
                    ctx.memory(|m| m.focused()),
                    app.layout.left_visible,
                    app.left_tool == LeftTool::Project,
                    app.focus_editor,
                    app.editor_keyboard(),
                );
                let num1_pressed_events = if tracing {
                    ui.input(|input| {
                        input
                            .events
                            .iter()
                            .filter(|event| {
                                matches!(
                                    event,
                                    egui::Event::Key {
                                        key: egui::Key::Num1,
                                        pressed: true,
                                        ..
                                    }
                                )
                            })
                            .count()
                    })
                } else {
                    0
                };
                eframe::App::ui(app, ui, &mut native_frame);
                if tracing {
                    passes.push(AltPassProbe {
                        time,
                        frame: ctx.cumulative_frame_nr(),
                        pass: ctx.cumulative_pass_nr(),
                        num1_pressed_events,
                        before_fonts,
                        after_fonts: Some(font_identity(ctx)),
                        before,
                        after: (
                            ctx.memory(|m| m.focused()),
                            app.layout.left_visible,
                            app.left_tool == LeftTool::Project,
                            app.focus_editor,
                            app.editor_keyboard(),
                        ),
                    });
                }
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        (update, passes)
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
    let alt_passes = std::cell::RefCell::new(Vec::new());
    let mut frame = |app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>| {
        let (update, primary_passes) = paint(app, ctx, time, events.clone());
        alt_passes
            .borrow_mut()
            .extend(primary_passes.into_iter().map(|pass| ("primary", pass)));
        let primary_focus = ctx.memory(|memory| memory.focused());
        let primary_queue = app.focus_editor;
        let (secondary_update, secondary_passes) =
            paint(&mut secondary_app, &secondary_ctx, time, events);
        alt_passes
            .borrow_mut()
            .extend(secondary_passes.into_iter().map(|pass| ("secondary", pass)));
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
    let alt_update = frame(&mut app, &ctx, 0.3, vec![]);
    if !ProjectTree::has_focus(&ctx) {
        eprintln!("PROJECT_ALT1_STAGE fields=time,frame,pass,num1_pressed_events,before/after(focus,left_visible,left_project,queued_editor,keyboard)");
        for (owner, pass) in alt_passes.borrow().iter() {
            eprintln!("PROJECT_ALT1_PASS owner={owner} time={} frame={} pass={} num1={} before={:?} after={:?}",
                pass.time,pass.frame,pass.pass,pass.num1_pressed_events,pass.before,pass.after);
            eprintln!(
                "PROJECT_ALT1_FONT_IDENTITY owner={owner} before={:?} after={:?}",
                pass.before_fonts, pass.after_fonts
            );
        }
        let focus = ctx.memory(|m| m.focused());
        for id in [
            focus,
            Some(ProjectTree::focus_id()),
            Some(egui::Id::new("project_panel").with("__resize")),
        ] {
            eprintln!(
                "PROJECT_ALT1_RESPONSE id={id:?} response={:?}",
                id.and_then(|id| ctx.read_response(id)).map(|r| (
                    r.id,
                    r.rect,
                    r.interact_rect,
                    r.sense,
                    r.enabled(),
                    r.has_focus()
                ))
            );
        }
        for (id, node) in &alt_update.nodes {
            if focus.is_some_and(|focus| focus.accesskit_id() == *id)
                || node.role() == egui::accesskit::Role::Tab
            {
                eprintln!(
                    "PROJECT_ALT1_NODE id={id:?} role={:?} label={:?} bounds={:?}",
                    node.role(),
                    node.label(),
                    node.bounds()
                );
            }
        }
    }
    assert!(
        ProjectTree::has_focus(&ctx),
        "real Alt+1 activates the actual Project tree"
    );
    let tree_selection = app.project.as_ref().unwrap().tree.selected.clone();
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
    for _ in 0..20 {
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

//! Connected rendering and ownership fixtures; execution is deferred.
use super::*;

fn fixture() -> IdeApp {
    let mut app = IdeApp::new(Settings::default(), None);
    let root = PathBuf::from("/rustpy-tool-framework-fixture");
    app.project = Some(Project {
        tree: ProjectTree::new(root.clone()),
        analysis: Arc::new(Analysis::for_project(&root)),
        run_configs: RunConfigStore::default(),
        root,
        interpreters: Vec::new(),
        interpreter: None,
        interpreter_rx: None,
        git: None,
        git_refreshed: -100.0,
        git_branch: None,
    });
    app.layout.left_visible = false;
    app.bottom = Some(BottomTool::Problems);
    let todo = tool_ref_id(ToolRef::Bottom(BottomTool::Todo));
    app.layout.tools.saved.open.push(todo.clone());
    app.layout.tools.saved.placement_mut(&todo).slot = Slot::BottomRight;
    app.focus_editor = false;
    app
}
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            focused: true,
            ..Default::default()
        },
        |ui| {
            app.layout_frame_start(ui.ctx());
            app.tool_windows_docks(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label("Editor remains independently available");
            });
            app.layout_frame_end(ui.ctx());
        },
    )
    .drop_without_applying_deltas();
}
#[test]
fn actual_bottom_split_and_move_use_distinct_live_content_rectangles() {
    let mut app = fixture();
    let ctx = context();
    app.layout.tools.saved.split_ratios[3] = 0.4;
    frame(&mut app, &ctx, vec![]);
    let problems = tool_ref_id(ToolRef::Bottom(BottomTool::Problems));
    let todo = tool_ref_id(ToolRef::Bottom(BottomTool::Todo));
    let a = app.layout.tools.rects[&problems].1;
    let b = app.layout.tools.rects[&todo].1;
    assert!(a.width() > 150.0 && b.width() > 150.0);
    assert!(
        a.right() < b.left(),
        "the two actual contents must not overlap"
    );
    app.layout.tools.saved.placement_mut(&todo).slot = Slot::RightTop;
    frame(&mut app, &ctx, vec![]);
    let moved = app.layout.tools.rects[&todo].1;
    assert!(
        moved.left() > 800.0,
        "Move to Right Top changes real dock geometry"
    );
    assert_eq!(
        app.bottom,
        Some(BottomTool::Problems),
        "rendering another tool preserves primary content authority"
    );
}
#[test]
fn unpinned_click_outside_hides_only_that_tool_and_hide_all_restores_splits() {
    let mut app = fixture();
    let ctx = context();
    let problems = tool_ref_id(ToolRef::Bottom(BottomTool::Problems));
    let todo = tool_ref_id(ToolRef::Bottom(BottomTool::Todo));
    app.layout.tools.saved.placement_mut(&problems).mode = Mode::DockUnpinned;
    frame(&mut app, &ctx, vec![]);
    frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(egui::pos2(20.0, 20.0)),
            egui::Event::PointerButton {
                pos: egui::pos2(20.0, 20.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert!(!app.framework_visible().contains(&problems));
    assert!(
        app.framework_visible().contains(&todo),
        "pinned neighbor remains open"
    );
    app.bottom = Some(BottomTool::Problems);
    let saved = app.tool_window_layout();
    app.hide_all_tool_windows();
    assert!(app.framework_visible().is_empty());
    assert!(!app.editor_behind_tool_window());
    app.hide_all_tool_windows();
    assert_eq!(app.tool_window_layout(), saved);
}
#[test]
fn contextual_arrows_stretch_real_geometry_and_preserve_editor_shortcut_dispatch() {
    let mut app = fixture();
    let ctx = context();
    frame(&mut app, &ctx, vec![]);
    let id = tool_ref_id(ToolRef::Bottom(BottomTool::Problems));
    app.layout.tools.focused = Some(id);
    let ratio = app.layout.tools.saved.split_ratios[3];
    assert!(app.tool_window_action(Action::MoveElementLeft, &ctx));
    assert!(app.layout.tools.saved.split_ratios[3] < ratio);
    let height = app.layout.bottom_height;
    assert!(app.tool_window_action(Action::VcsShowPrevChangeMarker, &ctx));
    assert_eq!(app.layout.bottom_height, height + 40.0);
    let state = egui::PanelState::load(&ctx, egui::Id::new("bottom_tools")).unwrap();
    assert_eq!(state.outer_rect.height(), height + 40.0);
    app.layout.tools.focused = None;
    assert!(
        !app.tool_window_action(Action::MoveElementLeft, &ctx),
        "editor action remains available without an actual tool owner"
    );
}
#[test]
fn named_state_round_trip_preserves_native_modes_metadata_and_right_tools() {
    let mut app = fixture();
    app.ai_panel.visible = true;
    let run = tool_ref_id(ToolRef::Bottom(BottomTool::Run));
    app.open_framework_tool(&run);
    let placement = app.layout.tools.saved.placement_mut(&run);
    placement.mode = Mode::Window;
    placement.sidebar = false;
    placement.pinned = true;
    placement.title = "Server output".into();
    placement.color = Some([12, 34, 56]);
    app.layout
        .tools
        .saved
        .window_bounds
        .insert(run.clone(), [100.0, 200.0, 900.0, 500.0]);
    app.layout.tools.saved.window_maximized.insert(run.clone());
    let saved = app.tool_window_layout();
    let json = serde_json::to_string(&saved).unwrap();
    let mut restored = fixture();
    restored.restore_tool_window_layout(&serde_json::from_str(&json).unwrap());
    assert!(restored.ai_panel.visible);
    assert_eq!(
        restored.layout.tools.saved.placement(&run).mode,
        Mode::Window
    );
    assert!(!restored.tool_sidebar_visible(&run));
    assert_eq!(
        restored.layout.tools.saved.window_bounds[&run],
        [100.0, 200.0, 900.0, 500.0]
    );
    assert_eq!(restored.tool_display_title(&run), "Server output");
    restored.reveal_tool_window_content(ToolRef::Bottom(BottomTool::Run));
    assert_eq!(restored.layout.tools.saved.selected[&Slot::BottomLeft], run);
}

#[test]
fn global_modal_hides_native_hosts_and_cancel_restores_levels_without_closing_tools() {
    let mut app = fixture();
    let float = tool_ref_id(ToolRef::Bottom(BottomTool::Todo));
    let window = tool_ref_id(ToolRef::Bottom(BottomTool::Problems));
    app.layout.tools.saved.placement_mut(&float).mode = Mode::Float;
    app.layout.tools.saved.placement_mut(&window).mode = Mode::Window;
    let saved = serde_json::to_value(&app.layout.tools.saved).unwrap();
    let ctx = context();
    ctx.set_embed_viewports(false);
    let observe = |app: &mut IdeApp| {
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.sync_native_tool_modality(ui.ctx());
        });
        let commands = output
            .viewport_output
            .iter()
            .map(|(id, viewport)| (*id, (viewport.commands.clone(), viewport.builder.clone())))
            .collect::<std::collections::HashMap<_, _>>();
        output.drop_without_applying_deltas();
        commands
    };
    let commands = |output: &std::collections::HashMap<
        egui::ViewportId,
        (Vec<egui::ViewportCommand>, egui::ViewportBuilder),
    >,
                    id: &str| {
        output[&egui::ViewportId::from_hash_of(("native-tool", id))]
            .0
            .clone()
    };
    // A modal opened after the normal tool frame still suppresses both real
    // native-host destinations; no close or logical placement edit is emitted.
    app.layout.exit_prompt = Some(layout::ExitPrompt {
        processes: vec!["Web Preview".into()],
        process_count: 1,
        dont_ask: false,
        cancel_id: None,
    });
    let hidden = observe(&mut app);
    for id in [&float, &window] {
        assert_eq!(
            hidden[&egui::ViewportId::from_hash_of(("native-tool", id))]
                .1
                .visible,
            Some(false)
        );
        let commands = commands(&hidden, id);
        assert!(commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Visible(false))));
        assert!(commands.iter().any(|c| matches!(
            c,
            egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)
        )));
        assert!(!commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Close)));
    }
    assert_eq!(
        serde_json::to_value(&app.layout.tools.saved).unwrap(),
        saved
    );
    app.layout.exit_prompt = None; // actual Cancel leaves the owner configuration intact
    let restored = observe(&mut app);
    for (id, level) in [
        (&float, egui::WindowLevel::AlwaysOnTop),
        (&window, egui::WindowLevel::Normal),
    ] {
        assert_eq!(
            restored[&egui::ViewportId::from_hash_of(("native-tool", id))]
                .1
                .visible,
            Some(true)
        );
        let commands = commands(&restored, id);
        assert!(commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Visible(true))));
        assert!(commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::WindowLevel(actual) if *actual == level)));
    }
    assert_eq!(
        serde_json::to_value(&app.layout.tools.saved).unwrap(),
        saved
    );
    // Coordinated quit returns before normal tool rendering. Its frame-start
    // synchronization must therefore also hide retained native hosts.
    app.coordinated_quit_committed = true;
    let quitting = observe(&mut app);
    assert!(commands(&quitting, &float)
        .iter()
        .any(|c| matches!(c, egui::ViewportCommand::Visible(false))));
    app.coordinated_quit_committed = false;
    app.hide_framework_tool(&float);
    let after_close = observe(&mut app);
    assert!(!after_close
        .get(&egui::ViewportId::from_hash_of(("native-tool", &float)))
        .is_some_and(|(commands, _)| commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Visible(true)))));
}
#[test]
fn layout_actions_are_find_action_entries_and_keymap_assignable() {
    let layout_actions = [
        (Action::StoreNewLayout, "StoreNewLayout"),
        (Action::StoreDefaultLayout, "StoreDefaultLayout"),
        (Action::RestoreDefaultLayout, "RestoreDefaultLayout"),
        (
            Action::RestoreFactoryDefaultLayout,
            "RestoreFactoryDefaultLayout",
        ),
        (Action::RestoreNamedLayout, "RestoreNamedLayout"),
        (Action::DeleteNamedLayout, "DeleteNamedLayout"),
    ];
    for (action, id) in layout_actions {
        // Find Action / Search Everywhere › Actions lists `Action::ALL` by text.
        assert!(Action::ALL.contains(&action));
        assert!(action.english_text().to_lowercase().contains("layout"));
        assert_eq!(Keymap::action_by_id(id), Some(action));
    }
    let mut keymap = Keymap::default_keymap();
    let chord = crate::actions::KeyChord::parse("Ctrl+Alt+Shift+L").unwrap();
    keymap.set_shortcut(Action::RestoreNamedLayout, chord);
    assert_eq!(
        keymap.shortcut_text(Action::RestoreNamedLayout),
        "Ctrl+Alt+Shift+L"
    );
}
#[test]
fn layout_actions_save_restore_and_delete_named_layouts() {
    let mut app = fixture();
    let ctx = context();
    let problems = tool_ref_id(ToolRef::Bottom(BottomTool::Problems));
    // Save Current Layout As… asks for the name, then stores the current layout.
    app.handle_action(Action::StoreNewLayout, &ctx);
    assert!(matches!(app.dialog, Dialog::SaveLayout(ref name) if name.is_empty()));
    app.dialog = Dialog::None;
    app.save_named_tool_window_layout("  Review  ");
    let review = app.tool_window_layout();
    assert_eq!(
        app.settings.tool_window_layouts.named.get("Review"),
        Some(&review)
    );
    // Store Current Layout as Default / Restore Default Layout.
    app.handle_action(Action::StoreDefaultLayout, &ctx);
    assert_eq!(
        app.settings.tool_window_layouts.default_layout,
        Some(review.clone())
    );
    app.hide_all_tool_windows();
    assert!(!app.framework_visible().contains(&problems));
    app.handle_action(Action::RestoreDefaultLayout, &ctx);
    assert_eq!(app.tool_window_layout(), review);
    // Restore Layout… opens a chooser of the named layouts; picking one restores it.
    app.handle_action(Action::RestoreFactoryDefaultLayout, &ctx);
    assert_ne!(app.tool_window_layout(), review);
    app.handle_action(Action::RestoreNamedLayout, &ctx);
    let choice = app.choice.as_ref().expect("Restore Layout chooser");
    assert!(choice.kind == ChoiceKind::RestoreLayout);
    assert_eq!(choice.items, vec![("Review".to_string(), String::new())]);
    app.apply_choice(0);
    assert!(app.choice.is_none());
    assert_eq!(app.tool_window_layout(), review);
    // Delete Layout… removes the picked layout; with none left there is no chooser.
    app.handle_action(Action::DeleteNamedLayout, &ctx);
    assert!(app.choice.as_ref().unwrap().kind == ChoiceKind::DeleteLayout);
    app.apply_choice(0);
    assert!(app.settings.tool_window_layouts.named.is_empty());
    app.handle_action(Action::RestoreNamedLayout, &ctx);
    assert!(app.choice.is_none());
}

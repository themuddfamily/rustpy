use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
fn fixture(trusted: bool, blocking: bool) -> (tempfile::TempDir, IdeApp, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let libraries = root.path().join("owned-library");
    std::fs::create_dir(&libraries).unwrap();
    let module = libraries.join("native.so");
    std::fs::write(
        &module,
        b"owned fake native image; never imported successfully",
    )
    .unwrap();
    let marker = root.path().join("interpreter-invoked");
    let interpreter = root.path().join("owned-interpreter");
    let tail = if blocking {
        "while :; do :; done\n"
    } else {
        "exit 1\n"
    };
    std::fs::write(
        &interpreter,
        format!("#!/bin/sh\nprintf invoked > '{}'\n{tail}", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut settings = Settings::default();
    if trusted {
        settings.trust_project(root.path());
    }
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    let project = app.project.as_mut().unwrap();
    // Controlled target-interpreter configuration, not runtime skeleton injection.
    project.interpreter_rx = None;
    project.interpreter = Some(Interpreter {
        path: interpreter,
        version: "3.12.3".into(),
        prefix: libraries.clone(),
        sys_path: vec![libraries],
        is_venv: false,
        platform: "linux".into(),
        kind: "owned fixture".into(),
    });
    project.analysis = Arc::new(Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![root.path().into()],
        vec![project.interpreter.as_ref().unwrap().sys_path[0].clone()],
        (3, 12),
    )));
    project.tree.selected = Some(module);
    (root, app, marker)
}
fn drain(app: &mut IdeApp) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let ctx = egui::Context::default();
    while app.native_skeletons_pending() {
        app.poll_native_skeletons(&ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "Actual managed worker must acknowledge cancellation and retire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(unix)]
#[test]
fn actual_generate_action_in_safe_mode_never_executes_selected_interpreter() {
    let (_root, mut app, marker) = fixture(false, false);
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    assert!(!app.native_skeletons_pending());
    assert!(!marker.exists());
    assert!(app.native_skeleton_ui.records.is_empty());
}

#[cfg(unix)]
#[test]
fn actual_generate_action_owns_failed_target_and_does_not_publish_partial_stub() {
    let (_root, mut app, marker) = fixture(true, false);
    let analysis = app.project.as_ref().unwrap().analysis.clone();
    assert!(analysis
        .module("native")
        .unwrap()
        .text
        .contains("__getattr__"));
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    assert!(
        app.native_skeletons_pending(),
        "Managed result ACK remains owned even after fast failure"
    );
    drain(&mut app);
    assert!(
        marker.exists(),
        "The actual configured interpreter was invoked"
    );
    assert!(app.native_skeleton_ui.records.is_empty());
    assert!(analysis
        .module("native")
        .unwrap()
        .text
        .contains("__getattr__"));
}

#[cfg(unix)]
#[test]
fn real_selection_change_cancels_exact_owned_task_and_waits_for_retirement() {
    let (_root, mut app, marker) = fixture(true, true);
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    let original_id = app.native_skeleton_ui.active.as_ref().unwrap().handle.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "Owned child must actually start"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    app.project.as_mut().unwrap().tree.selected = None;
    app.poll_native_skeletons(&egui::Context::default());
    assert!(app.native_skeleton_ui.active.is_none());
    if let Some(job) = app.native_skeleton_ui.draining.first() {
        assert_eq!(job.handle.id(), original_id);
        assert!(job.handle.is_cancelled());
    }
    drain(&mut app);
    assert!(app.native_skeleton_ui.draining.is_empty());
    assert!(app.native_skeleton_ui.records.is_empty());
}

#[cfg(unix)]
#[test]
fn actual_quit_cancel_retains_uncancelled_task_then_project_close_drains_it() {
    let (_root, mut app, marker) = fixture(true, true);
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    let original_id = app.native_skeleton_ui.active.as_ref().unwrap().handle.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(matches!(
        app.prepare_managed_coordinated_quit("native-stub-owner"),
        window_lifecycle::QuitProgress::PendingProcesses { .. }
    ));
    assert!(matches!(
        app.answer_coordinated_quit("native-stub-owner", window_lifecycle::QuitAnswer::Cancel),
        window_lifecycle::QuitProgress::Rejected { .. }
    ));
    app.poll_native_skeletons(&egui::Context::default());
    let retained = app.native_skeleton_ui.active.as_ref().unwrap();
    assert_eq!(retained.handle.id(), original_id);
    assert!(!retained.handle.is_cancelled());
    assert_eq!(
        app.abort_coordinated_quit("native-stub-owner"),
        window_lifecycle::QuitProgress::Aborted
    );
    app.cancel_project_tasks();
    drain(&mut app);
    assert!(app.native_skeleton_ui.active.is_none());
    assert!(app.native_skeleton_ui.records.is_empty());
}

#[cfg(unix)]
#[test]
fn rendered_project_native_context_button_routes_the_exact_selected_library() {
    fn paint(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (egui::accesskit::TreeUpdate, Option<egui::Pos2>) {
        let mut native = eframe::Frame::_new_kittest();
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1300.0, 850.0),
                )),
                ..Default::default()
            },
            |ui| eframe::App::ui(app, ui, &mut native),
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        let native_point = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "native.so" => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        });
        output.drop_without_applying_deltas();
        (update, native_point)
    }
    fn click(point: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: point,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    let (root, mut app, marker) = fixture(true, false);
    app.project
        .as_mut()
        .unwrap()
        .tree
        .expanded
        .insert(root.path().into());
    app.project
        .as_mut()
        .unwrap()
        .tree
        .expanded
        .insert(root.path().join("owned-library"));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    paint(&mut app, &ctx, 0.0, vec![]);
    let (_, point) = paint(&mut app, &ctx, 0.1, vec![]);
    let point = point.expect("The actual painted native library filename must be present");
    paint(
        &mut app,
        &ctx,
        0.2,
        click(point, egui::PointerButton::Secondary),
    );
    let (menu, _) = paint(&mut app, &ctx, 0.3, vec![]);
    let button = menu
        .nodes
        .iter()
        .find_map(|(_, node)| {
            (node.role() == egui::accesskit::Role::Button && node.label() == Some("Generate stub"))
                .then(|| node.bounds())
                .flatten()
        })
        .expect("Actual native file context menu exposes Generate stub");
    let point = egui::pos2(
        ((button.x0 + button.x1) / 2.0) as f32,
        ((button.y0 + button.y1) / 2.0) as f32,
    );
    paint(
        &mut app,
        &ctx,
        0.4,
        click(point, egui::PointerButton::Primary),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "Actual context button must invoke the selected interpreter"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drain(&mut app);
    assert_eq!(
        app.project.as_ref().unwrap().tree.selected.as_deref(),
        Some(root.path().join("owned-library/native.so").as_path())
    );
    assert_eq!(
        std::fs::read(root.path().join("owned-library/native.so")).unwrap(),
        b"owned fake native image; never imported successfully"
    );
    assert!(
        app.native_skeleton_ui.records.is_empty(),
        "Failed target cannot manufacture a successful skeleton"
    );
}

#[cfg(unix)]
#[test]
fn unsupported_project_native_target_is_visible_and_never_executes_interpreter() {
    let (root, mut app, marker) = fixture(true, false);
    let outside = root.path().join("project-native.so");
    std::fs::write(&outside, b"not an interpreter library").unwrap();
    app.project.as_mut().unwrap().tree.selected = Some(outside.clone());
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    assert!(!app.native_skeletons_pending());
    assert!(!marker.exists());
    assert!(app
        .status
        .as_ref()
        .unwrap()
        .0
        .contains("selected interpreter library"));
    assert_eq!(
        std::fs::read(outside).unwrap(),
        b"not an interpreter library"
    );
}

#[cfg(unix)]
#[test]
fn interpreter_generation_replacement_cancels_old_capture_and_refuses_publication() {
    let (_root, mut app, _marker) = fixture(true, true);
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    let captured = app
        .native_skeleton_ui
        .active
        .as_ref()
        .unwrap()
        .capture
        .clone();
    let id = app.native_skeleton_ui.active.as_ref().unwrap().handle.id();
    let old_generation = app.native_skeleton_ui.generation;
    assert!(app.native_capture_current(&captured));
    let mut replacement = app
        .project
        .as_ref()
        .unwrap()
        .interpreter
        .as_ref()
        .unwrap()
        .clone();
    replacement.version = "3.13.0".into();
    app.select_interpreter(replacement);
    assert!(!app.native_capture_current(&captured));
    assert!(app.native_skeleton_ui.generation > old_generation);
    assert!(app.native_skeleton_ui.active.is_none());
    if let Some(job) = app.native_skeleton_ui.draining.first() {
        assert_eq!(job.handle.id(), id);
        assert!(job.handle.is_cancelled());
    }
    drain(&mut app);
    assert!(app.native_skeleton_ui.records.is_empty());
}

#[cfg(unix)]
#[test]
fn safe_mode_revocation_rejects_captured_owner_and_drains_actual_task() {
    let (_root, mut app, _marker) = fixture(true, true);
    app.handle_action(Action::GenerateNativeStub, &egui::Context::default());
    let captured = app
        .native_skeleton_ui
        .active
        .as_ref()
        .unwrap()
        .capture
        .clone();
    let id = app.native_skeleton_ui.active.as_ref().unwrap().handle.id();
    assert!(app.native_capture_current(&captured));
    // This is the actual flag set by the real Preview in Safe Mode decision.
    app.safe_mode = true;
    assert!(!app.native_capture_current(&captured));
    app.poll_native_skeletons(&egui::Context::default());
    assert!(app.native_skeleton_ui.active.is_none());
    if let Some(job) = app.native_skeleton_ui.draining.first() {
        assert_eq!(job.handle.id(), id);
        assert!(job.handle.is_cancelled());
    }
    drain(&mut app);
    assert!(app.native_skeleton_ui.records.is_empty());
}

// These fixtures start a real managed verification task with candidate records.
// They do not fabricate a producer ACK or successful interpreter execution.
#[cfg(unix)]
fn candidate_publication(app: &IdeApp) -> Publication {
    let capture = app.capture_native_stub().unwrap();
    let token = CancellationToken::new();
    let interpreter = InterpreterBinding {
        path: capture.interpreter.clone(),
        sha256: rustpy_project::native_skeleton::fingerprint_file(&capture.interpreter, &token)
            .unwrap(),
        version: capture.version.clone(),
        cache_tag: "cpython-312".into(),
        soabi: "owned-fixture".into(),
    };
    let record = Record {
        protocol: 1,
        module_name: capture.module_name,
        module_path: capture.module_path.clone(),
        module_sha256: rustpy_project::native_skeleton::fingerprint_file(
            &capture.module_path,
            &token,
        )
        .unwrap(),
        interpreter: interpreter.clone(),
        pyi: "answer: int\n".into(),
        unsupported: Vec::new(),
    };
    Publication {
        state: NativeState {
            catalogue: Arc::new(Catalogue::default()),
            index: VerifiedNativeIndex {
                trusted: true,
                generation: 0,
                interpreter: Some(interpreter),
                modules: BTreeMap::new(),
            },
        },
        records: vec![record],
        identities: Vec::new(),
    }
}
#[cfg(unix)]
fn wait_verification_without_adoption(app: &mut IdeApp) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let job = app.native_skeleton_ui.active.as_mut().unwrap();
        job.poll();
        if !job.pending() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "real verification must retire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[cfg(unix)]
#[test]
fn actual_app_rejects_byte_change_after_verification_ack_before_adoption() {
    let (_root, mut app, marker) = fixture(true, false);
    let capture = app.capture_native_stub().unwrap();
    let candidate = candidate_publication(&app);
    app.native_skeleton_ui
        .revalidate(capture.clone(), candidate)
        .unwrap();
    wait_verification_without_adoption(&mut app);
    assert!(app
        .native_skeleton_ui
        .active
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .is_ok());
    std::fs::write(
        &capture.module_path,
        b"changed after actual verification ACK",
    )
    .unwrap();
    app.poll_native_skeletons(&egui::Context::default());
    assert!(app.native_skeleton_ui.records.is_empty());
    assert!(app.native_skeleton_ui.active.is_none());
    assert!(
        !marker.exists(),
        "verification does not execute the interpreter"
    );
}
#[cfg(unix)]
#[test]
fn actual_quit_consent_requires_new_verification_generation_after_abort() {
    let (_root, mut app, marker) = fixture(true, false);
    let capture = app.capture_native_stub().unwrap();
    let candidate = candidate_publication(&app);
    app.native_skeleton_ui
        .revalidate(capture.clone(), candidate)
        .unwrap();
    let old_generation = app.native_skeleton_ui.generation;
    assert!(matches!(
        app.prepare_managed_coordinated_quit("queued-native"),
        window_lifecycle::QuitProgress::PendingProcesses { .. }
    ));
    wait_verification_without_adoption(&mut app);
    app.poll_native_skeletons(&egui::Context::default());
    assert!(!app.native_skeleton_ui.active.as_ref().unwrap().validated);
    assert!(app.native_skeleton_ui.records.is_empty());
    std::fs::write(
        &capture.module_path,
        b"changed while actual quit consent retained candidate",
    )
    .unwrap();
    assert_eq!(
        app.abort_coordinated_quit("queued-native"),
        window_lifecycle::QuitProgress::Aborted
    );
    app.poll_native_skeletons(&egui::Context::default());
    assert!(app.native_skeleton_ui.generation > old_generation);
    drain(&mut app);
    assert!(app.native_skeleton_ui.records.is_empty());
    assert!(app.native_skeleton_ui.draining.is_empty());
    assert!(!marker.exists());
}

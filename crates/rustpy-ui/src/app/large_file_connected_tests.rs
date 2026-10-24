//! Rendered large-file Open/Cancel controls, with an owned real sparse file.
use super::*;
use std::io::{Seek, SeekFrom, Write};

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Rect)> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some((
                    text.galley.text().to_owned(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                ))
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}

fn click(app: &mut IdeApp, ctx: &egui::Context, time: &mut f64, label: &str) {
    *time += 0.1;
    let labels = frame(app, ctx, *time, vec![]);
    let point = labels
        .iter()
        .find(|(text, _)| text == label)
        .expect("actual rendered large-file control")
        .1
        .center();
    for pressed in [true, false] {
        *time += 0.1;
        frame(
            app,
            ctx,
            *time,
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

#[test]
fn actual_sparse_large_file_banner_cancel_discards_load_then_open_publishes_real_result() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large π.txt");
    let size = LARGE_FILE_GATE_BYTES + 1;
    let head = "head π🙂\n".as_bytes();
    let tail = "λ🙂 end\n".as_bytes();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(head).unwrap();
    file.set_len(size).unwrap();
    file.seek(SeekFrom::Start(size - tail.len() as u64))
        .unwrap();
    file.write_all(tail).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
    let mut settings = Settings::default();
    settings.completion.show_as_you_type = false;
    let mut app = IdeApp::new(settings, None);
    app.open_file(&path);
    let index = app.active.unwrap();
    assert!(app.is_large_file_placeholder(index));
    assert_eq!(app.tabs[index].state.path.as_deref(), Some(path.as_path()));
    assert_eq!(app.tabs[index].state.doc.text(), "");
    assert!(app.tabs[index].state.doc.is_read_only());
    let pending_identity = app.tabs[index].state.doc.identity();
    let pending_version = app.tabs[index].state.doc.version();
    let carets = app.tabs[index].state.carets.clone();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    let labels = frame(&mut app, &ctx, 0.1, vec![]);
    assert!(labels
        .iter()
        .any(|(text, _)| text == "Open in Large-File Mode"));
    assert!(labels.iter().any(|(text, _)| text == "Close Tab"));
    frame(
        &mut app,
        &ctx,
        0.2,
        vec![egui::Event::Text("MUST NOT EDIT PLACEHOLDER".into())],
    );
    app.handle_action(Action::Save, &ctx);
    assert_eq!(app.tabs[index].state.doc.text(), "");
    assert_eq!(app.tabs[index].state.carets, carets);
    assert!(!app.tabs[index].state.doc.can_undo());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), size);

    let mut time = 0.2;
    click(&mut app, &ctx, &mut time, "Open in Large-File Mode");
    let handle = app
        .bg
        .large_files
        .get(&path)
        .unwrap()
        .load
        .as_ref()
        .expect("real button starts maintained worker");
    let task_id = handle.id();
    let token = handle.token().clone();
    // Do not publish a finished result before clicking Cancel: even a fast
    // worker must not turn a cancelled UI admission into source publication.
    click(&mut app, &ctx, &mut time, "Cancel");
    assert!(token.is_cancelled());
    assert!(app.bg.large_files.get(&path).unwrap().load.is_none());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !TaskManager::global()
        .finished()
        .iter()
        .any(|task| task.id == task_id)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "owned cancelled worker must finish"
        );
        std::thread::yield_now();
    }
    app.poll_large_files();
    assert!(app.is_large_file_placeholder(index));
    assert_eq!(app.tabs[index].state.doc.identity(), pending_identity);
    assert_eq!(app.tabs[index].state.doc.version(), pending_version);
    assert_eq!(app.tabs[index].state.doc.text(), "");
    assert_eq!(app.tabs[index].state.carets, carets);
    assert!(!app.tabs[index].state.doc.can_undo());
    assert!(app.tabs[index].state.doc.is_read_only());
    time += 0.1;
    let labels = frame(&mut app, &ctx, time, vec![]);
    assert!(labels
        .iter()
        .any(|(text, _)| text == "Open in Large-File Mode"));
    assert!(!labels.iter().any(|(text, _)| text == "Cancel"));

    click(&mut app, &ctx, &mut time, "Open in Large-File Mode");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while app.is_large_file_placeholder(index) && std::time::Instant::now() < deadline {
        app.poll_large_files();
        std::thread::yield_now();
    }
    assert!(
        !app.is_large_file_placeholder(index),
        "real second admitted worker publishes"
    );
    let disk = std::fs::read(&path).unwrap();
    assert_eq!(disk.len() as u64, size);
    assert_eq!(&disk[..head.len()], head);
    assert_eq!(&disk[disk.len() - tail.len()..], tail);
    assert!(disk[head.len()..disk.len() - tail.len()]
        .iter()
        .all(|byte| *byte == 0));
    assert_eq!(app.tabs[index].state.doc.text().as_bytes(), disk);
    assert_ne!(app.tabs[index].state.doc.identity(), pending_identity);
    assert_eq!(app.tabs[index].state.path.as_deref(), Some(path.as_path()));
    assert!(!app.tabs[index].state.doc.is_modified());
    assert!(!app.tabs[index].state.doc.is_read_only());
    assert!(!app.tabs[index].state.doc.can_undo());
    assert!(app
        .status
        .as_ref()
        .is_some_and(|(text, _)| text.contains("opened in large-file mode")));
}

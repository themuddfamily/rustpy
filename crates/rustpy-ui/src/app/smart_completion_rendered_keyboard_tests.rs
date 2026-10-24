//! Real App RawInput shortcut, sustained popup and insertion/undo routes.
use super::*;
use crate::editor::Caret;
const LAMBDA: &str = "from typing import Callable\ndef zero(callback: Callable[[], int]): pass\ndef one(callback: Callable[[str], int]): pass\nzero()\none()\n";
const AWAIT: &str = "async def fetch() -> str: return 'ok'\ndef use(value: str): pass\nasync def run():\n    pending = fetch()\n    wrong = 1\n    use()\n";

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<String> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.dialogs(ui.ctx());
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
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
fn check(source: &str, call: &str, expected_insert: &str, pointer: Option<Pos2>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.py");
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.completion.show_as_you_type = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let i = app.active.unwrap();
    let caret = source.rfind(call).unwrap() + call.find('(').unwrap() + 1;
    app.tabs[i]
        .state
        .set_carets(vec![Caret::at(TextSize::new(caret as u32))]);
    app.focus_editor = true;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    let point = pointer.unwrap_or_else(|| {
        app.tabs[i]
            .view
            .caret_screen_rect
            .expect("painted actual caret geometry")
            .center()
    });
    frame(
        &mut app,
        &ctx,
        0.03,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    // Restore only the intended document caret after pointer positioning; do
    // not request or manufacture egui focus after the real click.
    app.tabs[i]
        .state
        .set_carets(vec![Caret::at(TextSize::new(caret as u32))]);
    frame(&mut app, &ctx, 0.035, vec![]);
    eprintln!(
        "smart-pointer pointer={pointer:?} point={point:?} focused_id={:?} view_focused={}",
        ctx.memory(|memory| memory.focused()),
        app.tabs[i].view.focused
    );
    assert!(
        ctx.memory(|memory| memory.has_focus(app.tabs[i].view.id)),
        "actual editor focus before shortcut"
    );
    assert!(app.tabs[i].view.focused);
    let version = app.tabs[i].state.doc.version();
    let original_caret = app.tabs[i].state.primary();
    frame(
        &mut app,
        &ctx,
        0.04,
        key(
            egui::Key::Space,
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        ),
    );
    assert!(
        app.completion.visible,
        "actual CtrlShiftSpace lookup must remain open after editor frame"
    );
    let labels = frame(&mut app, &ctx, 0.06, vec![]);
    assert!(
        app.completion.visible,
        "lookup survives first idle sizing/paint frame"
    );
    let labels2 = frame(&mut app, &ctx, 0.08, vec![]);
    assert!(
        app.completion.visible,
        "lookup survives sustained idle frame without forced focus"
    );
    frame(&mut app, &ctx, 0.8, vec![]);
    assert!(
        app.completion.visible,
        "lookup survives hover delay without focus injection"
    );
    frame(&mut app, &ctx, 1.0, vec![]);
    assert!(
        app.completion.visible,
        "lookup remains the actual keyboard owner after idle hover"
    );
    let index = app
        .completion
        .items
        .iter()
        .position(|item| item.insert == expected_insert)
        .expect("real smart item");
    assert!(index < 128, "bounded actual keyboard selection");
    let item = app.completion.items[index].clone();
    assert!(
        labels
            .iter()
            .chain(labels2.iter())
            .any(|text| text.contains(app.completion.items[app.completion.selected].label.as_str())),
        "actual painted lookup label"
    );
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.primary(), original_caret);
    let mut time = 1.1;
    for _ in 0..index {
        frame(
            &mut app,
            &ctx,
            time,
            key(egui::Key::ArrowDown, egui::Modifiers::NONE),
        );
        time += 0.01;
    }
    assert_eq!(app.completion.selected, index);
    let selected_labels = frame(&mut app, &ctx, time, vec![]);
    assert!(
        selected_labels
            .iter()
            .any(|text| text.contains(item.label.as_str())),
        "keyboard-selected smart item is actually painted"
    );
    time += 0.01;
    frame(
        &mut app,
        &ctx,
        time,
        key(egui::Key::Enter, egui::Modifiers::NONE),
    );
    let mut expected = source.to_owned();
    expected.insert_str(caret, expected_insert);
    assert_eq!(
        app.tabs[i].state.doc.text(),
        expected,
        "actual keyboard acceptance inserts smart text"
    );
    assert!(!app.completion.visible);
    let expected_caret = caret + item.caret_offset.unwrap_or(item.insert.len());
    assert_eq!(
        usize::from(app.tabs[i].state.primary().head()),
        expected_caret
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        source,
        "lookup insertion never saves implicitly"
    );
    frame(
        &mut app,
        &ctx,
        time + 0.02,
        key(egui::Key::Z, egui::Modifiers::CTRL),
    );
    assert_eq!(
        app.tabs[i].state.doc.text(),
        source,
        "single actual Undo restores source"
    );
    assert_eq!(
        app.tabs[i].state.primary(),
        original_caret,
        "Undo restores insertion caret"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
#[test]
fn actual_smart_zero_lambda_popup_sustains_and_inserts_with_body_caret() {
    check(LAMBDA, "zero()", "lambda: ", None);
}
#[test]
fn actual_smart_one_lambda_popup_sustains_and_inserts_with_body_caret() {
    check(LAMBDA, "one()", "lambda value, /: ", None);
}
#[test]
fn actual_smart_await_popup_sustains_and_inserts_matching_initialized_variable() {
    check(AWAIT, "use()", "await pending", None);
}

#[test]
fn actual_short_document_far_blank_click_retains_editor_focus_for_smart_shortcut() {
    check(LAMBDA, "zero()", "lambda: ", Some(Pos2::new(1050.0, 300.0)));
}

#[test]
fn actual_short_document_inside_blank_click_retains_editor_focus_for_smart_shortcut() {
    check(LAMBDA, "zero()", "lambda: ", Some(Pos2::new(500.0, 300.0)));
}
#[test]
fn actual_smart_constructor_and_constant_items_share_keyboard_acceptance_and_undo() {
    // Constructor bodies have an editable inner caret, while constants finish
    // at the accepted text's end. Exercise both through the same real UI path.
    for (source, insertion, body_caret) in [
        (
            "class Point:\n    def __init__(self,x:int):self.x=x\ndef use(value:Point):pass\nuse()\n",
            "Point()",
            Some(6),
        ),
        (
            "from typing import Optional\ndef use(value:Optional[str]):pass\nuse()\n",
            "None",
            None,
        ),
        (
            "from enum import Enum\nclass Color(Enum):\n    RED=1\n    GREEN=2\ndef use(value:Color):pass\nuse()\n",
            "Color.RED",
            None,
        ),
    ] {
        let analysis =
            rustpy_semantic::db::Analysis::for_project(std::path::Path::new("/shared-smart-items"));
        let id = analysis.set_file_text(std::path::Path::new("/shared-smart-items/m.py"), source);
        let file = analysis.file(id).unwrap();
        let at = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        let items = rustpy_semantic::completion2::smart_complete(&analysis, &file, at);
        let item = items
            .iter()
            .find(|item| item.insert == insertion)
            .expect("real backend smart item");
        assert_eq!(
            item.caret_offset, body_caret,
            "actual editable-constructor vs constant-end caret metadata"
        );
        check(source, "use()", insertion, None);
    }
}

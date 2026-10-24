//! The UI language applied end to end. Switching it changes the process-wide locale of
//! `rustpy-i18n`, so these tests live in their own test binary (the unit tests expect English
//! texts) and take turns.

use std::sync::Mutex;

use rustpy_ui::actions::Action;
use rustpy_ui::i18n::{self, LocalizedUi};

static LOCALE: Mutex<()> = Mutex::new(());

fn painted_texts(out: &egui::FullOutput) -> Vec<String> {
    fn walk(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, texts)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for clipped in &out.shapes {
        walk(&clipped.shape, &mut texts);
    }
    texts
}

#[test]
fn the_language_setting_switches_the_ui_language() {
    let _turn = LOCALE.lock().unwrap_or_else(|e| e.into_inner());
    // Without `i18n::init` no font plugin is registered: nothing looks for system fonts.
    let ctx = egui::Context::default();
    i18n::sync_language(&ctx, "ja");
    assert_eq!(rustpy_i18n::locale(), "ja");
    assert_eq!(Action::FindAction.text(), "アクションの検索…");
    assert_eq!(
        Action::FindAction.english_text(),
        "Find Action…",
        "the English text stays the key"
    );
    assert_eq!(Action::SortLines.to_string(), "行のソート");
    // Any spelling `set_locale` accepts; an unchanged setting is not applied again.
    i18n::sync_language(&ctx, "de_DE.UTF-8");
    assert_eq!(rustpy_i18n::locale(), "de");
    assert_eq!(Action::Undo.text(), "Rückgängig");
    i18n::sync_language(&ctx, "de_DE.UTF-8");
    assert_eq!(rustpy_i18n::locale(), "de");
    // English can be picked over any system language.
    i18n::sync_language(&ctx, "en");
    assert_eq!(rustpy_i18n::locale(), "en");
    assert_eq!(Action::Undo.text(), "Undo");
}

#[test]
fn menus_check_boxes_and_actions_render_in_the_ui_language() {
    let _turn = LOCALE.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = egui::Context::default();
    i18n::sync_language(&ctx, "zh-CN");
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            // As in `IdeApp::menu_bar`: English literals, translated by `LocalizedUi`.
            let ui = &mut LocalizedUi::new(ui);
            ui.menu_button("File", |_| {});
            ui.menu_button("Edit", |_| {});
            let mut on = true;
            ui.checkbox(&mut on, "Status Bar");
            let _ = ui.button(Action::Settings.text());
            ui.label("not a catalog key");
        });
    });
    out.textures_delta.clear();
    let texts = painted_texts(&out);
    for expected in ["文件", "编辑", "状态栏", "设置…", "not a catalog key"] {
        assert!(
            texts.iter().any(|t| t == expected),
            "{expected} in {texts:?}"
        );
    }
    assert!(
        !texts.iter().any(|t| t == "File" || t == "Status Bar"),
        "{texts:?}"
    );
    i18n::sync_language(&ctx, "en");
}

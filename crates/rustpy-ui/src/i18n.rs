//! UI localization (roadmap §8 "Internationalization") on top of `rustpy-i18n`.
//!
//! * **Language setting.** `appearance.language` holds a locale code (`zh-CN`, `ja`, …) or
//!   is empty for the system language ([`rustpy_i18n::system_locale`]: GNU `LANGUAGE`, then
//!   `LC_ALL` / `LC_MESSAGES` / `LANG`; only the environment is read, so on Windows and
//!   macOS, where GUI apps usually have no `LANG`, it is English). [`sync_language`]
//!   runs at the start of every frame and applies it when it changed, so the Settings
//!   dialog, its Cancel, Import Settings and Restore Defaults all take effect at once.
//! * **Strings.** UI code translates English source strings with [`tr`] / [`tr_static`]
//!   (falling back to the English text): action texts ([`crate::actions::Action::text`]),
//!   menus and check boxes of the main menu and the editor context menu (through
//!   [`LocalizedUi`]), tool window titles and the Settings dialog. The English text is the
//!   catalog key, so the `i18n_extract` example of `rustpy-i18n` finds the strings in these
//!   sources.
//! * **Fonts.** The bundled fonts have no Chinese, Japanese or Korean glyphs: the `fonts` module
//!   appends system CJK fonts to every font family, for a CJK UI language before the first
//!   frame and otherwise once CJK text is painted.

mod fonts;

use std::borrow::Cow;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use egui::{Atoms, IntoAtoms, WidgetText};

use crate::a11y::AccessibleCombo;

pub use rustpy_i18n::{tr, tr_fmt, tr_n, tr_static};

/// The UI locale for the `appearance.language` setting: the setting when it names a
/// supported locale (in any spelling `rustpy_i18n::set_locale` accepts), else the system
/// locale `system`, else English.
pub fn resolve_locale(setting: &str, system: Option<&str>) -> &'static str {
    Some(setting.trim())
        .filter(|s| !s.is_empty())
        .and_then(rustpy_i18n::match_locale)
        .or_else(|| system.and_then(rustpy_i18n::match_locale))
        .unwrap_or("en")
}

/// Whether a locale tag (`zh-CN`, `ja_JP.UTF-8`, …) is Chinese, Japanese or Korean.
pub fn is_cjk(tag: &str) -> bool {
    rustpy_i18n::parse_posix_locale(tag)
        .is_some_and(|t| matches!(t.split('-').next(), Some("zh" | "ja" | "ko")))
}

/// The raw message locale of the environment (`zh_TW.UTF-8` too, which has no catalog but
/// decides the glyph forms of Chinese text).
fn system_tag() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.trim().is_empty()))
}

/// The `appearance.language` value applied last (`None` before [`init`]).
static APPLIED: Mutex<Option<String>> = Mutex::new(None);

/// Applies the language setting before the first frame and registers the CJK font plugin.
/// For a Chinese, Japanese or Korean UI the font is read right away: without it the first
/// frames would show boxes.
pub fn init(ctx: &egui::Context, setting: &str) {
    ctx.add_plugin(fonts::CjkFonts::new(
        Arc::new(fonts::load_system_font),
        system_tag(),
    ));
    apply(ctx, setting, true);
}

/// Applies `appearance.language` when it changed since the last call. Cheap enough for
/// every frame.
pub fn sync_language(ctx: &egui::Context, setting: &str) {
    let changed = APPLIED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_deref()
        != Some(setting);
    if changed {
        apply(ctx, setting, false);
    }
}

fn apply(ctx: &egui::Context, setting: &str, before_first_frame: bool) {
    let system = rustpy_i18n::system_locale();
    let locale = resolve_locale(setting, system.as_deref());
    rustpy_i18n::set_locale(locale);
    *APPLIED.lock().unwrap_or_else(PoisonError::into_inner) = Some(setting.to_owned());
    log::info!(
        "UI language: {locale} ({})",
        if setting.trim().is_empty() {
            "system default"
        } else {
            setting
        }
    );
    let cjk = is_cjk(locale).then_some(locale);
    ctx.with_plugin::<fonts::CjkFonts, _>(|f| f.set_ui_locale(ctx, cjk, before_first_frame));
    ctx.request_repaint();
}

/// Whether the UI language is Chinese, Japanese or Korean but the system has no font for it.
pub fn cjk_font_missing(ctx: &egui::Context) -> bool {
    ctx.with_plugin::<fonts::CjkFonts, _>(|f| f.ui_font_missing())
        .unwrap_or(false)
}

/// Plain-text atoms of `label` passed through `translate`; rich text, layout jobs and
/// images are kept as they are.
fn localize_with<'a>(
    label: impl IntoAtoms<'a>,
    translate: impl Fn(&str) -> Cow<'static, str>,
) -> Atoms<'a> {
    let mut atoms = label.into_atoms();
    atoms.map_texts(|text| match text {
        WidgetText::Text(s) => WidgetText::Text(translate(&s).into_owned()),
        other => other,
    });
    atoms
}

/// A [`egui::Ui`] whose `menu_button` and `checkbox` translate their plain-text labels
/// with [`tr`]; everything else is the `Ui` itself (through `Deref`), and functions taking
/// `&mut egui::Ui` accept it. The main menu and the editor context menu are built on it,
/// so their (English, literal) labels stay the catalog keys the extractor reads.
pub struct LocalizedUi<'u>(&'u mut egui::Ui);

impl<'u> LocalizedUi<'u> {
    pub fn new(ui: &'u mut egui::Ui) -> Self {
        Self(ui)
    }

    /// [`egui::Ui::menu_button`] with a translated label; the menu is built on a `LocalizedUi`.
    pub fn menu_button<'a, R>(
        &mut self,
        label: impl IntoAtoms<'a>,
        add_contents: impl FnOnce(&mut LocalizedUi<'_>) -> R,
    ) -> egui::InnerResponse<Option<R>> {
        let mut atoms = label.into_atoms();
        let mut source = String::new();
        atoms.map_texts(|text| {
            if let WidgetText::Text(value) = &text {
                source = value.clone();
            }
            text
        });
        let top_level = !egui::menu::is_in_menu(self.0);
        let response = self.0.menu_button(localize_with(atoms, tr), |ui| {
            add_contents(&mut LocalizedUi(ui))
        });
        if top_level {
            crate::custom_menus::mnemonic(&response.response, &source);
        }
        response
    }

    pub fn button<'a>(&mut self, label: impl IntoAtoms<'a>) -> egui::Response {
        self.0.button(localize_with(label, tr))
    }
    pub fn small_button<'a>(&mut self, label: impl IntoAtoms<'a>) -> egui::Response {
        self.0.small_button(localize_with(label, tr))
    }
    pub fn label(&mut self, label: impl Into<WidgetText>) -> egui::Response {
        let text = label.into();
        self.0.label(match text {
            WidgetText::Text(source) => WidgetText::Text(tr(&source).into_owned()),
            other => other,
        })
    }
    /// [`egui::Ui::checkbox`] with a translated label.
    pub fn checkbox<'a>(
        &mut self,
        checked: &'a mut bool,
        label: impl IntoAtoms<'a>,
    ) -> egui::Response {
        self.0.checkbox(checked, localize_with(label, tr))
    }
}

impl Deref for LocalizedUi<'_> {
    type Target = egui::Ui;

    fn deref(&self) -> &egui::Ui {
        self.0
    }
}

impl DerefMut for LocalizedUi<'_> {
    fn deref_mut(&mut self) -> &mut egui::Ui {
        self.0
    }
}

/// Whether a search filter (lower case) matches an English UI string or its translation.
pub fn matches_filter(english: &str, filter: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    let filter = filter
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    filter.is_empty()
        || english
            .nfkc()
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .contains(&filter)
        || tr(english)
            .nfkc()
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .contains(&filter)
}

/// The language picker's entry for a locale code: its native name (`ja` → `日本語`), with
/// the share of translated strings when the catalog is incomplete (`Русский (87%)`).
fn language_label(code: &str) -> String {
    static LABELS: OnceLock<Vec<(&'static str, String)>> = OnceLock::new();
    let labels = LABELS.get_or_init(|| {
        rustpy_i18n::available_locales()
            .into_iter()
            .map(|(code, name)| {
                let label = match rustpy_i18n::translation_coverage(code) {
                    Some((done, total)) if done < total => {
                        format!("{name} ({}%)", done * 100 / total.max(1))
                    }
                    _ => name.to_owned(),
                };
                (code, label)
            })
            .collect()
    });
    labels
        .iter()
        .find(|(c, _)| *c == code)
        .map_or_else(|| code.to_owned(), |(_, label)| label.clone())
}

/// The "Language and Region" section of Settings › Appearance: a combo box of the UI
/// languages (by native name, the system default first), applied at once. The dialog's
/// OK / Apply store it and Cancel reverts it. Returns whether the setting changed.
pub fn language_section(ui: &mut egui::Ui, setting: &mut String) -> bool {
    ui.heading(tr_static("Language and Region"));
    let system = rustpy_i18n::system_locale();
    let system_label = format!(
        "{} ({})",
        tr_static("System default"),
        language_label(system.as_deref().unwrap_or("en"))
    );
    let current = Some(setting.trim())
        .filter(|s| !s.is_empty())
        .and_then(rustpy_i18n::match_locale);
    let mut changed = false;
    ui.horizontal(|ui| {
        let language_caption = ui.label(tr_static("Language"));
        let selected = current.map_or_else(|| system_label.clone(), language_label);
        egui::ComboBox::from_id_salt("ui_language")
            .selected_text(selected)
            .height(400.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current.is_none(), system_label.as_str())
                    .clicked()
                    && current.is_some()
                {
                    setting.clear();
                    changed = true;
                }
                for (code, _) in rustpy_i18n::available_locales() {
                    if ui
                        .selectable_label(current == Some(code), language_label(code))
                        .clicked()
                        && current != Some(code)
                    {
                        *setting = code.to_string();
                        changed = true;
                    }
                }
            })
            .combo_labelled(&language_caption);
    });
    if changed {
        sync_language(ui.ctx(), setting);
    }
    let effective = resolve_locale(setting, system.as_deref());
    if rustpy_i18n::translation_coverage(effective).is_some_and(|(done, total)| done < total) {
        ui.label(
            egui::RichText::new(tr_static(
                "Some texts are not translated yet and appear in English.",
            ))
            .weak(),
        );
    }
    if cjk_font_missing(ui.ctx()) {
        // In English: without the font a translated warning would be unreadable too.
        ui.colored_label(ui.visuals().warn_fg_color, "No Chinese, Japanese or Korean font was found, so this language shows as boxes. Install Noto Sans CJK (e.g. the fonts-noto-cjk package) and restart.");
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_wins_then_the_system_then_english() {
        assert_eq!(resolve_locale("ja", Some("de")), "ja");
        assert_eq!(
            resolve_locale(" zh_CN.UTF-8 ", None),
            "zh-CN",
            "any spelling set_locale accepts"
        );
        assert_eq!(resolve_locale("pt-PT", None), "pt-BR");
        assert_eq!(
            resolve_locale("", Some("de")),
            "de",
            "empty = system default"
        );
        assert_eq!(resolve_locale("", Some("ko_KR.UTF-8")), "ko");
        assert_eq!(resolve_locale("", None), "en");
        assert_eq!(
            resolve_locale("", Some("it")),
            "en",
            "a system language without catalog"
        );
        assert_eq!(
            resolve_locale("tlh", Some("fr")),
            "fr",
            "an unknown setting counts as the system default"
        );
        assert_eq!(
            resolve_locale("en", Some("fr")),
            "en",
            "English can be chosen over the system language"
        );
    }

    #[test]
    fn cjk_tags() {
        for tag in [
            "zh-CN",
            "zh_TW.UTF-8",
            "ja",
            "ja_JP.eucJP",
            "ko-KR",
            "ZH-hant",
        ] {
            assert!(is_cjk(tag), "{tag}");
        }
        for tag in ["en", "de_DE.UTF-8", "ru", "C", "", "pt-BR"] {
            assert!(!is_cjk(tag), "{tag}");
        }
    }

    #[test]
    fn language_labels_are_native_names_with_partial_coverage() {
        assert_eq!(language_label("en"), "English");
        assert_eq!(language_label("ja"), "日本語");
        assert_eq!(language_label("zh-CN"), "简体中文");
        assert!(
            language_label("ru").starts_with("Русский"),
            "{}",
            language_label("ru")
        );
        assert_eq!(language_label("xx"), "xx");
    }

    #[test]
    fn plain_text_labels_are_translated_rich_text_is_kept() {
        let de = |s: &str| rustpy_i18n::tr_in("de", s);
        let texts = |atoms: Atoms<'_>| {
            atoms
                .iter_texts()
                .map(|t| t.text().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(localize_with("File", de)), ["Datei"]);
        assert_eq!(
            texts(localize_with(String::from("Edit"), de)),
            ["Bearbeiten"]
        );
        assert_eq!(
            texts(localize_with("not a catalog key", de)),
            ["not a catalog key"]
        );
        assert_eq!(
            texts(localize_with(egui::RichText::new("File").strong(), de)),
            ["File"],
            "explicit rich text is left alone"
        );
        assert_eq!(
            texts(localize_with(("View", egui::RichText::new("x")), de)),
            ["Ansicht", "x"]
        );
    }

    #[test]
    fn filters_match_english_and_translations() {
        assert!(matches_filter("Keymap", ""));
        assert!(matches_filter("Keymap", "keym"));
        assert!(!matches_filter("Keymap", "xyz"));
    }

    #[test]
    fn localized_menus_run_in_egui() {
        // English (unit tests never switch the global locale; tests/i18n.rs does): labels
        // unchanged, and `&mut LocalizedUi` works where `&mut egui::Ui` is expected.
        let ctx = egui::Context::default();
        let takes_ui = |ui: &mut egui::Ui| ui.label("item");
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                let ui = &mut LocalizedUi::new(ui);
                ui.menu_button("File", |ui| {
                    takes_ui(ui);
                });
                let mut on = true;
                ui.checkbox(&mut on, "Status Bar");
                takes_ui(ui);
            });
        });
        out.textures_delta.clear();
        let texts = shape_texts(&out.shapes);
        for t in ["File", "Status Bar", "item"] {
            assert!(texts.iter().any(|s| s == t), "{t} in {texts:?}");
        }
    }

    #[test]
    fn the_main_menu_and_the_editor_context_menu_are_localized() {
        // Their labels are English literals that `LocalizedUi` translates: without it they stay English.
        let app = include_str!("app.rs");
        let menu_bar = &app[app.find("    fn menu_bar(").expect("IdeApp::menu_bar")..];
        let menu_bar = &menu_bar[..menu_bar[1..]
            .find("\n    fn ")
            .map_or(menu_bar.len(), |end| end + 1)];
        assert!(
            menu_bar.contains("LocalizedUi::new(ui)"),
            "IdeApp::menu_bar builds its menus on crate::i18n::LocalizedUi"
        );
        let view = include_str!("editor/view.rs");
        assert!(
            context_menu_prologue(view).contains("LocalizedUi::new(ui)"),
            "the editor context menu is built on crate::i18n::LocalizedUi"
        );
    }

    /// The editor context menu's closure up to its first item (`run(ui, …)`): where its `ui`
    /// has to become a `LocalizedUi` for the submenus to be translated.
    fn context_menu_prologue(view: &str) -> &str {
        let menu = &view[view
            .find("crate::a11y::bound_keyboard_context_menu(")
            .expect("the authority-bound editor context menu")..];
        let body = &menu[menu
            .find(".show(|ui| {")
            .expect("the editor context menu's actual popup closure")..];
        &body[..body
            .find("run(ui, ")
            .expect("the context menu adds its items with run(ui, …)")]
    }

    #[test]
    fn the_context_menu_check_reads_the_whole_prologue() {
        // Main's has_cells line, merged in before the wrapper, pushed it past the 300 characters
        // the check used to read.
        let long = format!("crate::a11y::bound_keyboard_context_menu(&resp, cfg.keyboard, authority).show(|ui| {{\n    let has_cells = {};\n    let ui = &mut crate::i18n::LocalizedUi::new(ui);\n    let mut run = |ui: &mut egui::Ui, a: Action| {{}};\n    run(ui, Action::Cut);\n", "x".repeat(400));
        assert!(context_menu_prologue(&long).contains("LocalizedUi::new(ui)"));
        let unrelated = format!(
            "other.show(|ui| {{ let ui = LocalizedUi::new(ui); run(ui, Action::Copy); }});\n{long}"
        );
        assert!(
            context_menu_prologue(&unrelated).contains("has_cells"),
            "the prologue comes from the actual bound menu, not an unrelated popup"
        );
        let late = "crate::a11y::bound_keyboard_context_menu(&resp, cfg.keyboard, authority).show(|ui| {\n    run(ui, Action::Cut);\n    let ui = &mut crate::i18n::LocalizedUi::new(ui);\n";
        assert!(
            !context_menu_prologue(late).contains("LocalizedUi::new(ui)"),
            "a wrapper after the first item does not count"
        );
    }

    /// The texts painted by a frame.
    pub(crate) fn shape_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for s in shapes {
            walk(&s.shape, &mut out);
        }
        out
    }
    #[test]
    fn settings_search_normalizes_search_keys_without_rewriting_source() {
        let source = "Café Ａction";
        assert!(matches_filter(source, "cafe\u{301}"));
        assert!(matches_filter(source, "action"));
        assert_eq!(source, "Café Ａction");
    }
}

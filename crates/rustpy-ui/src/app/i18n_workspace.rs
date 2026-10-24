//! Natural-language policy and explicit, previewed Unicode transformations.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::i18n::tr_static as tr;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}
impl Form {
    const ALL: [Self; 4] = [Self::Nfc, Self::Nfd, Self::Nfkc, Self::Nfkd];
    fn key(self) -> &'static str {
        match self {
            Self::Nfc => "NFC",
            Self::Nfd => "NFD",
            Self::Nfkc => "NFKC",
            Self::Nfkd => "NFKD",
        }
    }
    fn apply(self, text: &str) -> String {
        match self {
            Self::Nfc => text.nfc().collect(),
            Self::Nfd => text.nfd().collect(),
            Self::Nfkc => text.nfkc().collect(),
            Self::Nfkd => text.nfkd().collect(),
        }
    }
}
fn codepoints(text: &str) -> String {
    text.chars()
        .take(256)
        .map(|c| format!("U+{:04X}", c as u32))
        .collect::<Vec<_>>()
        .join(" ")
}
impl IdeApp {
    pub(super) fn ime_owns_shortcuts(&self, ui: &egui::Ui) -> bool {
        ui.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Ime(egui::ImeEvent::Preedit { .. } | egui::ImeEvent::Commit(_))
                )
            })
        }) || self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|tab| {
                tab.view.ime_composing() && ui.memory(|memory| memory.has_focus(tab.view.id))
            })
    }

    pub(super) fn regional_settings(&mut self, ui: &mut egui::Ui) {
        let a = &mut self.settings.appearance;
        ui.horizontal(|ui| {
            let date_label = ui.label(tr("Date locale"));
            egui::ComboBox::from_id_salt("date-region")
                .selected_text(if a.date_locale.is_empty() {
                    tr("Follow UI language")
                } else {
                    a.date_locale.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut a.date_locale,
                        String::new(),
                        tr("Follow UI language"),
                    );
                    for (key, label) in [
                        ("en-US", "English (United States)"),
                        ("en-GB", "English (United Kingdom)"),
                        ("zh-CN", "简体中文"),
                        ("ja", "日本語"),
                        ("ko", "한국어"),
                        ("de", "Deutsch"),
                        ("fr", "Français"),
                        ("es", "Español"),
                    ] {
                        ui.selectable_value(&mut a.date_locale, key.into(), tr(label));
                    }
                })
                .combo_labelled(&date_label);
        });
        ui.checkbox(&mut a.date_iso, tr("Use ISO dates (YYYY-MM-DD)"));
        ui.checkbox(&mut a.clock_12_hour, tr("Use 12-hour clock"));
        rustpy_i18n::regional::configure(&a.date_locale, a.date_iso, a.clock_12_hour);
        ui.monospace(rustpy_i18n::regional::timestamp_millis(
            chrono::Local::now().timestamp_millis(),
        ));
    }
    pub(super) fn page_natural_languages(&mut self, ui: &mut egui::Ui) {
        use rustpy_grammar::policy::{Category, Language};
        ui.heading(tr("Natural Languages"));
        let before = self.settings.inspections.clone();
        let policy = &mut self.settings.inspections;
        ui.checkbox(
            &mut policy.grammar,
            tr("Grammar (comments, docstrings, Markdown, commit messages)"),
        );
        egui::ComboBox::from_id_salt("natural-grammar-language")
            .selected_text(
                Language::from_key(&policy.grammar_language)
                    .map(|l| tr(l.label()).to_string())
                    .unwrap_or_else(|_| policy.grammar_language.clone()),
            )
            .show_ui(ui, |ui| {
                for language in Language::ALL {
                    ui.selectable_value(
                        &mut policy.grammar_language,
                        language.key().into(),
                        tr(language.label()),
                    );
                }
            })
            .combo_named(crate::i18n::tr_static("Grammar language"));
        ui.small(tr(
            "Built-in grammar and spelling rules support English. UI language is independent.",
        ));
        for category in Category::ALL {
            let mut enabled = !policy
                .grammar_disabled_categories
                .iter()
                .any(|k| k == category.key());
            if ui.checkbox(&mut enabled, tr(category.label())).changed() {
                policy
                    .grammar_disabled_categories
                    .retain(|k| k != category.key());
                if !enabled {
                    policy
                        .grammar_disabled_categories
                        .push(category.key().into());
                }
            }
        }
        if let Err(error) = super::grammar_policy::configured(policy) {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
        }
        ui.checkbox(
            &mut policy.spelling,
            tr("Spelling (typos in identifiers, comments, strings and text files)"),
        );
        if before != self.settings.inspections {
            self.invalidate_inspection_profile();
        }
        ui.separator();
        ui.heading(tr("Dictionaries"));
        let user = user_dictionary_path();
        ui.monospace(user.display().to_string());
        if ui
            .add_enabled(
                user.is_file(),
                egui::Button::new(tr("Open user dictionary")),
            )
            .clicked()
        {
            self.open_file(&user);
        }
        if let Some(root) = self.project.as_ref().map(|p| p.root.clone()) {
            let path = root.join(".rustpy/dictionaries/project.txt");
            ui.monospace(path.display().to_string());
            if ui
                .add_enabled(
                    path.is_file(),
                    egui::Button::new(tr("Open project dictionary")),
                )
                .clicked()
            {
                self.open_file(&path);
            }
        }
        let id = ui.id().with("dictionary-word");
        let mut word = ui.data_mut(|d| d.get_temp::<String>(id).unwrap_or_default());
        ui.horizontal(|ui| {
            ui.label(tr("Accepted word"));
            ui.text_edit_singleline(&mut word);
            if ui
                .add_enabled(
                    !word.trim().is_empty() && !word.trim().contains(char::is_whitespace),
                    egui::Button::new(tr("Add to dictionary")),
                )
                .clicked()
            {
                let path = self
                    .project
                    .as_ref()
                    .map(|p| p.root.join(".rustpy/dictionaries/project.txt"))
                    .unwrap_or_else(user_dictionary_path);
                match rustpy_spell::add_user_word(path, word.trim()) {
                    Ok(_) => {
                        static_reset_dictionary();
                        for tab in &mut self.tabs {
                            tab.state.semantic_version = None;
                        }
                        word.clear();
                    }
                    Err(error) => self.set_status(error.to_string()),
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(id, word));
        ui.separator();
        ui.heading(tr("Unicode text"));
        ui.checkbox(
            &mut self.settings.editor.unicode_bidi,
            tr("Render bidirectional text and shape Arabic"),
        );
        ui.horizontal(|ui| {
            let direction_label = ui.label(tr("Base direction"));
            egui::ComboBox::from_id_salt("unicode-direction")
                .selected_text(crate::i18n::tr(
                    &self.settings.editor.unicode_base_direction,
                ))
                .show_ui(ui, |ui| {
                    for direction in ["Auto", "LTR", "RTL"] {
                        ui.selectable_value(
                            &mut self.settings.editor.unicode_base_direction,
                            direction.into(),
                            tr(direction),
                        );
                    }
                })
                .combo_labelled(&direction_label);
        });
        ui.small(tr(
            "Display shaping keeps source bytes, identifiers and file spelling unchanged.",
        ));
        self.unicode_selection_tools(ui);
    }
    fn unicode_selection_tools(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr("Normalize selection"));
        ui.small(tr(
            "Normalization is an explicit undoable edit. Compatibility forms can change meaning.",
        ));
        let id = ui.id().with("normalization-form");
        let mut key = ui.data_mut(|d| d.get_temp::<String>(id).unwrap_or_else(|| "NFC".into()));
        egui::ComboBox::from_id_salt("normalization-choice")
            .selected_text(&key)
            .show_ui(ui, |ui| {
                for form in Form::ALL {
                    ui.selectable_value(&mut key, form.key().into(), form.key());
                }
            })
            .combo_named(crate::i18n::tr_static("Normalization form"));
        ui.data_mut(|d| d.insert_temp(id, key.clone()));
        let form = Form::ALL
            .into_iter()
            .find(|f| f.key() == key)
            .unwrap_or(Form::Nfc);
        let Some(index) = self.active.filter(|i| *i < self.tabs.len()) else {
            ui.label(tr("Select text in an editor to preview normalization."));
            return;
        };
        let state = &self.tabs[index].state;
        let caret = state.primary();
        if !caret.has_selection() || state.carets.len() != 1 {
            ui.label(tr("Select one text range to preview normalization."));
            return;
        }
        let original = state.doc.slice(caret.range());
        if original.len() > 64 * 1024 {
            ui.label(tr("Select at most 64 KiB for a Unicode preview."));
            return;
        }
        let normalized = form.apply(&original);
        let version = state.doc.version();
        let identity = state.doc.identity();
        ui.label(tr("Original code points"));
        ui.monospace(codepoints(&original));
        ui.label(tr("Normalized code points"));
        ui.monospace(codepoints(&normalized));
        ui.label(format!("{} → {} bytes", original.len(), normalized.len()));
        ui.collapsing(tr("Preview"), |ui| {
            ui.monospace(&normalized);
        });
        if ui
            .add_enabled(
                original != normalized && !state.doc.is_read_only(),
                egui::Button::new(tr("Apply normalization to selection")),
            )
            .clicked()
        {
            let state = &mut self.tabs[index].state;
            if state.doc.identity() == identity
                && state.doc.version() == version
                && state.primary() == caret
                && state.doc.slice(caret.range()) == original
            {
                state.apply_text_edits(
                    vec![(caret.range(), normalized)],
                    "Normalize Unicode selection",
                );
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_is_opt_in_and_preserves_original_fixture() {
        let source = "cafe\u{301} Ａ ﬃ 👩🏽‍💻";
        let original = source.as_bytes().to_vec();
        assert_eq!(Form::Nfc.apply(source), "café Ａ ﬃ 👩🏽‍💻");
        assert_eq!(Form::Nfkc.apply(source), "café A ffi 👩🏽‍💻");
        assert_eq!(source.as_bytes(), original);
        assert_eq!(Form::Nfd.apply("각"), "각");
    }
    #[test]
    fn normalization_uses_the_same_undo_authority_as_source_edits() {
        let original = "cafe\u{301}";
        let mut state =
            crate::editor::EditorState::from_text(original, crate::editor::Language::PlainText);
        let range = rustpy_core::TextRange::new(rustpy_core::TextSize::new(0), state.doc.len());
        assert!(state.apply_text_edits(
            vec![(range, Form::Nfc.apply(original))],
            "Normalize Unicode selection"
        ));
        assert_eq!(state.doc.text(), "café");
        assert!(state.undo());
        assert_eq!(state.doc.text().as_bytes(), original.as_bytes());
        state.doc.set_read_only(true);
        assert!(!state.apply_text_edits(
            vec![(range, "changed".into())],
            "Normalize Unicode selection"
        ));
        assert_eq!(state.doc.text(), original);
    }
    #[test]
    fn four_language_packs_cover_actions_and_daily_tips() {
        for locale in ["en", "zh-CN", "ja", "ko"] {
            let (done, total) = rustpy_i18n::translation_coverage(locale).unwrap();
            assert_eq!(done, total, "{locale}");
            for &action in Action::ALL {
                assert!(!rustpy_i18n::tr_in(locale, action.english_text()).is_empty());
            }
            for &tip in TIPS {
                let text = rustpy_i18n::tr_in(locale, tip);
                assert!(!text.is_empty());
                if locale != "en" {
                    assert_ne!(text, tip);
                }
            }
        }
    }
}

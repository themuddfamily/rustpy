//! Settings › Editor › Font and the UI font on Appearance (roadmap §1.6):
//! families from the installed fonts, ligatures, and the fallback fonts that
//! draw the scripts the chosen font lacks.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::fonts::{self, BUNDLED_EDITOR_FONT};

/// Text that shows the font, its ligatures and the fallback scripts.
const FONT_SAMPLE: &str = "abc ABC 0O 1lI {}[] -> != >= === // ~\n中文 日本語 한국어 · العربية · עברית · ไทย · हिन्दी · Ελληνικά";

/// A font family chooser: `default` (value, label) first, then `names`
/// (`None` while the installed fonts are not known yet).
fn family_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut String,
    names: Option<&[String]>,
    default: (&str, &str),
    name_label: &egui::Response,
) -> bool {
    let is_default = |v: &str| v.trim().is_empty() || v.trim().eq_ignore_ascii_case(default.0);
    let label = if is_default(value) {
        default.1.to_string()
    } else {
        value.clone()
    };
    let before = value.clone();
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(label)
        .width(260.0)
        .height(420.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(is_default(value), default.1).clicked() {
                *value = default.0.to_string();
            }
            match names {
                None => {
                    ui.label("Looking for installed fonts…");
                }
                Some(list) => {
                    for name in list.iter().filter(|n| !is_default(n)) {
                        if ui
                            .selectable_label(value.trim().eq_ignore_ascii_case(name), name)
                            .clicked()
                        {
                            *value = name.clone();
                        }
                    }
                }
            }
        })
        .combo_labelled(name_label);
    *value != before
}

impl IdeApp {
    /// Settings › Editor › Font (without the color scheme preview).
    pub(super) fn editor_font_page(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let show_all_id = egui::Id::new("rustpy.settings.font.show_all");
        let mut show_all = ui.data(|d| d.get_temp::<bool>(show_all_id).unwrap_or(false));
        let names = fonts::family_names(!show_all);
        let e = &mut self.settings.editor;
        egui::Grid::new("font_grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            let font_label = ui.label("Font");
            ui.horizontal(|ui| {
                family_combo(ui, "editor_font_family", &mut e.font_family, names.as_deref(), (BUNDLED_EDITOR_FONT, "Hack (bundled)"), &font_label);
                if ui.checkbox(&mut show_all, "Show non-monospaced fonts").changed() {
                    ui.data_mut(|d| d.insert_temp(show_all_id, show_all));
                }
            });
            ui.end_row();
            ui.label("Size");
            ui.add(egui::Slider::new(&mut e.font_size, 8.0..=32.0));
            ui.end_row();
            ui.label("Line height");
            ui.add(egui::Slider::new(&mut e.line_spacing, 1.0..=2.0));
            ui.end_row();
            ui.label("Enable ligatures");
            ui.checkbox(&mut e.enable_ligatures, "").on_hover_text("Draw sequences such as -> != >= as one symbol, in fonts that have ligatures (Fira Code, Cascadia Code, …)");
            ui.end_row();
        });
        ui.label(
            "Secondary / fallback font families, in priority order (one installed family per line)",
        );
        let mut fallback = e.preferences.font_fallbacks.join("\n");
        if ui.text_edit_multiline(&mut fallback).changed() {
            e.preferences.font_fallbacks = fallback
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .take(16)
                .map(str::to_owned)
                .collect();
        }
        let note = |ui: &mut egui::Ui, text: String, color: Color32| {
            ui.label(egui::RichText::new(text).color(color).small());
        };
        let family = self.settings.editor.font_family.trim().to_string();
        match fonts::is_installed(&family) {
            Some(false) => note(
                ui,
                format!("\"{family}\" is not installed: the bundled Hack is used."),
                theme.warning,
            ),
            None => note(ui, "Looking for installed fonts…".into(), theme.text_dim),
            Some(true) => {}
        }
        let fallbacks = fonts::fallback_names();
        if fallbacks.is_empty() && fonts::scanning() {
            note(ui, "Fallback fonts for Chinese, Japanese, Korean, Arabic, Hebrew, Thai and other scripts: looking for installed fonts…".into(), theme.text_dim);
        } else if fallbacks.is_empty() {
            note(ui, "No fallback fonts found: install a font such as Noto Sans CJK to see Chinese, Japanese or Korean text.".into(), theme.warning);
        } else {
            note(ui, format!("Characters the font lacks (Chinese, Japanese, Korean, Arabic, Hebrew, Thai, …) come from: {}.", fallbacks.join(", ")), theme.text_dim);
        }
        ui.add_space(4.0);
        let size = self.settings.editor.font_size.max(8.0);
        let fg = rgb(self.scheme.editor.foreground);
        egui::Frame::new()
            .fill(rgb(self.scheme.editor.background))
            .inner_margin(egui::Margin::same(8))
            .corner_radius(4)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    egui::RichText::new(FONT_SAMPLE)
                        .font(egui::FontId::new(
                            size,
                            crate::theme::mono_family(false, false),
                        ))
                        .color(fg),
                );
            });
        ui.add_space(8.0);
    }

    /// Appearance › UI font: a family for the menus, dialogs and tool windows.
    pub(super) fn ui_font_row(&mut self, ui: &mut egui::Ui) {
        let names = fonts::family_names(false);
        ui.horizontal(|ui| {
            let font_label = ui.label("UI font");
            family_combo(
                ui,
                "ui_font_family",
                &mut self.settings.appearance.ui_font_family,
                names.as_deref(),
                ("", "Default"),
                &font_label,
            );
        });
        let family = self.settings.appearance.ui_font_family.trim().to_string();
        if !family.is_empty() && fonts::is_installed(&family) == Some(false) {
            ui.label(
                egui::RichText::new(format!(
                    "\"{family}\" is not installed: the default font is used."
                ))
                .color(self.theme.warning)
                .small(),
            );
        }
    }
}

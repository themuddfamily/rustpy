//! Per-file edit options and the status-bar "Indent" widget (roadmap §2.9
//! "Indentation detection & Indent status bar widget").

use super::*;
use crate::editor::indent_options::{self, IndentSource};
use rustpy_text::util::IndentStyle;

impl IdeApp {
    /// Edit options for tab `i`: the settings plus its file's detected
    /// indentation, EditorConfig and the indentation chosen in the status bar.
    pub(super) fn edit_options_for(&self, i: usize) -> EditOptions {
        match self.tabs.get(i) {
            Some(tab) => {
                let mut settings = self.settings.editor.clone();
                if let Some(path) = tab.state.path.as_deref() {
                    self.settings.code_style.apply_language(path, &mut settings);
                }
                let mut opts = indent_options::file_edit_options(&settings, &tab.state).0;
                opts.page_lines = tab.view.page_lines;
                opts.confirm_refactoring_undo = self.settings.refactoring.confirm_undo
                    || self.needs_grouped_refactoring_undo(&tab.state.doc);
                let style = &self.settings.code_style;
                opts.paste_format_options.indent = opts.indent_size.max(1);
                opts.paste_format_options.continuation_indent = opts.continuation_indent;
                opts.paste_format_options.tabs =
                    opts.use_tabs.then_some(rustpy_format::TabIndent {
                        width: opts.tab_size,
                        smart: opts.smart_tabs.unwrap_or(false),
                    });
                opts.paste_format_options.max_line_length = formatter_margin(opts.right_margin);
                opts.paste_format_options.quote = match style.quote_style.as_str() {
                    "double" => rustpy_format::QuoteStyle::Double,
                    "single" => rustpy_format::QuoteStyle::Single,
                    _ => rustpy_format::QuoteStyle::Preserve,
                };
                opts.paste_format_options.magic_trailing_comma = style.magic_trailing_comma;
                opts.paste_format_options.blank_lines_top_level = style.blank_lines_top_level;
                opts.paste_format_options.blank_lines_in_class = style.blank_lines_in_class;
                rustpy_format::style_policy::apply(
                    &mut opts.paste_format_options,
                    style,
                    &Default::default(),
                );
                if opts.continuation_indent.is_none() {
                    opts.continuation_indent = opts.paste_format_options.continuation_indent;
                }
                if opts.smart_tabs.is_none() {
                    opts.smart_tabs = Some(style.python.indents.smart_tabs);
                }
                if let Some(path) = &tab.state.path {
                    let ec = self.settings.editor.editorconfig_for(path);
                    apply_ide_format_options(&ec, &mut opts.paste_format_options);
                    rustpy_format::style_policy::apply(&mut opts.paste_format_options, style, &ec);
                    if self.code_style_excludes(path).unwrap_or(true) {
                        opts.paste_reformat = false;
                    }
                    if ec.ide.formatter_enabled == Some(false) {
                        opts.paste_reformat = false;
                    }
                }
                opts
            }
            None => self.base_edit_options(),
        }
    }

    /// Status-bar indentation widget for the active editor: shows the effective
    /// indentation and where it comes from; its menu changes it for this file only.
    pub(super) fn indent_status_widget(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        let theme = self.theme.clone();
        let mut settings = self.settings.editor.clone();
        if let Some(path) = self.tabs[i].state.path.as_deref() {
            self.settings.code_style.apply_language(path, &mut settings);
        }
        let (opts, source) = indent_options::file_edit_options(&settings, &self.tabs[i].state);
        let current = indent_options::style_of(&opts);
        let label = match source {
            IndentSource::Settings => current.label(),
            IndentSource::Detected => format!("{} (detected)", current.label()),
            IndentSource::EditorConfig => format!("{} (EditorConfig)", current.label()),
            IndentSource::File => format!("{} (file)", current.label()),
        };
        let file_name = self.tabs[i].state.title();
        let mut open_settings = false;
        let mut editorconfig_preview = false;
        let mut redetect = false;
        let mut save_preferences = false;
        let resp = ui.menu_button(egui::RichText::new(label).color(theme.text), |ui| {
            ui.label(
                egui::RichText::new(format!("Indent for {file_name}"))
                    .color(theme.text_dim)
                    .small(),
            );
            let tab = &mut self.tabs[i];
            for style in [
                IndentStyle::Spaces(2),
                IndentStyle::Spaces(4),
                IndentStyle::Spaces(8),
                IndentStyle::Tabs,
            ] {
                if ui
                    .selectable_label(current == style, style.label())
                    .clicked()
                {
                    tab.state.indent_override = Some(style);
                    ui.close();
                }
            }
            ui.horizontal(|ui| {
                ui.label("Spaces per indent:");
                let mut size = opts.indent_size.clamp(1, 16);
                if ui
                    .add(egui::DragValue::new(&mut size).range(1..=16))
                    .changed()
                {
                    tab.state.indent_override = Some(IndentStyle::Spaces(size));
                }
            });
            if tab.state.indent_override.is_some()
                && ui
                    .button("Use Default Indent (settings, detected or EditorConfig)")
                    .clicked()
            {
                tab.state.indent_override = None;
                ui.close();
            }
            ui.separator();
            for (to_tabs, text) in [
                (false, "Convert Indents to Spaces"),
                (true, "Convert Indents to Tabs"),
            ] {
                if ui.button(crate::i18n::tr_static(text)).clicked() {
                    // The whole file: drop selections first (the command converts selected lines only).
                    let carets: Vec<crate::editor::Caret> = tab
                        .state
                        .carets
                        .iter()
                        .map(|c| crate::editor::Caret::at(c.head()))
                        .collect();
                    tab.state.set_carets(carets);
                    tab.state.convert_indents(to_tabs, &opts);
                    let size = if opts.use_tabs {
                        opts.tab_size
                    } else {
                        opts.indent_size
                    };
                    tab.state.indent_override = Some(if to_tabs {
                        IndentStyle::Tabs
                    } else {
                        IndentStyle::Spaces(size.max(1))
                    });
                    ui.close();
                }
            }
            ui.separator();
            if ui
                .checkbox(
                    &mut self.settings.editor.detect_indents,
                    "Detect and use existing file indents",
                )
                .changed()
            {
                save_preferences = true;
                redetect = self.settings.editor.detect_indents;
            }
            if ui
                .checkbox(
                    &mut self.settings.editor.editorconfig_enabled,
                    "Enable EditorConfig",
                )
                .changed()
            {
                save_preferences = true;
            }
            if ui.button("EditorConfig preview…").clicked() {
                editorconfig_preview = true;
                ui.close();
            }
            if ui.button("Configure Indents…").clicked() {
                open_settings = true;
                ui.close();
            }
        });
        resp.response.on_hover_text(format!(
            "Indent: {}, {}. Click to change it for this file.",
            current.label(),
            source.describe()
        ));
        if redetect {
            for tab in &mut self.tabs {
                if !tab.state.is_large_file() {
                    tab.state.redetect_indent();
                }
            }
        }
        if save_preferences {
            if let Err(error) = self.settings.save() {
                self.set_status(format!("Cannot save indentation preferences: {error}"));
            }
        }
        if editorconfig_preview {
            self.dialog = Dialog::EditorConfig(Box::new(self.editorconfig_preview_state()));
        }
        if open_settings {
            self.settings_ui.page = settings_ui::SettingsPage::CodeStyle;
            self.dialog = Dialog::Settings;
        }
    }
}

/// Zero means no length-based wrapping, not the formatter's minimum width.
pub(super) fn formatter_margin(margin: usize) -> usize {
    if margin == 0 {
        usize::MAX / 4
    } else {
        margin.max(40)
    }
}

pub(super) fn apply_ide_format_options(
    ec: &rustpy_settings::editorconfig::EditorConfigProps,
    options: &mut rustpy_format::FormatOptions,
) {
    options.formatter_tags = imported_formatter_tags(ec);
    if let Some(n) = ec.ide.blank_lines_top_level {
        options.blank_lines_top_level = n;
    }
    if let Some(n) = ec.ide.blank_lines_in_class {
        options.blank_lines_in_class = n;
    }
}

pub(super) fn imported_formatter_tags(
    ec: &rustpy_settings::editorconfig::EditorConfigProps,
) -> Option<rustpy_format::FormatterTags> {
    (ec.ide.formatter_tags_enabled == Some(true)).then(|| rustpy_format::FormatterTags {
        off: ec
            .ide
            .formatter_off_tag
            .clone()
            .unwrap_or_else(|| "@formatter:off".into()),
        on: ec
            .ide
            .formatter_on_tag
            .clone()
            .unwrap_or_else(|| "@formatter:on".into()),
    })
}

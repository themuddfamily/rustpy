//! Captured, modal Emmet preview and surround; explicit custom abbreviation editor.
use super::*;
use crate::a11y::DialogWindow;
use std::collections::BTreeMap;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Snippets {
    html: BTreeMap<String, String>,
    css: BTreeMap<String, String>,
}
impl Snippets {
    fn validate(&self) -> Result<(), String> {
        if self.html.len() + self.css.len() > 256
            || self.html.iter().chain(&self.css).any(|(key, value)| {
                key.is_empty() || key.len() > 256 || value.is_empty() || value.len() > 8192
            })
        {
            Err("Use at most 256 nonempty snippet names and bounded abbreviations".into())
        } else {
            Ok(())
        }
    }
    fn for_context(&self, language: &str, text: &str, offset: usize) -> &BTreeMap<String, String> {
        if rustpy_emmet::stylesheet_context(text, offset, language) {
            &self.css
        } else {
            &self.html
        }
    }
}
fn load(path: &Path) -> Result<Snippets, String> {
    let snapshot = match rustpy_core::vfs::Vfs::new().read(
        path,
        rustpy_core::vfs::ReadLimits {
            max_file_bytes: 1024 * 1024,
            max_cached_bytes: 0,
            max_cached_files: 0,
        },
    ) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snippets::default())
        }
        Err(error) => return Err(format!("Cannot load Emmet snippets: {error}")),
    };
    let value: Snippets = serde_json::from_slice(&snapshot.bytes)
        .map_err(|error| format!("Invalid Emmet snippets: {error}"))?;
    value.validate()?;
    Ok(value)
}
fn save(path: &Path, value: &Snippets) -> Result<(), String> {
    use std::io::Write;
    value.validate()?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Emmet snippets exceed 1 MiB".into());
    }
    let parent = path.parent().ok_or("Missing configuration directory")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

struct Preview {
    id: rustpy_text::DocumentId,
    version: u64,
    carets: Vec<crate::editor::Caret>,
    path: Option<PathBuf>,
    source_language: Language,
    virtual_carets: Vec<crate::editor::state::virtual_space::VirtualCaret>,
    column_mode: bool,
    text: String,
    range: TextRange,
    language: String,
    indent: String,
    abbreviation: String,
    surround: bool,
    snippets: Snippets,
    config: PathBuf,
    config_text: String,
    editing: bool,
    error: Option<String>,
    cached: Option<rustpy_emmet::Expansion>,
    cache_key: Option<String>,
}
#[derive(Default)]
pub(super) struct EmmetUi {
    preview: Option<Preview>,
}
impl EmmetUi {
    pub(super) fn active(&self) -> bool {
        self.preview.is_some()
    }
}

fn language(state: &EditorState) -> Option<&str> {
    let extension = state
        .path
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str());
    match state.language {
        Language::Html => Some(if matches!(extension, Some("xml" | "svg")) {
            "xml"
        } else {
            "html"
        }),
        Language::Css => Some(match extension {
            Some("scss") => "scss",
            Some("less") => "less",
            Some("sass") => "sass",
            _ => "css",
        }),
        Language::Template => Some("jinja"),
        Language::JavaScript if matches!(extension, Some("jsx" | "tsx")) => Some("jsx"),
        _ => None,
    }
}
impl Preview {
    fn expansion(&self) -> Option<rustpy_emmet::Expansion> {
        rustpy_emmet::configured_at(
            &self.text,
            usize::from(self.range.start())..usize::from(self.range.end()),
            &self.abbreviation,
            &self.language,
            &self.indent,
            self.snippets
                .for_context(&self.language, &self.text, usize::from(self.range.start())),
            self.surround.then(|| {
                &self.text[usize::from(self.range.start())..usize::from(self.range.end())]
            }),
        )
    }
}
fn configuration_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_CONFIGURATION.with(|value| value.borrow().clone()) {
        return path;
    }
    Settings::config_dir().join("emmet-snippets.json")
}
#[cfg(test)]
thread_local! {
    static TEST_CONFIGURATION: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

impl IdeApp {
    pub(super) fn expand_configured_emmet(&mut self) -> bool {
        let Some(index) = self.active.filter(|&index| index < self.tabs.len()) else {
            return false;
        };
        self.refresh_file_permissions(index);
        let state = &self.tabs[index].state;
        let Some(language) = language(state) else {
            return false;
        };
        if state.carets.len() != 1
            || state.has_selection()
            || state.column_mode
            || state.doc.is_read_only()
        {
            return false;
        }
        let caret = state.primary();
        let position = state.virtual_caret(0);
        if position.anchor != state.doc.offset_to_position(caret.sel.anchor)
            || position.head != state.doc.offset_to_position(caret.head())
        {
            return false;
        }
        let text = state.doc.text();
        let snippets = match load(&configuration_path()) {
            Ok(value) => value,
            Err(error) => {
                self.set_status(error);
                return false;
            }
        };
        let opts = self.edit_options_for(index);
        let indent = if opts.use_tabs {
            "\t".into()
        } else {
            " ".repeat(opts.indent_size.max(1))
        };
        let offset = usize::from(caret.head());
        let Some((range, expansion)) = rustpy_emmet::configured_before(
            &text,
            offset,
            language,
            &indent,
            snippets.for_context(language, &text, offset),
        ) else {
            return false;
        };
        let new_caret =
            crate::editor::Caret::at(TextSize::new((range.start + expansion.caret) as u32));
        self.tabs[index].state.apply(
            vec![rustpy_text::Edit::replace(
                TextRange::new(
                    TextSize::new(range.start as u32),
                    TextSize::new(range.end as u32),
                ),
                expansion.text,
            )],
            vec![new_caret],
            rustpy_text::EditGroup::Separate,
            Some("Emmet"),
        )
    }

    pub(super) fn open_emmet_preview(&mut self, surround: bool) {
        self.open_emmet_at(surround, configuration_path());
    }
    fn open_emmet_at(&mut self, surround: bool, config: PathBuf) {
        let Some(index) = self.active.filter(|&index| index < self.tabs.len()) else {
            return;
        };
        let state = &self.tabs[index].state;
        let Some(language) = language(state) else {
            self.set_status("Emmet requires markup or stylesheet source");
            return;
        };
        if state.carets.len() != 1 || state.column_mode || state.doc.is_read_only() {
            self.set_status("Emmet requires one writable ordinary caret");
            return;
        }
        let caret = state.primary();
        let virtual_position = state.virtual_caret(0);
        if virtual_position.anchor != state.doc.offset_to_position(caret.sel.anchor)
            || virtual_position.head != state.doc.offset_to_position(caret.head())
        {
            self.set_status("Emmet requires a source selection or caret");
            return;
        }
        if surround && !caret.has_selection() {
            self.set_status("Select text to surround with Emmet");
            return;
        }
        let text = state.doc.text();
        let mut range = caret.range();
        let mut abbreviation = String::from("div");
        if !surround {
            let offset = usize::from(caret.head());
            let extracted = if rustpy_emmet::is_css_context(language) {
                rustpy_emmet::css_abbreviation_before(&text, offset)
            } else {
                rustpy_emmet::abbreviation_before(&text, offset)
            };
            if let Some((start, abbr)) = extracted {
                range = TextRange::new(
                    TextSize::new(start as u32),
                    TextSize::new((start + abbr.len()) as u32),
                );
                abbreviation = abbr.into();
            }
        }
        let snippets = match load(&config) {
            Ok(value) => value,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let config_text = serde_json::to_string_pretty(&snippets).unwrap_or_default();
        let opts = self.edit_options_for(index);
        let indent = if opts.use_tabs {
            "\t".into()
        } else {
            " ".repeat(opts.indent_size.max(1))
        };
        self.emmet_ui.preview = Some(Preview {
            id: state.doc.identity(),
            version: state.doc.version(),
            carets: state.carets.clone(),
            path: state.path.clone(),
            source_language: state.language,
            virtual_carets: (0..state.carets.len())
                .map(|i| state.virtual_caret(i))
                .collect(),
            column_mode: state.column_mode,
            text,
            range,
            language: language.into(),
            indent,
            abbreviation,
            surround,
            snippets,
            config,
            config_text,
            editing: false,
            error: None,
            cached: None,
            cache_key: None,
        });
        self.completion.visible = false;
        self.focus_editor = false;
    }

    pub(super) fn emmet_preview_ui(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::None) {
            self.emmet_ui.preview = None;
            return;
        }
        let Some(mut preview) = self.emmet_ui.preview.take() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(if preview.surround {
            "Surround with Emmet"
        } else {
            "Emmet Preview"
        })
        .collapsible(false)
        .resizable(true)
        .default_width(620.)
        .open(&mut open)
        .show_modal(ctx, |ui| {
            ui.label("Abbreviation (filters: |c comments, |e escaped markup, |s single line)");
            ui.add(
                egui::TextEdit::singleline(&mut preview.abbreviation)
                    .char_limit(8192)
                    .desired_width(f32::INFINITY),
            );
            if preview.cache_key.as_deref() != Some(preview.abbreviation.as_str()) {
                preview.cached = preview.expansion();
                preview.cache_key = Some(preview.abbreviation.clone());
            }
            let expansion = preview.cached.clone();
            let mut text = expansion
                .as_ref()
                .map(|expansion| expansion.text.clone())
                .unwrap_or_else(|| {
                    "Invalid abbreviation or unavailable in this source context".into()
                });
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .interactive(false)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8),
            );
            if ui.button("Custom snippets…").clicked() {
                preview.editing = !preview.editing;
            }
            if preview.editing {
                ui.label("JSON: html/css maps from name to abbreviation. Saved for this IDE.");
                ui.add(
                    egui::TextEdit::multiline(&mut preview.config_text)
                        .code_editor()
                        .desired_rows(5),
                );
                if ui.button("Save snippets").clicked() {
                    let result = serde_json::from_str::<Snippets>(&preview.config_text)
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            save(&preview.config, &value)?;
                            Ok(value)
                        });
                    match result {
                        Ok(value) => {
                            preview.snippets = value;
                            preview.error = None;
                            preview.cache_key = None
                        }
                        Err(error) => preview.error = Some(error),
                    }
                }
            }
            if let Some(error) = &preview.error {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(expansion.is_some(), egui::Button::new("Apply"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if !preview.editing
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                apply = expansion.is_some();
            }
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                cancel = true;
            }
        });
        if !open || cancel {
            self.focus_editor = true;
            return;
        }
        if apply {
            match self.apply_emmet_preview(&preview) {
                Ok(()) => {
                    self.focus_editor = true;
                    return;
                }
                Err(error) => preview.error = Some(error),
            }
        }
        self.emmet_ui.preview = Some(preview);
    }

    fn apply_emmet_preview(&mut self, preview: &Preview) -> Result<(), String> {
        let Some(index) = self.active.filter(|&index| index < self.tabs.len()) else {
            return Err("Source editor was closed".into());
        };
        self.refresh_file_permissions(index);
        let state = &mut self.tabs[index].state;
        if state.doc.identity() != preview.id
            || state.doc.version() != preview.version
            || state.carets != preview.carets
            || state.path != preview.path
            || state.language != preview.source_language
            || state.column_mode != preview.column_mode
            || (0..state.carets.len())
                .map(|i| state.virtual_caret(i))
                .collect::<Vec<_>>()
                != preview.virtual_carets
            || state.doc.is_read_only()
        {
            return Err("Source changed; reopen the Emmet preview".into());
        }
        let expansion = preview.expansion().ok_or("Invalid abbreviation")?;
        let caret =
            crate::editor::Caret::at(preview.range.start() + TextSize::new(expansion.caret as u32));
        if !state.apply(
            vec![rustpy_text::Edit::replace(preview.range, expansion.text)],
            vec![caret],
            rustpy_text::EditGroup::Separate,
            Some("Emmet"),
        ) {
            return Err("Emmet edit was refused".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "emmet_ui_tests.rs"]
mod tests;

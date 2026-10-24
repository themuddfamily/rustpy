//! Markdown editor, native/browser previews and owned export sessions.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::{
    markdown_view::{PreviewEvent, Resources},
    markdown_workspace as md,
};
use rustpy_settings::markdown::{Layout, Split};
use std::{collections::HashMap, io::Write};
#[derive(Default)]
struct Scroll {
    source: usize,
    preview: f32,
}
struct Artifact {
    id: u64,
    root: PathBuf,
    file: Option<PathBuf>,
    version: u64,
    directory: tempfile::TempDir,
    output: Option<Export>,
}
struct Export {
    destination: PathBuf,
    temporary: PathBuf,
    baseline: Option<rustpy_core::vfs::ContentRevision>,
}
#[derive(Default)]
pub(super) struct Workspace {
    resources: Resources,
    scroll: HashMap<egui::Id, Scroll>,
    pub(super) initialized: bool,
    completion: bool,
    completion_prefix: String,
    completion_capture: Option<(egui::Id, u64, TextRange)>,
    export_path: String,
    overwrite: bool,
    artifacts: Vec<Artifact>,
    message: String,
    inspected: Option<(egui::Id, u64)>,
    inspection_time: Option<std::time::Instant>,
    diagnostics: Vec<rustpy_core::Diagnostic>,
}
fn baseline(path: &Path) -> anyhow::Result<Option<rustpy_core::vfs::ContentRevision>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_symlink() || !metadata.is_file() || metadata.permissions().readonly() {
                anyhow::bail!("Export destination is not a writable regular file");
            }
            Ok(Some(rustpy_core::vfs::ContentRevision::of(&std::fs::read(
                path,
            )?)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn publish(
    path: &Path,
    bytes: &[u8],
    expected: Option<rustpy_core::vfs::ContentRevision>,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing export parent directory"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    if baseline(path)? != expected {
        anyhow::bail!("Export destination changed while the export was prepared");
    }
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn range(range: std::ops::Range<usize>) -> TextRange {
    TextRange::new(
        TextSize::new(range.start as u32),
        TextSize::new(range.end as u32),
    )
}
struct PreviewLocation {
    view: egui::Id,
    source: usize,
}
fn preview(
    ui: &mut egui::Ui,
    workspace: &mut Workspace,
    theme: &UiTheme,
    text: &str,
    file: Option<&Path>,
    settings: &rustpy_settings::markdown::Settings,
    location: PreviewLocation,
) -> (Vec<PreviewEvent>, Option<usize>) {
    let PreviewLocation { view: id, source } = location;
    let scroll = workspace.scroll.entry(id).or_default();
    let desired = workspace
        .resources
        .lines
        .iter()
        .rev()
        .find(|(line, _)| *line <= source)
        .map(|(_, y)| *y)
        .unwrap_or(0.);
    let mut area = egui::ScrollArea::vertical()
        .id_salt(("md_preview_scroll", id))
        .auto_shrink([false, false]);
    if settings.sync_scroll && scroll.source != source {
        area = area.vertical_scroll_offset(desired);
    }
    let mut events = Vec::new();
    let output = area.show(ui, |ui| {
        ui.set_width(ui.available_width());
        events = crate::markdown_view::render_document(
            ui,
            theme,
            text,
            settings.font_size.clamp(8., 48.),
            file,
            settings,
            &mut workspace.resources,
        );
    });
    let user_scrolled = ui.input(|i| i.smooth_scroll_delta.y.abs() > 0.)
        && ui.rect_contains_pointer(output.inner_rect);
    let target = if settings.sync_scroll
        && user_scrolled
        && (scroll.preview - output.state.offset.y).abs() > 1.
    {
        workspace
            .resources
            .lines
            .iter()
            .rev()
            .find(|(_, y)| *y <= output.state.offset.y)
            .map(|(line, _)| *line)
    } else {
        None
    };
    scroll.source = target.unwrap_or(source);
    scroll.preview = output.state.offset.y;
    (events, target)
}
/// The Markdown completion context at `offset`: the replaced range start and
/// whether it is a fence language (else a link target), if any.
fn markdown_completion_context(source: &str, offset: usize) -> Option<(usize, bool)> {
    let range = md::completion_prefix(source, offset);
    let line = source[..range.start].rfind('\n').map_or(0, |at| at + 1);
    let head = &source[line..range.start];
    let typed = &source[range.clone()];
    if head.ends_with("](") {
        // A link target still being typed (not a closed `(…)` earlier on the line).
        (!typed.contains([')', ' ', '\t'])).then_some((range.start, false))
    } else if head.trim_start() == "```" {
        // Only an opening fence (an even number of fences above) at the start
        // of the line takes a language.
        let fences_above = source[..line]
            .lines()
            .filter(|previous| previous.trim_start().starts_with("```"))
            .count();
        (fences_above % 2 == 0 && !typed.contains(char::is_whitespace))
            .then_some((range.start, true))
    } else {
        None
    }
}

impl IdeApp {
    /// Markdown completion in the shared lookup: heading anchors and files for
    /// link targets, languages after an opening code fence.
    pub(super) fn open_markdown_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.language != Language::Markdown {
            return false;
        }
        let source = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let Some((start, fence)) = markdown_completion_context(&source, usize::from(caret)) else {
            return false;
        };
        let prefix = &source[start..usize::from(caret)];
        let (labels, detail): (Vec<String>, &str) = if fence {
            (
                md::FENCE_LANGUAGES
                    .iter()
                    .filter(|language| language.starts_with(prefix))
                    .map(|language| language.to_string())
                    .collect(),
                "Code fence language",
            )
        } else {
            (
                md::completions(&source, tab.state.path.as_deref(), prefix),
                "Markdown link target",
            )
        };
        let items = labels
            .into_iter()
            .take(150)
            .map(|label| CompletionItem {
                insert: label.clone(),
                label: label.into(),
                detail: detail.into(),
                kind: if fence {
                    CompletionKind::Keyword
                } else {
                    CompletionKind::Variable
                },
                caret_offset: None,
                priority: 0,
            })
            .collect::<Vec<_>>();
        let version = tab.state.doc.version();
        self.cancel_basic_completion();
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.completion_ui.due = None;
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: 0,
            prefix_start: TextSize::new(start as u32),
            doc_version: version,
            caret,
            explicit,
            rect: None,
        };
        true
    }
    fn markdown_apply(&mut self, i: usize, edits: Vec<(TextRange, String)>, label: &str) {
        if self.tabs[i].state.apply_text_edits(edits, label) {
            self.tabs[i].state.scroll_to_caret = true;
            self.focus_editor = true;
        }
    }
    fn markdown_insert(&mut self, i: usize, before: &str, after: &str, placeholder: &str) {
        let selection = self.tabs[i].state.primary().range();
        let selected = self.tabs[i].state.doc.slice(selection);
        let selected = if selected.is_empty() {
            placeholder
        } else {
            &selected
        };
        self.markdown_apply(
            i,
            vec![(selection, format!("{before}{selected}{after}"))],
            "Markdown toolbar",
        );
    }
    fn markdown_toolbar(&mut self, ui: &mut egui::Ui, i: usize, workspace: &mut Workspace) {
        ui.horizontal_wrapped(|ui| {
            for (label, before, after, placeholder) in [
                ("B", "**", "**", "bold"),
                ("I", "*", "*", "italic"),
                ("Strike", "~~", "~~", "text"),
                ("Code", "`", "`", "code"),
                ("Link", "[", "](url)", "label"),
                ("Image", "![", "](image.png)", "alt text"),
                ("H1", "# ", "", "Heading"),
                ("H2", "## ", "", "Heading"),
                ("• List", "- ", "", "item"),
                ("1. List", "1. ", "", "item"),
                ("Task", "- [ ] ", "", "task"),
            ] {
                if ui.small_button(crate::i18n::tr_static(label)).clicked() {
                    self.markdown_insert(i, before, after, placeholder);
                }
            }
            if ui.small_button("Table").clicked() {
                self.markdown_insert(
                    i,
                    "\n| Header | Header |\n| --- | --- |\n| ",
                    " | |\n",
                    "cell",
                );
            }
            ui.menu_button("Heading", |ui| {
                for level in 1..=6 {
                    if ui.button(format!("Heading {level}")).clicked() {
                        self.markdown_insert(i, &format!("{} ", "#".repeat(level)), "", "Heading");
                        ui.close();
                    }
                }
            });
            ui.menu_button("Table actions", |ui| {
                let text = self.tabs[i].state.doc.text();
                let offset = usize::from(self.tabs[i].state.primary().head());
                if let Some(mut table) = md::tables(&text)
                    .into_iter()
                    .find(|t| t.range.start <= offset && offset <= t.range.end)
                {
                    for (label, action) in [("Format table", 0), ("Add row", 1), ("Add column", 2)]
                    {
                        if ui.button(crate::i18n::tr_static(label)).clicked() {
                            match action {
                                1 => table.add_row(),
                                2 => table.add_column(),
                                _ => {}
                            }
                            self.markdown_apply(
                                i,
                                vec![(range(table.range.clone()), table.format())],
                                "Edit Markdown table",
                            );
                            ui.close();
                        }
                    }
                } else {
                    ui.label("Place the caret in a table");
                }
            });
            if ui.small_button("Reformat Markdown").clicked() {
                self.handle_action(Action::ReformatCode, ui.ctx());
            }
            ui.menu_button("Send to notebook", |ui| {
                let targets = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| tab.notebook.is_some())
                    .map(|(index, tab)| {
                        (
                            index,
                            tab.state
                                .path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "Notebook".into()),
                        )
                    })
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    ui.label("Open a notebook, or export Markdown as .ipynb");
                }
                for (index, label) in targets {
                    if ui.button(label).clicked() {
                        let selection = self.tabs[i].state.primary().range();
                        let source = if selection.is_empty() {
                            self.tabs[i].state.doc.text()
                        } else {
                            self.tabs[i].state.doc.slice(selection)
                        };
                        if let Some(notebook) = self.tabs[index].notebook.as_mut() {
                            workspace.message = match notebook.append_markdown(source) {
                                Ok(()) => "Appended Markdown cell to notebook".into(),
                                Err(error) => {
                                    self.set_status(error.clone());
                                    error
                                }
                            };
                        }
                        ui.close();
                    }
                }
            });
            if ui.small_button("TOC").clicked() {
                let text = self.tabs[i].state.doc.text();
                let toc = md::toc(&text);
                self.markdown_insert(i, &toc, "", "");
            }
            if ui.small_button("Complete link / fence").clicked() {
                workspace.completion = true;
            }
            if ui.small_button("Markdown settings").clicked() {
                self.settings_ui.page = settings_ui::SettingsPage::Markdown;
                self.dialog = Dialog::Settings;
            }
        });
    }
    pub(super) fn markdown_editor_workspace(&mut self, ui: &mut egui::Ui, i: usize) -> bool {
        if !self.builtin_plugin_enabled("markdown") {
            return false;
        }
        let project_safe_mode = self.project_safe_mode();
        let markdown = self.tabs[i].state.language == Language::Markdown;
        let mut workspace = std::mem::take(&mut self.markdown_workspace);
        if !workspace.initialized {
            self.markdown_mode = match self.settings.markdown.layout {
                Layout::Editor => MarkdownMode::EditorOnly,
                Layout::Split => MarkdownMode::Split,
                Layout::Preview => MarkdownMode::PreviewOnly,
            };
            workspace.initialized = true;
        }
        let mut new_mode = self.markdown_mode;
        ui.horizontal_wrapped(|ui| {
            if new_mode == MarkdownMode::EditorOnly {
                ui.label(
                    egui::RichText::new("Preview")
                        .color(self.theme.text_dim)
                        .small(),
                );
            }
            for (label, mode) in [
                ("Editor", MarkdownMode::EditorOnly),
                ("Split", MarkdownMode::Split),
                ("Preview", MarkdownMode::PreviewOnly),
            ] {
                if ui
                    .selectable_label(new_mode == mode, crate::i18n::tr_static(label))
                    .clicked()
                {
                    new_mode = mode;
                }
            }
            if markdown {
                egui::ComboBox::from_id_salt(("md_split", self.tabs[i].view.id))
                    .selected_text(match self.settings.markdown.split {
                        Split::Right => "Preview right",
                        Split::Below => "Preview below",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.settings.markdown.split,
                            Split::Right,
                            "Preview right",
                        );
                        ui.selectable_value(
                            &mut self.settings.markdown.split,
                            Split::Below,
                            "Preview below",
                        );
                    })
                    .combo_named(crate::i18n::tr_static("Preview layout"));
                ui.checkbox(&mut self.settings.markdown.sync_scroll, "Sync scroll");
            }
        });
        if self.markdown_mode != new_mode {
            self.settings.markdown.layout = match new_mode {
                MarkdownMode::EditorOnly => Layout::Editor,
                MarkdownMode::Split => Layout::Split,
                MarkdownMode::PreviewOnly => Layout::Preview,
            };
            let _ = self.settings.save();
            let tab = &mut self.tabs[i];
            tab.view.reanchor_session_scroll(&tab.state);
        }
        self.markdown_mode = new_mode;
        if !markdown {
            self.rst_actions_ui(ui, i);
        }
        if markdown && self.settings.markdown.toolbar {
            self.markdown_toolbar(ui, i, &mut workspace);
        }
        let source = self.tabs[i].state.doc.text();
        let file = self.tabs[i].state.path.clone();
        let version = self.tabs[i].state.doc.version();
        let id = self.tabs[i].view.id;
        if markdown {
            if workspace.completion && workspace.completion_capture.is_none() {
                let offset = usize::from(self.tabs[i].state.primary().head());
                let prefix = md::completion_prefix(&source, offset);
                workspace.completion_prefix = source[prefix.clone()].into();
                workspace.completion_capture = Some((id, version, range(prefix)));
            }
            if workspace
                .completion_capture
                .is_some_and(|(view, revision, _)| view != id || revision != version)
            {
                workspace.completion = false;
                workspace.completion_capture = None;
            }
            if !workspace.completion {
                workspace.completion_capture = None;
            }
            if self.settings.markdown.inspections
                && (workspace.inspected != Some((id, version))
                    || workspace
                        .inspection_time
                        .is_none_or(|time| time.elapsed() > std::time::Duration::from_secs(1)))
            {
                self.tabs[i]
                    .state
                    .extra_diagnostics
                    .retain(|d| !d.code.starts_with("Markdown"));
                workspace.diagnostics.clear();
                if source.len() <= 1024 * 1024 {
                    let diagnostics = md::inspect(&source, file.as_deref())
                        .into_iter()
                        .take(500)
                        .map(|f| {
                            rustpy_core::Diagnostic::warning(
                                range(f.range),
                                "MarkdownUnresolvedReference",
                                f.message,
                            )
                        });
                    workspace.diagnostics = diagnostics.collect();
                }
                workspace.inspected = Some((id, version));
                workspace.inspection_time = Some(std::time::Instant::now());
            } else if !self.settings.markdown.inspections {
                self.tabs[i]
                    .state
                    .extra_diagnostics
                    .retain(|d| !d.code.starts_with("Markdown"));
                workspace.inspected = None;
            }
            if self.settings.markdown.inspections {
                self.tabs[i]
                    .state
                    .extra_diagnostics
                    .retain(|d| !d.code.starts_with("Markdown"));
                self.tabs[i]
                    .state
                    .extra_diagnostics
                    .extend(workspace.diagnostics.iter().cloned());
            }
            // Ctrl+Space is the keymap's Basic Completion: link targets and
            // fence languages appear in the shared lookup (`open_markdown_completion`).
            let editor_focused = ui.memory(|memory| memory.has_focus(id));
            let offset = usize::from(self.tabs[i].state.primary().head());
            let backward = ui.input(|input| input.modifiers.shift);
            if editor_focused
                && !ui.input(|input| {
                    input.modifiers.ctrl || input.modifiers.alt || input.modifiers.command
                })
                && self.tabs[i].state.primary().range().is_empty()
            {
                if let Some(next) = md::next_table_cell(&source, offset, backward) {
                    if ui.input_mut(|input| {
                        input.consume_key(
                            if backward {
                                egui::Modifiers::SHIFT
                            } else {
                                egui::Modifiers::NONE
                            },
                            egui::Key::Tab,
                        )
                    }) {
                        let mut target = next;
                        if self.settings.markdown.auto_format_tables {
                            if let Some(table) = md::tables(&source)
                                .into_iter()
                                .find(|t| t.range.contains(&offset))
                            {
                                let formatted = table.format();
                                self.markdown_apply(
                                    i,
                                    vec![(range(table.range), formatted)],
                                    "Auto-format Markdown table",
                                );
                                let updated = self.tabs[i].state.doc.text();
                                let at = usize::from(self.tabs[i].state.primary().head());
                                target = md::next_table_cell(&updated, at, backward).unwrap_or(at);
                            }
                        }
                        self.tabs[i].state.set_carets(vec![crate::editor::Caret::at(
                            TextSize::new(target as u32),
                        )]);
                        self.tabs[i].state.scroll_to_caret = true;
                    }
                }
            }
            if workspace.completion {
                let mut open = true;
                egui::Window::new("Markdown completion")
                    .id(egui::Id::new("markdown_completion"))
                    .open(&mut open)
                    .show_modeless(ui.ctx(), |ui| {
                        ui.text_edit_singleline(&mut workspace.completion_prefix);
                        for candidate in
                            (if workspace.completion_capture.is_some_and(|(_, _, range)| {
                                source[..usize::from(range.start())]
                                    .trim_end()
                                    .ends_with("```")
                            }) {
                                md::FENCE_LANGUAGES
                                    .iter()
                                    .filter(|language| {
                                        language.starts_with(&workspace.completion_prefix)
                                    })
                                    .map(|language| language.to_string())
                                    .collect::<Vec<_>>()
                            } else {
                                md::completions(
                                    &source,
                                    file.as_deref(),
                                    &workspace.completion_prefix,
                                )
                            })
                            .into_iter()
                            .take(150)
                        {
                            if ui.button(&candidate).clicked() {
                                let selection = workspace
                                    .completion_capture
                                    .map(|(_, _, range)| range)
                                    .unwrap_or_else(|| self.tabs[i].state.primary().range());
                                self.markdown_apply(
                                    i,
                                    vec![(selection, candidate)],
                                    "Complete Markdown reference",
                                );
                                workspace.completion = false;
                            }
                        }
                    });
                workspace.completion &= open;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Copy as HTML").clicked() {
                    ui.ctx().copy_text(md::html_document(
                        &source,
                        file.as_deref(),
                        &self.settings.markdown,
                        false,
                    ));
                }
                if ui
                    .small_button("Open in browser / enhanced preview")
                    .clicked()
                {
                    self.markdown_browser(&source, file.as_deref(), ui.ctx(), &mut workspace);
                }
                ui.menu_button("Export", |ui| {
                    ui.label("Destination (blank uses source basename)");
                    ui.text_edit_singleline(&mut workspace.export_path);
                    ui.checkbox(&mut workspace.overwrite, "Overwrite existing export");
                    for format in ["html", "pdf", "docx", "ipynb"] {
                        if ui.button(format!("Export {format}")).clicked() {
                            self.markdown_export(
                                i,
                                &source,
                                file.as_deref(),
                                format,
                                &mut workspace,
                            );
                        }
                    }
                });
            });
            if !workspace.message.is_empty() {
                ui.label(&workspace.message);
            }
        }
        let text = if markdown {
            source
        } else {
            rustpy_syntax::rst_editing::preview_markdown(&source)
        };
        let mut settings = self.settings.markdown.clone();
        settings.remote_images &= !project_safe_mode;
        let theme = if settings.dark {
            UiTheme::dark()
        } else {
            UiTheme::light()
        };
        let source_line = self.tabs[i]
            .view
            .session_position(&self.tabs[i].state)
            .scroll_line as usize;
        let mut events = Vec::new();
        let mut scroll_to = None;
        let mut draw = |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Preview").color(theme.text_dim).small());
                for (label, m) in [
                    ("Editor", MarkdownMode::EditorOnly),
                    ("Split", MarkdownMode::Split),
                    ("Preview", MarkdownMode::PreviewOnly),
                ] {
                    if ui
                        .selectable_label(new_mode == m, crate::i18n::tr_static(label))
                        .clicked()
                    {
                        new_mode = m;
                    }
                }
            });
            ui.separator();
            (events, scroll_to) = preview(
                ui,
                &mut workspace,
                &theme,
                &text,
                file.as_deref(),
                &settings,
                PreviewLocation {
                    view: id,
                    source: source_line,
                },
            );
        };
        match self.markdown_mode {
            MarkdownMode::Split => {
                let frame = egui::Frame::new()
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(10));
                if settings.split == Split::Below {
                    let height = ui.available_height() / 2.;
                    egui::Panel::bottom(egui::Id::new(("md_preview", id)))
                        .resizable(true)
                        .default_size(height)
                        .min_size(100.)
                        .frame(frame)
                        .show(ui, |ui| draw(ui));
                } else {
                    let width = ui.available_width() / 2.;
                    egui::Panel::right(egui::Id::new(("md_preview", id)))
                        .resizable(true)
                        .default_size(width)
                        .min_size(160.)
                        .frame(frame)
                        .show(ui, |ui| draw(ui));
                }
            }
            MarkdownMode::PreviewOnly => {
                egui::Frame::new()
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        draw(ui);
                    });
            }
            MarkdownMode::EditorOnly => {}
        }
        if let Some(line) = scroll_to {
            let tab = &mut self.tabs[i];
            tab.view.request_scroll_line(&tab.state, line as u32);
        }
        if self.markdown_mode != new_mode {
            self.settings.markdown.layout = match new_mode {
                MarkdownMode::EditorOnly => Layout::Editor,
                MarkdownMode::Split => Layout::Split,
                MarkdownMode::PreviewOnly => Layout::Preview,
            };
            let _ = self.settings.save();
            let tab = &mut self.tabs[i];
            tab.view.reanchor_session_scroll(&tab.state);
        }
        self.markdown_mode = new_mode;
        for event in events {
            match event {
                PreviewEvent::Link(link) => self.markdown_link(i, &link, ui.ctx(), &mut workspace),
                PreviewEvent::RunPython(code) => self.markdown_run_block(i, &code, &mut workspace),
            }
        }
        self.markdown_workspace = workspace;
        self.markdown_mode == MarkdownMode::PreviewOnly
    }
    fn markdown_link(
        &mut self,
        i: usize,
        link: &str,
        ctx: &egui::Context,
        workspace: &mut Workspace,
    ) {
        if !md::safe_url(link) {
            self.set_status("Blocked unsafe Markdown link scheme");
            return;
        }
        if let Some(anchor) = link.strip_prefix('#') {
            workspace.resources.anchor_target = Some(anchor.into());
            return;
        }
        if let Some((path, anchor)) = self.tabs[i]
            .state
            .path
            .as_ref()
            .and_then(|file| md::local_link(file, link))
        {
            if !path.is_file() {
                self.set_status(format!("Markdown link not found: {}", path.display()));
                return;
            }
            self.open_file(&path);
            if let (Some(index), Some(anchor)) = (self.active, anchor) {
                let text = self.tabs[index].state.doc.text();
                if let Some(header) = md::headings(&text).into_iter().find(|h| h.id == anchor) {
                    self.tabs[index]
                        .state
                        .reveal_offset(TextSize::new(header.range.start as u32));
                }
            }
        } else {
            ctx.open_url(egui::OpenUrl::new_tab(link));
        }
    }
    fn markdown_snapshot(&self, source: &str) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
        let base = Settings::config_dir().join("markdown");
        std::fs::create_dir_all(&base)?;
        let directory = tempfile::Builder::new()
            .prefix("snapshot-")
            .tempdir_in(base)?;
        let path = directory.path().join("document.md");
        std::fs::write(&path, source)?;
        Ok((directory, path))
    }
    fn markdown_browser(
        &mut self,
        source: &str,
        file: Option<&Path>,
        ctx: &egui::Context,
        workspace: &mut Workspace,
    ) {
        let project_safe_mode = self.project_safe_mode();
        if let Some(file) = file {
            self.open_web_file(file, None, false);
            return;
        }
        if project_safe_mode {
            workspace.message =
                "Trust the project before opening enhanced previews with external assets".into();
            return;
        }
        let result = (|| -> anyhow::Result<PathBuf> {
            let base = Settings::config_dir().join("markdown");
            std::fs::create_dir_all(&base)?;
            let mut snapshot = tempfile::Builder::new()
                .prefix("preview-")
                .suffix(".html")
                .tempfile_in(base)?;
            snapshot.write_all(
                md::html_document(source, file, &self.settings.markdown, true).as_bytes(),
            )?;
            let (_, path) = snapshot.keep()?;
            Ok(path)
        })();
        match result {
            Ok(path) => {
                ctx.open_url(egui::OpenUrl::new_tab(md::file_url(&path)));
                workspace.message="Browser preview uses the captured source; reopen to refresh. Math/diagram assets use configured endpoints.".into();
            }
            Err(e) => workspace.message = e.to_string(),
        }
    }
    fn markdown_export(
        &mut self,
        i: usize,
        source: &str,
        file: Option<&Path>,
        format: &str,
        workspace: &mut Workspace,
    ) {
        let project_safe_mode = self.project_safe_mode();
        let output = if workspace.export_path.trim().is_empty() {
            file.map(|p| p.with_extension(format))
        } else {
            Some(PathBuf::from(&workspace.export_path))
        };
        let Some(output) = output else {
            workspace.message = "Choose an export destination for an unsaved document".into();
            return;
        };
        let output = if output.is_absolute() {
            output
        } else {
            self.project
                .as_ref()
                .map(|p| p.root.join(&output))
                .unwrap_or(output)
        };
        let output = rustpy_core::vfs::normalize_path(&output);
        if file.is_some_and(|file| rustpy_core::vfs::normalize_path(file) == output) {
            workspace.message = "Export destination must differ from the Markdown source".into();
            return;
        }
        if output.exists() && !workspace.overwrite {
            workspace.message = "Destination exists; enable overwrite to export".into();
            return;
        }
        if self.tabs.iter().any(|tab| {
            tab.state
                .path
                .as_ref()
                .is_some_and(|p| rustpy_core::vfs::normalize_path(p) == output)
                && tab.has_unsaved_changes()
        }) {
            workspace.message = "Save or close the edited export destination first".into();
            return;
        }
        let expected = match baseline(&output) {
            Ok(value) => value,
            Err(error) => {
                workspace.message = error.to_string();
                return;
            }
        };
        if format == "html" || format == "ipynb" {
            let bytes = if format == "html" {
                md::html_document(source, file, &self.settings.markdown, true).into_bytes()
            } else {
                let mut notebook = rustpy_notebook::Notebook::empty();
                notebook.cells.push(rustpy_notebook::Cell::markdown(source));
                match serde_json::to_vec_pretty(&notebook.to_json()) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        workspace.message = error.to_string();
                        return;
                    }
                }
            };
            match publish(&output, &bytes, expected) {
                Ok(()) => {
                    workspace.message = format!("Exported {}", output.display());
                    if format == "ipynb" {
                        self.open_file(&output);
                    }
                }
                Err(error) => workspace.message = error.to_string(),
            }
            return;
        }
        if project_safe_mode {
            workspace.message = "Trust the project before running Pandoc".into();
            return;
        }
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            workspace.message = "Open a project before exporting through Pandoc".into();
            return;
        };
        match self.markdown_snapshot(source) {
            Ok((directory, input)) => {
                let temporary_output = directory.path().join(format!("output.{format}"));
                let cwd = file.and_then(Path::parent).unwrap_or(&root).to_path_buf();
                let mut args = vec![
                    "--from=gfm+tex_math_dollars".into(),
                    input.to_string_lossy().into_owned(),
                    "--standalone".into(),
                    "--output".into(),
                    temporary_output.to_string_lossy().into_owned(),
                    "--resource-path".into(),
                    cwd.to_string_lossy().into_owned(),
                ];
                if format == "pdf" && !self.settings.markdown.pdf_engine.trim().is_empty() {
                    args.push(format!(
                        "--pdf-engine={}",
                        self.settings.markdown.pdf_engine
                    ));
                }
                let cfg = RunConfig {
                    name: format!("Markdown: export {format}"),
                    kind: RunConfigKind::Program {
                        program: PathBuf::from(&self.settings.markdown.pandoc),
                        args,
                    },
                    working_dir: Some(cwd),
                    temporary: true,
                    add_content_roots: false,
                    add_source_roots: false,
                    ..Default::default()
                };
                let id = self.next_session_id;
                self.next_session_id += 1;
                self.run_panel
                    .add_session(RunSession::spawn(id, &cfg, None, &root));
                self.bottom = Some(BottomTool::Run);
                workspace.artifacts.push(Artifact {
                    id,
                    root,
                    file: file.map(Path::to_path_buf),
                    version: self.tabs[i].state.doc.version(),
                    directory,
                    output: Some(Export {
                        destination: output,
                        temporary: temporary_output,
                        baseline: expected,
                    }),
                });
            }
            Err(e) => workspace.message = e.to_string(),
        }
    }
    fn markdown_run_block(&mut self, i: usize, code: &str, workspace: &mut Workspace) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            workspace.message = "Trust the project before running fenced code".into();
            return;
        }
        let Some((root, python)) = self.project.as_ref().and_then(|p| {
            p.interpreter
                .as_ref()
                .map(|sdk| (p.root.clone(), sdk.path.clone()))
        }) else {
            workspace.message = "Select a Python interpreter first".into();
            return;
        };
        match self.markdown_snapshot(code) {
            Ok((directory, _)) => {
                let path = directory.path().join("block.py");
                if let Err(e) = std::fs::write(&path, code) {
                    workspace.message = e.to_string();
                    return;
                }
                let cfg = RunConfig {
                    name: "Markdown: Python block".into(),
                    kind: RunConfigKind::Program {
                        program: python,
                        args: vec!["-u".into(), path.to_string_lossy().into_owned()],
                    },
                    working_dir: self.tabs[i]
                        .state
                        .path
                        .as_ref()
                        .and_then(|p| p.parent().map(Path::to_path_buf)),
                    temporary: true,
                    add_content_roots: false,
                    add_source_roots: false,
                    ..Default::default()
                };
                let id = self.next_session_id;
                self.next_session_id += 1;
                self.run_panel
                    .add_session(RunSession::spawn(id, &cfg, None, &root));
                self.bottom = Some(BottomTool::Run);
                workspace.artifacts.push(Artifact {
                    id,
                    root,
                    file: self.tabs[i].state.path.clone(),
                    version: self.tabs[i].state.doc.version(),
                    directory,
                    output: None,
                });
            }
            Err(e) => workspace.message = e.to_string(),
        }
    }
    pub(super) fn poll_markdown_jobs(&mut self) {
        if !self.builtin_plugin_enabled("markdown") {
            for artifact in &self.markdown_workspace.artifacts {
                self.run_panel.request_stop(artifact.id);
            }
        }
        let artifacts = std::mem::take(&mut self.markdown_workspace.artifacts);
        for artifact in artifacts {
            if self
                .run_panel
                .sessions
                .iter()
                .any(|s| s.id == artifact.id && s.is_running())
            {
                self.markdown_workspace.artifacts.push(artifact);
                continue;
            }
            let current = !self.project_safe_mode()
                && self.builtin_plugin_enabled("markdown")
                && self
                    .project
                    .as_ref()
                    .is_some_and(|p| p.root == artifact.root)
                && self.tabs.iter().any(|t| {
                    t.state.path == artifact.file && t.state.doc.version() == artifact.version
                });
            if let Some(output) = &artifact.output {
                let result = (|| -> anyhow::Result<()> {
                    if !current {
                        anyhow::bail!("Markdown source/project changed; export was not published");
                    }
                    if !self.run_panel.sessions.iter().any(|s| {
                        s.id == artifact.id
                            && matches!(s.status, rustpy_run::SessionStatus::Exited(0))
                    }) {
                        anyhow::bail!("Export failed or stopped; see Run console");
                    }
                    if self.tabs.iter().any(|tab| {
                        tab.state.path.as_ref().is_some_and(|path| {
                            rustpy_core::vfs::normalize_path(path) == output.destination
                        }) && tab.has_unsaved_changes()
                    }) {
                        anyhow::bail!("Export destination has unsaved edits");
                    }
                    if std::fs::metadata(&output.temporary)?.len() > 128 * 1024 * 1024 {
                        anyhow::bail!("Export exceeds 128 MiB");
                    }
                    publish(
                        &output.destination,
                        &std::fs::read(&output.temporary)?,
                        output.baseline,
                    )
                })();
                self.markdown_workspace.message = match result {
                    Ok(()) => format!("Export completed: {}", output.destination.display()),
                    Err(error) => error.to_string(),
                };
            }
            // Artifact ownership lasts until the actual child is terminal.
            drop(artifact.directory);
        }
    }
    pub(super) fn markdown_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Markdown");
        let settings = &mut self.settings.markdown;
        ui.horizontal(|ui| {
            ui.label("Default layout");
            for (label, mode) in [
                ("Editor only", Layout::Editor),
                ("Editor & preview", Layout::Split),
                ("Preview only", Layout::Preview),
            ] {
                ui.radio_value(&mut settings.layout, mode, crate::i18n::tr_static(label));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Split layout");
            ui.radio_value(&mut settings.split, Split::Right, "Right");
            ui.radio_value(&mut settings.split, Split::Below, "Below");
        });
        ui.checkbox(&mut settings.sync_scroll, "Synchronize scrolling");
        ui.checkbox(&mut settings.toolbar, "Show Markdown toolbar");
        ui.checkbox(
            &mut settings.auto_format_tables,
            "Auto-format tables when navigating with Tab",
        );
        ui.checkbox(
            &mut settings.inspections,
            "Inspect links, references, headers and tables",
        );
        ui.checkbox(&mut settings.remote_images, "Load remote images");
        ui.checkbox(&mut settings.dark, "Dark preview");
        ui.horizontal(|ui| {
            ui.label("Preview font size");
            ui.add(egui::DragValue::new(&mut settings.font_size).range(8.0..=48.0));
        });
        ui.label("Custom preview stylesheet (browser and HTML export)");
        ui.text_edit_multiline(&mut settings.custom_css);
        ui.checkbox(&mut settings.math, "Enable KaTeX math in enhanced preview");
        ui.checkbox(
            &mut settings.mermaid,
            "Enable Mermaid diagrams in enhanced preview",
        );
        ui.checkbox(
            &mut settings.plantuml,
            "Enable PlantUML diagrams in enhanced preview",
        );
        for (label, value) in [
            ("Pandoc executable", &mut settings.pandoc),
            (
                "PDF engine executable (empty = Pandoc default)",
                &mut settings.pdf_engine,
            ),
            ("KaTeX asset directory URL", &mut settings.katex_assets),
            ("Mermaid module URL", &mut settings.mermaid_script),
            (
                "PlantUML SVG renderer endpoint",
                &mut settings.plantuml_server,
            ),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(value);
            });
        }
        ui.label("Enhanced preview opens the captured HTML in your browser. Diagram endpoints receive diagram text; remote assets require network access. Native preview remains available offline.");
        self.markdown_workspace.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_publication_rejects_changed_destination_without_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.html");
        std::fs::write(&path, "original").unwrap();
        let expected = baseline(&path).unwrap();
        std::fs::write(&path, "external revision").unwrap();
        assert!(publish(&path, b"captured export", expected).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external revision");
    }
    #[test]
    fn toolbar_edits_and_notebook_export_use_the_current_markdown_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.md");
        std::fs::write(&path, "# Source\n").unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        app.markdown_insert(index, "**", "**", "Bold");
        let source = app.tabs[index].state.doc.text();
        assert!(source.starts_with("**Bold**"));
        let output = directory.path().join("notebook.ipynb");
        let mut state = Workspace {
            export_path: output.to_string_lossy().into_owned(),
            ..Default::default()
        };
        app.markdown_export(index, &source, Some(&path), "ipynb", &mut state);
        let notebook = rustpy_notebook::Notebook::load(&output).unwrap();
        assert_eq!(notebook.cells[0].kind, rustpy_notebook::CellKind::Markdown);
        assert_eq!(notebook.cells[0].source, source);
        assert!(app.run_panel.sessions.is_empty());
    }
    #[test]
    fn safe_mode_blocks_python_and_pandoc_before_process_creation() {
        let mut app = IdeApp::new_welcome(Settings::default());
        app.safe_mode = true;
        let mut state = Workspace::default();
        app.markdown_run_block(usize::MAX, "print('must not run')", &mut state);
        assert!(state.message.contains("Trust"));
        assert!(app.run_panel.sessions.is_empty());
    }
}

#[cfg(test)]
mod configured_export_tests;

#[cfg(all(test, target_os = "linux"))]
mod configured_render_tests;

#[cfg(all(test, target_os = "linux"))]
mod configured_plantuml_tests;

#[cfg(test)]
mod configured_python_tests;

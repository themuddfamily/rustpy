//! Quick Documentation controller and popup/docked content, using semantic hover
//! and the existing multi-format docstring/Markdown renderer.
use super::*;
use rustpy_semantic::Inferer;
use rustpy_text::DocumentId;

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    info: HoverInfo,
    external: Option<String>,
    path: PathBuf,
    offset: TextSize,
    context: PathBuf,
    context_text: Arc<str>,
    problems: Vec<String>,
    peps: Vec<u16>,
    doc_owner: Option<(PathBuf, Arc<str>, TextSize)>,
}
#[derive(Clone, PartialEq, Eq)]
struct HoverTarget {
    editor: egui::Id,
    document: DocumentId,
    version: u64,
    offset: TextSize,
}
pub(super) struct DocumentationUi {
    history: Vec<Entry>,
    index: usize,
    pub docked: bool,
    pub pinned: bool,
    pub auto_update: bool,
    pub font_size: f32,
    rendered: bool,
    query: String,
    match_index: usize,
    hover: Option<(HoverTarget, f64)>,
    shown_hover: Option<HoverTarget>,
    last_caret: Option<HoverTarget>,
    unavailable: bool,
}
impl Default for DocumentationUi {
    fn default() -> Self {
        Self {
            history: vec![],
            index: 0,
            docked: false,
            pinned: false,
            auto_update: true,
            font_size: 13.0,
            rendered: true,
            query: String::new(),
            match_index: 0,
            hover: None,
            shown_hover: None,
            last_caret: None,
            unavailable: false,
        }
    }
}
impl DocumentationUi {
    fn push(&mut self, entry: Entry) {
        self.unavailable = false;
        if self
            .history
            .get(self.index)
            .is_some_and(|old| old == &entry)
        {
            return;
        }
        self.history.truncate(self.index.saturating_add(1));
        self.history.push(entry);
        if self.history.len() > 64 {
            self.history.remove(0);
        }
        self.index = self.history.len() - 1;
    }
    fn current(&self) -> Option<&Entry> {
        self.history.get(self.index)
    }
}

fn keyword(word: &str) -> Option<(String, Vec<u16>)> {
    let (text, peps) = match word {
        "False" | "True" => ("Boolean constants represent false and true truth values.", vec![]),
        "None" => ("The singleton value representing absence of a value.", vec![]),
        "and" | "or" | "not" => ("Boolean operators evaluate truth values; and/or short-circuit and return an operand.", vec![]),
        "as" => ("Binds an alias or target in imports, exception handling, and context managers.", vec![]),
        "assert" => ("Checks a condition and raises AssertionError if it is false; assertions may be disabled by optimization.", vec![]),
        "async" | "await" => ("Declares coroutine functions or waits for an awaitable within a coroutine.", vec![492]),
        "break" | "continue" => ("Leaves the nearest loop or starts its next iteration.", vec![]),
        "class" => ("Defines a class, its base classes, and its body.", vec![3115]),
        "def" => ("Defines a function with parameters, optional annotations, and a body.", vec![3107]),
        "del" => ("Deletes a name binding, item, or attribute.", vec![]),
        "elif" | "else" | "if" => ("Selects a conditional branch; else also handles normal loop or try completion.", vec![]),
        "except" | "finally" | "try" => ("Handles exceptions and guarantees finalization with a finally clause.", vec![3134]),
        "for" | "while" => ("Repeats a suite over an iterable or while a condition is true.", vec![]),
        "from" | "import" => ("Loads a module or imports selected names into the current scope.", vec![328]),
        "global" | "nonlocal" => ("Directs assignment to a module scope or an enclosing function scope.", vec![3104]),
        "in" | "is" => ("Tests membership in a container or identity of two objects.", vec![]),
        "lambda" => ("Creates an anonymous function whose body is a single expression.", vec![]),
        "pass" => ("A statement that performs no operation.", vec![]),
        "raise" => ("Raises an exception, optionally specifying its cause; bare raise reraises the current exception.", vec![3134]),
        "return" => ("Exits the current function and returns a value (None when omitted).", vec![]),
        "with" => ("Runs a suite under context managers which enter and exit around the suite.", vec![343]),
        "yield" => ("Produces a generator value or delegates to another iterable with yield from.", vec![255, 380]),
        "match" | "case" => ("Matches a subject against structural patterns, with optional guards.", vec![634]),
        "type" => ("Introduces a type alias using the type statement (Python 3.12 and later).", vec![695]),
        _ => return None,
    };
    Some((text.into(), peps))
}
fn links(markdown: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut current: Option<(String, String)> = None;
    for event in pulldown_cmark::Parser::new(markdown) {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                current = Some((String::new(), dest_url.to_string()))
            }
            pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) => {
                if let Some((label, _)) = &mut current {
                    label.push_str(&text);
                }
            }
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Link) => {
                if let Some(link) = current.take() {
                    if !result.contains(&link) && result.len() < 32 {
                        result.push(link);
                    }
                }
            }
            _ => {}
        }
    }
    result
}
fn markdown(doc: &str) -> String {
    // Existing renderer detects Google/NumPy/reST/Epytext; retain Markdown links.
    let mut end = doc.len().min(128 * 1024);
    while !doc.is_char_boundary(end) {
        end -= 1;
    }
    rustpy_semantic::docstrings::render_markdown(
        &doc[..end],
        rustpy_semantic::docstrings::detect_one(&doc[..end]),
    )
}
/// Root mounts on existing Editor/Code Insight settings page and persists both
/// fields in EditorSettings; the default is disabled, with a 500 ms delay.
pub(super) fn documentation_options(ui: &mut egui::Ui, enabled: &mut bool, delay_ms: &mut u64) {
    ui.checkbox(enabled, "Show documentation on mouse move");
    ui.add_enabled(
        *enabled,
        egui::Slider::new(delay_ms, 100..=5000).text("Documentation popup delay (ms)"),
    );
}
impl IdeApp {
    fn documentation_entry(&mut self, i: usize, offset: TextSize) -> Option<Entry> {
        let (analysis, file) = self.analysis_file(i)?;
        let token = features::ident_at(&file, offset)?;
        let word = token.text();
        let semantic = features::hover_at(&analysis, &file, offset);
        let (info, external, peps) = if semantic.is_none() {
            let (description, peps) = keyword(word)?;
            (
                HoverInfo {
                    signature: format!("Python keyword: {word}"),
                    docstring: Some(description),
                    origin: Some("Python language reference".into()),
                    kind: Some("keyword"),
                },
                Some("https://docs.python.org/3/reference/index.html".into()),
                peps,
            )
        } else {
            let info = semantic?;
            let python = self
                .project
                .as_ref()
                .and_then(|project| project.interpreter.as_ref())
                .map(|interpreter| interpreter.major_minor())
                .unwrap_or((3, 12));
            let patterns = self
                .settings
                .external_doc_urls
                .iter()
                .map(|entry| (entry.module.clone(), entry.url.clone()))
                .collect::<Vec<_>>();
            let external = rustpy_semantic::external_docs::external_doc_url(
                &analysis, &file, offset, python, &patterns,
            );
            (info, external, vec![])
        };
        let path = features::definition_at(&analysis, &file, offset)
            .first()
            .map(|location| location.path.clone())
            .unwrap_or_else(|| file.path.clone());
        let doc_owner = analysis
            .file_for_path(&path)
            .and_then(|owner| documentation_owner(&owner, info.docstring.as_deref()?));
        Some(Entry {
            doc_owner,
            info,
            external,
            path,
            offset,
            context: file.path.clone(),
            context_text: file.text.clone(),
            problems: crate::editor::diag_style::messages_at(
                &self.tabs[i].state.diagnostics(),
                offset,
            ),
            peps,
        })
    }
    /// CtrlQ: uses actual active document/semantic snapshot, including keywords.
    pub(super) fn show_quick_documentation(&mut self) {
        let Some(i) = self.active else {
            return;
        };
        let offset = self.tabs[i].state.primary().head();
        self.open_documentation_at(i, offset, true);
    }
    fn open_documentation_at(&mut self, i: usize, offset: TextSize, explicit: bool) {
        if !explicit
            && (self.documentation.pinned
                || (self.documentation.docked && !self.documentation.auto_update))
        {
            return;
        }
        if explicit {
            self.documentation.shown_hover = None;
            self.documentation.hover = None;
        }
        self.documentation.last_caret = Some(HoverTarget {
            editor: self.tabs[i].view.id,
            document: self.tabs[i].state.doc.identity(),
            version: self.tabs[i].state.doc.version(),
            offset: self.tabs[i].state.primary().head(),
        });
        let Some(entry) = self.documentation_entry(i, offset) else {
            if self.documentation.docked && !self.documentation.pinned {
                self.documentation.unavailable = true;
                self.quick_doc = None;
                self.docs.quick_doc_url = None;
            }
            if explicit {
                self.set_status("No documentation found");
            }
            return;
        };
        self.documentation.unavailable = false;
        self.quick_doc = Some(entry.info.clone());
        self.docs.quick_doc_url = entry.external.clone();
        self.quick_doc_at = Some((
            self.tabs[i].view.id,
            self.tabs[i].state.doc.version(),
            self.tabs[i].state.primary().head(),
        ));
        self.documentation.push(entry);
    }
    /// Root invokes after active View paints, using its actual hovered_offset.
    /// Never follows hidden/inactive editors or a modal; Root enforces keyboard/
    /// pointer ownership before invoking. Requests another repaint for delay.
    pub(super) fn tick_documentation_hover(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        enabled: bool,
        delay_ms: u64,
    ) {
        if !enabled || self.documentation.pinned || self.active != Some(i) {
            if self.documentation.shown_hover.is_some()
                && !self.documentation.pinned
                && !self.documentation.docked
            {
                self.quick_doc = None;
                self.quick_doc_at = None;
                self.docs.quick_doc_url = None;
            }
            self.documentation.hover = None;
            self.documentation.shown_hover = None;
            return;
        }
        let Some(offset) = self.tabs[i].view.hovered_offset else {
            self.documentation.hover = None;
            self.documentation.shown_hover = None;
            return;
        };
        let Some((_, file)) = self.analysis_file(i) else {
            self.documentation.hover = None;
            self.documentation.shown_hover = None;
            return;
        };
        let Some(token) = features::ident_at(&file, offset) else {
            self.documentation.hover = None;
            self.documentation.shown_hover = None;
            return;
        };
        let target = HoverTarget {
            editor: self.tabs[i].view.id,
            document: self.tabs[i].state.doc.identity(),
            version: self.tabs[i].state.doc.version(),
            offset: token.text_range().start(),
        };
        let now = ui.input(|input| input.time);
        if self
            .documentation
            .hover
            .as_ref()
            .is_none_or(|(old, _)| *old != target)
        {
            self.documentation.hover = Some((target.clone(), now));
        }
        let since = self.documentation.hover.as_ref().unwrap().1;
        let delay = delay_ms.clamp(100, 5000) as f64 / 1000.0;
        if now - since >= delay && self.documentation.shown_hover.as_ref() != Some(&target) {
            self.open_documentation_at(i, offset, false);
            self.documentation.shown_hover = Some(target);
        } else if now - since < delay {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(delay - (now - since)));
        }
    }
    pub(super) fn refresh_documentation_tool(&mut self) {
        if self.documentation.docked && self.documentation.auto_update && !self.documentation.pinned
        {
            if let Some(i) = self.active {
                let target = HoverTarget {
                    editor: self.tabs[i].view.id,
                    document: self.tabs[i].state.doc.identity(),
                    version: self.tabs[i].state.doc.version(),
                    offset: self.tabs[i].state.primary().head(),
                };
                if self.documentation.last_caret.as_ref() != Some(&target) {
                    self.open_documentation_at(i, target.offset, false);
                }
            }
        }
    }
    pub(super) fn follow_inline_documentation_link(
        &mut self,
        index: usize,
        offset: TextSize,
        url: &str,
    ) {
        self.sync_analysis_now();
        let Some((_, file)) = self.analysis_file(index) else {
            return;
        };
        self.documentation.push(Entry {
            info: HoverInfo {
                signature: String::new(),
                docstring: None,
                origin: None,
                kind: None,
            },
            external: None,
            path: file.path.clone(),
            offset,
            context: file.path.clone(),
            context_text: file.text.clone(),
            problems: vec![],
            peps: vec![],
            doc_owner: None,
        });
        self.active = Some(index);
        self.follow_documentation_link(url);
        self.quick_doc_at = Some((
            self.tabs[index].view.id,
            self.tabs[index].state.doc.version(),
            self.tabs[index].state.primary().head(),
        ));
    }
    fn follow_documentation_link(&mut self, url: &str) {
        if crate::editor::view::is_openable_doc_link(url) {
            self.open_url(url);
            return;
        }
        let name = url
            .strip_prefix("python:")
            .or_else(|| url.strip_prefix('#'));
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            self.set_status("Unsupported documentation link");
            return;
        };
        if self.documentation.unavailable {
            self.set_status("No documentation at the caret");
            return;
        }
        let Some(entry) = self.documentation.current().cloned() else {
            return;
        };
        let Some(analysis) = self
            .project
            .as_ref()
            .map(|project| project.analysis.clone())
        else {
            return;
        };
        let Some(file) = analysis.file_for_path(&entry.context) else {
            return;
        };
        // A pinned/history entry cannot resolve an old offset in a changed scope.
        // Check open buffers too: analysis synchronization may follow the edit.
        let changed_buffer = self.tabs.iter().any(|tab| {
            tab.state.path.as_deref() == Some(entry.context.as_path())
                && tab.state.doc.text().as_str() != entry.context_text.as_ref()
        });
        if file.text != entry.context_text || changed_buffer {
            self.set_status("Documentation link unavailable: source context changed");
            return;
        }
        if entry.doc_owner.is_none() && entry.info.docstring.is_some() {
            self.set_status(
                "Documentation link unavailable: defining source context is not retained",
            );
            return;
        }
        let (file, offset) = if let Some((path, text, offset)) = &entry.doc_owner {
            let Some(owner) = analysis.file_for_path(path) else {
                return;
            };
            if owner.text != *text
                || self.tabs.iter().any(|tab| {
                    tab.state.path.as_deref() == Some(path.as_path())
                        && tab.state.doc.text().as_str() != text.as_ref()
                })
            {
                self.set_status("Documentation owner changed; reopen documentation");
                return;
            }
            (owner, *offset)
        } else {
            (file, entry.offset)
        };
        let scope = file.scopes.scope_at(offset);
        let Some(definition) = documentation_definition(&analysis, &file, scope, name) else {
            self.set_status(format!("No documentation for {name}"));
            return;
        };
        let Some(info) =
            features::hover_at(&analysis, &definition.file, definition.range().start())
        else {
            self.set_status(format!("No documentation for {name}"));
            return;
        };
        let doc_owner = info
            .docstring
            .as_deref()
            .and_then(|doc| documentation_owner(&definition.file, doc));
        let entry = Entry {
            doc_owner,
            info,
            external: None,
            path: definition.file.path.clone(),
            offset: definition.range().start(),
            context: definition.file.path.clone(),
            context_text: definition.file.text.clone(),
            problems: vec![],
            peps: vec![],
        };
        self.quick_doc = Some(entry.info.clone());
        self.documentation.push(entry);
    }
    fn documentation_content(&mut self, ui: &mut egui::Ui) {
        let mut follow = None;
        let before = (self.documentation.font_size, self.documentation.auto_update);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.documentation.index > 0
                        || (self.documentation.unavailable
                            && !self.documentation.history.is_empty()),
                    egui::Button::new("←"),
                )
                .clicked()
            {
                if !self.documentation.unavailable {
                    self.documentation.index -= 1;
                }
                self.documentation.unavailable = false;
            }
            if ui
                .add_enabled(
                    self.documentation.index + 1 < self.documentation.history.len(),
                    egui::Button::new("→"),
                )
                .clicked()
            {
                self.documentation.index += 1;
                self.documentation.unavailable = false;
            }
            ui.checkbox(&mut self.documentation.pinned, "Pin");
            if ui.small_button("−").clicked() {
                self.documentation.font_size = (self.documentation.font_size - 1.0).max(10.0);
            }
            ui.label(format!("{} pt", self.documentation.font_size));
            if ui.small_button("+").clicked() {
                self.documentation.font_size = (self.documentation.font_size + 1.0).min(24.0);
            }
            ui.checkbox(&mut self.documentation.docked, "Dock");
            ui.checkbox(&mut self.documentation.rendered, "Render documentation");
            ui.checkbox(&mut self.documentation.auto_update, "Auto-update");
        });
        if before != (self.documentation.font_size, self.documentation.auto_update) {
            self.settings.documentation.font_size =
                self.documentation.font_size.clamp(10.0, 24.0) as u8;
            self.settings.documentation.auto_update = self.documentation.auto_update;
            let _ = self.settings.save();
        }
        let Some(entry) = self.documentation.current().cloned() else {
            ui.label("No documentation selected");
            return;
        };
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.documentation.query)
                    .hint_text("Find in documentation"),
            );
            if response.changed() {
                self.documentation.match_index = 0;
            }
            if ui.small_button("Previous match").clicked() {
                self.documentation.match_index = self.documentation.match_index.saturating_sub(1);
            }
            if ui.small_button("Next match").clicked() {
                self.documentation.match_index = self.documentation.match_index.saturating_add(1);
            }
        });
        if !self.documentation.query.trim().is_empty() {
            let text = format!(
                "{}\n{}",
                entry.info.signature,
                entry.info.docstring.as_deref().unwrap_or_default()
            );
            let needle = self.documentation.query.to_lowercase();
            let matching = text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&needle))
                .collect::<Vec<_>>();
            self.documentation.match_index = self
                .documentation
                .match_index
                .min(matching.len().saturating_sub(1));
            ui.label(format!(
                "{} of {} matching lines",
                if matching.is_empty() {
                    0
                } else {
                    self.documentation.match_index + 1
                },
                matching.len()
            ));
            if let Some((line, _)) = matching.get(self.documentation.match_index) {
                egui::ScrollArea::both()
                    .id_salt("documentation-search-preview")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        for (index, text) in text
                            .lines()
                            .enumerate()
                            .skip(line.saturating_sub(2))
                            .take(5)
                        {
                            ui.monospace(format!(
                                "{} {}",
                                if index == *line { "▶" } else { " " },
                                text
                            ));
                        }
                    });
            }
        }
        for problem in &entry.problems {
            ui.label(egui::RichText::new(problem).color(self.theme.text));
        }
        if !entry.problems.is_empty() {
            ui.separator();
        }
        let font = self.documentation.font_size.clamp(10.0, 24.0);
        ui.label(
            egui::RichText::new(&entry.info.signature)
                .monospace()
                .size(font)
                .color(self.theme.text),
        );
        if let Some(origin) = &entry.info.origin {
            ui.label(
                egui::RichText::new(origin)
                    .size(font - 1.0)
                    .color(self.theme.text_dim),
            );
        }
        ui.label(
            egui::RichText::new(entry.path.display().to_string())
                .size(font - 1.0)
                .color(self.theme.text_dim),
        );
        if let Some(doc) = &entry.info.docstring {
            let md = markdown(doc);
            if self.documentation.rendered {
                let mut end = doc.len().min(128 * 1024);
                while !doc.is_char_boundary(end) {
                    end -= 1;
                }
                let blocks = rustpy_semantic::doc_blocks::render_text(&doc[..end]);
                if let Some(url) = crate::documentation_view::show(
                    ui,
                    &blocks,
                    entry.doc_owner.as_ref().map(|(path, _, _)| path.as_path()),
                    &self.scheme,
                    font,
                ) {
                    follow = Some(url);
                }
            } else {
                ui.label(egui::RichText::new(doc).monospace().size(font));
            }
            let doc_links = links(&md);
            if !doc_links.is_empty() {
                ui.separator();
                ui.label("Related links");
            }
            for (label, url) in doc_links {
                if ui.link(label).on_hover_text(&url).clicked() {
                    follow = Some(url);
                }
            }
        }
        for pep in entry.peps {
            if ui.link(format!("PEP {pep}")).clicked() {
                follow = Some(format!("https://peps.python.org/pep-{pep:04}/"));
            }
        }
        if let Some(url) = entry.external {
            if ui
                .link("External documentation (Shift+F1)")
                .on_hover_text(&url)
                .clicked()
            {
                follow = Some(url);
            }
        }
        if let Some(url) = follow {
            self.follow_documentation_link(&url);
        }
    }
    /// Documentation is subordinate to competing keyboard/pointer owners.
    /// Pin preserves content, not permission to steal another popup's Escape.
    pub(super) fn documentation_popup_owned(&self, ui: &egui::Ui) -> bool {
        self.editor_keyboard()
            && !self.find.visible
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && self.intentions.is_none()
            && self.inplace_rename.is_none()
            && !self.completion.visible
            && self.import_completion.is_none()
            && !self.run_panel.launcher.is_open()
            && !self.coordinated_quit_active()
            && !egui::Popup::is_any_open(ui.ctx())
    }
    pub(super) fn draw_quick_documentation(&mut self, ui: &mut egui::Ui, caret: Rect) {
        if self.documentation.docked
            || self.quick_doc.is_none()
            || !self.documentation_popup_owned(ui)
        {
            return;
        }
        let mut close = false;
        egui::Area::new(ui.id().with("quick_doc"))
            .order(egui::Order::Foreground)
            .fixed_pos(caret.left_bottom() + Vec2::new(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(self.theme.panel_bg)
                    .show(ui, |ui| {
                        ui.set_max_width(560.0);
                        ui.horizontal(|ui| {
                            ui.heading("Documentation");
                            if ui.small_button("×").clicked() {
                                close = true;
                            }
                        });
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| self.documentation_content(ui));
                    });
            });
        if close {
            self.quick_doc = None;
        }
    }
    pub(super) fn reopen_documentation_popup(&mut self) {
        self.quick_doc = self.documentation.current().map(|entry| entry.info.clone());
    }
    pub(super) fn reset_documentation(&mut self) {
        self.quick_doc = None;
        self.quick_doc_at = None;
        self.docs.quick_doc_url = None;
        self.documentation = Default::default();
        self.documentation.font_size =
            f32::from(self.settings.documentation.font_size.clamp(10, 24));
        self.documentation.auto_update = self.settings.documentation.auto_update;
    }
    /// Actual bottom-dock content; Root registers Documentation tool routing.
    pub(super) fn documentation_tool_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| self.documentation_content(ui));
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::editor::Caret;
    pub(in crate::app) fn fixture() -> (tempfile::TempDir, IdeApp, String) {
        let dir = tempfile::tempdir().unwrap();
        let source = "def first(π: str) -> str:\n    \"\"\"Go [Related second](python:second).\n\n    Args:\n        π (str): Unicode argument.\n    Returns:\n        str: Result.\n    \"\"\"\n    return π\ndef second():\n    \"\"\"Second documentation.\"\"\"\n    return 2\nfirst('a')\nlen([])\nasync def task():\n    await task()\n".to_owned();
        let path = dir.path().join("main.py");
        std::fs::write(&path, &source).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        let root = dir.path().to_path_buf();
        app.project = Some(Project {
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            root,
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.open_file(&path);
        let offset = source.rfind("first('").unwrap() + 1;
        app.tabs[app.active.unwrap()]
            .state
            .set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
        (dir, app, source)
    }
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx
    }
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                if app.documentation.docked {
                    app.documentation_tool_ui(ui);
                } else {
                    app.draw_quick_documentation(
                        ui,
                        Rect::from_min_size(Pos2::new(100.0, 40.0), Vec2::new(2.0, 18.0)),
                    );
                }
            },
        )
    }
    fn text_rect(output: &egui::FullOutput, label: &str) -> Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("actual documentation control missing {label:?}"))
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, rect: Rect) {
        let event = |pressed| egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            app,
            ctx,
            time,
            vec![egui::Event::PointerMoved(rect.center()), event(true)],
        )
        .drop_without_applying_deltas();
        frame(app, ctx, time + 0.02, vec![event(false)]).drop_without_applying_deltas();
    }
    #[test]
    fn documentation_auto_update_freeze_pin_and_empty_caret_keep_history_authority() {
        let (_root, mut app, source) = fixture();
        let index = app.active.unwrap();
        app.open_documentation_at(
            index,
            TextSize::new(source.rfind("first('").unwrap() as u32),
            true,
        );
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("first"));
        app.documentation.docked = true;
        app.documentation.auto_update = false;
        app.tabs[index]
            .state
            .set_carets(vec![Caret::at(TextSize::new(
                source.find("second():").unwrap() as u32,
            ))]);
        app.refresh_documentation_tool();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("first"));
        app.documentation.auto_update = true;
        app.refresh_documentation_tool();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("second"));
        app.documentation.pinned = true;
        app.tabs[index]
            .state
            .set_carets(vec![Caret::at(TextSize::new(source.len() as u32))]);
        app.refresh_documentation_tool();
        assert!(!app.documentation.unavailable);
        app.documentation.pinned = false;
        app.refresh_documentation_tool();
        assert!(app.documentation.unavailable);
        assert!(app.quick_doc.is_none());
        assert!(app.documentation.history.len() >= 2);
    }
    #[test]
    fn persisted_documentation_preferences_survive_project_reset_and_clamp_font() {
        let (_root, mut app, _) = fixture();
        app.settings.documentation.auto_update = false;
        app.settings.documentation.font_size = 255;
        app.reset_documentation();
        assert!(!app.documentation.auto_update);
        assert_eq!(app.documentation.font_size, 24.0);
        assert!(!app.documentation.pinned);
        assert!(app.documentation.history.is_empty());
    }
    #[test]
    fn actual_popup_signature_module_path_formats_and_links_navigate_without_editing() {
        let (_dir, mut app, source) = fixture();
        let i = app.active.unwrap();
        let version = app.tabs[i].state.doc.version();
        app.show_quick_documentation();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("first"));
        let ctx = context();
        frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.02, vec![]);
        text_rect(&output, "Pin");
        text_rect(&output, "Dock");
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("main.py"))));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("Parameters"))));
        let related = text_rect(&output, "Related second");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.04, related);
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("second"));
        let output = frame(&mut app, &ctx, 0.08, vec![]);
        let back = text_rect(&output, "←");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.10, back);
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("first"));
        assert_eq!(app.tabs[i].state.doc.text(), source);
        assert_eq!(app.tabs[i].state.doc.version(), version);
        assert!(!app.tabs[i].state.doc.can_undo());
    }
    #[test]
    fn actual_pin_dock_font_controls_and_tool_follow_policy() {
        let (_dir, mut app, source) = fixture();
        app.show_quick_documentation();
        let ctx = context();
        frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.02, vec![]);
        let pin = text_rect(&output, "Pin");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.04, pin);
        assert!(app.documentation.pinned);
        let i = app.active.unwrap();
        app.tabs[i].state.set_carets(vec![Caret::at(TextSize::new(
            source.find("second()").unwrap() as u32 + 1,
        ))]);
        app.documentation.docked = true;
        app.refresh_documentation_tool();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("first"));
        let output = frame(&mut app, &ctx, 0.08, vec![]);
        let plus = text_rect(&output, "+");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.10, plus);
        assert_eq!(app.documentation.font_size, 14.0);
        app.documentation.pinned = false;
        app.refresh_documentation_tool();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("second"));
        assert_eq!(app.tabs[i].state.doc.text(), source);
        assert!(!app.tabs[i].state.doc.can_undo());
    }
    #[test]
    fn actual_keyword_pep_and_builtin_semantic_documentation() {
        let (_dir, mut app, source) = fixture();
        let i = app.active.unwrap();
        app.tabs[i].state.set_carets(vec![Caret::at(TextSize::new(
            source.find("await").unwrap() as u32 + 1,
        ))]);
        app.show_quick_documentation();
        assert_eq!(app.documentation.current().unwrap().peps, vec![492]);
        let ctx = context();
        frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.02, vec![]);
        text_rect(&output, "PEP 492");
        text_rect(&output, "External documentation (Shift+F1)");
        output.drop_without_applying_deltas();
        app.tabs[i].state.set_carets(vec![Caret::at(TextSize::new(
            source.find("len([])").unwrap() as u32 + 1,
        ))]);
        app.show_quick_documentation();
        assert!(app
            .documentation
            .current()
            .unwrap()
            .info
            .signature
            .contains("len"));
    }
    #[test]
    fn actual_hover_delay_disable_pin_and_document_version_authority() {
        let (_dir, mut app, source) = fixture();
        let i = app.active.unwrap();
        let ctx = context();
        app.tabs[i].view.hovered_offset =
            Some(TextSize::new(source.rfind("first('").unwrap() as u32 + 1));
        let tick = |app: &mut IdeApp, time, enabled| {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| app.tick_documentation_hover(ui, i, enabled, 500),
            )
            .drop_without_applying_deltas()
        };
        tick(&mut app, 0.0, true);
        assert!(app.quick_doc.is_none());
        tick(&mut app, 0.49, true);
        assert!(app.quick_doc.is_none());
        tick(&mut app, 0.51, true);
        assert!(app.quick_doc.is_some());
        let count = app.documentation.history.len();
        tick(&mut app, 0.70, true);
        assert_eq!(app.documentation.history.len(), count);
        app.quick_doc = None;
        tick(&mut app, 0.80, false);
        tick(&mut app, 2.0, false);
        assert!(app.quick_doc.is_none());
        app.documentation.pinned = true;
        tick(&mut app, 3.0, true);
        assert!(app.quick_doc.is_none());
        app.reset_documentation();
        assert!(app.documentation.current().is_none());
        assert!(!app.documentation.docked);
    }
    #[test]
    fn actual_hover_leave_whitespace_and_reenter_require_fresh_continuous_delay() {
        let (_dir, mut app, source) = fixture();
        let i = app.active.unwrap();
        let ctx = context();
        let symbol = TextSize::new(source.rfind("first('").unwrap() as u32 + 1);
        let whitespace = TextSize::new(source.find("    return").unwrap() as u32 + 1);
        let tick = |app: &mut IdeApp, time| {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| app.tick_documentation_hover(ui, i, true, 500),
            )
            .drop_without_applying_deltas()
        };
        app.tabs[i].view.hovered_offset = Some(symbol);
        tick(&mut app, 0.0);
        app.tabs[i].view.hovered_offset = Some(whitespace);
        tick(&mut app, 0.30);
        assert!(app.documentation.hover.is_none());
        app.tabs[i].view.hovered_offset = Some(symbol);
        tick(&mut app, 0.51);
        assert!(
            app.quick_doc.is_none(),
            "original hover timer must not survive whitespace"
        );
        tick(&mut app, 0.99);
        assert!(app.quick_doc.is_none());
        tick(&mut app, 1.02);
        assert!(app.quick_doc.is_some());
        app.quick_doc = None;
        app.tabs[i].view.hovered_offset = None;
        tick(&mut app, 1.10);
        assert!(app.documentation.shown_hover.is_none());
        app.tabs[i].view.hovered_offset = Some(symbol);
        tick(&mut app, 1.20);
        tick(&mut app, 1.69);
        assert!(app.quick_doc.is_none());
        tick(&mut app, 1.71);
        assert!(
            app.quick_doc.is_some(),
            "same symbol can reopen after leaving closed popup"
        );
    }
    #[test]
    fn shared_definition_equal_offsets_keep_new_captured_context_and_display_authority() {
        let mut state = DocumentationUi::default();
        let first = Entry {
            info: HoverInfo {
                signature: "shared()".into(),
                docstring: Some("[local](python:local)".into()),
                origin: Some("library".into()),
                kind: Some("function"),
            },
            external: Some("https://first.example/docs".into()),
            path: PathBuf::from("/library/shared.py"),
            offset: TextSize::new(10),
            context: PathBuf::from("/project/first.py"),
            context_text: Arc::from("shared()"),
            problems: vec![],
            peps: vec![],
            doc_owner: None,
        };
        state.push(first.clone());
        state.push(first.clone());
        assert_eq!(state.history.len(), 1);
        let second = Entry {
            context: PathBuf::from("/project/second.py"),
            ..first
        };
        state.push(second.clone());
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.current().unwrap().context, second.context);
        state.push(Entry {
            external: Some("https://second.example/docs".into()),
            peps: vec![634],
            ..second
        });
        assert_eq!(state.history.len(), 3);
        assert_eq!(
            state.current().unwrap().external.as_deref(),
            Some("https://second.example/docs")
        );
        assert_eq!(state.current().unwrap().peps, vec![634]);
    }
    #[test]
    fn pinned_link_refuses_changed_caller_scope_before_analysis_sync_and_after_sync() {
        let (_root, mut app, source) = fixture();
        app.show_quick_documentation();
        app.documentation.pinned = true;
        let original = app.documentation.current().unwrap().clone();
        let i = app.active.unwrap();
        let changed = format!("def wrapper():\n    second = 1\n{}", source);
        app.tabs[i].state.doc.reset(&changed);
        app.follow_documentation_link("python:second");
        assert!(app.documentation.current().unwrap() == &original);
        assert_eq!(app.documentation.history.len(), 1);
        let project = app.project.as_ref().unwrap();
        project
            .analysis
            .set_file_text(app.tabs[i].state.path.as_ref().unwrap(), &changed);
        app.follow_documentation_link("python:second");
        assert!(app.documentation.current().unwrap() == &original);
        assert_eq!(app.documentation.history.len(), 1);
        assert_eq!(app.tabs[i].state.doc.text(), changed);
    }
    #[test]
    fn markdown_formats_unicode_and_link_schemes_are_bounded_and_preserved() {
        for doc in [
            "Summary.\n\n:param π: Unicode parameter.\n:returns: Result.",
            "Summary.\n\n@param π: Unicode parameter.\n@return: Result.",
            "Summary.\n\nArgs:\n    π (str): Unicode parameter.\nReturns:\n    str: Result.",
            "Summary.\n\nParameters\n----------\nπ : str\n    Unicode parameter.\nReturns\n-------\nstr\n    Result.",
        ] {
            let rendered = markdown(doc);
            assert!(rendered.contains("π")); assert!(rendered.contains("Unicode parameter")); assert!(rendered.contains("Result"));
        }
        assert_eq!(
            links(&markdown(
                "[Related](python:first) and [Python](https://docs.python.org/3/)"
            )),
            vec![
                ("Related".into(), "python:first".into()),
                ("Python".into(), "https://docs.python.org/3/".into())
            ]
        );
        assert!(!markdown(&"π😀".repeat(50_000)).is_empty());
    }
}

fn documentation_definition(
    db: &rustpy_semantic::Analysis,
    file: &Arc<rustpy_semantic::FileInfo>,
    scope: rustpy_semantic::scope::ScopeId,
    name: &str,
) -> Option<rustpy_semantic::infer::Definition> {
    use rustpy_semantic::{infer::Definition, types::Type};
    if name.len() > 1024 {
        return None;
    }
    let parts: Vec<_> = name.split('.').collect();
    if parts.is_empty() || parts.len() > 32 {
        return None;
    }
    let inf = Inferer::new(db);
    let mut def = inf.resolve_name(file, scope, parts[0]);
    let mut ty = def
        .as_ref()
        .map(|d| inf.type_of_binding(&d.file, d.binding))
        .or_else(|| db.module(parts[0]).map(|module| Type::Module(module.id)))?;
    for name in &parts[1..] {
        def = match &ty {
            Type::Module(id) => {
                let module = db.file(*id)?;
                inf.resolve_in_module(&module, name, 0)
            }
            Type::Class(class, _) | Type::Instance(class, _) => {
                inf.mro(class).iter().take(32).find_map(|owner| {
                    let info = inf.class_info(owner)?;
                    let member = info.members.get(*name)?;
                    Some(Definition {
                        file: db.file(member.file)?,
                        binding: member.binding?,
                    })
                })
            }
            _ => None,
        };
        ty = inf.lookup_member(&ty, name)?;
    }
    let def = def?;
    Some(rustpy_semantic::usages::name_origin(&inf, &def))
}

fn documentation_owner(
    file: &Arc<rustpy_semantic::FileInfo>,
    doc: &str,
) -> Option<(PathBuf, Arc<str>, TextSize)> {
    use rustpy_syntax::ast::{self, AstNode};
    let mut matches = Vec::new();
    for node in file.parse.syntax_node().descendants() {
        let value = if let Some(function) = ast::FuncDef::cast(node.clone()) {
            function.docstring()
        } else if let Some(class) = ast::ClassDef::cast(node.clone()) {
            class.docstring()
        } else {
            ast::Module::cast(node).and_then(|module| module.docstring())
        };
        if let Some(value) = value.filter(|value| value.value() == doc) {
            matches.push(value.syntax().text_range().start());
        }
    }
    (matches.len() == 1).then(|| (file.path.clone(), file.text.clone(), matches[0]))
}

#[cfg(test)]
mod rendered_documentation_ownership_tests {
    use super::*;
    use rustpy_semantic::Analysis;
    #[test]
    fn qualified_role_links_follow_real_module_and_class_bindings() {
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(
            Path::new("/proj/library.py"),
            "class Thing:\n    def call(self):\n        \"\"\"Call docs.\"\"\"\n        pass\n",
        );
        db.set_file_text(Path::new("/proj/main.py"), "import library as alias\n");
        let file = db.file_for_path(Path::new("/proj/main.py")).unwrap();
        let target =
            documentation_definition(&db, &file, file.scopes.module_scope(), "alias.Thing.call")
                .unwrap();
        assert_eq!(target.file.path, Path::new("/proj/library.py"));
        assert_eq!(target.binding().name, "call");
        assert!(documentation_definition(
            &db,
            &file,
            file.scopes.module_scope(),
            "alias.Thing.missing"
        )
        .is_none());
        let owner = documentation_owner(&target.file, "Call docs.").unwrap();
        assert_eq!(owner.0, target.file.path);
        assert!(owner.2 > target.range().start());
    }
    #[test]
    fn duplicate_or_inherited_unowned_text_cannot_guess_an_image_or_role_owner() {
        let db = Analysis::for_project(Path::new("/proj"));
        let path = Path::new("/proj/main.py");
        db.set_file_text(path,"def first():\n    \"\"\"Same.\"\"\"\n    pass\ndef second():\n    \"\"\"Same.\"\"\"\n    pass\n");
        let file = db.file_for_path(path).unwrap();
        assert!(documentation_owner(&file, "Same.").is_none());
        assert!(documentation_owner(&file, "Inherited docs from another file.").is_none());
    }
}

#[cfg(test)]
mod inline_reference_controller_tests {
    use super::*;
    #[test]
    fn inline_reference_opens_matching_documentation_and_retains_editor_history_bytes() {
        let (dir, mut app, source) = super::tests::fixture();
        let i = app.active.unwrap();
        let version = app.tabs[i].state.doc.version();
        let offset = TextSize::new(source.find("Go [Related").unwrap() as u32);
        app.follow_inline_documentation_link(i, offset, "python:second");
        assert!(app.quick_doc.as_ref().unwrap().signature.contains("second"));
        assert_eq!(
            app.documentation
                .current()
                .unwrap()
                .info
                .docstring
                .as_deref(),
            Some("Second documentation.")
        );
        assert_eq!(
            app.quick_doc_at,
            Some((
                app.tabs[i].view.id,
                version,
                app.tabs[i].state.primary().head()
            ))
        );
        assert_eq!(app.tabs[i].state.doc.version(), version);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("main.py")).unwrap(),
            source
        );
    }
}

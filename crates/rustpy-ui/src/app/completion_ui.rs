//! Lookup presentation, selected-symbol help and context commit behavior.
use super::*;
use crate::a11y::DialogWindow;
#[path = "prose_completion.rs"]
mod prose_completion;

#[derive(PartialEq, Eq)]
struct CompletionHelpKey {
    editor: egui::Id,
    version: u64,
    caret: TextSize,
    label: String,
    origin: String,
    world: u64,
    buffers: Vec<(egui::Id, u64)>,
}
#[derive(Default)]
pub(super) struct CompletionUiState {
    pub due: Option<(egui::Id, u64, TextSize, f64)>,
    pub paths: bool,
    pub auxiliary_rect: Option<Rect>,
    documentation: bool,
    import_options: bool,
    recent: Vec<String>,
    help_key: Option<CompletionHelpKey>,
    help: Option<HoverInfo>,
    definition: Option<(PathBuf, usize, String)>,
    /// `(prefix start, label)` of the item the user moved the selection to
    /// with the keyboard. It focuses that lookup for context-character commits
    /// for as long as the same item stays selected in the same lookup.
    navigated: Option<(TextSize, String)>,
}

impl CompletionUiState {
    pub fn reset_help(&mut self) {
        self.help_key = None;
        self.help = None;
        self.definition = None;
        self.auxiliary_rect = None;
    }
    pub fn popup_wait(
        &mut self,
        view: egui::Id,
        version: u64,
        caret: TextSize,
        focused: bool,
        now: f64,
    ) -> Option<std::time::Duration> {
        let (owner, expected_version, expected_caret, due) = self.due?;
        if owner != view || expected_version != version || expected_caret != caret || !focused {
            self.due = None;
            return None;
        }
        if now >= due {
            self.due = None;
            return Some(std::time::Duration::ZERO);
        }
        Some(std::time::Duration::from_secs_f64(due - now))
    }
}

pub(super) fn highlight_label(
    label: &str,
    prefix: &str,
    normal: Color32,
    accent: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut wanted = prefix.chars();
    let mut next = wanted.next();
    for ch in label.chars() {
        let hit = next.is_some_and(|want| ch.to_lowercase().eq(want.to_lowercase()));
        if hit {
            next = wanted.next();
        }
        job.append(
            &ch.to_string(),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(13.5),
                color: if hit { accent } else { normal },
                ..Default::default()
            },
        );
    }
    job
}

impl IdeApp {
    pub(super) fn order_completion_items(
        &self,
        mut items: Vec<CompletionItem>,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        use rustpy_settings::CompletionCaseMatch;
        let mode = if self.settings.completion.case_sensitive {
            CompletionCaseMatch::All
        } else {
            self.settings.completion.match_case
        };
        items.retain(|item| match mode {
            CompletionCaseMatch::None => true,
            CompletionCaseMatch::FirstLetter => prefix
                .chars()
                .next()
                .is_none_or(|first| item.label.starts_with(first)),
            CompletionCaseMatch::All => {
                let mut wanted = prefix.chars();
                let mut next = wanted.next();
                for ch in item.label.chars() {
                    if next == Some(ch) {
                        next = wanted.next();
                    }
                }
                next.is_none()
            }
        });
        if self.settings.completion.sort_lexicographically {
            items.sort_by(|a, b| {
                a.label
                    .to_lowercase()
                    .cmp(&b.label.to_lowercase())
                    .then(a.label.cmp(&b.label))
            });
        } else {
            items = rank_by_usage(items, prefix, &self.completion_stats);
            // Ranking: prefix match quality first (an exact-case prefix
            // before a case-insensitive one, humps, then fuzzy subsequences),
            // then semantic/local/type relevance ahead of learning. Recency
            // breaks equal relevance and frequency is stable. Remaining ties
            // prefer keyword arguments, then submodules (`os.pa` → `path`
            // before `pardir`), then the shorter, closer name.
            items.sort_by_key(|item| {
                (
                    prefix_match_tier(&item.label, prefix),
                    item.priority,
                    self.completion_ui
                        .recent
                        .iter()
                        .position(|name| name == item.label.as_str())
                        .unwrap_or(usize::MAX),
                    std::cmp::Reverse(
                        self.completion_stats
                            .get(item.label.as_str())
                            .copied()
                            .filter(|_| item.label.starts_with(prefix))
                            .unwrap_or(0),
                    ),
                    !features::is_keyword_argument(item),
                    item.kind != CompletionKind::Module,
                    item.label.chars().count(),
                )
            });
        }
        items
    }
    pub(super) fn refresh_completion_order(&mut self) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let prefix = tab.state.doc.slice(TextRange::new(
            self.completion.prefix_start,
            self.completion.caret,
        ));
        let selected = self.completion.items.get(self.completion.selected).cloned();
        let sorted = self.order_completion_items(self.completion.items.clone(), &prefix);
        let mut pairs: Vec<_> = std::mem::take(&mut self.completion.items)
            .into_iter()
            .zip(std::mem::take(&mut self.completion.imports))
            .collect();
        for item in sorted {
            if let Some(index) = pairs.iter().position(|(candidate, _)| candidate == &item) {
                let (item, import) = pairs.remove(index);
                self.completion.items.push(item);
                self.completion.imports.push(import);
            }
        }
        self.completion.selected = selected
            .and_then(|selected| {
                self.completion
                    .items
                    .iter()
                    .position(|item| item == &selected)
            })
            .unwrap_or(if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            });
    }
    pub(super) fn remember_completion(&mut self, label: &str) {
        let uses = self.completion_stats.entry(label.to_owned()).or_insert(0);
        *uses = uses.saturating_add(1);
        save_completion_stats(&self.completion_stats);
        self.completion_ui.recent.retain(|name| name != label);
        self.completion_ui.recent.insert(0, label.to_owned());
        self.completion_ui.recent.truncate(64);
    }
    /// Returns the current string's final path segment; no Python evaluation.
    pub(super) fn path_completion_context(
        &mut self,
        index: usize,
    ) -> Option<(usize, PathBuf, String, char, bool)> {
        if self.non_text_source_hidden() {
            return None;
        }
        let tab = self.tabs.get(index)?;
        if tab.state.language != Language::Python {
            return None;
        }
        let text = tab.state.doc.text();
        let caret = usize::from(tab.state.primary().head());
        let before = text.get(..caret)?;
        // Only a single-line string literal qualifies: lexing from the
        // caret's line start (see `lex_restart`) finds it without a parse.
        let restart = self.tabs[index]
            .state
            .lex_restart(TextSize::new(caret as u32));
        let tab = &self.tabs[index];
        let restart = if restart == 0 {
            0
        } else {
            before.rfind('\n').map_or(0, |at| at + 1)
        };
        let (tokens, _) = rustpy_syntax::tokenize(&before[restart..]);
        let token = tokens
            .iter()
            .rev()
            .find(|token| !token.range.is_empty())
            .filter(|token| usize::from(token.range.end()) == caret - restart)?;
        if token.kind != rustpy_syntax::SyntaxKind::STRING {
            return None;
        }
        let start = restart + usize::from(token.range.start());
        let raw = before.get(start..)?;
        let quote = raw.find(['\'', '"'])?;
        let delimiter = raw.as_bytes()[quote] as char;
        let content = raw.get(quote + 1..)?;
        // Triple/f-strings and escape interpolation need a separate provider.
        if content.starts_with(delimiter)
            || raw[..quote].contains(['f', 'F', 'b', 'B'])
            || content.contains(['\n', '\r', '\\'])
        {
            return None;
        }
        if content.contains(delimiter) {
            return None;
        }
        let segment = content.rfind('/').map_or(0, |at| at + 1);
        let directory = &content[..segment];
        let base = tab.state.path.as_ref()?.parent()?;
        Some((
            start + quote + 1 + segment,
            base.join(directory),
            content[segment..].to_owned(),
            delimiter,
            raw[..quote].contains(['r', 'R']),
        ))
    }
    pub(super) fn open_path_completion(&mut self, explicit: bool) -> bool {
        self.completion_ui.paths = false;
        let Some(index) = self.active else {
            return false;
        };
        let Some((start, directory, prefix, delimiter, raw)) = self.path_completion_context(index)
        else {
            return false;
        };
        if !explicit && !self.background_features().completion_popup {
            return false;
        }
        let mut items = Vec::new();
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.take(1024).flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !name.to_lowercase().starts_with(&prefix.to_lowercase()) {
                    continue;
                }
                let directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
                let Some(insert) = path_insert(
                    &format!("{name}{}", if directory { "/" } else { "" }),
                    delimiter,
                    raw,
                ) else {
                    continue;
                };
                items.push(CompletionItem {
                    label: name.into(),
                    kind: if directory {
                        CompletionKind::Module
                    } else {
                        CompletionKind::Variable
                    },
                    detail: if directory { "directory" } else { "file" }.into(),
                    insert,
                    caret_offset: None,
                    priority: 0,
                });
            }
        }
        let items = self.order_completion_items(items, &prefix);
        let tab = &self.tabs[index];
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: TextSize::new(start as u32),
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            explicit,
            rect: None,
        };
        self.completion_ui.paths = true;
        self.completion_ui.due = None;
        true
    }
    fn prepare_completion_help(&mut self) {
        let Some(index) = self.active else {
            return;
        };
        let Some(item) = self.completion.items.get(self.completion.selected) else {
            return;
        };
        let tab = &self.tabs[index];
        let import = self
            .completion
            .imports
            .get(self.completion.selected)
            .cloned()
            .flatten();
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        let key = CompletionHelpKey {
            editor: tab.view.id,
            version: tab.state.doc.version(),
            caret: self.completion.caret,
            label: item.label.to_string(),
            origin: import
                .as_ref()
                .map(|(module, _)| module.clone())
                .unwrap_or_default(),
            world: analysis.revision_excluding(&file.path),
            buffers: self
                .tabs
                .iter()
                .map(|tab| (tab.view.id, tab.state.doc.version()))
                .collect(),
        };
        if self.completion_ui.help_key.as_ref() == Some(&key) {
            return;
        }
        self.completion_ui.help_key = Some(key);
        self.completion_ui.help = Some(HoverInfo {
            signature: format!("{} {}", item.label, item.detail),
            ..Default::default()
        });
        self.completion_ui.definition = None;
        if self.completion_ui.paths {
            return;
        }
        // Query selected names in a private analysis world, preserving all open
        // buffers. Never edit the user's document or the project's live cache.
        let resolver = rustpy_semantic::module::ModuleResolver::new(
            analysis.resolver.roots.clone(),
            analysis.resolver.search_paths.clone(),
            analysis.resolver.python_version,
        )
        .with_stub_roots(analysis.resolver.stub_roots.iter().cloned())
        .with_platform(&analysis.resolver.platform)
        .with_excluded(analysis.resolver.excluded_paths().iter().cloned());
        let query = Analysis::new(resolver);
        query.set_runtime_types((*analysis.runtime_types()).clone());
        let mut bytes = 0usize;
        for tab in self.tabs.iter().filter(|tab| tab.state.doc.is_modified()) {
            if let Some(path) = tab
                .state
                .path
                .as_ref()
                .filter(|_| tab.state.language == Language::Python)
            {
                let text = tab.state.doc.text();
                bytes = bytes.saturating_add(text.len());
                if bytes > 8 * 1024 * 1024 {
                    return;
                }
                query.set_file_text(path, &text);
            }
        }
        if file.text.len() > 8 * 1024 * 1024 {
            return;
        }
        let (target, offset) = if let Some((module, name)) = import {
            let Some(target) = query.module(&module) else {
                return;
            };
            let offset = if name.is_empty() {
                TextSize::new(0)
            } else {
                let Some(binding) = target
                    .scopes
                    .bindings
                    .iter()
                    .find(|binding| binding.name == name)
                else {
                    return;
                };
                binding.range.start()
            };
            (target, offset)
        } else {
            let mut text = file.text.to_string();
            let start = usize::from(self.completion.prefix_start);
            let end = usize::from(self.completion.caret);
            text.replace_range(start..end, &item.label);
            let id = query.set_file_text(&file.path, &text);
            let Some(target) = query.file(id) else {
                return;
            };
            (target, self.completion.prefix_start)
        };
        if let Some(mut info) = features::hover_at(&query, &target, offset) {
            info.docstring = info.docstring.map(|doc| doc.chars().take(16384).collect());
            self.completion_ui.help = Some(info);
        }
        if let Some(location) = features::definition_at(&query, &target, offset).first() {
            if let Some(definition) = query.file_for_path(&location.path) {
                let at = usize::from(location.range.start());
                let line = definition.text[..at.min(definition.text.len())]
                    .matches('\n')
                    .count()
                    + 1;
                // Definition locations identify the name token. Preview the
                // whole source line so `def`/`class` and decorators' indentation
                // are not dropped from the user-visible definition excerpt.
                let start = definition.text[..at.min(definition.text.len())]
                    .rfind('\n')
                    .map_or(0, |newline| newline + 1);
                let excerpt = definition.text[start..]
                    .lines()
                    .take(40)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.completion_ui.definition = Some((
                    location.path.clone(),
                    line,
                    excerpt.chars().take(16384).collect(),
                ));
            }
        }
    }
    pub(super) fn draw_completion_documentation(&mut self, ui: &mut egui::Ui, lookup: Rect) {
        if !self.editor_keyboard() {
            return;
        }
        self.completion_ui.auxiliary_rect = None;
        if self.completion_ui.import_options {
            let label = self
                .completion
                .items
                .get(self.completion.selected)
                .map(|item| item.label.clone());
            let options: Vec<_> = self
                .completion
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| label.as_ref().is_none_or(|label| &item.label == label))
                .filter_map(|(index, _)| {
                    self.completion
                        .imports
                        .get(index)
                        .and_then(Option::as_ref)
                        .map(|(module, name)| {
                            (
                                index,
                                if name.is_empty() {
                                    format!("import {module}")
                                } else {
                                    format!("from {module} import {name}")
                                },
                            )
                        })
                })
                .collect();
            let mut open = true;
            let mut chosen = None;
            let window = egui::Window::new("Import options")
                .open(&mut open)
                .collapsible(false)
                .show_modeless(ui.ctx(), |ui| {
                    if options.is_empty() {
                        ui.label("Searching import options…");
                    }
                    for (index, description) in &options {
                        if ui.button(description).clicked() {
                            chosen = Some(*index);
                        }
                    }
                });
            self.completion_ui.auxiliary_rect = window.map(|window| window.response.rect);
            self.completion_ui.import_options = open && chosen.is_none();
            if let Some(index) = chosen {
                self.completion.selected = index;
                self.accept_completion(false);
            }
        }
        if !(self.settings.completion.show_documentation || self.completion_ui.documentation) {
            return;
        }
        self.prepare_completion_help();
        let Some(help) = self.completion_ui.help.as_ref() else {
            return;
        };
        let screen = ui.ctx().content_rect();
        let width = 360.0_f32.min(screen.width() / 2.0);
        let x = if lookup.right() + width + 8.0 <= screen.right() {
            lookup.right() + 4.0
        } else {
            (lookup.left() - width - 4.0).max(screen.left())
        };
        let area = egui::Area::new(ui.id().with("completion_documentation"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(x, lookup.top()))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.label(egui::RichText::new(&help.signature).monospace());
                    if let Some(origin) = &help.origin {
                        ui.label(egui::RichText::new(origin).color(self.theme.text_dim));
                    }
                    if let Some(doc) = &help.docstring {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                ui.label(doc);
                            });
                    }
                    ui.label(
                        egui::RichText::new("Ctrl+Q documentation · Ctrl+Shift+I definition")
                            .small()
                            .color(self.theme.text_dim),
                    );
                });
            });
        self.completion_ui.auxiliary_rect = Some(
            self.completion_ui
                .auxiliary_rect
                .map_or(area.response.rect, |previous| {
                    previous.union(area.response.rect)
                }),
        );
    }
    /// Records a keyboard move of the lookup selection (Up/Down/PageUp/PageDown).
    pub(super) fn completion_navigated(&mut self) {
        self.completion_ui.navigated = self
            .completion
            .items
            .get(self.completion.selected)
            .map(|item| (self.completion.prefix_start, item.label.to_string()));
    }
    /// Whether a context character (space, dot, `(`, `=`) may insert the
    /// selected item (the "Insert selected suggestion by pressing space, dot,
    /// or other context-dependent keys" behaviour): the preselected item
    /// commits once a prefix is typed, but a lookup with an EMPTY prefix (right
    /// after `{%`, `{{` or `.`) commits only an item the user moved to. So
    /// `{% url` never turns into `{%autoescape url`.
    ///
    /// A preselected item commits only when its lookup string really begins
    /// with the typed prefix (case-insensitively after an exact first letter,
    /// or as camel humps: `gV` → `getValue`). A fuzzy subsequence such as
    /// `anext` for `x`, or `ValueError` for `value`, is text: typing `x = 1`
    /// must never become `anext = 1`.
    fn context_commit_allowed(&self, item: &CompletionItem, prefix: &str) -> bool {
        let navigated = self
            .completion_ui
            .navigated
            .as_ref()
            .is_some_and(|(start, label)| {
                *start == self.completion.prefix_start && label.as_str() == item.label.as_str()
            });
        navigated || !prefix.is_empty() && context_commit_matches(&item.label, prefix)
    }
    pub(super) fn completion_context_keys(&mut self, ui: &mut egui::Ui) {
        if !self.editor_keyboard() {
            return;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Q)) {
            self.completion_ui.documentation = !self.completion_ui.documentation;
            self.prepare_completion_help();
        }
        if ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::I)
        }) {
            self.prepare_completion_help();
            if let Some((path, line, text)) = self.completion_ui.definition.clone() {
                self.dialog = Dialog::QuickDefinition {
                    title: format!("{}:{line}", path.display()),
                    path,
                    line,
                    text,
                };
            }
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::ALT, egui::Key::Enter)) {
            self.completion_ui.import_options = true;
            if self.completion.imports.iter().all(Option::is_none) {
                self.open_completion(true);
            }
        }
        if !self.settings.completion.insert_by_context_characters {
            return;
        }
        let Some(item) = self.completion.items.get(self.completion.selected) else {
            return;
        };
        // Shortcuts run before the editor consumes this frame's text. A later
        // space/dot in a batch cannot commit a lookup computed for the prefix
        // BEFORE earlier characters in that same batch were inserted.
        if self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_none_or(|tab| {
                self.completion.doc_version != tab.state.doc.version()
                    || self.completion.caret != tab.state.primary().head()
            })
        {
            self.completion.visible = false;
            return;
        }
        let commit = ui.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::Text(text) => {
                        return matches!(text.as_str(), "." | "(" | "=" | " ").then(|| text.clone())
                    }
                    egui::Event::Paste(_)
                    | egui::Event::Cut
                    | egui::Event::Ime(_)
                    | egui::Event::PointerButton { pressed: true, .. } => return None,
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command
                        || modifiers.ctrl
                        || modifiers.alt
                        || matches!(
                            key,
                            egui::Key::Backspace
                                | egui::Key::Delete
                                | egui::Key::ArrowLeft
                                | egui::Key::ArrowRight
                                | egui::Key::ArrowUp
                                | egui::Key::ArrowDown
                                | egui::Key::Home
                                | egui::Key::End
                                | egui::Key::Enter
                                | egui::Key::Tab
                        ) =>
                    {
                        return None
                    }
                    _ => {}
                }
            }
            None
        });
        let Some(commit) = commit else {
            return;
        };
        let prefix = self
            .active
            .and_then(|index| self.tabs.get(index))
            .filter(|_| self.completion.prefix_start <= self.completion.caret)
            .map(|tab| {
                tab.state.doc.slice(TextRange::new(
                    self.completion.prefix_start,
                    self.completion.caret,
                ))
            })
            .unwrap_or_default();
        if !self.context_commit_allowed(item, &prefix) {
            return;
        }
        let mut html_attribute = false;
        if let Some(tab) = self
            .active
            .and_then(|index| self.tabs.get(index))
            .filter(|tab| tab.state.language == Language::Html)
        {
            let source = tab.state.doc.text();
            let caret = usize::from(tab.state.primary().head());
            let Some(context) = super::data_file_completion::html_name_context(&source, caret)
            else {
                return;
            };
            if context.start() == caret
                || context.start() != usize::from(self.completion.prefix_start)
            {
                return;
            }
            html_attribute = matches!(
                context,
                super::data_file_completion::HtmlNameContext::Attribute(_)
            );
            if commit != " " && !(commit == "=" && html_attribute) {
                return;
            }
        }
        let callable = matches!(
            item.kind,
            CompletionKind::Function | CompletionKind::Method | CompletionKind::Class
        );
        if commit == "(" && !callable
            || commit == "=" && !html_attribute && !features::is_keyword_argument(item)
        {
            return;
        }
        let swallow = commit == "("
            && self.settings.completion.insert_parentheses
            && item.insert.contains('(')
            || commit == "=" && item.insert.contains('=');
        // Only a callable insertion can own a closing delimiter. A variable
        // completed inside an existing argument must keep its context character
        // inside that argument.
        let owns_closing = callable
            && self.settings.completion.insert_parentheses
            && item.insert.ends_with("()")
            && item.caret_offset == item.insert.len().checked_sub(1)
            && self
                .active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| tab.state.doc.char_at(tab.state.primary().head()) != Some('('));
        self.accept_completion(false);
        if swallow {
            ui.input_mut(|input| {
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Text(text) if text == &commit))
            });
        } else if commit != "(" && owns_closing {
            if let Some(tab) = self.active.and_then(|index| self.tabs.get_mut(index)) {
                let caret = tab.state.primary().head();
                if tab.state.doc.char_at(caret) == Some(')') {
                    tab.state
                        .set_carets(vec![crate::editor::Caret::at(caret + TextSize::new(1))]);
                }
            }
        }
    }
}

/// Prefix match quality of `label` for `prefix` (lower is better): 0 exact-case
/// prefix, 1 case-insensitive prefix, 2 camel humps, 3 any other (fuzzy) match.
fn prefix_match_tier(label: &str, prefix: &str) -> u8 {
    if label.starts_with(prefix) {
        0
    } else if label.to_lowercase().starts_with(&prefix.to_lowercase()) {
        1
    } else if rustpy_semantic::completion2::camel_hump_match(label, prefix) {
        2
    } else {
        3
    }
}

/// Whether a context character may commit `label` for the typed `prefix`: the
/// first letter matches exactly (the default first-letter case match)
/// and the rest is a case-insensitive prefix or camel-hump match.
fn context_commit_matches(label: &str, prefix: &str) -> bool {
    label.chars().next() == prefix.chars().next() && prefix_match_tier(label, prefix) < 3
}

fn path_insert(name: &str, delimiter: char, raw: bool) -> Option<String> {
    if raw {
        // A raw literal cannot represent its delimiter or an odd trailing slash.
        if name.contains(delimiter)
            || name.contains(['\n', '\r'])
            || name.chars().rev().take_while(|ch| *ch == '\\').count() % 2 == 1
        {
            return None;
        }
        return Some(name.into());
    }
    let mut out = String::new();
    for ch in name.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch == delimiter => {
                out.push('\\');
                out.push(ch);
            }
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    Some(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            kind: CompletionKind::Variable,
            detail: String::new(),
            insert: label.into(),
            caret_offset: None,
            priority: 0,
        }
    }
    fn app(source: &str) -> (tempfile::TempDir, IdeApp) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        assert!(
            app.editor_keyboard(),
            "fixture must own source input before completion"
        );
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.len() as u32
            ))]);
        (root, app)
    }
    #[test]
    fn popup_delay_requires_same_editor_version_caret_and_focus() {
        let mut state = CompletionUiState::default();
        let view = egui::Id::new("source");
        let caret = TextSize::new(4);
        state.due = Some((view, 2, caret, 1.5));
        assert_eq!(
            state.popup_wait(view, 2, caret, true, 1.0),
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(
            state.popup_wait(view, 2, caret, true, 1.5),
            Some(std::time::Duration::ZERO)
        );
        assert!(state.due.is_none());
        for (actual_view, version, actual_caret, focused) in [
            (egui::Id::new("other"), 2, caret, true),
            (view, 3, caret, true),
            (view, 2, TextSize::new(5), true),
            (view, 2, caret, false),
        ] {
            state.due = Some((view, 2, caret, 1.5));
            assert!(state
                .popup_wait(actual_view, version, actual_caret, focused, 2.0)
                .is_none());
            assert!(state.due.is_none());
        }
    }
    #[test]
    fn matching_modes_lexical_order_and_recency_are_independent_preferences() {
        let (_root, mut app) = app("value=1\n");
        let values = vec![item("readText"), item("ReadText"), item("renderTree")];
        app.settings.completion.match_case = rustpy_settings::CompletionCaseMatch::FirstLetter;
        let found = app.order_completion_items(values.clone(), "rT");
        assert_eq!(found.len(), 2);
        app.settings.completion.match_case = rustpy_settings::CompletionCaseMatch::All;
        assert_eq!(
            app.order_completion_items(values.clone(), "RT")
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["ReadText"]
        );
        app.settings.completion.match_case = rustpy_settings::CompletionCaseMatch::None;
        app.settings.completion.sort_lexicographically = true;
        let found = app.order_completion_items(vec![item("zebra"), item("alpha")], "");
        assert_eq!(found[0].label.as_str(), "alpha");
        app.settings.completion.sort_lexicographically = false;
        app.completion_ui.recent = vec!["zebra".into()];
        assert_eq!(
            app.order_completion_items(vec![item("alpha"), item("zebra")], "")[0]
                .label
                .as_str(),
            "zebra"
        );
        let job = highlight_label("readText", "rT", Color32::WHITE, Color32::YELLOW);
        assert_eq!(
            job.sections
                .iter()
                .filter(|section| section.format.color == Color32::YELLOW)
                .count(),
            2
        );
    }
    #[test]
    fn context_commit_needs_a_real_prefix_and_ranking_prefers_prefix_matches() {
        for (label, prefix, commits) in [
            ("path", "pa", true),
            ("value", "val", true),
            ("ValueError", "Value", true),
            ("ValueError", "VE", true),
            ("getValue", "gV", true),
            ("get_value", "gv", true),
            ("anext", "x", false),
            ("ValueError", "value", false),
            ("pardir", "pth", false),
        ] {
            assert_eq!(
                context_commit_matches(label, prefix),
                commits,
                "{label} / {prefix}"
            );
        }
        let (_root, app) = app("import os\n");
        let mut module = item("path");
        module.kind = CompletionKind::Module;
        let ordered = app.order_completion_items(
            vec![
                item("anext"),
                item("pardir"),
                item("pathsep"),
                module,
                item("Pa"),
            ],
            "pa",
        );
        assert_eq!(
            ordered.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            vec!["path", "pardir", "pathsep", "Pa", "anext"]
        );
    }
    #[test]
    fn path_completion_works_explicitly_with_auto_popup_disabled_and_preserves_quotes() {
        let (root, mut app) = app("path = 'im'\n");
        std::fs::write(root.path().join("image.png"), "data").unwrap();
        app.settings.completion.show_as_you_type = false;
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(10))]);
        app.settings.completion.preselect_first = false;
        assert!(app.open_path_completion(true));
        assert_eq!(app.completion.selected, usize::MAX);
        let selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "image.png")
            .unwrap();
        app.completion.selected = selected;
        app.accept_completion(false);
        assert_eq!(app.tabs[index].state.doc.text(), "path = 'image.png'\n");
    }
    #[test]
    fn selected_documentation_and_definition_follow_candidate_without_editing_buffer() {
        let source = "def convert(value: int) -> str:\n    \"\"\"Converts the selected value.\"\"\"\n    return str(value)\ncon";
        let (_root, mut app) = app(source);
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "convert")
            .unwrap();
        app.prepare_completion_help();
        assert!(app
            .completion_ui
            .help
            .as_ref()
            .unwrap()
            .signature
            .contains("convert"));
        assert_eq!(
            app.completion_ui
                .help
                .as_ref()
                .unwrap()
                .docstring
                .as_deref(),
            Some("Converts the selected value.")
        );
        assert!(app
            .completion_ui
            .definition
            .as_ref()
            .unwrap()
            .2
            .contains("def convert"));
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
    }
    #[test]
    fn callable_commit_parenthesis_is_not_inserted_twice_and_parameter_help_opens() {
        let (_root, mut app) = app("def convert(value: int) -> str: return str(value)\ncon");
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "convert")
            .unwrap();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Text("(".into())],
                ..Default::default()
            },
            |ui| {
                app.completion_context_keys(ui);
                assert!(!ui.input(|input| input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Text(text) if text == "("))));
            },
        );
        output.drop_without_applying_deltas();
        let tab = &app.tabs[app.active.unwrap()];
        assert!(tab.state.doc.text().ends_with("convert()"));
        assert_eq!(tab.state.doc.char_at(tab.state.primary().head()), Some(')'));
        assert!(app.param_info.is_some());
    }
    #[test]
    fn variable_context_commit_keeps_existing_argument_delimiter() {
        for (source, expected) in [
            ("value = 1\nconsume(val)", "value = 1\nconsume(value)"),
            (
                "def value(): return 1\nconsume(val)",
                "def value(): return 1\nconsume(value())",
            ),
        ] {
            let (_root, mut app) = app(source);
            let index = app.active.unwrap();
            let caret = app.tabs[index].state.doc.len() - TextSize::new(1);
            app.tabs[index]
                .state
                .set_carets(vec![crate::editor::Caret::at(caret)]);
            app.open_completion(true);
            app.completion.selected = app
                .completion
                .items
                .iter()
                .position(|item| item.label == "value")
                .unwrap();
            let ctx = egui::Context::default();
            let output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Text(".".into())],
                    ..Default::default()
                },
                |ui| {
                    app.completion_context_keys(ui);
                },
            );
            output.drop_without_applying_deltas();
            let tab = &app.tabs[index];
            assert_eq!(tab.state.doc.text(), expected);
            assert_eq!(tab.state.doc.char_at(tab.state.primary().head()), Some(')'));
        }
    }
    #[test]
    fn path_candidates_cannot_break_the_existing_python_literal() {
        let (root, mut app) = app("path = 'it'");
        std::fs::write(root.path().join("it's.txt"), "").unwrap();
        std::fs::write(root.path().join("item.txt"), "").unwrap();
        let index = app.active.unwrap();
        let caret = app.tabs[index].state.doc.len() - TextSize::new(1);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(caret)]);
        assert!(app.open_path_completion(true));
        assert!(app
            .completion
            .items
            .iter()
            .any(|item| item.label == "item.txt"));
        let item = app
            .completion
            .items
            .iter()
            .find(|item| item.label == "it's.txt")
            .unwrap();
        assert_eq!(
            rustpy_syntax::string_literal::string(&format!("'{}'", item.insert)).unwrap(),
            rustpy_syntax::string_literal::Decoded::Text("it's.txt".into(), None)
        );
        for name in ["name\\part", "it's.txt", "line\nbreak"] {
            let encoded = path_insert(name, '\'', false).unwrap();
            assert_eq!(
                rustpy_syntax::string_literal::string(&format!("'{encoded}'")).unwrap(),
                rustpy_syntax::string_literal::Decoded::Text(name.into(), None)
            );
        }
    }
    #[test]
    fn completion_preferences_round_trip_with_safe_old_settings_defaults() {
        let prefs = rustpy_settings::CompletionPrefs {
            match_case: rustpy_settings::CompletionCaseMatch::FirstLetter,
            lookup_item_height: 30.0,
            preselect_first: false,
            show_full_signatures: true,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&prefs).unwrap();
        assert_eq!(
            serde_json::from_str::<rustpy_settings::CompletionPrefs>(&encoded).unwrap(),
            prefs
        );
        let old: rustpy_settings::CompletionPrefs =
            serde_json::from_str("{\"auto_popup_delay_ms\":125}").unwrap();
        assert_eq!(old.auto_popup_delay_ms, 125);
        assert!(old.preselect_first);
        assert!(old.show_parameter_info);
    }
}

#[cfg(test)]
#[path = "completion_input_tests.rs"]
mod input_tests;

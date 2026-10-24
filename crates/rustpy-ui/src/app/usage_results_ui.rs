//! Find Usages sessions: independently pinned results, options, preview and rerun.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_search::usages::results::{Group, Hit, Options, Scope, Session, View};
use rustpy_settings::scopes::ScopeSelection;

/// Scope chooser rows; the last one defers to the shared Settings → Scopes model.
const SCOPE_LABELS: [&str; 6] = [
    "Project",
    "Current file",
    "Directory",
    "Open files",
    "Custom paths",
    "Named scope",
];
const NAMED_SCOPE: usize = 5;

#[derive(Clone, Debug)]
pub(super) struct Target {
    pub path: PathBuf,
    pub offset: TextSize,
    pub name: String,
}
#[derive(Clone, Default)]
struct Saved {
    alias: String,
    color: Option<[u8; 3]>,
    protected: bool,
    session: Session,
    target: Option<Target>,
    options: Options,
    scope_choice: usize,
    scope_paths: String,
    scope_selection: ScopeSelection,
}
pub(super) struct UsageUi {
    pub options: Options,
    pub target: Option<Target>,
    pub session: Session,
    pinned: Vec<Saved>,
    active_pin: Option<usize>,
    pub(super) searching: bool,
    group: Group,
    sort_by: usize,
    filter: String,
    merge: bool,
    reverse: bool,
    reads: bool,
    writes: bool,
    dynamic: bool,
    show_excluded: bool,
    preview: bool,
    follow_to: bool,
    follow_from: bool,
    editor: Option<(PathBuf, TextSize)>,
    selected: Option<usize>,
    row_focus: Option<(egui::Context, egui::Id)>,
    pub(super) settings: bool,
    scope_choice: usize,
    scope_paths: String,
    scope_selection: ScopeSelection,
    export_path: String,
}
impl Default for UsageUi {
    fn default() -> Self {
        Self {
            options: Options::default(),
            target: None,
            session: Session::default(),
            pinned: Vec::new(),
            active_pin: None,
            searching: false,
            group: Group::File,
            sort_by: 0,
            filter: String::new(),
            merge: false,
            reverse: false,
            reads: true,
            writes: true,
            dynamic: true,
            show_excluded: false,
            preview: true,
            follow_to: false,
            follow_from: false,
            editor: None,
            selected: None,
            row_focus: None,
            settings: false,
            scope_choice: 0,
            scope_paths: String::new(),
            scope_selection: ScopeSelection::default(),
            export_path: "usages.tsv".into(),
        }
    }
}
impl UsageUi {
    pub(super) fn in_file(&mut self) {
        self.scope_choice = 1;
    }
    fn store_active_pin(&mut self) {
        if let Some(index) = self.active_pin {
            if let Some(saved) = self.pinned.get_mut(index) {
                *saved = Saved {
                    alias: saved.alias.clone(),
                    color: saved.color,
                    protected: saved.protected,
                    session: self.session.clone(),
                    target: self.target.clone(),
                    options: self.options.clone(),
                    scope_choice: self.scope_choice,
                    scope_paths: self.scope_paths.clone(),
                    scope_selection: self.scope_selection.clone(),
                };
            }
        }
    }
    pub(super) fn begin_search(&mut self) {
        self.store_active_pin();
        self.active_pin = None;
        self.searching = true;
    }
    pub(super) fn adopt(&mut self, title: String, hits: Vec<Hit>) {
        self.store_active_pin();
        self.active_pin = None;
        self.session = Session {
            title,
            hits,
            ..Default::default()
        };
        self.selected = None;
        self.row_focus = None;
    }
    fn pin(&mut self) {
        self.store_active_pin();
        if self.pinned.len() >= 64 {
            return;
        }
        self.pinned.push(Saved {
            protected: true,
            session: self.session.clone(),
            target: self.target.clone(),
            options: self.options.clone(),
            scope_choice: self.scope_choice,
            scope_paths: self.scope_paths.clone(),
            scope_selection: self.scope_selection.clone(),
            ..Default::default()
        });
        self.active_pin = Some(self.pinned.len() - 1);
    }
}
impl IdeApp {
    pub(super) fn open_usage_settings(&mut self) {
        self.usage_ui.settings = true;
        self.select_usage_results();
    }
    pub(super) fn usage_options(&self, origin: &Path) -> Options {
        let mut options = self.usage_ui.options.clone();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.as_path())
            .unwrap_or(Path::new(""));
        let paths = || {
            self.usage_ui
                .scope_paths
                .split(';')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(|path| {
                    let path = PathBuf::from(path);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    };
                    path.canonicalize().unwrap_or(path)
                })
                .collect::<Vec<_>>()
        };
        options.scope = match self.usage_ui.scope_choice {
            1 => Scope::File(origin.into()),
            2 => Scope::Directory(
                paths()
                    .first()
                    .cloned()
                    .unwrap_or_else(|| origin.parent().unwrap_or(root).into()),
            ),
            3 => Scope::OpenFiles(
                self.tabs
                    .iter()
                    .filter_map(|tab| tab.state.path.clone())
                    .collect(),
            ),
            4 => Scope::Custom(paths()),
            NAMED_SCOPE => self.usage_named_scope(),
            _ => Scope::Project,
        };
        options
    }
    /// Resolves the chosen Settings → Scopes selection with the same compiler and
    /// captured context as Find in Files and Rename; an invalid scope admits nothing.
    fn usage_named_scope(&self) -> Scope {
        let selection = &self.usage_ui.scope_selection;
        let label = self.settings.scopes.label(selection);
        let Ok(compiled) = self.settings.scopes.compile(selection) else {
            return Scope::Named(label, Default::default());
        };
        let mut files = self.project_files();
        files.extend(self.tabs.iter().filter_map(|tab| tab.state.path.clone()));
        let mut context = self.capture_scope_context();
        if context.project_files.is_empty() {
            let scope = self.project_scope();
            let mut project: Vec<_> = files
                .iter()
                .filter(|path| scope.as_ref().is_some_and(|scope| scope.contains(path)))
                .cloned()
                .collect();
            project.sort();
            project.dedup();
            context.project_files = std::sync::Arc::new(project);
        }
        let admitted = files
            .into_iter()
            .filter(|path| compiled.matches(path, &context))
            .collect();
        Scope::Named(label, std::sync::Arc::new(admitted))
    }
    pub(super) fn rerun_usages(&mut self) {
        let Some(target) = self.usage_ui.target.clone() else {
            self.set_status("Start Find Usages on a symbol before re-running");
            return;
        };
        self.sync_open_tabs_now();
        let Some(db) = self.analysis_handle() else {
            return;
        };
        let Some(file) = db.file_for_path(&target.path) else {
            self.set_status("Search source no longer exists");
            return;
        };
        let name = features::ident_at(&file, target.offset).map(|token| token.text().to_owned());
        if name.as_deref() != Some(target.name.as_str()) {
            self.set_status("Search target changed; invoke Find Usages on the symbol again");
            return;
        }
        let options = self.usage_options(&target.path);
        self.start_usage_search(target, options, db, file);
    }
    /// Shared by the Find window, popup promotion and Code Vision delivery.
    pub(super) fn usages_view(&mut self, ui: &mut egui::Ui) {
        if self.usage_ui.session.title != self.usages_title
            || !self
                .usage_ui
                .session
                .hits
                .iter()
                .map(|hit| &hit.usage)
                .eq(self.usages.iter())
        {
            self.usage_ui.adopt(
                self.usages_title.clone(),
                self.usages.iter().cloned().map(Hit::from_usage).collect(),
            );
            // A legacy popup/Code Vision result does not carry a replay target.
            self.usage_ui.target = None;
        }
        let mut state = std::mem::take(&mut self.usage_ui);
        let mut rerun = false;
        let mut navigate = None;
        let mut export_error = None;
        ui.horizontal_wrapped(|ui| {
            rerun = ui.button("Re-run Find Usages").clicked();
            if ui.button("Find Usages Settings").clicked() {
                state.settings = !state.settings;
            }
            if ui
                .add_enabled(
                    !state.session.hits.is_empty() && !state.searching && state.pinned.len() < 64,
                    egui::Button::new("Pin search tab"),
                )
                .clicked()
            {
                state.pin();
            }
            if ui.button("Copy all as TSV").clicked() {
                ui.ctx().copy_text(state.session.export_tsv());
            }
            ui.checkbox(&mut state.preview, "Preview");
            ui.checkbox(&mut state.follow_to, "Autoscroll to source");
            ui.checkbox(&mut state.follow_from, "Autoscroll from source");
        });
        let mut select_pin = None;
        let mut close_pin = None;
        let mut close_others = None;
        ui.horizontal_wrapped(|ui| {
            for (index, saved) in state.pinned.iter_mut().enumerate() {
                ui.push_id(("pinned_usage", index), |ui| {
                    let title = if saved.alias.is_empty() {
                        &saved.session.title
                    } else {
                        &saved.alias
                    };
                    let mut text = egui::RichText::new(format!(
                        "{}{}",
                        if saved.protected { "📌 " } else { "" },
                        title
                    ));
                    if let Some(color) = saved.color {
                        text = text.color(egui::Color32::from_rgb(color[0], color[1], color[2]));
                    }
                    let response = ui.selectable_label(false, text);
                    response.context_menu(|ui| {
                        ui.checkbox(&mut saved.protected, "Pin tab");
                        ui.label("Rename tab");
                        ui.text_edit_singleline(&mut saved.alias);
                        let mut color = saved.color.unwrap_or([120, 160, 220]);
                        if ui.color_edit_button_srgb(&mut color).changed() {
                            saved.color = Some(color);
                        }
                        if ui.button("Close Other Tabs").clicked() {
                            close_others = Some(index);
                            ui.close();
                        }
                    });
                    if response.clicked() {
                        select_pin = Some(index);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Close pinned search")
                        .clicked()
                    {
                        close_pin = Some(index);
                    }
                });
            }
        });
        if let Some(keep) = close_others {
            state.store_active_pin();
            let active = state.active_pin;
            let mut retained = Vec::new();
            let mut next_active = None;
            for (index, saved) in state.pinned.drain(..).enumerate() {
                if index == keep || saved.protected {
                    if active == Some(index) {
                        next_active = Some(retained.len());
                    }
                    retained.push(saved);
                }
            }
            state.pinned = retained;
            state.active_pin = next_active;
            select_pin = None;
            close_pin = None;
        }
        if let Some(index) = select_pin {
            state.store_active_pin();
            let saved = state.pinned[index].clone();
            state.session = saved.session;
            state.target = saved.target;
            state.options = saved.options;
            state.scope_choice = saved.scope_choice;
            state.scope_paths = saved.scope_paths;
            state.scope_selection = saved.scope_selection;
            state.selected = None;
            state.row_focus = None;
            state.active_pin = Some(index);
            self.usages_title = state.session.title.clone();
            self.usages = state
                .session
                .hits
                .iter()
                .map(|hit| hit.usage.clone())
                .collect();
        }
        if let Some(index) = close_pin {
            state.store_active_pin();
            state.pinned.remove(index);
            state.active_pin = state.active_pin.and_then(|active| {
                if active == index {
                    None
                } else if active > index {
                    Some(active - 1)
                } else {
                    Some(active)
                }
            });
        }
        if state.settings {
            ui.group(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut state.options.usages, "Usages");
                    ui.checkbox(&mut state.options.implementing, "Implementing classes");
                    ui.checkbox(&mut state.options.overriding, "Overriding methods");
                    ui.checkbox(&mut state.options.dynamic, "Dynamic / string references");
                    ui.checkbox(&mut state.options.skip_single, "Skip results tab with one usage");
                });
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut state.options.strings, "Strings"); ui.checkbox(&mut state.options.comments, "Comments"); ui.checkbox(&mut state.options.text_occurrences, "All text occurrences");
                    egui::ComboBox::from_id_salt("usage_scope").selected_text(SCOPE_LABELS[state.scope_choice]).show_ui(ui, |ui| {
                        for (index, label) in SCOPE_LABELS.iter().enumerate() { ui.selectable_value(&mut state.scope_choice, index, *label); }
                    }).combo_named(crate::i18n::tr_static("Scope"));
                    if state.scope_choice == NAMED_SCOPE {
                        let names = self.scope_names();
                        super::scope_ui::scope_selector(ui, &mut state.scope_selection, &names, "usage_named_scope");
                    }
                });
                if matches!(state.scope_choice, 2 | 4) { ui.label("Directory / custom files or directories, separated by semicolons (relative to project):"); ui.text_edit_singleline(&mut state.scope_paths); }
                ui.small("Dynamic and text occurrences are candidates, not proof that two symbols have the same owner.");
                ui.horizontal(|ui| { ui.label("Export file:"); ui.text_edit_singleline(&mut state.export_path); if ui.button("Write TSV file").clicked() {
                    let root = self.project.as_ref().map(|project| project.root.as_path()).unwrap_or(Path::new(""));
                    let path = PathBuf::from(&state.export_path); let path = if path.is_absolute() { path } else { root.join(path) };
                    use std::io::Write;
                    let result = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut file| file.write_all(state.session.export_tsv().as_bytes()));
                    export_error = Some(match result { Ok(()) => format!("Exported usages to {}", path.display()), Err(error) => format!("Cannot export usages: {error}") });
                } });
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter:");
            let speed = crate::tool_tabs::type_to_filter(
                ui.ctx(),
                state.row_focus.as_ref().map(|(_, id)| *id),
                &mut state.filter,
            );
            let search = ui.text_edit_singleline(&mut state.filter);
            if speed {
                search.request_focus();
            }
            egui::ComboBox::from_id_salt("usage_group")
                .selected_text(format!("{:?}", state.group))
                .show_ui(ui, |ui| {
                    for group in [
                        Group::UsageType,
                        Group::Module,
                        Group::Directory,
                        Group::File,
                        Group::Class,
                    ] {
                        ui.selectable_value(&mut state.group, group, format!("{group:?}"));
                    }
                })
                .combo_named(crate::i18n::tr_static("Group"));
            egui::ComboBox::from_id_salt("usage_sort")
                .selected_text(["Source order", "Usage type", "Source text"][state.sort_by])
                .show_ui(ui, |ui| {
                    for (index, label) in ["Source order", "Usage type", "Source text"]
                        .iter()
                        .enumerate()
                    {
                        ui.selectable_value(&mut state.sort_by, index, *label);
                    }
                })
                .combo_named(crate::i18n::tr_static("Sort by"));
            ui.checkbox(&mut state.reverse, "Reverse sort");
            ui.checkbox(&mut state.merge, "Merge same line");
            ui.checkbox(&mut state.reads, "Reads");
            ui.checkbox(&mut state.writes, "Writes");
            ui.checkbox(&mut state.dynamic, "Dynamic");
            ui.checkbox(&mut state.show_excluded, "Show excluded");
            if ui.button("Include all").clicked() {
                state.session.excluded.clear();
            }
        });
        ui.label(format!(
            "{} — {} result(s), {} excluded",
            state.session.title,
            state.session.hits.len(),
            state.session.excluded.len()
        ));
        let editor = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| Some((tab.state.path.clone()?, tab.state.primary().head())));
        let follow_changed = state.follow_from && state.editor != editor;
        if follow_changed {
            if let Some((path, offset)) = &editor {
                state.selected = state
                    .session
                    .hits
                    .iter()
                    .enumerate()
                    .filter(|(_, hit)| {
                        &hit.usage.path == path && hit.usage.range.start() <= *offset
                    })
                    .max_by_key(|(_, hit)| hit.usage.range.start())
                    .map(|(index, _)| index);
            }
        }
        state.editor = editor;
        let mut groups: Vec<_> = state
            .session
            .grouped(
                state.group,
                View {
                    filter: &state.filter,
                    show_excluded: state.show_excluded,
                    reads: state.reads,
                    writes: state.writes,
                    dynamic: state.dynamic,
                    merge_lines: state.merge,
                },
            )
            .into_iter()
            .collect();
        if state.reverse {
            groups.reverse();
        }
        let height = if state.preview {
            ui.available_height().max(100.0) * 0.58
        } else {
            ui.available_height()
        };
        egui::ScrollArea::vertical()
            .id_salt("usage_results_tree")
            .max_height(height)
            .show(ui, |ui| {
                for (group, mut indices) in groups {
                    indices.sort_by(|left, right| {
                        let left = &state.session.hits[*left];
                        let right = &state.session.hits[*right];
                        match state.sort_by {
                            1 => (left.kind, &left.usage.path, left.usage.line).cmp(&(
                                right.kind,
                                &right.usage.path,
                                right.usage.line,
                            )),
                            2 => left.usage.line_text.cmp(&right.usage.line_text),
                            _ => (&left.usage.path, left.usage.range.start())
                                .cmp(&(&right.usage.path, right.usage.range.start())),
                        }
                    });
                    if state.reverse {
                        indices.reverse();
                    }
                    egui::CollapsingHeader::new(group)
                        .default_open(true)
                        .show(ui, |ui| {
                            for index in indices {
                                let hit = &state.session.hits[index];
                                let usage = &hit.usage;
                                let mut included = !state.session.is_excluded(index);
                                let label = format!(
                                    "{} {}:{}:{}  {}",
                                    hit.kind.label(),
                                    usage.path.file_name().unwrap_or_default().to_string_lossy(),
                                    usage.line + 1,
                                    usage.column + 1,
                                    usage.line_text.trim()
                                );
                                let path = usage.path.clone();
                                let range = usage.range;
                                let line = usage.line;
                                let kind = hit.kind;
                                let response = ui
                                    .horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(kind.icon()).monospace().color(
                                                if kind
                                                    == rustpy_search::usages::results::Kind::Write
                                                {
                                                    self.theme.warning
                                                } else {
                                                    self.theme.text_dim
                                                },
                                            ),
                                        )
                                        .on_hover_text(kind.label());
                                        let changed = ui
                                            .checkbox(&mut included, "")
                                            .on_hover_text("Include / exclude this occurrence")
                                            .changed();
                                        let response = ui
                                            .selectable_label(state.selected == Some(index), label);
                                        (changed, response)
                                    })
                                    .inner;
                                if response.0 {
                                    if state.merge {
                                        state.session.set_line_excluded(&path, line, !included);
                                    } else {
                                        state.session.set_excluded(index, !included);
                                    }
                                }
                                if response.1.clicked() {
                                    state.selected = Some(index);
                                    response.1.request_focus();
                                    state.row_focus = Some((ui.ctx().clone(), response.1.id));
                                    self.focus_editor = false;
                                    if state.follow_to {
                                        navigate = Some((path.clone(), range));
                                    }
                                }
                                if follow_changed && state.selected == Some(index) {
                                    response.1.scroll_to_me(Some(egui::Align::Center));
                                }
                                if response.1.double_clicked() {
                                    navigate = Some((path.clone(), range));
                                }
                                response.1.context_menu(|ui| {
                                    if ui.button("Jump to source").clicked() {
                                        navigate = Some((path, range));
                                        ui.close();
                                    }
                                });
                            }
                        });
                }
            });
        if state.preview {
            if let Some(hit) = state
                .selected
                .and_then(|index| state.session.hits.get(index))
            {
                ui.separator();
                ui.label(format!(
                    "{}:{}",
                    hit.usage.path.display(),
                    hit.usage.line + 1
                ));
                let text = self
                    .tabs
                    .iter()
                    .find(|tab| tab.state.path.as_deref() == Some(hit.usage.path.as_path()))
                    .map(|tab| tab.state.doc.text())
                    .or_else(|| {
                        self.analysis_handle()
                            .and_then(|db| db.current_text(&hit.usage.path))
                            .map(|text| text.to_string())
                    });
                if let Some(text) = text {
                    egui::ScrollArea::both()
                        .id_salt("usage_preview")
                        .show(ui, |ui| {
                            for (line, value) in text
                                .lines()
                                .enumerate()
                                .skip(hit.usage.line.saturating_sub(3))
                                .take(7)
                            {
                                ui.label(
                                    egui::RichText::new(format!("{:>5} {value}", line + 1))
                                        .monospace()
                                        .color(if line == hit.usage.line {
                                            self.theme.accent
                                        } else {
                                            self.theme.text
                                        }),
                                );
                            }
                        });
                }
                if ui.button("Jump to source (F4)").clicked() {
                    navigate = Some((hit.usage.path.clone(), hit.usage.range));
                }
            }
        }
        state.store_active_pin();
        self.usage_ui = state;
        if let Some(status) = export_error {
            self.set_status(status);
        }
        if rerun {
            self.rerun_usages();
        }
        if let Some((path, range)) = navigate {
            // Double-click uses the same owned-row route as central F4; the
            // explicit preview button may itself own focus instead of the row.
            let selected_matches = self
                .usage_ui
                .selected
                .and_then(|index| self.usage_ui.session.hits.get(index))
                .is_some_and(|hit| hit.usage.path == path && hit.usage.range == range);
            if !selected_matches || !self.jump_to_selected_usage() {
                self.navigate_usage(&path, range);
            }
        }
    }
    /// Central F4 dispatch: only the actually focused selected Find row owns this route.
    pub(super) fn jump_to_selected_usage(&mut self) -> bool {
        let focused = self
            .usage_ui
            .row_focus
            .as_ref()
            .is_some_and(|(ctx, id)| ctx.memory(|memory| memory.focused() == Some(*id)));
        if !focused {
            return false;
        }
        let Some(hit) = self
            .usage_ui
            .selected
            .and_then(|index| self.usage_ui.session.hits.get(index))
        else {
            return false;
        };
        let path = hit.usage.path.clone();
        let range = hit.usage.range;
        self.navigate_usage(&path, range);
        true
    }
    fn navigate_usage(&mut self, path: &Path, range: TextRange) {
        let hit = self
            .usage_ui
            .session
            .hits
            .iter()
            .find(|hit| hit.usage.path.as_path() == path && hit.usage.range == range);
        let current = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(path))
            .map(|tab| tab.state.doc.text())
            .or_else(|| {
                self.analysis_handle()
                    .and_then(|db| db.current_text(path))
                    .map(|text| text.to_string())
            });
        let fresh = hit.zip(current.as_deref()).is_some_and(|(hit, text)| {
            let offset: usize = text
                .split_inclusive('\n')
                .take(hit.usage.line)
                .map(str::len)
                .sum::<usize>()
                + hit.usage.column;
            offset == usize::from(range.start())
                && text.split('\n').nth(hit.usage.line) == Some(hit.usage.line_text.as_str())
        });
        if fresh {
            self.navigate_to(path, range);
        } else {
            self.set_status("Result source changed; re-run Find Usages before navigating");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit(text: &str, write: bool) -> Hit {
        Hit::from_usage(Usage {
            path: "/usage-example.py".into(),
            range: TextRange::new(TextSize::new(0), TextSize::new(5)),
            line: 0,
            column: 0,
            line_text: text.into(),
            is_write: write,
            is_definition: false,
            dynamic: false,
            non_code: false,
        })
    }
    fn deliver(app: &mut IdeApp, title: &str, hits: Vec<Hit>) {
        app.usages_title = title.into();
        app.usages = hits.iter().map(|hit| hit.usage.clone()).collect();
        app.usage_ui.adopt(title.into(), hits);
    }
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<(String, Rect)> {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0))),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.usages_view(ui));
            },
        );
        let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        text
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, rect: Rect) {
        for (dt, pressed) in [(0.0, true), (0.02, false)] {
            frame(
                app,
                ctx,
                time + dt,
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::PointerButton {
                        pos: rect.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn settings_action_and_rendered_pin_keep_independent_results_and_exclusions() {
        let mut app = IdeApp::new(Settings::default(), None);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.handle_action(Action::FindUsagesSettings, &ctx);
        assert!(app.usage_ui.settings);
        deliver(&mut app, "first", vec![hit("value = 1  # π", true)]);
        frame(&mut app, &ctx, 0.0, vec![]);
        let labels = frame(&mut app, &ctx, 0.02, vec![]);
        let pin = labels
            .iter()
            .find(|(text, _)| text == "Pin search tab")
            .expect("actual rendered pin control")
            .1;
        click(&mut app, &ctx, 0.1, pin);
        assert_eq!(app.usage_ui.pinned.len(), 1);
        app.usage_ui.session.set_excluded(0, true);
        deliver(&mut app, "second", vec![hit("print(value)", false)]);
        assert_eq!(app.usage_ui.pinned[0].session.title, "first");
        assert!(app.usage_ui.pinned[0].session.is_excluded(0));
        assert_eq!(app.usage_ui.session.title, "second");
        assert!(!app.usage_ui.session.is_excluded(0));
        let labels = frame(&mut app, &ctx, 0.2, vec![]);
        let writes = labels
            .iter()
            .find(|(text, _)| text == "Writes")
            .expect("actual write-access filter")
            .1;
        click(&mut app, &ctx, 0.3, writes);
        assert!(!app.usage_ui.writes);
    }
    #[test]
    fn actual_result_selection_previews_current_source_and_refuses_stale_jump() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample.py");
        let source = "value = 1\nprint(value)\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, None);
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let offset = source.rfind("value").unwrap() as u32;
        let mut result = hit("print(value)", false);
        result.usage.path = path.clone();
        result.usage.line = 1;
        result.usage.column = 6;
        result.usage.range = TextRange::new(TextSize::new(offset), TextSize::new(offset + 5));
        deliver(&mut app, "value", vec![result]);
        frame(&mut app, &ctx, 0.0, vec![]);
        let labels = frame(&mut app, &ctx, 0.2, vec![]);
        let row = labels
            .iter()
            .find(|(label, _)| {
                label.starts_with("Read sample.py") && label.ends_with("print(value)")
            })
            .expect("actual rendered usage row")
            .1;
        click(&mut app, &ctx, 0.3, row);
        assert_eq!(app.usage_ui.selected, Some(0));
        let labels = frame(&mut app, &ctx, 0.4, vec![]);
        assert!(
            labels.iter().any(|(label, _)| label.contains("value = 1")),
            "preview must paint surrounding actual source"
        );
        let index = app.active.unwrap();
        app.tabs[index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# changed\n".into())],
            "fixture edit",
        );
        let caret = app.tabs[index].state.primary().head();
        assert!(
            app.jump_to_selected_usage(),
            "actual focused row must own the central jump route"
        );
        assert_eq!(app.tabs[index].state.primary().head(), caret);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn scope_controls_snapshot_open_files_and_custom_paths() {
        let app = IdeApp::new(Settings::default(), None);
        assert!(matches!(
            app.usage_options(Path::new("/source.py")).scope,
            Scope::Project
        ));
        let mut app = app;
        app.usage_ui.in_file();
        assert_eq!(
            app.usage_options(Path::new("/source.py")).scope,
            Scope::File("/source.py".into())
        );
        app.usage_ui.scope_choice = 4;
        app.usage_ui.scope_paths = "/pkg/source.py; /pkg/sub".into();
        let options = app.usage_options(Path::new("/source.py"));
        assert!(options.scope.contains(Path::new("/pkg/sub/module.py")));
        assert!(!options.scope.contains(Path::new("/pkg/other.py")));
        app.usage_ui.scope_choice = 3;
        assert_eq!(
            app.usage_options(Path::new("/source.py")).scope,
            Scope::OpenFiles(Vec::new())
        );
    }
    #[test]
    fn named_scope_choice_filters_usages_and_is_remembered_by_pins() {
        use rustpy_settings::scopes::NamedScope;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let source = root.join("a.py");
        let inside = root.join("uses.py");
        let outside = root.join("other.py");
        std::fs::write(&source, "value = 1\n").unwrap();
        std::fs::write(&inside, "from a import value\nprint(value)\n").unwrap();
        std::fs::write(&outside, "from a import value\nprint(value)\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(&root);
        settings.scopes.named.push(NamedScope {
            name: "Uses only".into(),
            pattern: "file:uses.py".into(),
            color: None,
            shared: false,
        });
        let mut app = IdeApp::new(settings, Some(root.clone()));
        app.open_file(&source);
        let search = |app: &mut IdeApp| {
            let options = app.usage_options(&source);
            app.sync_analysis_now();
            let (analysis, file) = app.analysis_file(app.active.unwrap()).unwrap();
            rustpy_search::usages::results::search(
                &analysis,
                &file,
                TextSize::new(0),
                &app.project_files(),
                &options,
                &rustpy_core::ProgressReporter::new(),
            )
            .unwrap()
        };
        let project = search(&mut app);
        assert!(project.iter().any(|hit| hit.usage.path == outside));
        app.usage_ui.scope_choice = NAMED_SCOPE;
        app.usage_ui.scope_selection = ScopeSelection::Named("Uses only".into());
        let options = app.usage_options(&source);
        assert!(options.scope.contains(&inside));
        assert!(!options.scope.contains(&outside));
        let scoped = search(&mut app);
        assert!(!scoped.is_empty());
        assert!(scoped.iter().all(|hit| hit.usage.path == inside));
        // An unknown scope admits nothing rather than silently widening to Project.
        app.usage_ui.scope_selection = ScopeSelection::Named("Missing".into());
        assert!(search(&mut app).is_empty());
        app.usage_ui.scope_selection = ScopeSelection::Named("Uses only".into());
        deliver(&mut app, "scoped", vec![hit("print(value)", false)]);
        app.usage_ui.pin();
        assert_eq!(app.usage_ui.pinned[0].scope_choice, NAMED_SCOPE);
        assert_eq!(
            app.usage_ui.pinned[0].scope_selection,
            ScopeSelection::Named("Uses only".into())
        );
    }
}

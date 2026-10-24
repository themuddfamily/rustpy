//! Retained Find / Replace in Files profiles in the actual Find tool window.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::search_ui::{FindInFiles, Grouping, SavedSearch};
use std::collections::BTreeMap;

struct Tab {
    search: SavedSearch,
    pinned: bool,
}
#[derive(Default)]
pub(super) struct FileResults {
    tabs: Vec<Tab>,
    pub(super) active: Option<usize>,
    selected: usize,
    filter: String,
    show_excluded: bool,
    follow_to: bool,
    follow_from: bool,
    hide_preview: bool,
    editor: Option<(PathBuf, usize)>,
}
impl FileResults {
    fn add(&mut self, search: SavedSearch, new_tab: bool) -> Result<usize, &'static str> {
        if !new_tab {
            if let Some(index) = self
                .active
                .filter(|index| self.tabs.get(*index).is_some_and(|tab| !tab.pinned))
            {
                self.tabs[index].search = search;
                self.selected = 0;
                return Ok(index);
            }
        }
        if self.tabs.len() == 8 {
            let index = self
                .tabs
                .iter()
                .position(|tab| !tab.pinned)
                .ok_or("Close a pinned Find tab before opening another")?;
            self.tabs.remove(index);
        }
        self.tabs.push(Tab {
            search,
            pinned: false,
        });
        self.selected = 0;
        Ok(self.tabs.len() - 1)
    }
}
impl IdeApp {
    pub(super) fn show_file_search(&mut self, search: SavedSearch, new_tab: bool) {
        let title = search.title.clone();
        match self.file_results.add(search, new_tab) {
            Ok(index) => {
                self.pin_find_results(title, Vec::new());
                self.file_results.active = Some(index);
            }
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn file_search_results_view(&mut self, ui: &mut egui::Ui) {
        let mut state = std::mem::take(&mut self.file_results);
        let mut close = None;
        let mut selected_tab = None;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in state.tabs.iter().enumerate() {
                ui.push_id(("file_search_tab", index), |ui| {
                    if ui
                        .selectable_label(
                            state.active == Some(index),
                            format!(
                                "{}{}",
                                if tab.pinned { "Pinned: " } else { "" },
                                tab.search.title
                            ),
                        )
                        .clicked()
                    {
                        selected_tab = Some(index);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Close search results tab")
                        .clicked()
                    {
                        close = Some(index);
                    }
                });
            }
        });
        if let Some(index) = selected_tab {
            state.active = Some(index);
            state.selected = 0;
        }
        if let Some(index) = close {
            state.tabs.remove(index);
            state.active = if state.tabs.is_empty() {
                None
            } else {
                Some(index.min(state.tabs.len() - 1))
            };
            state.selected = 0;
        }
        let Some(index) = state.active else {
            self.file_results = state;
            return;
        };
        let Some(tab) = state.tabs.get_mut(index) else {
            state.active = None;
            self.file_results = state;
            return;
        };
        let mut rerun = false;
        let mut replace_all = false;
        let mut replace_selected = false;
        let mut navigate = None;
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut tab.pinned, "Pin this search");
            rerun = ui.button("Re-run / edit search").clicked();
            ui.checkbox(&mut tab.search.replace, "Replace in Path");
            ui.label("Filter:");
            ui.text_edit_singleline(&mut state.filter);
            ui.checkbox(&mut state.show_excluded, "Show excluded");
            ui.checkbox(&mut state.follow_to, "Autoscroll to source");
            ui.checkbox(&mut state.follow_from, "Autoscroll from source");
            let mut preview = !state.hide_preview;
            ui.checkbox(&mut preview, "Preview");
            state.hide_preview = !preview;
            if ui.button("Include all").clicked() {
                tab.search.skipped.clear();
            }
            egui::ComboBox::from_id_salt("file_results_group")
                .selected_text(format!("{:?}", tab.search.features.grouping))
                .show_ui(ui, |ui| {
                    for grouping in [Grouping::Flat, Grouping::File, Grouping::Directory] {
                        ui.selectable_value(
                            &mut tab.search.features.grouping,
                            grouping,
                            format!("{grouping:?}"),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Group results by"));
        });
        if tab.search.replace {
            ui.horizontal(|ui| {
                ui.label("Replacement:");
                ui.text_edit_singleline(&mut tab.search.replacement);
                replace_all = ui.button("Replace all included").clicked();
                replace_selected = ui
                    .add_enabled(
                        state.selected < tab.search.matches.len(),
                        egui::Button::new("Replace selected"),
                    )
                    .clicked();
            });
        }
        let editor = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| {
                Some((
                    tab.state.path.clone()?,
                    tab.state.doc.line_of(tab.state.primary().head()),
                ))
            });
        let follow_changed = state.follow_from && state.editor != editor;
        if follow_changed {
            if let Some((path, line)) = &editor {
                if let Some((index, _)) = tab
                    .search
                    .matches
                    .iter()
                    .enumerate()
                    .filter(|(_, found)| &found.path == path)
                    .min_by_key(|(_, found)| found.line.abs_diff(*line))
                {
                    state.selected = index;
                }
            }
        }
        state.editor = editor;
        let filter = state.filter.to_lowercase();
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, found) in tab.search.matches.iter().enumerate() {
            if !state.show_excluded
                && tab
                    .search
                    .skipped
                    .contains(&(found.path.clone(), found.line, found.start))
            {
                continue;
            }
            if !filter.is_empty()
                && !format!("{} {}", found.path.display(), found.line_text)
                    .to_lowercase()
                    .contains(&filter)
            {
                continue;
            }
            let group = match tab.search.features.grouping {
                Grouping::Flat => String::new(),
                Grouping::File => found.path.display().to_string(),
                Grouping::Directory => found
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .display()
                    .to_string(),
            };
            groups.entry(group).or_default().push(index);
        }
        egui::ScrollArea::vertical()
            .id_salt("file_results_rows")
            .max_height(ui.available_height().max(120.0) * 0.62)
            .show(ui, |ui| {
                for (group, indices) in groups {
                    let mut rows = |ui: &mut egui::Ui| {
                        if tab.search.replace && tab.search.features.grouping == Grouping::File {
                            let mut include = indices.iter().all(|index| {
                                let found = &tab.search.matches[*index];
                                !tab.search.skipped.contains(&(
                                    found.path.clone(),
                                    found.line,
                                    found.start,
                                ))
                            });
                            if ui.checkbox(&mut include, "Replace in this file").changed() {
                                for index in &indices {
                                    let found = &tab.search.matches[*index];
                                    let key = (found.path.clone(), found.line, found.start);
                                    if include {
                                        tab.search.skipped.remove(&key);
                                    } else {
                                        tab.search.skipped.insert(key);
                                    }
                                }
                            }
                        }
                        for index in &indices {
                            let found = &tab.search.matches[*index];
                            let key = (found.path.clone(), found.line, found.start);
                            let mut included = !tab.search.skipped.contains(&key);
                            let response = ui
                                .horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut included, "")
                                        .on_hover_text("Include / exclude occurrence")
                                        .changed()
                                    {
                                        if included {
                                            tab.search.skipped.remove(&key);
                                        } else {
                                            tab.search.skipped.insert(key);
                                        }
                                    }
                                    ui.selectable_label(
                                        state.selected == *index,
                                        format!(
                                            "{}:{}:{}  {}",
                                            found
                                                .path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy(),
                                            found.line + 1,
                                            found.start + 1,
                                            found.line_text.replace('\n', " ↵ ")
                                        ),
                                    )
                                })
                                .inner;
                            if response.clicked() {
                                state.selected = *index;
                                if state.follow_to {
                                    navigate = Some((found.path.clone(), found.line, found.start));
                                }
                            }
                            if follow_changed && state.selected == *index {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.double_clicked() {
                                navigate = Some((found.path.clone(), found.line, found.start));
                            }
                        }
                    };
                    if group.is_empty() {
                        rows(ui);
                    } else {
                        egui::CollapsingHeader::new(group)
                            .default_open(true)
                            .show(ui, rows);
                    }
                }
            });
        if !state.hide_preview {
            if let Some(found) = tab.search.matches.get(state.selected) {
                ui.separator();
                ui.label(format!(
                    "Preview: {}:{}",
                    found.path.display(),
                    found.line + 1
                ));
                egui::ScrollArea::both()
                    .id_salt("file_result_preview")
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                found.line_text.chars().take(8192).collect::<String>(),
                            )
                            .monospace(),
                        );
                        if tab.search.replace {
                            if let Ok(regex) =
                                rustpy_search::build_regex(&tab.search.query, &tab.search.options)
                            {
                                let replacement = rustpy_search::replacement_text(
                                    &regex,
                                    &found.line_text,
                                    found.start,
                                    &tab.search.replacement,
                                    tab.search.options.regex,
                                );
                                ui.label(
                                    egui::RichText::new(format!("Replace with: {replacement}"))
                                        .monospace(),
                                );
                            }
                        }
                    });
                if ui.button("Jump to source").clicked() {
                    navigate = Some((found.path.clone(), found.line, found.start));
                }
            }
        }
        let title = tab.search.title.clone();
        let replay = rerun.then(|| tab.search.clone());
        let replacement = if replace_all || replace_selected {
            Some(FindInFiles::saved_edits(
                &tab.search,
                replace_selected.then_some(state.selected),
                &|path| Self::buffer_text(&self.tabs, path),
            ))
        } else {
            None
        };
        self.file_results = state;
        if selected_tab.is_some() || close.is_some() {
            let active = self.file_results.active;
            self.pin_find_results(title, Vec::new());
            self.file_results.active = active;
        }
        if let Some(replay) = replay {
            self.find_in_files.restore_search(&replay);
        }
        if let Some(edits) = replacement {
            self.apply_file_edits(edits, "Replace in Files");
        }
        if let Some((path, line, column)) = navigate {
            self.mark_nav_jump();
            self.open_at(&path, line, column);
        }
    }
}

impl IdeApp {
    /// The sole scope-language parser is the shared Settings model.
    pub(super) fn refresh_find_scope_filters(&mut self) {
        use rustpy_search::file_search::CapturedScope;
        use rustpy_settings::scopes::{PredefinedScope, ScopeSelection};
        use std::hash::{Hash, Hasher};
        let context = self.capture_scope_context();
        let mut revision = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_vec(&self.settings.scopes)
            .unwrap_or_default()
            .hash(&mut revision);
        // This runs every frame: the project's file list (100k paths in a large
        // project) is identified by its shared allocation, not hashed. The
        // list is retained with the stamp, so the allocation cannot be reused.
        let files_ptr = if context.project_files.is_empty() {
            0
        } else {
            std::sync::Arc::as_ptr(&context.project_files) as usize
        };
        (files_ptr, context.project_files.len()).hash(&mut revision);
        context.open_files.hash(&mut revision);
        context.changed_files.hash(&mut revision);
        context.current_file.hash(&mut revision);
        context.recently_viewed.hash(&mut revision);
        context.recently_changed.hash(&mut revision);
        context.modules.hash(&mut revision);
        let features = &mut self.find_in_files.features;
        if features.named_scope.is_empty() {
            features.named_scope = PredefinedScope::ALL
                .first()
                .map(|scope| scope.label().to_string())
                .unwrap_or_default();
        }
        (
            &features.custom_expression,
            &features.named_scope,
            self.find_in_files.scope == crate::search_ui::SearchScope::Pattern,
        )
            .hash(&mut revision);
        let revision = revision.finish();
        if features
            .scope_inputs
            .as_ref()
            .is_some_and(|(stamp, files)| {
                *stamp == revision
                    && (std::sync::Arc::ptr_eq(files, &context.project_files)
                        || files.is_empty() && context.project_files.is_empty())
            })
        {
            return;
        }
        features.scope_inputs = Some((revision, context.project_files.clone()));
        features.scope_revision = revision;
        features.scope_filters.clear();
        features.scope_error = None;
        let choices: Vec<_> = PredefinedScope::ALL
            .iter()
            .map(|scope| {
                (
                    scope.label().to_string(),
                    ScopeSelection::Predefined(*scope),
                )
            })
            .chain(self.settings.scopes.named.iter().map(|scope| {
                (
                    format!("Named: {}", scope.name),
                    ScopeSelection::Named(scope.name.clone()),
                )
            }))
            .collect();
        features.named_scopes = choices
            .iter()
            .map(|(name, _)| (name.clone(), Vec::new()))
            .collect();
        if features.named_scope.is_empty() {
            features.named_scope = choices
                .first()
                .map(|(name, _)| name.clone())
                .unwrap_or_default();
        }
        for (name, selection) in choices {
            match self.settings.scopes.compile(&selection) {
                Ok(compiled) => {
                    features.scope_filters.insert(
                        name,
                        CapturedScope {
                            compiled,
                            context: context.clone(),
                        },
                    );
                }
                Err(error) if name == features.named_scope => features.scope_error = Some(error),
                Err(_) => {}
            }
        }
        match self
            .settings
            .scopes
            .compile(&ScopeSelection::Custom(features.custom_expression.clone()))
        {
            Ok(compiled) => features.custom_filter = Some(CapturedScope { compiled, context }),
            Err(error) => {
                features.custom_filter = None;
                if self.find_in_files.scope == crate::search_ui::SearchScope::Pattern {
                    features.scope_error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn search(title: &str) -> SavedSearch {
        FindInFiles::default().snapshot(title.into())
    }
    #[test]
    fn per_frame_scope_filter_refresh_is_skipped_while_its_inputs_are_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("main.py"), "x = 1\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.clone()));
        app.refresh_find_scope_filters();
        let revision = app.find_in_files.features.scope_revision;
        assert!(!app.find_in_files.features.scope_filters.is_empty());
        // A skipped refresh leaves the captured filters untouched.
        app.find_in_files.features.scope_filters.clear();
        app.refresh_find_scope_filters();
        assert!(app.find_in_files.features.scope_filters.is_empty());
        assert_eq!(app.find_in_files.features.scope_revision, revision);
        // Any input change (here the custom scope expression) recaptures them.
        app.find_in_files.features.custom_expression = "file:*.py".into();
        app.refresh_find_scope_filters();
        assert!(!app.find_in_files.features.scope_filters.is_empty());
        assert_ne!(app.find_in_files.features.scope_revision, revision);
    }
    #[test]
    fn pinned_result_tabs_survive_new_searches_and_have_a_finite_capacity() {
        let mut state = FileResults::default();
        state.active = Some(state.add(search("first"), false).unwrap());
        state.tabs[0].pinned = true;
        assert_eq!(state.add(search("second"), false).unwrap(), 1);
        assert_eq!(state.tabs[0].search.title, "first");
        for index in 2..8 {
            state.add(search(&index.to_string()), true).unwrap();
        }
        for tab in &mut state.tabs {
            tab.pinned = true;
        }
        assert!(state.add(search("ninth"), true).is_err());
        assert_eq!(state.tabs.len(), 8);
        assert_eq!(state.tabs[0].search.title, "first");
    }
}

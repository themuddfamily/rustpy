//! Extended Find in Files controls and independently retained result profiles.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_search::ContextFilter;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    Flat,
    #[default]
    File,
    Directory,
}
#[derive(Clone, Default)]
pub struct FileFeatures {
    pub custom_paths: String,
    pub custom_expression: String,
    pub scope_filters: HashMap<String, rustpy_search::file_search::CapturedScope>,
    pub custom_filter: Option<rustpy_search::file_search::CapturedScope>,
    pub scope_error: Option<String>,
    pub scope_revision: u64,
    /// The inputs `scope_filters` were captured from (stamp, project file list).
    pub scope_inputs: Option<(u64, std::sync::Arc<Vec<PathBuf>>)>,
    pub named_scope: String,
    pub named_scopes: Vec<(String, Vec<PathBuf>)>,
    pub changed_files: Vec<PathBuf>,
    pub library_roots: Vec<PathBuf>,
    pub masks: Vec<String>,
    pub grouping: Grouping,
    pub filter: String,
    pub new_tab: bool,
}
#[derive(Clone)]
pub struct SavedSearch {
    pub title: String,
    pub query: String,
    pub replacement: String,
    pub options: SearchOptions,
    pub scope: SearchScope,
    pub directory: Option<PathBuf>,
    pub features: FileFeatures,
    pub matches: Vec<SearchMatch>,
    pub skipped: HashSet<(PathBuf, usize, usize)>,
    pub replace: bool,
}
impl FindInFiles {
    pub fn set_named_scopes(&mut self, scopes: Vec<(String, Vec<PathBuf>)>) {
        self.features.named_scopes = scopes;
    }
    pub fn set_extra_sources(&mut self, changed: Vec<PathBuf>, libraries: Vec<PathBuf>) {
        self.features.changed_files = changed;
        self.features.library_roots = libraries;
    }
    pub fn snapshot(&self, title: String) -> SavedSearch {
        SavedSearch {
            title,
            query: self.query.clone(),
            replacement: self.replacement.clone(),
            options: self.options.clone(),
            scope: self.scope,
            directory: self.directory.clone(),
            features: self.features.clone(),
            matches: self.results.matches.clone(),
            skipped: self.skipped.clone(),
            replace: self.replace_mode,
        }
    }
    pub fn restore_search(&mut self, saved: &SavedSearch) {
        self.cancel_job();
        self.query = saved.query.clone();
        self.replacement = saved.replacement.clone();
        self.options = saved.options.clone();
        self.scope = saved.scope;
        self.directory = saved.directory.clone();
        self.features = saved.features.clone();
        self.results.matches = saved.matches.clone();
        self.skipped = saved.skipped.clone();
        self.replace_mode = saved.replace;
        self.visible = true;
        self.last_query.clear();
        self.focus_query = true;
    }
    pub fn saved_edits(
        saved: &SavedSearch,
        selected: Option<usize>,
        buffer: &dyn Fn(&Path) -> Option<String>,
    ) -> FileEdits {
        let mut find = Self {
            query: saved.query.clone(),
            replacement: saved.replacement.clone(),
            options: saved.options.clone(),
            results: SearchResults {
                matches: saved.matches.clone(),
                ..Default::default()
            },
            skipped: saved.skipped.clone(),
            ..Default::default()
        };
        if let Some(index) = selected {
            for (other, matched) in saved.matches.iter().enumerate() {
                if other != index {
                    find.skipped
                        .insert((matched.path.clone(), matched.line, matched.start));
                }
            }
        }
        find.replace_edits(buffer)
    }
    pub(super) fn feature_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        (
            &self.features.custom_paths,
            &self.features.custom_expression,
            self.features.scope_revision,
            &self.features.named_scope,
            &self.features.named_scopes,
            &self.features.changed_files,
            &self.features.library_roots,
            &self.open_files,
            &self.current_dir,
        )
            .hash(&mut hash);
        hash.finish()
    }
    pub(super) fn extra_only(&self, root: &Path) -> Option<Vec<PathBuf>> {
        match self.scope {
            SearchScope::OpenFiles => Some(self.open_files.clone()),
            SearchScope::Changed => Some(self.features.changed_files.clone()),
            SearchScope::Libraries => Some(self.features.library_roots.clone()),
            SearchScope::Custom => Some(
                self.features
                    .custom_paths
                    .split(';')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(|path| {
                        let path = PathBuf::from(path);
                        if path.is_absolute() {
                            path
                        } else {
                            root.join(path)
                        }
                    })
                    .collect(),
            ),
            SearchScope::Named
                if self
                    .features
                    .scope_filters
                    .contains_key(&self.features.named_scope) =>
            {
                None
            }
            SearchScope::Pattern => self.features.custom_filter.is_none().then(Vec::new),
            SearchScope::Named => Some(
                self.features
                    .named_scopes
                    .iter()
                    .find(|(name, _)| name == &self.features.named_scope)
                    .map(|(_, paths)| paths.clone())
                    .unwrap_or_default(),
            ),
            _ => None,
        }
    }
    pub(super) fn extended_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("file_context")
                .selected_text(self.options.context.label())
                .show_ui(ui, |ui| {
                    for context in [
                        ContextFilter::Anywhere,
                        ContextFilter::Comments,
                        ContextFilter::Strings,
                        ContextFilter::ExceptComments,
                        ContextFilter::ExceptStrings,
                        ContextFilter::Code,
                    ] {
                        ui.selectable_value(&mut self.options.context, context, context.label());
                    }
                })
                .combo_named(crate::i18n::tr_static("Context"));
            ui.checkbox(&mut self.options.multiline, "Multiline");
            ui.label("Result limit:");
            ui.add(egui::DragValue::new(&mut self.options.max_results).range(1..=100_000));
            let mut large = self.options.max_file_bytes > 4 * 1024 * 1024;
            if ui.checkbox(&mut large, "Search in large files").changed() {
                self.options.max_file_bytes = if large {
                    64 * 1024 * 1024
                } else {
                    4 * 1024 * 1024
                };
            }
            ui.checkbox(
                &mut self.options.include_generated,
                "Generated / ignored folders",
            );
            ui.checkbox(&mut self.options.include_excluded, "Excluded folders");
        });
        ui.horizontal_wrapped(|ui| {
            for scope in [
                SearchScope::Module,
                SearchScope::Custom,
                SearchScope::Named,
                SearchScope::Changed,
                SearchScope::Libraries,
                SearchScope::Pattern,
            ] {
                ui.selectable_value(&mut self.scope, scope, scope.label());
            }
            if self.scope == SearchScope::Custom {
                ui.add(
                    egui::TextEdit::singleline(&mut self.features.custom_paths)
                        .hint_text("files / directories separated by ;"),
                );
            }
            if self.scope == SearchScope::Pattern {
                ui.add(
                    egui::TextEdit::singleline(&mut self.features.custom_expression)
                        .hint_text("file:src/*//*.py&&!file:*/test*"),
                );
            }
            if self.scope == SearchScope::Named {
                egui::ComboBox::from_id_salt("find_named_scope")
                    .selected_text(&self.features.named_scope)
                    .show_ui(ui, |ui| {
                        for (name, _) in &self.features.named_scopes {
                            ui.selectable_value(&mut self.features.named_scope, name.clone(), name);
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Named scope"));
            }
            egui::ComboBox::from_id_salt("find_mask_history")
                .selected_text("Mask history")
                .show_ui(ui, |ui| {
                    for mask in &self.features.masks {
                        if ui.button(mask).clicked() {
                            self.options.file_mask = mask.clone();
                            ui.close();
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Mask history"));
            ui.checkbox(&mut self.features.new_tab, "Open in new results tab");
        });
        if self.options.multiline {
            ui.label("Multiline search template:");
            ui.add(
                egui::TextEdit::multiline(&mut self.query)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_profile_selected_replace_preserves_occurrence_exclusions() {
        let path = PathBuf::from("a.py");
        let mut find = FindInFiles {
            query: "old".into(),
            replacement: "new".into(),
            ..Default::default()
        };
        find.results.matches = rustpy_search::file_search::matches(
            &path,
            "old old\n",
            &rustpy_search::build_regex("old", &find.options).unwrap(),
            &find.options,
        );
        find.features.custom_paths = "src;tests".into();
        find.scope = SearchScope::Custom;
        let saved = find.snapshot("retained".into());
        let edits = FindInFiles::saved_edits(&saved, Some(1), &|_| Some("old old\n".into()));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1.len(), 1);
        assert_eq!(usize::from(edits[0].1[0].0.start()), 4);
        find.query.clear();
        find.restore_search(&saved);
        assert_eq!(find.query, "old");
        assert_eq!(find.features.custom_paths, "src;tests");
        assert_eq!(find.scope, SearchScope::Custom);
    }
    #[test]
    fn explicit_scope_candidates_keep_directories_and_root_relative_paths() {
        let find = FindInFiles {
            scope: SearchScope::Custom,
            features: FileFeatures {
                custom_paths: "src; /tmp/extra.py ; ;tests".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            find.extra_only(Path::new("/project")),
            Some(vec![
                PathBuf::from("/project/src"),
                PathBuf::from("/tmp/extra.py"),
                PathBuf::from("/project/tests")
            ])
        );
    }
}

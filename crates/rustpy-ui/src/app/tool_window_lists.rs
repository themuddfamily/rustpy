//! Tool-window list controls reuse canonical TODO, inspection and VCS ledgers.
use super::*;
use crate::a11y::AccessibleCombo;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum TodoTab {
    Current,
    #[default]
    Scope,
    Changelist,
    Project,
}
pub(super) struct State {
    tab: TodoTab,
    query: String,
    grouping: usize,
    pattern: Option<usize>,
    flatten: bool,
    preview: bool,
    follow: bool,
    follow_source: bool,
    selected: Option<(PathBuf, usize)>,
    pinned: Option<(PathBuf, project_index::TodoList, Vec<[u8; 3]>)>,
    pub problems_query: String,
    pub severity: usize,
    pub problems_preview: bool,
    pub problems_follow: bool,
    pub problems_pin: Option<(PathBuf, egui::Id)>,
    pub vulnerabilities: bool,
    pub problem_selected: Option<(PathBuf, usize)>,
    pub legacy_find_query: String,
    pub legacy_find_group: bool,
    pub legacy_find_to: bool,
    pub legacy_find_from: bool,
    pub legacy_find_preview: bool,
    pub legacy_find_selected: Option<(PathBuf, usize)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            tab: TodoTab::Scope,
            query: String::new(),
            grouping: 0,
            pattern: None,
            flatten: false,
            preview: true,
            follow: true,
            follow_source: false,
            selected: None,
            pinned: None,
            problems_query: String::new(),
            severity: 0,
            problems_preview: false,
            problems_follow: true,
            problems_pin: None,
            vulnerabilities: false,
            problem_selected: None,
            legacy_find_query: String::new(),
            legacy_find_group: true,
            legacy_find_to: true,
            legacy_find_from: false,
            legacy_find_preview: true,
            legacy_find_selected: None,
        }
    }
}
impl IdeApp {
    pub(super) fn todo_workspace(&mut self, ui: &mut egui::Ui) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        if self
            .tool_lists
            .pinned
            .as_ref()
            .is_some_and(|(captured, _, _)| captured != &root)
        {
            self.tool_lists.pinned = None;
        }
        let before = self.tool_lists.tab;
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (TodoTab::Current, "Current File"),
                (TodoTab::Scope, "Scope-based"),
                (TodoTab::Changelist, "Default changelist"),
                (TodoTab::Project, "Project"),
            ] {
                ui.selectable_value(&mut self.tool_lists.tab, tab, crate::i18n::tr_static(label));
            }
        });
        if before != self.tool_lists.tab {
            self.todo_refreshed = -100.0;
        }
        if self.tool_lists.pinned.is_none() {
            self.request_todo_scan(ui.input(|input| input.time));
        }
        if self.tool_lists.tab == TodoTab::Scope {
            let names = self.scope_names();
            scope_ui::scope_selector(
                ui,
                &mut self.settings.scopes.todo,
                &names,
                "todo-workspace-scope",
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.tool_lists.query)
                    .hint_text("Speed search TODO text / path"),
            );
            egui::ComboBox::from_id_salt("todo-grouping")
                .selected_text(["Files", "Modules", "Packages"][self.tool_lists.grouping.min(2)])
                .show_ui(ui, |ui| {
                    for (index, label) in ["Files", "Modules", "Packages"].into_iter().enumerate() {
                        ui.selectable_value(&mut self.tool_lists.grouping, index, label);
                    }
                })
                .combo_named(crate::i18n::tr_static("Group by"));
            ui.checkbox(&mut self.tool_lists.flatten, "Flatten packages");
            ui.checkbox(&mut self.tool_lists.preview, "Preview");
            ui.checkbox(&mut self.tool_lists.follow, "Autoscroll to source");
            ui.checkbox(&mut self.tool_lists.follow_source, "Autoscroll from source");
            let mut pin = self.tool_lists.pinned.is_some();
            if ui.checkbox(&mut pin, "Pin results").changed() {
                self.tool_lists.pinned = pin.then(|| {
                    (
                        root.clone(),
                        self.todo_items.clone(),
                        self.todo_colors.clone(),
                    )
                });
                if !pin {
                    self.todo_refreshed = -100.0;
                }
            }
            if ui.button("Refresh").clicked() {
                self.tool_lists.pinned = None;
                self.todo_refreshed = -100.0;
            }
        });
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("todo-pattern-filter")
                .selected_text(
                    self.tool_lists
                        .pattern
                        .and_then(|index| self.settings.todo.patterns.get(index))
                        .map(|pattern| pattern.regex.as_str())
                        .unwrap_or("All patterns"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.tool_lists.pattern, None, "All patterns");
                    for (index, pattern) in self.settings.todo.patterns.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.tool_lists.pattern,
                            Some(index),
                            &pattern.regex,
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Pattern filter"));
            ui.checkbox(&mut self.settings.todo.comments, "Comments / prose");
            ui.checkbox(&mut self.settings.todo.strings, "Strings");
            ui.checkbox(&mut self.settings.todo.multiline, "Multiline");
            if ui.button("Configure patterns…").clicked() {
                self.open_settings_page("todo");
            }
        });
        let context = self.capture_scope_context();
        let compiled = match self.settings.scopes.compile(&self.settings.scopes.todo) {
            Ok(scope) => scope,
            Err(error) => {
                ui.colored_label(Color32::RED, error);
                return;
            }
        };
        let (items, colors) = self
            .tool_lists
            .pinned
            .as_ref()
            .map(|(_, items, colors)| (items, colors))
            .unwrap_or((&self.todo_items, &self.todo_colors));
        // The default list also owns changed paths not explicitly assigned to any list.
        let repository = self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
            .map(|repo| repo.root());
        let default_paths = repository
            .map(|root| {
                self.git_panel
                    .commit_workspace
                    .default_paths_for(root, &context.changed_files)
            })
            .unwrap_or_default();
        let model = self.project.as_ref().map(|project| project.tree.model());
        let active = self.active.and_then(|index| self.tabs.get(index));
        let active_path = active.and_then(|tab| tab.state.path.as_ref());
        let active_line = active.map(|tab| tab.state.doc.line_of(tab.state.primary().head()));
        let pattern = self
            .tool_lists
            .pattern
            .and_then(|index| self.settings.todo.patterns.get(index))
            .and_then(|pattern| {
                regex::RegexBuilder::new(&pattern.regex)
                    .case_insensitive(!pattern.case_sensitive)
                    .size_limit(1024 * 1024)
                    .build()
                    .ok()
            });
        let project_scope = self.project_scope();
        let rows = items
            .iter()
            .enumerate()
            .filter(|(_, (path, _, text))| {
                let tab = match self.tool_lists.tab {
                    TodoTab::Current => active_path == Some(path),
                    TodoTab::Scope => compiled.matches(path, &context),
                    TodoTab::Project => project_scope
                        .as_ref()
                        .is_some_and(|scope| scope.contains(path)),
                    TodoTab::Changelist => default_paths.contains(path),
                };
                tab && pattern
                    .as_ref()
                    .is_none_or(|pattern| pattern.is_match(text))
                    && crate::project_tree::speed_search_matches(
                        &format!("{} {text}", path.display()),
                        &self.tool_lists.query,
                    )
            })
            .map(|(index, row)| {
                (
                    row.clone(),
                    colors.get(index).copied().unwrap_or([168, 192, 35]),
                )
            })
            .collect::<Vec<_>>();
        ui.label(format!(
            "{} TODO item(s){}",
            rows.len(),
            if self.todo_scan_running() {
                " · scanning…"
            } else {
                ""
            }
        ));
        let mut selected = None;
        let mut navigate = None;
        let follow = self
            .tool_lists
            .follow_source
            .then(|| {
                rows.iter()
                    .rfind(|((path, line, _), _)| {
                        Some(path) == active_path && Some(*line) <= active_line
                    })
                    .map(|((path, line, _), _)| (path.clone(), *line))
            })
            .flatten();
        let mut groups = TodoGroups::default();
        for (index, ((path, _, _), _)) in rows.iter().enumerate() {
            let labels = if self.tool_lists.grouping == 2 && !self.tool_lists.flatten {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .components()
                    .take(64)
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            } else {
                vec![todo_group(
                    model.as_ref(),
                    &root,
                    path,
                    self.tool_lists.grouping,
                    self.tool_lists.flatten,
                )]
            };
            groups.insert(&labels, index);
        }
        egui::ScrollArea::vertical()
            .id_salt("todo-workspace-rows")
            .max_height(if self.tool_lists.preview {
                320.0
            } else {
                ui.available_height()
            })
            .show(ui, |ui| {
                groups.show(
                    ui,
                    !self.tool_lists.query.is_empty() || follow.is_some(),
                    &mut |index, ui| {
                        let ((path, line, text), color) = &rows[index];
                        let key = (path.clone(), *line);
                        let response = ui.selectable_label(
                            self.tool_lists.selected.as_ref() == Some(&key),
                            egui::RichText::new(format!(
                                "{}:{} · {text}",
                                path.file_name().unwrap_or_default().to_string_lossy(),
                                line + 1
                            ))
                            .color(Color32::from_rgb(color[0], color[1], color[2])),
                        );
                        if follow.as_ref() == Some(&key)
                            && self.tool_lists.selected.as_ref() != Some(&key)
                        {
                            response.scroll_to_me(Some(egui::Align::Center));
                            selected = Some(key.clone());
                        }
                        if response.clicked() {
                            selected = Some(key.clone());
                            if self.tool_lists.follow {
                                navigate = Some(key.clone());
                            }
                        }
                        if response.double_clicked()
                            || response.has_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            navigate = Some(key);
                        }
                    },
                );
            });
        if let Some(selected) = selected {
            self.tool_lists.selected = Some(selected);
        }
        if self.tool_lists.preview {
            if let Some((path, line)) = &self.tool_lists.selected {
                self.tool_source_preview(ui, path, *line);
            }
        }
        if let Some((path, line)) = navigate {
            self.mark_nav_jump();
            self.goto_file_line(&path, line);
        }
    }
    pub(super) fn tool_source_preview(&self, ui: &mut egui::Ui, path: &Path, line: usize) {
        let source = self
            .tabs
            .iter()
            .find(|tab| {
                tab.state.path.as_deref() == Some(path)
                    && u32::from(tab.state.doc.len()) <= 1024 * 1024
            })
            .map(|tab| tab.state.doc.text())
            .or_else(|| {
                let snapshot = rustpy_core::vfs::Vfs::new()
                    .read(
                        path,
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 1024 * 1024,
                            max_cached_bytes: 0,
                            max_cached_files: 0,
                        },
                    )
                    .ok()?;
                String::from_utf8(snapshot.bytes.to_vec()).ok()
            });
        ui.separator();
        ui.strong(path.display().to_string());
        if let Some(source) = source {
            egui::ScrollArea::both()
                .id_salt(("tool-source-preview", path))
                .max_height(180.0)
                .show(ui, |ui| {
                    for (index, text) in source
                        .lines()
                        .enumerate()
                        .skip(line.saturating_sub(3))
                        .take(9)
                    {
                        ui.monospace(format!(
                            "{} {:>5} {}",
                            if index == line { "▶" } else { " " },
                            index + 1,
                            text.chars().take(1000).collect::<String>()
                        ));
                    }
                });
        } else {
            ui.label("Preview unavailable; open source to inspect this file");
        }
    }
    pub(super) fn problems_filters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.tool_lists.problems_query)
                    .hint_text("Speed search problems"),
            );
            egui::ComboBox::from_id_salt("problem-severity-filter")
                .selected_text(
                    [
                        "All severities",
                        "Errors",
                        "Warnings",
                        "Weak warnings",
                        "Info / typos",
                    ][self.tool_lists.severity.min(4)],
                )
                .show_ui(ui, |ui| {
                    for (index, label) in [
                        "All severities",
                        "Errors",
                        "Warnings",
                        "Weak warnings",
                        "Info / typos",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        ui.selectable_value(&mut self.tool_lists.severity, index, label);
                    }
                })
                .combo_named(crate::i18n::tr_static("Severity filter"));
            ui.checkbox(&mut self.tool_lists.problems_preview, "Preview");
            ui.checkbox(&mut self.tool_lists.problems_follow, "Autoscroll to source");
            if !self.bg.problems.show {
                let mut pinned = self.tool_lists.problems_pin.is_some();
                if ui.checkbox(&mut pinned, "Pin current file").changed() {
                    self.tool_lists.problems_pin = if pinned {
                        self.active.and_then(|index| {
                            let tab = &self.tabs[index];
                            Some((self.project.as_ref()?.root.clone(), tab.view.id))
                        })
                    } else {
                        None
                    };
                }
            }
        });
    }
    pub(super) fn problem_visible(
        &self,
        path: &Path,
        diagnostic: &rustpy_core::Diagnostic,
    ) -> bool {
        severity_matches(self.tool_lists.severity, diagnostic.severity)
            && crate::project_tree::speed_search_matches(
                &format!(
                    "{} {} {}",
                    path.display(),
                    diagnostic.code,
                    diagnostic.message
                ),
                &self.tool_lists.problems_query,
            )
    }
}
#[derive(Default)]
struct TodoGroups {
    rows: Vec<usize>,
    children: std::collections::BTreeMap<String, TodoGroups>,
}
impl TodoGroups {
    fn insert(&mut self, labels: &[String], index: usize) {
        if let Some((first, rest)) = labels.split_first() {
            self.children
                .entry(first.clone())
                .or_default()
                .insert(rest, index);
        } else {
            self.rows.push(index);
        }
    }
    fn show(
        &self,
        ui: &mut egui::Ui,
        force_open: bool,
        row: &mut impl FnMut(usize, &mut egui::Ui),
    ) {
        for index in &self.rows {
            row(*index, ui);
        }
        for (label, group) in &self.children {
            egui::CollapsingHeader::new(if label.is_empty() {
                "(root)"
            } else {
                label.as_str()
            })
            .default_open(true)
            .open(force_open.then_some(true))
            .show(ui, |ui| group.show(ui, force_open, row));
        }
    }
}

fn severity_matches(filter: usize, severity: rustpy_core::Severity) -> bool {
    use rustpy_core::Severity::*;
    match filter {
        1 => severity == Error,
        2 => severity == Warning,
        3 => severity == WeakWarning,
        4 => matches!(severity, Info | Typo),
        _ => true,
    }
}
fn todo_group(
    model: Option<&rustpy_project::project::ProjectModel>,
    root: &Path,
    path: &Path,
    group: usize,
    flatten: bool,
) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    match group {
        1 => model
            .and_then(|model| model.module_for(path))
            .map(|module| module.content_root.display().to_string())
            .unwrap_or_else(|| root.display().to_string()),
        2 => {
            let parent = relative.parent().unwrap_or(Path::new(""));
            if flatten {
                parent
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(".")
            } else {
                parent.display().to_string()
            }
        }
        _ => relative.display().to_string(),
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn severity_and_module_package_grouping_use_canonical_owners() {
        let mut model = rustpy_project::project::ProjectModel::default();
        model.modules.push(rustpy_project::project::ProjectModule {
            content_root: "/p/attached".into(),
            ..Default::default()
        });
        assert_eq!(
            todo_group(
                Some(&model),
                Path::new("/p"),
                Path::new("/p/attached/pkg/a.py"),
                1,
                false
            ),
            "/p/attached"
        );
        assert_eq!(
            todo_group(
                Some(&model),
                Path::new("/p"),
                Path::new("/p/attached/pkg/a.py"),
                2,
                true
            ),
            "attached.pkg"
        );
        assert!(severity_matches(1, rustpy_core::Severity::Error));
        assert!(!severity_matches(1, rustpy_core::Severity::Warning));
    }
}

#[cfg(test)]
mod connected_fixtures {
    use super::*;
    fn labels(app: &mut IdeApp, ctx: &egui::Context) -> Vec<String> {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 850.0),
                )),
                ..Default::default()
            },
            |ui| app.todo_workspace(ui),
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        labels
    }
    #[test]
    fn actual_todo_tabs_search_pattern_filter_and_pinned_results_share_existing_rows() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.py");
        let second = directory.path().join("second.py");
        std::fs::write(&first, "# TODO current λ\n").unwrap();
        std::fs::write(&second, "# FIXME project\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&first);
        app.todo_items = vec![
            (first.clone(), 0, "TODO current λ".into()),
            (second.clone(), 0, "FIXME project".into()),
        ];
        app.todo_colors = vec![[1, 2, 3]; 2];
        app.tool_lists.pinned = Some((
            directory.path().into(),
            app.todo_items.clone(),
            app.todo_colors.clone(),
        ));
        app.tool_lists.tab = TodoTab::Current;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let current = labels(&mut app, &ctx);
        assert!(current.iter().any(|label| label.contains("TODO current λ")));
        assert!(!current.iter().any(|label| label.contains("FIXME project")));
        for tab in [
            "Current File",
            "Scope-based",
            "Default changelist",
            "Project",
        ] {
            assert!(current.iter().any(|label| label == tab));
        }
        app.tool_lists.tab = TodoTab::Project;
        assert!(labels(&mut app, &ctx)
            .iter()
            .any(|label| label.contains("FIXME project")));
        app.tool_lists.query = "current".into();
        assert!(!labels(&mut app, &ctx)
            .iter()
            .any(|label| label.contains("FIXME project")));
        app.tool_lists.query.clear();
        app.tool_lists.pattern = Some(1);
        let filtered = labels(&mut app, &ctx);
        assert!(filtered.iter().any(|label| label.contains("FIXME project")));
        assert!(!filtered
            .iter()
            .any(|label| label.contains("TODO current λ")));
        app.todo_items.clear();
        assert!(labels(&mut app, &ctx)
            .iter()
            .any(|label| label.contains("FIXME project")));
    }
}

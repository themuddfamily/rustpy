//! Inspection profiles, explicit batch runs and result operations.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_analysis::inspections::infrastructure::{self, GroupBy, Policy};
use rustpy_settings::inspection_profiles::{CustomSeverity, HighlightingLevel, Rule};
#[derive(Default)]
pub(super) struct InfrastructureUi {
    pub dialog: bool,
    search: String,
    scope: String,
    project_scope: bool,
    pub group: GroupBy,
    format: String,
    fix_title: String,
    profile_name: String,
    profile_path: String,
    export_path: String,
    profile_project: bool,
    severity_name: String,
    severity_base: usize,
    selected: String,
    pub policy: Option<Policy>,
    pub available_fixes: Vec<String>,
    pub rerun_policy: bool,
}
impl IdeApp {
    /// Returns whether any editor's effective options invalidated its highlights.
    pub(super) fn sync_inspection_highlighting(&mut self) -> bool {
        let enabled = self.background_features().semantic_highlighting;
        let rainbow = self.settings.editor.rainbow_brackets;
        let mut changed = false;
        for tab in &mut self.tabs {
            let semantic = enabled && tab.state.inspection_level == HighlightingLevel::All;
            if tab.state.semantic_highlighting != semantic || tab.state.rainbow_brackets != rainbow
            {
                tab.state.semantic_highlighting = semantic;
                tab.state.rainbow_brackets = rainbow;
                tab.state.invalidate_highlight();
                changed = true;
            }
        }
        changed
    }
    pub(super) fn inspection_profiles_page(&mut self, ui: &mut egui::Ui) {
        let root = self.project.as_ref().map(|project| project.root.clone());
        let state = &mut self.bg.problems.infrastructure;
        let profiles = &mut self.settings.inspections.profiles;
        let profiles_before = profiles.clone();
        if root.is_none() {
            state.profile_project = false;
        }
        let root_key = root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned());
        ui.heading("Inspection profiles");
        let mut active = root_key
            .as_ref()
            .and_then(|key| profiles.project_active.get(key))
            .unwrap_or(&profiles.active)
            .clone();
        let names: Vec<_> = profiles
            .ide
            .iter()
            .map(|profile| profile.name.clone())
            .chain(
                root_key
                    .as_ref()
                    .and_then(|key| profiles.project.get(key))
                    .into_iter()
                    .flatten()
                    .map(|profile| profile.name.clone()),
            )
            .collect();
        egui::ComboBox::from_id_salt("inspection-profile")
            .selected_text(&active)
            .show_ui(ui, |ui| {
                for name in &names {
                    ui.selectable_value(&mut active, name.clone(), name);
                }
            })
            .combo_named(crate::i18n::tr_static("Inspection profile"));
        if let Some(key) = &root_key {
            profiles.project_active.insert(key.clone(), active.clone());
        } else {
            profiles.active = active.clone();
        }
        ui.horizontal(|ui| {
            ui.label("New/copy profile name");
            ui.text_edit_singleline(&mut state.profile_name);
            ui.add_enabled(
                root.is_some(),
                egui::Checkbox::new(&mut state.profile_project, "Project profile"),
            );
        });
        let mut message = None;
        ui.horizontal(|ui| {
            ui.label("Profile XML path");
            ui.add(
                egui::TextEdit::singleline(&mut state.profile_path)
                    .hint_text("inspection-profile.xml"),
            );
        });
        ui.horizontal(|ui|{
            if ui.add_enabled(!state.profile_name.trim().is_empty()&&!names.iter().any(|name|name==state.profile_name.trim()),egui::Button::new("Copy active profile")).clicked(){
                let mut profile=profiles.resolved(root.as_deref()).unwrap_or_default();profile.name=state.profile_name.trim().into();
                if state.profile_project {if let Some(key)=&root_key {profiles.project.entry(key.clone()).or_default().push(profile.clone());profiles.project_active.insert(key.clone(),profile.name.clone());}}else{profiles.active=profile.name.clone();profiles.ide.push(profile.clone());}active=profile.name;
            }
            if ui.add_enabled(!state.profile_path.trim().is_empty(), egui::Button::new("Import XML")).clicked(){let path=PathBuf::from(state.profile_path.trim());{let mut imported=profiles.clone();match imported.import_file(&path){
                Ok(profile)=>{if names.contains(&profile.name){message=Some("A profile with this name exists; rename or copy it before importing".into());}else if state.profile_project{profiles.custom_severities=imported.custom_severities;if let Some(key)=&root_key {profiles.project.entry(key.clone()).or_default().push(profile.clone());profiles.project_active.insert(key.clone(),profile.name);}}else{profiles.custom_severities=imported.custom_severities;profiles.active=profile.name.clone();profiles.ide.push(profile);}},Err(error)=>message=Some(error)}}}
            if ui.add_enabled(!state.profile_path.trim().is_empty(), egui::Button::new("Export XML")).clicked(){if let Ok(profile)=profiles.resolved(root.as_deref()){let path=PathBuf::from(state.profile_path.trim());if let Err(error)=std::fs::write(path,profiles.export_profile(&profile)){message=Some(error.to_string());}}}
        });
        let project_entry = root_key
            .as_ref()
            .and_then(|key| profiles.project.get(key))
            .is_some_and(|entries| entries.iter().any(|profile| profile.name == active));
        let entries = if project_entry {
            profiles
                .project
                .get_mut(root_key.as_ref().unwrap())
                .unwrap()
        } else {
            &mut profiles.ide
        };
        if let Some(profile) = entries.iter_mut().find(|profile| profile.name == active) {
            let mut parent = profile.parent.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("inspection-parent")
                .selected_text(if parent.is_empty() {
                    "No parent"
                } else {
                    &parent
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut parent, String::new(), "No parent");
                    for name in &names {
                        if name != &active {
                            ui.selectable_value(&mut parent, name.clone(), name);
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Parent profile"));
            profile.parent = (!parent.is_empty()).then_some(parent);
        }
        if let Err(error) = profiles.resolved(root.as_deref()) {
            ui.colored_label(egui::Color32::RED, error);
        }
        ui.collapsing("Custom severities", |ui| {
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut state.severity_name);
                let levels = ["Information", "Typo", "Weak Warning", "Warning", "Error"];
                egui::ComboBox::from_id_salt("custom-severity-base")
                    .selected_text(levels[state.severity_base.min(4)])
                    .show_ui(ui, |ui| {
                        for (index, label) in levels.iter().enumerate() {
                            ui.selectable_value(&mut state.severity_base, index, *label);
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Base severity"));
                if ui
                    .add_enabled(
                        !state.severity_name.trim().is_empty()
                            && state.severity_name.len() <= 128
                            && rustpy_settings::inspection_profiles::Profiles::default()
                                .severity(&state.severity_name)
                                .is_none(),
                        egui::Button::new("Add custom severity"),
                    )
                    .clicked()
                {
                    let base = [
                        rustpy_core::Severity::Info,
                        rustpy_core::Severity::Typo,
                        rustpy_core::Severity::WeakWarning,
                        rustpy_core::Severity::Warning,
                        rustpy_core::Severity::Error,
                    ][state.severity_base.min(4)];
                    profiles
                        .custom_severities
                        .retain(|severity| severity.name != state.severity_name);
                    profiles.custom_severities.push(CustomSeverity {
                        name: state.severity_name.clone(),
                        base,
                    });
                }
            });
            for severity in &profiles.custom_severities {
                ui.label(format!("{} ({})", severity.name, severity.base.label()));
            }
        });
        if let Some(path) = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_ref())
        {
            let mut level = profiles.level(path);
            egui::ComboBox::from_id_salt("file-highlighting-level")
                .selected_text(format!("Current file highlighting: {level:?}"))
                .show_ui(ui, |ui| {
                    for value in [
                        HighlightingLevel::None,
                        HighlightingLevel::Syntax,
                        HighlightingLevel::All,
                    ] {
                        ui.selectable_value(&mut level, value, format!("{value:?}"));
                    }
                })
                .combo_named(crate::i18n::tr_static("Highlighting level"));
            profiles
                .file_levels
                .insert(path.to_string_lossy().into_owned(), level);
        }
        ui.separator();
        ui.label("Find inspection by name or ID");
        ui.text_edit_singleline(&mut state.search);
        let filter = state.search.to_lowercase();
        let selected_request = self
            .settings_ui
            .inspection_request_focus
            .then(|| self.settings_ui.inspection_focus.clone())
            .flatten();
        if let Some(request) = selected_request {
            if state.selected != request {
                state.selected = request;
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("inspection-registry")
            .max_height(180.0)
            .show(ui, |ui| {
                for entry in infrastructure::REGISTRY {
                    if !filter.is_empty()
                        && !format!("{} {} {}", entry.id, entry.name, entry.group)
                            .to_lowercase()
                            .contains(&filter)
                    {
                        continue;
                    }
                    if ui
                        .selectable_label(
                            state.selected == entry.id,
                            format!("{} · {}", entry.group, entry.name),
                        )
                        .on_hover_text(entry.id)
                        .clicked()
                    {
                        state.selected = entry.id.into();
                    }
                }
            });
        if let Some(entry) = infrastructure::inspection(&state.selected) {
            ui.strong(format!("{} ({})", entry.name, entry.id));
            // Registry HTML is escaped in exports; the editor description viewer
            // renders the maintained plain text of the same documentation.
            ui.label(
                entry
                    .description_html
                    .replace("<p>", "")
                    .replace("</p>", "\n"),
            );
            let levels: Vec<String> = [
                "Default",
                "Error",
                "Warning",
                "Weak Warning",
                "Typo",
                "Server problem",
                "Information",
            ]
            .into_iter()
            .map(str::to_owned)
            .chain(
                profiles
                    .custom_severities
                    .iter()
                    .map(|severity| severity.name.clone()),
            )
            .collect();
            let entries = if project_entry {
                profiles
                    .project
                    .get_mut(root_key.as_ref().unwrap())
                    .unwrap()
            } else {
                &mut profiles.ide
            };
            if let Some(profile) = entries.iter_mut().find(|profile| profile.name == active) {
                let mut overridden = profile.inspections.contains_key(entry.id);
                if ui
                    .checkbox(&mut overridden, "Override inherited inspection setting")
                    .changed()
                {
                    if overridden {
                        profile.inspections.insert(entry.id.into(), Rule::default());
                    } else {
                        profile.inspections.remove(entry.id);
                    }
                }
                if let Some(rule) = profile.inspections.get_mut(entry.id) {
                    ui.checkbox(&mut rule.enabled, "Enabled");
                    let mut severity = rule.severity.clone().unwrap_or_else(|| "Default".into());
                    egui::ComboBox::from_id_salt("inspection-severity")
                        .selected_text(&severity)
                        .show_ui(ui, |ui| {
                            for level in levels {
                                ui.selectable_value(&mut severity, level.clone(), level);
                            }
                        })
                        .combo_named(crate::i18n::tr_static("Severity"));
                    rule.severity = (severity != "Default").then_some(severity);
                    let mut include = rule.include.join(";");
                    let mut exclude = rule.exclude.join(";");
                    ui.label("Include file globs (semicolon separated; empty = all)");
                    if ui.text_edit_singleline(&mut include).changed() {
                        rule.include = split_patterns(&include);
                    }
                    ui.label("Exclude file globs");
                    if ui.text_edit_singleline(&mut exclude).changed() {
                        rule.exclude = split_patterns(&exclude);
                    }
                    for option in entry.options {
                        ui.horizontal(|ui| {
                            ui.label(*option);
                            ui.text_edit_singleline(
                                rule.options.entry((*option).into()).or_default(),
                            );
                        });
                    }
                }
            }
        }
        if *profiles != profiles_before {
            if let Some(root) = root.as_deref() {
                if let Err(error) = profiles.save_project(root) {
                    message = Some(format!("Cannot save project inspection profiles: {error}"));
                }
            }
        }
        if let Some(message) = message {
            self.set_status(message);
        }
    }
    pub(super) fn show_inspection_runner_for_path(&mut self, path: &Path) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        self.bg.problems.infrastructure.project_scope = true;
        self.bg.problems.infrastructure.scope = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        self.show_inspection_runner();
    }
    pub(super) fn show_inspection_runner(&mut self) {
        self.bg.problems.infrastructure.dialog = true;
        self.focus_editor = false;
    }
    pub(super) fn inspection_runner_dialog(&mut self, ctx: &egui::Context) {
        let root_key = self
            .project
            .as_ref()
            .map(|project| project.root.to_string_lossy().into_owned());
        let profiles = &self.settings.inspections.profiles;
        let mut profile = root_key
            .as_ref()
            .and_then(|key| profiles.project_active.get(key))
            .unwrap_or(&profiles.active)
            .clone();
        let previous = profile.clone();
        let names: Vec<_> = profiles
            .ide
            .iter()
            .map(|profile| profile.name.clone())
            .chain(
                root_key
                    .as_ref()
                    .and_then(|key| profiles.project.get(key))
                    .into_iter()
                    .flatten()
                    .map(|profile| profile.name.clone()),
            )
            .collect();
        let scope_names = self.scope_names();
        let mut run = None;
        let mut close = false;
        egui::Window::new("Inspect Code / Run inspection by name")
            .collapsible(false)
            .resizable(true)
            .show_modal(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Named scope:");
                    super::scope_ui::scope_selector(
                        ui,
                        &mut self.settings.scopes.inspections,
                        &scope_names,
                        "inspection-shared-scope",
                    );
                });
                let state = &mut self.bg.problems.infrastructure;
                ui.label("Inspection (empty = entire profile)");
                ui.text_edit_singleline(&mut state.search);
                let query = state.search.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for entry in infrastructure::REGISTRY {
                            if format!("{} {}", entry.id, entry.name)
                                .to_lowercase()
                                .contains(&query)
                                && ui
                                    .selectable_label(
                                        state.selected == entry.id,
                                        format!("{} ({})", entry.name, entry.id),
                                    )
                                    .clicked()
                            {
                                state.selected = entry.id.into();
                            }
                        }
                    });
                if ui.button("Use entire profile").clicked() {
                    state.selected.clear();
                }
                ui.label(format!(
                    "Selected: {}",
                    if state.selected.is_empty() {
                        "Entire profile"
                    } else {
                        &state.selected
                    }
                ));
                ui.checkbox(&mut state.project_scope, "Project / directory scope");
                if state.project_scope {
                    ui.label("Directory relative to project root (empty = project)");
                    ui.text_edit_singleline(&mut state.scope);
                }
                egui::ComboBox::from_id_salt("batch-inspection-profile")
                    .selected_text(format!("Profile: {profile}"))
                    .show_ui(ui, |ui| {
                        for name in &names {
                            ui.selectable_value(&mut profile, name.clone(), name);
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Inspection profile"));
                if ui.button("Run").clicked() {
                    run = Some((
                        if state.selected.is_empty() {
                            "*".into()
                        } else {
                            state.selected.clone()
                        },
                        state.project_scope,
                        state.scope.clone(),
                    ));
                }
                close = ui.button("Cancel").clicked();
            });
        if profile != previous {
            if let Some(key) = root_key {
                self.settings
                    .inspections
                    .profiles
                    .project_active
                    .insert(key, profile);
            } else {
                self.settings.inspections.profiles.active = profile;
            }
            self.invalidate_inspection_profile();
            if let Err(error) = self.settings.save() {
                self.set_status(format!(
                    "Profile changed for this session; cannot save settings: {error}"
                ));
            }
        }
        close |= ctx.input(|input| input.key_pressed(egui::Key::Escape));
        if let Some((code, project, scope)) = run {
            let file = if project {
                self.project
                    .as_ref()
                    .map(|project| project.root.join(scope))
            } else {
                self.active
                    .and_then(|index| self.tabs.get(index))
                    .and_then(|tab| tab.state.path.clone())
            };
            if let Some(path) = file {
                if self.project.as_ref().is_some_and(|project| {
                    path.canonicalize().ok().is_some_and(|path| {
                        path.starts_with(
                            project
                                .root
                                .canonicalize()
                                .unwrap_or_else(|_| project.root.clone()),
                        )
                    })
                }) {
                    self.run_selected_inspection_for(code, Some(path));
                    close = true;
                } else {
                    self.set_status("Inspection scope must be inside the current project");
                }
            } else {
                self.set_status("Open a project and save the current file before inspecting");
            }
        }
        if close {
            self.bg.problems.infrastructure.dialog = false;
            self.focus_editor = true;
        }
    }
    pub(super) fn inspection_result_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut export = false;
        let mut apply = false;
        ui.horizontal(|ui| {
            let state = &mut self.bg.problems.infrastructure;
            egui::ComboBox::from_id_salt("inspection-result-group")
                .selected_text(format!("Group: {:?}", state.group))
                .show_ui(ui, |ui| {
                    for value in [GroupBy::Directory, GroupBy::Inspection, GroupBy::Severity] {
                        ui.selectable_value(&mut state.group, value, format!("{value:?}"));
                    }
                })
                .combo_named(crate::i18n::tr_static("Group"));
            egui::ComboBox::from_id_salt("inspection-report-format")
                .selected_text(if state.format.is_empty() {
                    "HTML"
                } else {
                    &state.format
                })
                .show_ui(ui, |ui| {
                    for format in ["HTML", "XML", "SARIF"] {
                        ui.selectable_value(
                            &mut state.format,
                            format.into(),
                            crate::i18n::tr_static(format),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Format"));
            export = ui.button("Export results…").clicked();
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("inspection-batch-fix")
                .selected_text(if self.bg.problems.infrastructure.fix_title.is_empty() {
                    "Cleanup-capable primary fixes"
                } else {
                    &self.bg.problems.infrastructure.fix_title
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.bg.problems.infrastructure.fix_title,
                        String::new(),
                        "Cleanup-capable primary fixes",
                    );
                    for title in self.bg.problems.infrastructure.available_fixes.clone() {
                        ui.selectable_value(
                            &mut self.bg.problems.infrastructure.fix_title,
                            title.clone(),
                            title,
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Batch fix"));
            ui.horizontal(|ui| {
                ui.label("Report output path");
                ui.add(
                    egui::TextEdit::singleline(&mut self.bg.problems.infrastructure.export_path)
                        .hint_text("inspection-results.html"),
                );
            });
            ui.label("Quick fix title (empty = cleanup-capable primary fixes)");
            ui.text_edit_singleline(&mut self.bg.problems.infrastructure.fix_title);
            apply = ui
                .add_enabled(
                    self.bg.problems.job.is_none(),
                    egui::Button::new("Apply quick fixes to all"),
                )
                .clicked();
        });
        if export {
            let state = &self.bg.problems.infrastructure;
            let format = if state.format.is_empty() {
                "HTML"
            } else {
                state.format.as_str()
            };
            let findings = self.inspection_findings();
            match infrastructure::export(
                &findings,
                format,
                state
                    .policy
                    .as_ref()
                    .map(|policy| policy.profile.name.as_str())
                    .unwrap_or("Project Default"),
            ) {
                Ok(text) => {
                    let state = &self.bg.problems.infrastructure;
                    let path = PathBuf::from(state.export_path.trim());
                    if path.as_os_str().is_empty() {
                        self.set_status("Enter the inspection report output path");
                    } else if let Err(error) = std::fs::write(&path, text) {
                        self.set_status(error.to_string());
                    } else {
                        self.set_status(format!(
                            "Inspection report exported to {}",
                            path.display()
                        ));
                    }
                }
                Err(error) => self.set_status(error),
            }
        }
        if apply {
            self.apply_inspection_result_fixes();
        }
    }
    fn inspection_findings(&self) -> Vec<infrastructure::Finding> {
        self.bg
            .problems
            .results
            .iter()
            .flat_map(|(path, problems)| {
                problems
                    .iter()
                    .map(move |(line, column, diagnostic)| infrastructure::Finding {
                        path: path.clone(),
                        line: *line,
                        column: *column,
                        diagnostic: diagnostic.clone(),
                        severity_name: self
                            .bg
                            .problems
                            .infrastructure
                            .policy
                            .as_ref()
                            .and_then(|policy| policy.severity_name(&diagnostic.code)),
                    })
            })
            .collect()
    }
    fn apply_inspection_result_fixes(&mut self) {
        let paths: Vec<_> = self
            .bg
            .problems
            .results
            .iter()
            .map(|(path, _)| path.clone())
            .take(1000)
            .collect();
        let selected = self
            .bg
            .problems
            .inspection
            .as_ref()
            .map(|(code, _)| code.clone())
            .filter(|code| code != "*");
        let title = self.bg.problems.infrastructure.fix_title.clone();
        let mut applied = 0;
        for path in paths {
            self.open_file(&path);
            let Some(index) = self.opened_editor_index(&path) else {
                continue;
            };
            self.sync_analysis_now();
            let Some((db, file)) = self.analysis_file(index) else {
                continue;
            };
            let mut inputs = self.analysis_inputs();
            // Explicit batch results already overrode legacy per-inspection disables.
            // Re-analysis for applying fixes must use the same effective policy.
            if self.bg.problems.inspection.is_some() {
                inputs.disabled_inspections.clear();
            }
            if selected.is_some() {
                inputs.inspections = InspectionOptions {
                    max_line_length: inputs.inspections.max_line_length,
                    editorconfig_enabled: inputs.inspections.editorconfig_enabled,
                    editorconfig: inputs.inspections.editorconfig.clone(),
                    style: inputs.inspections.style.clone(),
                    structural_rules: inputs.inspections.structural_rules.clone(),
                    framework: inputs.inspections.framework.clone(),
                    python_version: inputs.inspections.python_version,
                    project_symbols: inputs.inspections.project_symbols.clone(),
                    missing_docstrings: true,
                    ..Default::default()
                };
            }
            inputs.inspections.policy = self.bg.problems.infrastructure.policy.clone();
            let language = self.tabs[index].state.language;
            let problems = if matches!(language, Language::Python | Language::Cython) {
                let Ok(result) = analysis_jobs::analyze_file(
                    &db,
                    &file,
                    &inputs,
                    &rustpy_core::CancellationToken::new(),
                ) else {
                    continue;
                };
                result
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| {
                        let fixes = result
                            .quick_fixes
                            .iter()
                            .filter(|(range, _)| *range == diagnostic.range)
                            .flat_map(|(_, fixes)| fixes.clone())
                            .collect();
                        inspections::Problem { diagnostic, fixes }
                    })
                    .collect()
            } else if matches!(language, Language::Markdown | Language::PlainText) {
                let text = self.tabs[index].state.doc.text();
                let mut problems = Vec::new();
                if inputs.grammar {
                    if let Some(policy) = inputs.grammar_policy {
                        let context = if language == Language::Markdown {
                            rustpy_grammar::policy::Context::Markdown
                        } else {
                            rustpy_grammar::policy::Context::Prose
                        };
                        problems.extend(rustpy_analysis::file_analysis::grammar_problems(
                            &rustpy_grammar::policy::check(&text, context, policy),
                        ));
                    }
                }
                if let Some(dictionary) = inputs.dictionary.as_deref() {
                    problems.extend(rustpy_analysis::file_analysis::spelling_problems(
                        &rustpy_spell::check_plain_text(&text, dictionary),
                        rustpy_analysis::file_analysis::DictionaryActionPolicy::ReplacementsOnly,
                    ));
                }
                if let Some(policy) = &inputs.inspections.policy {
                    policy.apply(&path, &mut problems);
                }
                problems
            } else {
                continue;
            };
            let edits = infrastructure::batch_edits(
                &problems,
                selected.as_deref(),
                (!title.is_empty()).then_some(title.as_str()),
                title.is_empty(),
            );
            if !edits.is_empty()
                && self.tabs[index]
                    .state
                    .apply_text_edits(edits, "Apply inspection fixes")
            {
                applied += 1;
            }
        }
        self.set_status(format!(
            "Applied fixes in {applied} file(s); edits are undoable and have not been saved"
        ));
    }
}
fn split_patterns(text: &str) -> Vec<String> {
    text.split(';')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (IdeApp, tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "import os\nmissing_name\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.inspections.spelling = false;
        settings.inspections.grammar = false;
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        (app, root, path)
    }
    #[test]
    fn actual_editor_policy_changes_invalidate_cache_and_modal_owns_keyboard() {
        let (mut app, _root, path) = app();
        let index = app.active.unwrap();
        app.sync_open_tabs_now();
        let before = app.analysis_inputs();
        app.settings.inspections.profiles.ide[0].inspections.insert(
            "PyUnusedImports".into(),
            Rule {
                enabled: false,
                ..Rule::default()
            },
        );
        let after = app.analysis_inputs();
        assert_ne!(before.fingerprint(), after.fingerprint());
        let (db, file) = app.analysis_file(index).unwrap();
        let result =
            analysis_jobs::analyze_file(&db, &file, &after, &rustpy_core::CancellationToken::new())
                .unwrap();
        assert!(!result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "PyUnusedImports"));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "PyUnresolvedReferences"));
        app.settings
            .inspections
            .profiles
            .file_levels
            .insert(path.to_string_lossy().into_owned(), HighlightingLevel::None);
        let none = app.analysis_inputs();
        let result =
            analysis_jobs::analyze_file(&db, &file, &none, &rustpy_core::CancellationToken::new())
                .unwrap();
        assert!(result.diagnostics.is_empty());
        app.tabs[index].state.inspection_level = HighlightingLevel::None;
        assert!(app.tabs[index].state.diagnostics().is_empty());
        let source = app.tabs[index].state.doc.text();
        app.show_inspection_runner();
        assert!(!app.editor_controls_available());
        assert!(app.ide_shortcuts_blocked());
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.inspection_runner_dialog(ui.ctx()));
        // This model fixture has no renderer to apply egui's texture changes.
        output.textures_delta.clear();
        drop(output);
        assert!(!app.bg.problems.infrastructure.dialog);
        assert_eq!(app.tabs[index].state.doc.text(), source);
    }
    #[test]
    fn effective_file_levels_do_not_reinvalidate_on_unchanged_frames() {
        let (mut app, _root, _path) = app();
        let index = app.active.unwrap();
        app.settings.semantic_highlighting = true;
        assert!(app.background_features().semantic_highlighting);
        for level in [HighlightingLevel::Syntax, HighlightingLevel::None] {
            app.tabs[index].state.inspection_level = level;
            app.tabs[index].state.semantic_highlighting = true;
            assert!(app.sync_inspection_highlighting());
            assert!(!app.tabs[index].state.semantic_highlighting);
            assert!(!app.sync_inspection_highlighting());
            assert!(!app.sync_inspection_highlighting());
        }
        app.tabs[index].state.inspection_level = HighlightingLevel::All;
        assert!(app.sync_inspection_highlighting());
        assert!(app.tabs[index].state.semantic_highlighting);
        assert!(!app.sync_inspection_highlighting());
    }
    #[test]
    fn actual_project_batch_collects_warning_results_and_retains_scope_for_rerun() {
        let (mut app, root, path) = app();
        std::fs::write(root.path().join("other.py"), "import sys\n").unwrap();
        // Deliver the explicit refresh before capturing shared-scope membership;
        // the fixture does not poll filesystem watcher notifications.
        app.refresh_project_files();
        assert!(app
            .project_index
            .files
            .wait_idle(std::time::Duration::from_secs(20)));
        app.settings
            .inspections
            .disabled_inspections
            .push("PyUnusedImports".into());
        let index = app.active.unwrap();
        app.sync_open_tabs_now();
        let (db, file) = app.analysis_file(index).unwrap();
        let ordinary = analysis_jobs::analyze_file(
            &db,
            &file,
            &app.analysis_inputs(),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
        assert!(!ordinary
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "PyUnusedImports"));
        app.run_selected_inspection_for("PyUnusedImports".into(), Some(root.path().to_path_buf()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while app.bg.problems.job.is_some() && std::time::Instant::now() < deadline {
            app.poll_project_problems();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.bg.problems.job.is_none(), "batch did not finish");
        assert_eq!(app.bg.problems.summary.unwrap().0, 2);
        assert_eq!(app.bg.problems.results.len(), 2);
        assert!(app
            .bg
            .problems
            .results
            .iter()
            .any(|(file, _)| file == &path));
        assert!(app
            .bg
            .problems
            .results
            .iter()
            .flat_map(|(_, results)| results)
            .all(|(_, _, diagnostic)| diagnostic.code == "PyUnusedImports"));
        assert!(
            app.bg
                .problems
                .infrastructure
                .policy
                .as_ref()
                .unwrap()
                .batch
        );
        let findings = app.inspection_findings();
        assert_eq!(findings.len(), 2);
        app.apply_inspection_result_fixes();
        assert!(app
            .tabs
            .iter()
            .filter(|tab| tab
                .state
                .path
                .as_ref()
                .is_some_and(|file| file.extension().is_some_and(|extension| extension == "py")))
            .all(|tab| !tab.state.doc.text().contains("import ")));
        assert!(std::fs::read_to_string(path).unwrap().contains("import os"));
    }
}

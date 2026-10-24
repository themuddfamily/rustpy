//! Problems › Project Errors (roadmap §2.5 "Problems tool window (Current
//! File / Project Errors)", §2.2 "parallel analysis across cores"): every
//! Python file of the project is inspected in parallel by a cancellable
//! background task, and the errors are listed by file.

use std::time::Duration;

use rustpy_core::{Diagnostic, TaskError, TaskHandle, TaskManager};

use super::*;

/// A file's errors: `(0-based line, 0-based character column, diagnostic)`.
pub(super) type FileErrors = (PathBuf, Vec<(usize, usize, Diagnostic)>);

/// Findings, inspected-file count and offered quick-fix titles from one owned run.
type ProjectScanResult = (Vec<FileErrors>, usize, Vec<String>);

/// State of the Project Errors scope.
#[derive(Default)]
pub(super) struct ProjectProblems {
    /// The Problems tool window shows the project's errors (else the current file's).
    pub show: bool,
    pub(super) infrastructure: super::inspection_infrastructure::InfrastructureUi,
    pub(super) results: Vec<FileErrors>,
    /// Files inspected and how long it took, once a run finished.
    pub(super) summary: Option<(usize, Duration)>,
    pub(super) job: Option<TaskHandle<ProjectScanResult>>,
    /// Explicit single-inspection run; None retains the existing Project Errors scope.
    pub(super) inspection: Option<(String, Option<PathBuf>)>,
    // Per-App, test-only pacing proves overlap without serializing the suite.
    #[cfg(test)]
    scan_gate: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}

impl ProjectProblems {
    /// Cancels only this App’s owned task; another window can inspect independently.
    pub fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
    }

    /// Forgets the results (another project).
    pub fn clear(&mut self) {
        self.cancel();
        self.results.clear();
        self.summary = None;
        self.inspection = None;
        self.infrastructure = Default::default();
    }
}

/// 0-based line and character column of byte `offset` in `text`.
pub(super) fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..rustpy_core::floor_char_boundary(text, offset)];
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line, col)
}

/// The errors among `problems`, with their positions in `text`.
pub(super) fn errors_of(
    text: &str,
    problems: Vec<inspections::Problem>,
) -> Vec<(usize, usize, Diagnostic)> {
    problems
        .into_iter()
        .filter(|p| p.diagnostic.severity == rustpy_core::Severity::Error)
        .map(|p| {
            let (line, col) = line_col(text, usize::from(p.diagnostic.range.start()));
            (line, col, p.diagnostic)
        })
        .collect()
}

#[cfg(test)]
fn pause_scan(
    gate: Option<(
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
) -> rustpy_core::tasks::TaskResult<()> {
    if let Some((entered, release)) = gate {
        entered.send(()).map_err(|error| error.to_string())?;
        release
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

impl IdeApp {
    /// Runs one known inspection explicitly, including warning/typo results.
    pub(super) fn run_selected_inspection(&mut self, code: String, project: bool) {
        let file = if project {
            None
        } else {
            self.active
                .and_then(|i| self.tabs.get(i))
                .and_then(|tab| tab.state.path.clone())
        };
        if !project && file.is_none() {
            self.set_status("Save this file before running an inspection");
            return;
        }
        self.run_selected_inspection_for(code, file);
    }
    pub(super) fn run_selected_inspection_for(&mut self, code: String, file: Option<PathBuf>) {
        if code != "*" && rustpy_analysis::inspections::infrastructure::inspection(&code).is_none()
        {
            self.set_status(format!("Unknown inspection: {code}"));
            return;
        }
        let Some(project) = &self.project else {
            self.set_status("Open a project before running an inspection");
            return;
        };
        let root = project.root.clone();
        let db = project.analysis.clone();
        let excluded = self.excluded_dirs();
        self.sync_open_tabs_now();
        let mut inputs = self.analysis_inputs();
        inputs.disabled_inspections.clear(); // Explicit Run can inspect a disabled profile entry.
        let mut profiles = self.settings.inspections.profiles.clone();
        if std::mem::take(&mut self.bg.problems.infrastructure.rerun_policy) {
            if let Some(previous) = &self.bg.problems.infrastructure.policy {
                profiles.project_active.insert(
                    root.to_string_lossy().into_owned(),
                    previous.profile.name.clone(),
                );
            }
        }
        let policy = match rustpy_analysis::inspections::infrastructure::Policy::from_settings(
            &profiles,
            Some(&root),
            true,
        ) {
            Ok(policy) => Some(policy),
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        if code != "*" {
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
        inputs.inspections.policy = policy;
        if let Some(policy) = inputs.inspections.policy.as_mut() {
            policy.batch = true;
            if code != "*" {
                policy
                    .profile
                    .inspections
                    .entry(code.clone())
                    .or_default()
                    .enabled = true;
            }
        }
        self.bg.problems.infrastructure.policy = inputs.inspections.policy.clone();
        inputs.grammar = code == "Grammar" || (code == "*" && self.settings.inspections.grammar);
        if inputs.grammar {
            match grammar_policy::configured(&self.settings.inspections) {
                Ok(policy) => inputs.grammar_policy = Some(policy),
                Err(error) => {
                    self.set_status(error.to_string());
                    return;
                }
            }
        }
        inputs.dictionary = (code == "SpellCheckingInspection"
            || (code == "*" && self.settings.inspections.spelling))
            .then(|| {
                spell_dictionary_for(self.project.as_ref().map(|project| project.root.as_path()))
            });
        inputs.parameter_hints = false;
        inputs.type_hints = false;
        let open_text: std::collections::BTreeMap<PathBuf, String> = self
            .tabs
            .iter()
            .filter_map(|tab| Some((tab.state.path.clone()?, tab.state.doc.text())))
            .collect();
        let mut shared_scope_context = self.capture_scope_context();
        let captured_project_scope = self.project_scope();
        let shared_scope = match self
            .settings
            .scopes
            .compile(&self.settings.scopes.inspections)
        {
            Ok(scope) => scope,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let library_paths: Vec<PathBuf> = if shared_scope.includes_non_project() {
            self.everywhere
                .index
                .library
                .as_ref()
                .map(|index| index.files().map(|(path, _, _)| path).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let selected_file = file.clone();
        let selected_code = code.clone();
        #[cfg(test)]
        let scan_gate = self.bg.problems.scan_gate.take();
        self.bg.problems.cancel();
        self.bg.problems.results.clear();
        self.bg.problems.summary = None;
        self.bg.problems.inspection = Some((code, file));
        self.bg.problems.show = true;
        let handle = TaskManager::global().task(format!("Running {selected_code}")).spawn(move |progress| {
            #[cfg(test)]
            pause_scan(scan_gate)?;
            let explicit_boundary=selected_file.clone();
            let mut files:Vec<PathBuf> = match selected_file {
                Some(file) if file.is_dir()=>rustpy_search::project_files_with_progress(&file,&excluded,progress).into_iter().filter(|p|p.extension().and_then(|extension|extension.to_str()).is_some_and(|extension|matches!(extension,"py"|"pyi"|"md"|"markdown"|"txt"|"rst"))).collect(),
                Some(file) => vec![file],
                None => captured_project_scope.as_ref().map(|scope|rustpy_project::files::project_files_in_with_cancel(scope,&||progress.is_cancelled())).unwrap_or_else(||rustpy_search::project_files_with_progress(&root, &excluded, progress)).into_iter().filter(|p| p.extension().and_then(|s| s.to_str()).is_some_and(|extension|matches!(extension,"py"|"pyi"|"md"|"markdown"|"txt"|"rst"))).collect(),
            };
            // Project membership comes from this run's actual ProjectScope enumeration,
            // even if the asynchronous editor file catalogue was not ready at launch.
            if shared_scope_context.project_files.is_empty() {
                if let Some(project_scope)=&captured_project_scope {
                    let mut project_files:Vec<_>=files.iter().filter(|path|project_scope.contains(path)).cloned().collect();
                    project_files.sort();project_files.dedup();shared_scope_context.project_files=Arc::new(project_files);
                }
            }
            if shared_scope.includes_non_project() && explicit_boundary.as_ref().is_none_or(|path|path.is_dir()) {
                files.extend(library_paths);
                files.extend(shared_scope_context.open_files.iter().cloned());
                files.extend(shared_scope_context.changed_files.iter().cloned());
                files.extend(shared_scope_context.recently_viewed.iter().cloned());
                files.extend(shared_scope_context.recently_changed.iter().cloned());
                for directory in &shared_scope_context.scratch_roots {files.extend(crate::scratches::list(directory));}
                if let Some(boundary)=&explicit_boundary {if boundary!=&root {files.retain(|path|path.starts_with(boundary));}}
            }
            files.sort();files.dedup();
            files.retain(|path|shared_scope.matches(path,&shared_scope_context));
            let mut results = Vec::new();
            let mut fix_titles=std::collections::BTreeSet::new();
            for path in &files {
                progress.check()?; progress.set_text(path.display().to_string());
                let text = open_text.get(path).cloned().or_else(||db.current_text(path).map(|text|text.to_string())).ok_or_else(|| format!("Cannot read {}", path.display()))?;
                let language = Language::from_path(path);
                let diagnostics = if matches!(language, Language::Python | Language::Cython) {
                    let file = db.file_for_path(path).ok_or_else(|| format!("Cannot analyze {}", path.display()))?;
                    let result=analysis_jobs::analyze_file(&db, &file, &inputs, progress.token())?;
                    for (range,fixes) in &result.quick_fixes {if result.diagnostics.iter().any(|diagnostic|diagnostic.range==*range&&rustpy_analysis::inspections::infrastructure::matches_inspection(&selected_code,&diagnostic.code)) {for fix in fixes {if !fix.is_suppression()&&!fix.edits.is_empty()&&fix_titles.len()<1000 {fix_titles.insert(fix.title.clone());}}}}
                    result.diagnostics
                } else if matches!(language, Language::Markdown | Language::PlainText) {
                    let mut found=Vec::new();
                    if inputs.grammar {let context=if language==Language::Markdown {rustpy_grammar::policy::Context::Markdown}else{rustpy_grammar::policy::Context::Prose};if let Some(policy)=inputs.grammar_policy {found.extend(rustpy_analysis::file_analysis::grammar_problems(&rustpy_grammar::policy::check(&text,context,policy)));}}
                    if let Some(dictionary)=inputs.dictionary.as_deref(){found.extend(rustpy_analysis::file_analysis::spelling_problems(&rustpy_spell::check_plain_text(&text,dictionary),rustpy_analysis::file_analysis::DictionaryActionPolicy::ReplacementsOnly));}
                    if let Some(policy)=&inputs.inspections.policy{policy.apply(path,&mut found);}
                    found.into_iter().map(|problem|problem.diagnostic).collect()
                } else { return Err(format!("Inspection {selected_code} does not support {}", path.display()).into()); };
                let matches: Vec<_> = diagnostics.into_iter().filter(|d| rustpy_analysis::inspections::infrastructure::matches_inspection(&selected_code,&d.code)).map(|d| { let (line,col)=line_col(&text, usize::from(d.range.start())); (line,col,d) }).collect();
                if !matches.is_empty() { results.push((path.clone(),matches)); }
            }
            progress.check()?;
            Ok((results,files.len(),fix_titles.into_iter().collect()))
        });
        self.bg.problems.job = Some(handle);
        self.bottom = Some(BottomTool::Problems);
    }

    /// Inspects every Python file of the project in the background.
    pub(super) fn inspect_project(&mut self) {
        let Some(p) = &self.project else { return };
        let (root, db) = (p.root.clone(), p.analysis.clone());
        let scope = self
            .project_scope()
            .unwrap_or_else(|| rustpy_project::ProjectScope::new(root));
        // Open editors' texts are what gets inspected.
        self.sync_open_tabs_now();
        let inputs = self.analysis_inputs();
        let mut opts = inputs.inspections;
        if let Some(policy) = opts.policy.as_mut() {
            policy.batch = true;
        }
        let disabled = inputs.disabled_inspections;
        self.bg.problems.inspection = None;
        self.bg.problems.cancel();
        let handle = TaskManager::global()
            .task("Inspecting project")
            .spawn(move |progress| {
                progress.set_text("Inspecting project files");
                let files: Vec<PathBuf> =
                    rustpy_project::files::project_files_in_with_cancel(&scope, &|| {
                        progress.is_cancelled()
                    })
                    .into_iter()
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|extension| matches!(extension, "py" | "pyi"))
                    })
                    .collect();
                progress.check()?;
                let found = inspections::inspect_files(&db, &files, &opts, progress)?;
                let fix_titles: Vec<_> = found
                    .iter()
                    .flat_map(|(_, problems)| problems.iter())
                    .flat_map(|problem| problem.fixes.iter())
                    .filter(|fix| !fix.is_suppression() && !fix.edits.is_empty())
                    .map(|fix| fix.title.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .take(1000)
                    .collect();
                let errors: Vec<FileErrors> = found
                    .into_iter()
                    .filter_map(|(path, problems)| {
                        let text = db.current_text(&path)?;
                        let problems = problems
                            .into_iter()
                            .filter(|p| !disabled.contains(&p.diagnostic.code.to_string()))
                            .collect();
                        let errors = errors_of(&text, problems);
                        (!errors.is_empty()).then_some((path, errors))
                    })
                    .collect();
                Ok((errors, files.len(), fix_titles))
            });
        self.bg.problems.job = Some(handle);
    }

    pub(super) fn poll_project_problems(&mut self) {
        let Some(result) = self.bg.problems.job.as_mut().and_then(|j| j.try_result()) else {
            return;
        };
        let took = self
            .bg
            .problems
            .job
            .take()
            .map(|j| j.elapsed())
            .unwrap_or_default();
        match result {
            Ok((errors, files, fix_titles)) => {
                self.bg.problems.infrastructure.available_fixes = fix_titles;
                self.bg.problems.results = errors;
                self.bg.problems.summary = Some((files, took));
            }
            Err(TaskError::Cancelled) => {}
            Err(TaskError::Failed(e)) => self.set_status(format!(
                "{}: {e}",
                if self.bg.problems.inspection.is_some() {
                    "Running inspection failed"
                } else {
                    "Inspecting the project failed"
                }
            )),
        }
    }

    /// The Problems tool window's scope tabs. True when the Project Errors
    /// scope is shown (it drew the rest of the window).
    pub(super) fn problems_scope(&mut self, ui: &mut egui::Ui) -> bool {
        let mut project = self.bg.problems.show;
        ui.horizontal(|ui| {
            if ui.selectable_label(!project, "Current File").clicked() {
                project = false;
                self.tool_lists.vulnerabilities = false;
            }
            if ui
                .selectable_label(
                    project,
                    if self.bg.problems.inspection.is_some() {
                        "Inspection Results"
                    } else {
                        "Project Errors"
                    },
                )
                .on_hover_text(if self.bg.problems.inspection.is_some() {
                    "Results of the explicitly selected inspection and scope"
                } else {
                    "Errors in every Python file of the project"
                })
                .clicked()
            {
                project = true;
                self.tool_lists.vulnerabilities = false;
            }
            if ui
                .selectable_label(self.tool_lists.vulnerabilities, "Vulnerable Dependencies")
                .clicked()
            {
                self.tool_lists.vulnerabilities = true;
            }
        });
        if self.tool_lists.vulnerabilities {
            let interpreter = self
                .project
                .as_ref()
                .and_then(|project| project.interpreter.as_ref())
                .map(|interpreter| interpreter.path.clone());
            let (installed, advisories) = self
                .packages_panel
                .inspection_snapshot(interpreter.as_deref());
            ui.label("OSV results from the selected interpreter's Python Packages ledger. An empty result is meaningful only after an explicit check.");
            if ui
                .button("Open Python Packages / Check vulnerabilities…")
                .clicked()
            {
                self.bottom = Some(BottomTool::Packages);
            }
            if installed.is_none() {
                ui.label(
                    "Load the interpreter's package inventory before checking vulnerabilities.",
                );
            }
            for (package, version, id) in advisories {
                ui.horizontal(|ui| {
                    ui.label(format!("{package} {version}"));
                    ui.hyperlink_to(&id, format!("https://osv.dev/vulnerability/{id}"));
                });
            }
            return true;
        }
        if project
            && !self.bg.problems.show
            && self.bg.problems.summary.is_none()
            && self.bg.problems.job.is_none()
        {
            self.inspect_project();
        }
        self.bg.problems.show = project;
        if project {
            self.project_errors_view(ui);
        }
        project
    }

    fn project_errors_view(&mut self, ui: &mut egui::Ui) {
        self.inspection_result_toolbar(ui);
        self.problems_filters(ui);
        let selected = self.bg.problems.inspection.clone();
        let theme = self.theme.clone();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let running = self.bg.problems.job.as_ref().map(|j| j.progress());
        let mut rerun = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            match (&running, self.bg.problems.summary) {
                (Some((_, details, fraction)), _) => {
                    crate::a11y::spinner_sized(ui, 12.0);
                    cancel = ui.button("Cancel inspection").clicked();
                    let pct = fraction
                        .map(|f| format!(" {:.0}%", f * 100.0))
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(format!(
                            "Inspecting…{pct} {}",
                            crate::progress_ui::ellipsize(details, 60)
                        ))
                        .color(theme.text_dim),
                    );
                    ui.ctx().request_repaint_after(Duration::from_millis(150));
                }
                (None, Some((files, took))) => {
                    let errors: usize = self.bg.problems.results.iter().map(|(_, e)| e.len()).sum();
                    ui.label(
                        egui::RichText::new(format!(
                            "{errors} {} in {} file(s) · {files} file(s) inspected in {}",
                            if selected.is_some() {
                                "problem(s)"
                            } else {
                                "error(s)"
                            },
                            self.bg.problems.results.len(),
                            crate::progress_ui::format_elapsed(took)
                        ))
                        .color(theme.text_dim),
                    );
                }
                (None, None) => {
                    ui.label(
                        egui::RichText::new("The project has not been inspected yet")
                            .color(theme.text_dim),
                    );
                }
            }
            rerun = ui
                .small_button("⟳")
                .on_hover_text("Inspect the project again")
                .clicked();
        });
        if cancel {
            self.bg.problems.cancel();
            self.set_status("Inspection cancelled");
        }
        ui.separator();
        let mut open: Option<(PathBuf, usize, usize)> = None;
        egui::ScrollArea::vertical()
            .id_salt("project_errors")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if running.is_none()
                    && self.bg.problems.summary.is_some()
                    && self.bg.problems.results.is_empty()
                {
                    ui.label(
                        egui::RichText::new(if selected.is_some() {
                            "✔ No matching problems found"
                        } else {
                            "✔ No errors found"
                        })
                        .color(theme.success),
                    );
                }
                let group = self.bg.problems.infrastructure.group;
                let mut grouped: std::collections::BTreeMap<
                    String,
                    Vec<(&PathBuf, usize, usize, &Diagnostic)>,
                > = std::collections::BTreeMap::new();
                for (path, errors) in &self.bg.problems.results {
                    for (line, col, diagnostic) in errors {
                        if !self.problem_visible(path, diagnostic) {
                            continue;
                        }
                        let label = match group {
                            rustpy_analysis::inspections::infrastructure::GroupBy::Directory => {
                                path.parent()
                                    .unwrap_or(&root)
                                    .strip_prefix(&root)
                                    .unwrap_or(path)
                                    .display()
                                    .to_string()
                            }
                            rustpy_analysis::inspections::infrastructure::GroupBy::Inspection => {
                                diagnostic.code.to_string()
                            }
                            rustpy_analysis::inspections::infrastructure::GroupBy::Severity => self
                                .bg
                                .problems
                                .infrastructure
                                .policy
                                .as_ref()
                                .and_then(|policy| policy.severity_name(&diagnostic.code))
                                .unwrap_or_else(|| diagnostic.severity.label().into()),
                        };
                        grouped
                            .entry(label)
                            .or_default()
                            .push((path, *line, *col, diagnostic));
                    }
                }
                for (group, entries) in grouped {
                    ui.strong(format!("{group} ({})", entries.len()));
                    for (path, line, col, d) in entries {
                        let one = [(line, col, d.clone())];
                        let errors = &one;
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({})",
                                path.strip_prefix(&root).unwrap_or(path).display(),
                                errors.len()
                            ))
                            .strong()
                            .color(theme.text),
                        );
                        for (line, col, d) in errors {
                            let resp = ui.horizontal(|ui| {
                                ui.add_space(14.0);
                                let (dot, _) =
                                    ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                ui.painter().circle_filled(
                                    dot.center(),
                                    4.0,
                                    if d.severity == rustpy_core::Severity::Error {
                                        theme.error
                                    } else {
                                        theme.warning
                                    },
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&d.message).color(theme.text),
                                    )
                                    .sense(Sense::click()),
                                )
                                .on_hover_text(d.code.as_str())
                                .union(
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "{}:{}",
                                                line + 1,
                                                col + 1
                                            ))
                                            .color(theme.text_dim),
                                        )
                                        .sense(Sense::click()),
                                    ),
                                )
                            });
                            if resp.inner.clicked() {
                                self.tool_lists.problem_selected = Some((path.clone(), *line));
                                if self.tool_lists.problems_follow {
                                    open = Some((path.clone(), *line, *col));
                                }
                            }
                            if resp.inner.double_clicked() {
                                open = Some((path.clone(), *line, *col));
                            }
                        }
                    }
                }
            });
        if self.tool_lists.problems_preview {
            if let Some((path, line)) = &self.tool_lists.problem_selected {
                self.tool_source_preview(ui, path, *line);
            }
        }
        if rerun {
            if let Some((code, file)) = selected {
                self.bg.problems.infrastructure.rerun_policy = true;
                self.run_selected_inspection_for(code, file);
            } else {
                self.inspect_project();
            }
        }
        if let Some((path, line, col)) = open {
            self.open_file(&path);
            if let Some(tab) = self
                .opened_editor_index(&path)
                .and_then(|i| self.tabs.get_mut(i))
            {
                tab.view.cancel_session_restore();
                tab.state.go_to_line(line + 1, Some(col + 1));
                tab.state.scroll_to_caret = true;
            }
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_lines_and_characters() {
        let text = "a = 1\nb = ä + c\n";
        assert_eq!(line_col(text, 0), (0, 0));
        assert_eq!(line_col(text, 6), (1, 0));
        // Byte offset 15 is the `c` after `b = ä + ` (ä takes two bytes): character column 8.
        assert_eq!(line_col(text, 15), (1, 8));
        // Inside a character or past the end: clamped.
        assert_eq!(line_col(text, 11), (1, 4));
        assert_eq!(line_col(text, 1000), (2, 0));
    }

    #[test]
    fn only_errors_are_listed() {
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "import os\nx = undefined_name\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let problems = inspections::inspect(&db, &f, &InspectionOptions::default());
        assert!(
            problems
                .iter()
                .any(|p| p.diagnostic.severity != rustpy_core::Severity::Error),
            "the unused import is a warning"
        );
        let errors = errors_of(src, problems);
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .all(|(_, _, d)| d.severity == rustpy_core::Severity::Error));
        assert!(
            errors
                .iter()
                .any(|(line, col, d)| (*line, *col) == (1, 4)
                    && d.message.contains("undefined_name")),
            "{errors:?}"
        );
    }
}

#[cfg(test)]
mod selected_inspection_tests {
    use super::*;
    fn wait(app: &mut IdeApp) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.bg.problems.job.is_some() {
            app.poll_project_problems();
            assert!(
                std::time::Instant::now() < deadline,
                "selected inspection did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.bg.problems.summary.is_some(), "{:?}", app.status);
    }
    #[test]
    fn selected_inspection_uses_the_shared_named_scope_for_actual_batch_inputs() {
        use rustpy_settings::scopes::{NamedScope, ScopeSelection};
        let root = tempfile::tempdir().unwrap();
        let included = root.path().join("included.py");
        let excluded = root.path().join("excluded.py");
        std::fs::write(&included, "import os\n").unwrap();
        std::fs::write(&excluded, "import sys\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.scopes.named.push(NamedScope {
            name: "One source".into(),
            pattern: "file:included.py".into(),
            color: None,
            shared: false,
        });
        settings.scopes.inspections = ScopeSelection::Named("One source".into());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.run_selected_inspection("PyUnusedImports".into(), true);
        wait(&mut app);
        assert_eq!(app.bg.problems.summary.as_ref().unwrap().0, 1);
        assert!(app
            .bg
            .problems
            .results
            .iter()
            .any(|(path, _)| path == &included));
        assert!(!app
            .bg
            .problems
            .results
            .iter()
            .any(|(path, _)| path == &excluded));
    }
    #[test]
    fn actual_project_run_lists_selected_warnings_and_inspects_open_and_closed_files() {
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("π.py");
        let closed = root.path().join("closed.py");
        std::fs::write(&open, "value = 1\n").unwrap();
        std::fs::write(&closed, "import sys\nmissing_name\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.inspections.unused_imports = false;
        settings.inspections.disabled_inspections = vec!["PyUnusedImports".into()];
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&open);
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "import os\n".into())],
            "Unsaved import",
        );
        app.run_selected_inspection("PyUnusedImports".into(), true);
        wait(&mut app);
        assert_eq!(app.bg.problems.summary.unwrap().0, 2);
        assert_eq!(app.bg.problems.results.len(), 2);
        assert!(app
            .bg
            .problems
            .results
            .iter()
            .flat_map(|(_, items)| items)
            .all(|(_, _, d)| d.code == "PyUnusedImports"
                && d.severity == rustpy_core::Severity::Warning));
        assert_eq!(std::fs::read_to_string(&open).unwrap(), "value = 1\n");
        assert_eq!(
            std::fs::read_to_string(closed).unwrap(),
            "import sys\nmissing_name\n"
        );
        app.run_selected_inspection("PyUnusedImports".into(), false);
        wait(&mut app);
        assert_eq!(app.bg.problems.summary.unwrap().0, 1);
        assert_eq!(app.bg.problems.results[0].0, open);
        assert!(app.tabs[0].state.doc.can_undo());
        app.tabs[0].state.doc.undo().unwrap();
        assert!(
            !app.tabs[0].state.doc.can_undo(),
            "inspection added no editor undo unit"
        );
    }
    #[test]
    fn actual_markdown_file_run_honors_grammar_categories_not_profile_disable_and_never_writes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("note.md");
        let text = "the the answer happens alot";
        std::fs::write(&path, text).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.inspections.grammar = false;
        settings.inspections.disabled_inspections = vec!["Grammar".into()];
        settings.inspections.grammar_disabled_categories = vec!["grammar".into()];
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        app.run_selected_inspection("Grammar".into(), false);
        wait(&mut app);
        assert_eq!(app.bg.problems.summary.unwrap().0, 1);
        assert_eq!(app.bg.problems.results[0].1.len(), 1);
        assert_eq!(
            app.bg.problems.results[0].1[0].2.range.start(),
            TextSize::new(text.find("alot").unwrap() as u32)
        );
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }
    #[test]
    fn actual_emitted_incorrect_docstring_code_runs_instead_of_unknown_refusal() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("doc.py");
        let source = "def f(x):\n    \"\"\":param ghost: value\"\"\"\n    return x\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        app.run_selected_inspection("PyIncorrectDocstring".into(), false);
        wait(&mut app);
        assert_eq!(app.bg.problems.summary.unwrap().0, 1);
        assert!(!app.bg.problems.results.is_empty());
        assert!(app
            .bg
            .problems
            .results
            .iter()
            .flat_map(|(_, items)| items)
            .all(|(_, _, d)| d.code == "PyIncorrectDocstring"));
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
    // Releases the actual worker even if an assertion fails; no test owns a global lock.
    struct ReleaseScan(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseScan {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() {
                let _ = release.send(());
            }
        }
    }
    fn hold_next_scan(app: &mut IdeApp) -> (std::sync::mpsc::Receiver<()>, ReleaseScan) {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        app.bg.problems.scan_gate = Some((entered_tx, release_rx));
        (entered_rx, ReleaseScan(Some(release_tx)))
    }
    fn inspection_app() -> (tempfile::TempDir, IdeApp) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("m.py");
        std::fs::write(&path, "import os\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        (root, app)
    }
    #[test]
    fn actual_two_apps_overlap_without_cancelling_each_other_and_same_app_supersedes() {
        let (_first_root, mut first) = inspection_app();
        let (_second_root, mut second) = inspection_app();
        let (entered, release) = hold_next_scan(&mut first);
        first.run_selected_inspection("PyUnusedImports".into(), false);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let first_token = first.bg.problems.job.as_ref().unwrap().token().clone();
        second.run_selected_inspection("PyUnusedImports".into(), false);
        assert!(
            !first_token.is_cancelled(),
            "a second App must not cancel the first App's held real inspection"
        );
        drop(release);
        wait(&mut first);
        wait(&mut second);
        assert_eq!(first.bg.problems.results[0].1.len(), 1);
        assert_eq!(second.bg.problems.results[0].1.len(), 1);
        let (entered, release) = hold_next_scan(&mut first);
        first.run_selected_inspection("PyUnusedImports".into(), false);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let old_token = first.bg.problems.job.as_ref().unwrap().token().clone();
        first.run_selected_inspection("PyIncorrectDocstring".into(), false);
        assert!(
            old_token.is_cancelled(),
            "same-App replacement still cancels its owned previous job"
        );
        drop(release);
        wait(&mut first);
        assert!(first.bg.problems.results.is_empty());
        assert_eq!(first.bg.problems.summary.unwrap().0, 1);
        assert!(!first.tabs[0].state.doc.can_undo());
        assert!(!second.tabs[0].state.doc.can_undo());
    }
}

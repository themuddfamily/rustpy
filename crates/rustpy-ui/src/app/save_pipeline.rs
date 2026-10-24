//! Prepared save: all transformations operate on an isolated candidate. Only
//! a successful checked disk write permits publication into the live editor.
use super::*;
use crate::editor::{state::map_offset, Caret};
use rustpy_core::vfs::{ContentRevision, Snapshot};
use rustpy_text::{DocumentId, Edit, PreparedSavePlan, SourcePolicy};

struct TabSavePlan {
    path: PathBuf,
    tab_id: egui::Id,
    document: DocumentId,
    version: u64,
    session: u64,
    carets: Vec<Caret>,
    notebook_text: Option<String>,
    original_text: String,
    baseline: Option<ContentRevision>,
    disk_changed: bool,
    plan: PreparedSavePlan,
}

fn stage(
    candidate: &mut Document,
    carets: &mut Vec<Caret>,
    stages: &mut Vec<Vec<Edit>>,
    mut edits: Vec<Edit>,
) -> anyhow::Result<()> {
    if edits.is_empty() {
        return Ok(());
    }
    edits.sort_by_key(|edit| edit.range.start());
    let next: Vec<_> = carets
        .iter()
        .map(|caret| {
            Caret::new(
                map_offset(&edits, caret.sel.anchor, false),
                map_offset(&edits, caret.sel.head, false),
            )
        })
        .collect();
    candidate.edit(
        edits.clone(),
        &[],
        &[],
        rustpy_text::EditGroup::Separate,
        None,
    )?;
    *carets = next;
    stages.push(edits);
    Ok(())
}

impl IdeApp {
    pub(super) fn prepare_format_edits(
        &self,
        index: usize,
        candidate: &Document,
        carets: &[Caret],
    ) -> anyhow::Result<Vec<Edit>> {
        let project_safe_mode = self.project_safe_mode();
        let tab = &self.tabs[index];
        let path = tab.state.path.as_deref().unwrap_or_else(|| Path::new("."));
        let ec = self.settings.editor.editorconfig_for(path);
        if ec.ide.formatter_enabled == Some(false) {
            return Ok(Vec::new());
        }
        let mut settings = self.settings.editor.clone();
        let style = self.settings.code_style.clone();
        style.apply_language(path, &mut settings);
        if self.code_style_excludes(path).map_err(anyhow::Error::msg)? {
            return Ok(Vec::new());
        }
        use rustpy_format::candidate::{format_candidate, Formatter, FormatterLimits};
        let text = candidate.text();
        let token = rustpy_core::CancellationToken::new();
        let mut formatted = if self.selected_formatter() == "builtin" {
            let file_options =
                crate::editor::indent_options::file_edit_options(&settings, &tab.state).0;
            let mut options = rustpy_format::FormatOptions::pep8();
            options.max_line_length =
                super::indent_status::formatter_margin(file_options.right_margin);
            options.indent = file_options.indent_size.max(1);
            options.continuation_indent = file_options.continuation_indent;
            options.tabs = file_options.use_tabs.then_some(rustpy_format::TabIndent {
                width: file_options.tab_size,
                smart: file_options.smart_tabs.unwrap_or(false),
            });
            options.quote = match style.quote_style.as_str() {
                "double" => rustpy_format::QuoteStyle::Double,
                "single" => rustpy_format::QuoteStyle::Single,
                _ => rustpy_format::QuoteStyle::Preserve,
            };
            options.magic_trailing_comma = style.magic_trailing_comma;
            options.blank_lines_top_level = style.blank_lines_top_level;
            options.blank_lines_in_class = style.blank_lines_in_class;
            super::indent_status::apply_ide_format_options(&ec, &mut options);
            rustpy_format::style_policy::apply(&mut options, &style, &ec);
            let primary = carets[0];
            let range = if primary.has_selection() {
                primary.range()
            } else {
                TextRange::new(TextSize::new(0), candidate.len())
            };
            format_candidate(
                &text,
                Formatter::Builtin {
                    range,
                    options: &options,
                },
                &token,
            )?
        } else {
            if style.python != rustpy_settings::python_style::PythonStyle::default()
                || style.reformat_scope != "all"
                || style.keep_line_breaks
                || style.keep_simple_blocks
                || style.keep_first_column_comments
                || style.formatter_markers
            {
                anyhow::bail!(
                    "Selected changed-text/keep/marker policy requires the builtin formatter"
                );
            }
            if super::indent_status::imported_formatter_tags(&ec)
                .is_some_and(|tags| tags.has_start(&text))
            {
                anyhow::bail!(
                    "Configured formatter control tags require the builtin formatter; protected source was left unchanged"
                );
            }
            if project_safe_mode {
                anyhow::bail!("trust the project before running an external formatter");
            }
            let command =
                self.external_formatter_command(index, self.selected_formatter(), false)?;
            let output = rustpy_format::candidate::external_command_output(
                &text,
                command,
                FormatterLimits::default(),
                false,
                &token,
            )?;
            let output = String::from_utf8(output)?;
            if !text.is_empty() && output.is_empty() {
                anyhow::bail!("formatter returned empty output");
            }
            let output = rustpy_text::LineEnding::normalize_to_lf(&output).into_owned();
            rustpy_format::candidate::FormatCandidate {
                edits: if output == text {
                    Vec::new()
                } else {
                    vec![(
                        TextRange::new(TextSize::new(0), candidate.len()),
                        output.clone(),
                    )]
                },
                text: output,
            }
        };
        if style.reformat_scope != "all" && !carets[0].has_selection() {
            let changes = if style.reformat_scope == "vcs" {
                let git = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("VCS changed text requires a Git repository"))?;
                git.line_changes(path, &text)
            } else {
                let saved = std::fs::read_to_string(path).map_err(|_| {
                    anyhow::anyhow!("Changed-lines formatting requires a saved UTF-8 file")
                })?;
                rustpy_vcs::line_changes(&rustpy_text::LineEnding::normalize_to_lf(&saved), &text)
            };
            let lines = changes
                .iter()
                .map(|change| (change.start, change.count))
                .collect::<Vec<_>>();
            let restricted =
                rustpy_format::style_policy::changed_lines(&text, &formatted.text, &lines)?;
            formatted.edits = if restricted == text {
                Vec::new()
            } else {
                vec![(
                    TextRange::new(TextSize::new(0), candidate.len()),
                    restricted,
                )]
            };
        }
        Ok(document_markers::refine_formatter_edits(
            candidate,
            &formatted.edits,
        ))
    }
    fn prepare_tab_save(
        &self,
        index: usize,
        baseline: Option<&Snapshot>,
    ) -> anyhow::Result<TabSavePlan> {
        let tab = &self.tabs[index];
        let path = tab
            .state
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the editor has no file path"))?;
        let settings = self.settings.editor.clone();
        let style = self.settings.code_style.clone();
        let ec = self.settings.editor.editorconfig_for(&path);
        let original_text = tab.state.doc.text();
        let mut candidate = Document::new(&original_text);
        candidate.set_encoding(tab.state.doc.encoding());
        candidate.set_line_ending(tab.state.doc.line_ending());
        let mut carets = tab.state.carets.clone();
        let mut stages = Vec::new();
        let notebook_text = tab
            .notebook
            .as_ref()
            .filter(|notebook| notebook.is_dirty())
            .map(|notebook| notebook.text());
        if let Some(text) = &notebook_text {
            let whole = TextRange::new(TextSize::new(0), candidate.len());
            let edits =
                document_markers::refine_formatter_edits(&candidate, &[(whole, text.clone())]);
            stage(&mut candidate, &mut carets, &mut stages, edits)?;
        }
        let actions_apply = self.save_actions_apply(&path);
        let transforms_allowed = actions_apply
            && !self
                .code_style_excludes(&path)
                .map_err(anyhow::Error::msg)?
            && ec.ide.formatter_enabled != Some(false);
        if transforms_allowed
            && tab.state.language == Language::Python
            && (style.cleanup_on_save || style.rearrange_on_save)
        {
            if let Some(project) = &self.project {
                // Speculative cleanup belongs to an isolated analysis world;
                // the shared editor/database are published only after save succeeds.
                let resolver = rustpy_semantic::module::ModuleResolver::new(
                    project.analysis.resolver.roots.clone(),
                    project.analysis.resolver.search_paths.clone(),
                    project.analysis.resolver.python_version,
                )
                .with_stub_roots(project.analysis.resolver.stub_roots.clone())
                .with_platform(&project.analysis.resolver.platform);
                let analysis = Analysis::new(resolver);
                for source in project.analysis.open_files() {
                    analysis.set_file_text(&source.path, &source.text);
                }
                let options = super::reformat_workflows::options_for(
                    &settings,
                    &style,
                    &path,
                    &candidate.text(),
                    style.keep_line_breaks,
                );
                for _ in 0..3 {
                    if !style.cleanup_on_save {
                        break;
                    }
                    let id = analysis.set_file_text(&path, &candidate.text());
                    let file = analysis
                        .file(id)
                        .ok_or_else(|| anyhow::anyhow!("Cleanup candidate disappeared"))?;
                    let edits = super::reformat_workflows::policy_edits(
                        &candidate.text(),
                        super::reformat_workflows::cleanup_edits(
                            &analysis,
                            &file,
                            &self.analysis_inputs().inspections,
                        ),
                        &options,
                    );
                    if edits.is_empty() {
                        break;
                    }
                    stage(
                        &mut candidate,
                        &mut carets,
                        &mut stages,
                        edits
                            .into_iter()
                            .map(|(range, text)| Edit::replace(range, text))
                            .collect(),
                    )?;
                }
                if style.rearrange_on_save {
                    let mut arrangement = style.python.arrangement.clone();
                    arrangement.enabled = true;
                    let id = analysis.set_file_text(&path, &candidate.text());
                    let file = analysis
                        .file(id)
                        .ok_or_else(|| anyhow::anyhow!("Arrangement candidate disappeared"))?;
                    let edits = super::reformat_workflows::policy_edits(
                        &candidate.text(),
                        rustpy_format::python_style::arrange_with_db(
                            &analysis,
                            &file,
                            &arrangement,
                        ),
                        &options,
                    );
                    stage(
                        &mut candidate,
                        &mut carets,
                        &mut stages,
                        edits
                            .into_iter()
                            .map(|(range, text)| Edit::replace(range, text))
                            .collect(),
                    )?;
                }
            }
        }
        if transforms_allowed
            && style.format_on_save
            && tab.state.language == Language::Python
            && (style.format_whole_file_on_save
                || self.active == Some(index)
                || self.selected_formatter() != "builtin")
            && (self.selected_formatter() == "builtin" || self.external_format_enabled(index, true))
        {
            let save_carets = if style.format_whole_file_on_save {
                vec![Caret::at(TextSize::new(0))]
            } else {
                carets.clone()
            };
            let edits = self.prepare_format_edits(index, &candidate, &save_carets)?;
            stage(&mut candidate, &mut carets, &mut stages, edits)?;
        }
        if transforms_allowed
            && style.optimize_imports_on_save
            && tab.state.language == Language::Python
        {
            // Reparse the formatted candidate without publishing speculative
            // editor text into the shared analysis database.
            if let Some((project, file)) = self.project.as_ref().and_then(|project| {
                project
                    .analysis
                    .file_for_path(&path)
                    .map(|file| (project, file))
            }) {
                let file = Arc::new(file.with_candidate_text(candidate.text()));
                let options = super::reformat_workflows::options_for(
                    &settings,
                    &style,
                    &path,
                    &candidate.text(),
                    style.keep_line_breaks,
                );
                let edits = super::reformat_workflows::policy_edits(
                    &candidate.text(),
                    rustpy_format::imports::optimize_imports_with_style(
                        &file,
                        &inspections::unused_import_ranges_with(&project.analysis, &file),
                        &style.python.imports,
                    ),
                    &options,
                );
                stage(
                    &mut candidate,
                    &mut carets,
                    &mut stages,
                    edits
                        .into_iter()
                        .map(|(range, text)| Edit::replace(range, text))
                        .collect(),
                )?;
            }
        }
        if actions_apply && notebook_text.is_none() {
            if self.settings.save_actions.update_copyright {
                if let Some(edit) = self.prepare_save_copyright(&path, &candidate.text())? {
                    stage(&mut candidate, &mut carets, &mut stages, vec![edit])?;
                }
            }
            if transforms_allowed
                && (self.settings.save_actions.prettier.enabled
                    || self.settings.save_actions.eslint.enabled)
            {
                let source = candidate.text();
                let output = self.prepare_web_save_candidate(&path, &source)?;
                if output != source {
                    let edits = document_markers::refine_formatter_edits(
                        &candidate,
                        &[(TextRange::new(TextSize::new(0), candidate.len()), output)],
                    );
                    stage(&mut candidate, &mut carets, &mut stages, edits)?;
                }
            }
        }
        let policy = crate::editor::save_actions::SavePolicy::resolve(
            settings.strip_trailing_spaces_on_save,
            settings.ensure_newline_at_eof,
            &ec,
        );
        let baseline_encoding = if policy.strip == rustpy_settings::StripTrailing::ModifiedLines
            && baseline.is_some()
        {
            self.encoding_override(&path).map_err(anyhow::Error::msg)?
        } else {
            None
        };
        let (cleanup, encoding, ending) = super::save_ui::prepare_save_cleanup(
            &candidate,
            &path,
            tab.state.language,
            &carets,
            &settings,
            &ec,
            baseline.map(|snapshot| snapshot.bytes.as_ref()),
            baseline_encoding,
        )?;
        stages.push(cleanup);
        let plan =
            tab.state
                .doc
                .prepare_save(stages, encoding, ending, SourcePolicy::for_path(&path))?;
        let disk_changed = baseline.is_none_or(|snapshot| snapshot.bytes.as_ref() != plan.bytes());
        Ok(TabSavePlan {
            path,
            tab_id: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            session: self.file_session,
            carets: tab.state.carets.clone(),
            notebook_text,
            original_text,
            baseline: baseline.map(|snapshot| snapshot.revision),
            disk_changed,
            plan,
        })
    }
    fn save_plan_is_current(&self, index: usize, prepared: &TabSavePlan) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            tab.view.id == prepared.tab_id
                && tab.state.path.as_deref() == Some(prepared.path.as_path())
                && tab.state.doc.identity() == prepared.document
                && tab.state.doc.version() == prepared.version
                && tab.state.carets == prepared.carets
                && self.file_session == prepared.session
                && tab
                    .notebook
                    .as_ref()
                    .filter(|notebook| notebook.is_dirty())
                    .map(|notebook| notebook.text())
                    == prepared.notebook_text
        })
    }
    pub(super) fn save_tab_prepared(&mut self, index: usize) -> bool {
        if !self.refresh_file_permissions(index) {
            return false;
        }
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let Some(path) = tab.state.path.clone() else {
            return false;
        };
        if tab.image.is_some() || tab.state.doc.is_read_only() {
            let modified = tab.state.doc.is_modified()
                || tab
                    .notebook
                    .as_ref()
                    .is_some_and(|notebook| notebook.is_dirty());
            if modified && matches!(self.dialog, Dialog::None) {
                self.request_make_writable(index);
            }
            self.set_status(format!(
                "{} is read-only{}",
                path.display(),
                if modified {
                    ": changes could not be saved"
                } else {
                    ": nothing to save"
                }
            ));
            return !modified;
        }
        let expected = self.disk_revisions.get(&path).copied().flatten();
        let baseline = match self.file_vfs().read(&path, self.editor_read_limits()) {
            Ok(snapshot) => Some(snapshot),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                self.set_status(format!("Cannot save {}: {error}", path.display()));
                return false;
            }
        };
        let observed = baseline.as_ref().map(|snapshot| snapshot.revision);
        if expected != observed {
            if matches!(self.dialog, Dialog::None) {
                self.observed_revisions.insert(path.clone(), observed);
                self.dialog = Dialog::ExternalChange(path.clone());
            }
            self.set_status(format!(
                "Not saved: {} changed on disk; choose Reload or Keep first",
                path.display()
            ));
            return false;
        }
        let _operation = match rustpy_vcs::RepositoryOperation::enter(&path, "Saving file") {
            Ok(operation) => operation,
            Err(error) => {
                self.set_status(format!("Not saved: {error}"));
                return false;
            }
        };
        let prepared = match self.prepare_tab_save(index, baseline.as_ref()) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_status(format!("Cannot save {}: {error}", path.display()));
                return false;
            }
        };
        if !self.save_plan_is_current(index, &prepared) {
            self.set_status("Not saved: the editor changed while preparing the save");
            return false;
        }
        if let Err(error) = self.tabs[index]
            .state
            .doc
            .check_prepared_save(&prepared.plan)
        {
            self.set_status(format!("Cannot save {}: {error}", path.display()));
            return false;
        }
        let revision =
            match self.checked_editor_write(&path, prepared.plan.bytes(), prepared.baseline) {
                Ok(revision) => revision,
                Err(error) => {
                    self.set_status(format!("Cannot save {}: {error}", path.display()));
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        let observed = match self.file_vfs().read(&path, self.editor_read_limits())
                        {
                            Ok(snapshot) => Some(Some(snapshot.revision)),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                Some(None)
                            }
                            Err(_) => None,
                        };
                        if let Some(observed) = observed {
                            self.observed_revisions.insert(path.clone(), observed);
                        }
                        if matches!(self.dialog, Dialog::None) {
                            self.dialog = Dialog::ExternalChange(path.clone());
                        }
                    }
                    return false;
                }
            };
        let tab = &mut self.tabs[index];
        if let Err(error) = tab.state.commit_prepared_save(prepared.plan) {
            self.set_status(format!("Cannot publish saved {}: {error}", path.display()));
            return false;
        }
        tab.state.doc.mark_saved();
        if prepared.notebook_text.is_some() {
            if let Some(notebook) = &mut tab.notebook {
                notebook.set_dirty(false);
                notebook.set_synced_version(Some(tab.state.doc.version()));
            }
        }
        if path.file_name().is_some_and(|name| name == ".editorconfig") {
            rustpy_settings::editorconfig::invalidate_cache();
        }
        self.finish_saved_tab(
            index,
            path,
            revision,
            &prepared.original_text,
            prepared.disk_changed,
        );
        true
    }
}

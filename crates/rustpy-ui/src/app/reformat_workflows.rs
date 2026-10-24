//! Captured Reformat File/directory workflows. Owned background candidates are
//! published only through the common checked Preview/Undo transaction.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::{CancellationToken, TaskError, TaskHandle, TaskManager};
use rustpy_format::{
    candidate::{external_command_output, FormatterLimits},
    FormatOptions,
};
use rustpy_refactor::refactor2::FileOp;
use rustpy_semantic::{module::ModuleResolver, FileInfo};
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    File,
    Selection,
    Directory,
}
#[derive(Clone, Debug)]
struct Choices {
    scope: Scope,
    directory: PathBuf,
    masks: String,
    reformat: bool,
    optimize: bool,
    rearrange: bool,
    cleanup: bool,
    vcs: bool,
    keep_breaks: bool,
}
pub(super) struct ReformatState {
    path: PathBuf,
    selection: Option<TextRange>,
    document: rustpy_text::Document,
    version: u64,
    choices: Choices,
    silent: bool,
    error: Option<String>,
}
struct Input {
    path: PathBuf,
    text: String,
    options: FormatOptions,
    range: Option<TextRange>,
    changed: Option<Vec<(usize, usize)>>,
    command: Option<Command>,
    markdown: bool,
    ending: Option<rustpy_text::LineEnding>,
    editorconfig: Option<Arc<rustpy_settings::editorconfig::EditorConfigProps>>,
}
struct Guard {
    path: PathBuf,
    text: String,
    document: Option<(rustpy_text::Document, u64)>,
    revision: Option<rustpy_core::vfs::ContentRevision>,
}
struct Output {
    operations: Vec<FileOp>,
    lines: usize,
    files: usize,
    bytes: usize,
}
pub(super) struct Job {
    handle: TaskHandle<Output>,
    guards: Vec<Guard>,
    project: Arc<Analysis>,
    attached: bool,
    interpreter: Option<PathBuf>,
    style: rustpy_settings::CodeStyleSettings,
    editor: rustpy_settings::EditorSettings,
    external: bool,
    epoch_path: PathBuf,
    epoch: u64,
    silent: bool,
    label: String,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.handle.cancel();
    }
}
fn task_error(error: impl std::fmt::Display) -> TaskError {
    TaskError::Failed(error.to_string())
}
fn workflow_label(choices: &Choices) -> &'static str {
    if choices.cleanup {
        "Code Cleanup"
    } else if choices.rearrange && !choices.reformat && !choices.optimize {
        "Rearrange Code"
    } else if choices.optimize && !choices.reformat && !choices.rearrange {
        "Optimize Imports"
    } else {
        "Reformat Code"
    }
}
fn unused_import_cleanup_enabled(options: &InspectionOptions, path: &Path) -> bool {
    options.unused_imports
        && options
            .policy
            .as_ref()
            .is_none_or(|policy| policy.enabled(path, "PyUnusedImports"))
}
fn apply(text: &str, edits: &[(TextRange, String)]) -> Result<String, String> {
    rustpy_semantic::edit_helpers::try_apply_edits(text, edits).map_err(|e| e.to_string())
}
fn whole(text: &str) -> TextRange {
    TextRange::new(TextSize::new(0), TextSize::new(text.len() as u32))
}
fn selection_lines(text: &str, range: TextRange) -> Vec<(usize, usize)> {
    let start = text[..usize::from(range.start())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let end = text[..usize::from(range.end())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    vec![(
        start,
        (end - start
            + usize::from(
                usize::from(range.end()) == text.len()
                    || !text[..usize::from(range.end())].ends_with('\n'),
            ))
        .max(1),
    )]
}
/// Offset-aware fenced blocks: formatting never rewrites Markdown prose,
/// unsupported languages, fence delimiters or a parser-normalized host slice.
pub(super) fn markdown_edits(
    text: &str,
    options: &FormatOptions,
    selection: Option<TextRange>,
) -> Result<Vec<(TextRange, String)>, String> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
    if text.len() > 1024 * 1024 {
        return Err("Markdown formatting source exceeds 1 MiB".into());
    }
    let mut active = false;
    let mut edits = Vec::new();
    let mut nodes = 0usize;
    for (event, range) in Parser::new(text).into_offset_iter() {
        nodes += 1;
        if nodes > 100_000 {
            return Err("Markdown formatting exceeds its syntax budget".into());
        }
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                active = matches!(
                    info.split_whitespace().next(),
                    Some("python" | "python3" | "py")
                );
            }
            Event::End(TagEnd::CodeBlock) => active = false,
            Event::Text(code) if active => {
                let source = text
                    .get(range.clone())
                    .ok_or("Invalid Markdown source range")?;
                if source != code.as_ref() {
                    continue;
                }
                let host = TextRange::new(
                    TextSize::new(range.start as u32),
                    TextSize::new(range.end as u32),
                );
                if selection.is_some_and(|selection| !selection.contains_range(host)) {
                    continue;
                }
                let formatted =
                    rustpy_format::format_source(source, options).map_err(|e| e.to_string())?;
                if formatted != source {
                    if formatted.len() > 8 * 1024 * 1024
                        || edits
                            .iter()
                            .map(|(_, text): &(TextRange, String)| text.len())
                            .sum::<usize>()
                            + formatted.len()
                            > 8 * 1024 * 1024
                    {
                        return Err("Formatted Markdown exceeds 8 MiB".into());
                    }
                    edits.push((host, formatted));
                }
            }
            _ => {}
        }
    }
    for (source_range, formatted) in crate::markdown_workspace::format_tables(text) {
        let host = TextRange::new(
            TextSize::new(source_range.start as u32),
            TextSize::new(source_range.end as u32),
        );
        if selection.is_none_or(|selected| selected.contains_range(host)) {
            edits.push((host, formatted));
        }
    }
    Ok(edits)
}
pub(super) fn options_for(
    settings: &rustpy_settings::EditorSettings,
    style: &rustpy_settings::CodeStyleSettings,
    path: &Path,
    text: &str,
    keep_breaks: bool,
) -> FormatOptions {
    let mut editor = settings.clone();
    style.apply_language(path, &mut editor);
    let mut state = EditorState::from_text(text, Language::Python);
    state.path = Some(path.into());
    let file = crate::editor::indent_options::file_edit_options(&editor, &state).0;
    let ec = editor.editorconfig_for(path);
    let mut options = FormatOptions::pep8();
    options.indent = file.indent_size.max(1);
    options.max_line_length = super::indent_status::formatter_margin(file.right_margin);
    options.continuation_indent = file.continuation_indent;
    options.tabs = file.use_tabs.then_some(rustpy_format::TabIndent {
        width: file.tab_size,
        smart: file.smart_tabs.unwrap_or(false),
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
    rustpy_format::style_policy::apply(&mut options, style, &ec);
    if !keep_breaks {
        options.keep_line_breaks = false;
    }
    options
}
fn candidate_file(analysis: &Analysis, path: &Path, text: &str) -> Result<Arc<FileInfo>, String> {
    let id = analysis.set_file_text(path, text);
    analysis
        .file(id)
        .ok_or_else(|| "Candidate source was evicted".into())
}
pub(super) fn cleanup_edits(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    options: &InspectionOptions,
) -> Vec<(TextRange, String)> {
    let mut options = options.clone();
    if let Some(policy) = options.policy.as_mut() {
        policy.batch = true;
    }
    let mut edits = Vec::<(TextRange, String)>::new();
    for problem in inspections::inspect(analysis, file, &options) {
        if !inspections::infrastructure::inspection(problem.diagnostic.code.as_ref())
            .is_some_and(|entry| entry.cleanup)
        {
            continue;
        }
        let Some(fix) = problem.primary_fix() else {
            continue;
        };
        if fix.edits.iter().any(|(range, _)| {
            edits.iter().any(|(old, _)| {
                old.start() == range.start()
                    || old
                        .intersect(*range)
                        .is_some_and(|intersection| !intersection.is_empty())
            })
        }) {
            continue;
        }
        edits.extend(fix.edits.iter().cloned());
    }
    edits
}
fn transform(
    analysis: &Analysis,
    input: &mut Input,
    choices: &Choices,
    _style: &rustpy_settings::CodeStyleSettings,
    inspection: &InspectionOptions,
    token: &CancellationToken,
) -> Result<String, String> {
    token.check().map_err(|e| e.to_string())?;
    let mut inspection = inspection.clone();
    inspection.editorconfig = input.editorconfig.clone();
    if input.markdown && !choices.reformat {
        return Ok(input.text.clone());
    }
    if input.markdown {
        let text = apply(
            &input.text,
            &markdown_edits(&input.text, &input.options, input.range)?,
        )?;
        return if let Some(lines) = &input.changed {
            restricted_hunks(&input.text, &text, lines, false)
        } else {
            Ok(text)
        };
    }
    let mut text = input.text.clone();
    if input.range.is_some() && (choices.cleanup || choices.optimize || choices.rearrange) {
        return Err(
            "Optimize/rearrange/cleanup require file or directory scope, not a partial selection"
                .into(),
        );
    }
    for round in 0..3 {
        if !choices.cleanup {
            break;
        }
        let file = candidate_file(analysis, &input.path, &text)?;
        let edits = policy_edits(
            &text,
            cleanup_edits(analysis, &file, &inspection),
            &input.options,
        );
        if edits.is_empty() {
            break;
        }
        text = apply(&text, &edits)?;
        token.check().map_err(|e| e.to_string())?;
        if round == 2 {
            break;
        }
    }
    if choices.optimize {
        let file = candidate_file(analysis, &input.path, &text)?;
        let edits = policy_edits(
            &text,
            rustpy_format::imports::optimize_imports_with_style(
                &file,
                &if unused_import_cleanup_enabled(&inspection, &file.path) {
                    inspections::unused_import_ranges_with(analysis, &file)
                } else {
                    Vec::new()
                },
                &input.options.python.imports,
            ),
            &input.options,
        );
        text = apply(&text, &edits)?;
    }
    if choices.rearrange {
        let mut arrangement = input.options.python.arrangement.clone();
        arrangement.enabled = true;
        let file = candidate_file(analysis, &input.path, &text)?;
        text = apply(
            &text,
            &policy_edits(
                &text,
                rustpy_format::python_style::arrange_with_db(analysis, &file, &arrangement),
                &input.options,
            ),
        )?;
    }
    token.check().map_err(|e| e.to_string())?;
    let formatted = if !choices.reformat {
        text
    } else if let Some(command) = input.command.take() {
        let bytes =
            external_command_output(&text, command, FormatterLimits::default(), false, token)
                .map_err(|e| e.to_string())?;
        let output = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        if output.is_empty() && !text.is_empty() {
            return Err("Formatter returned empty output".into());
        }
        let output = rustpy_text::LineEnding::normalize_to_lf(&output).into_owned();
        if !rustpy_syntax::parse(&output).errors().is_empty() {
            return Err("Formatter returned invalid Python".into());
        }
        if rustpy_format::semantic_fingerprint(&text)
            != rustpy_format::semantic_fingerprint(&output)
        {
            return Err("External formatter changed Python semantics".into());
        }
        output
    } else {
        let range = input.range.unwrap_or_else(|| whole(&text));
        apply(
            &text,
            &rustpy_format::format_range(&text, range, &input.options)
                .map_err(|e| e.to_string())?,
        )?
    };
    let mut output = formatted;
    if let Some(range) = input.range {
        output = rustpy_format::style_policy::changed_lines(
            &input.text,
            &output,
            &selection_lines(&input.text, range),
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(changed) = &input.changed {
        output = if choices.cleanup || choices.optimize || choices.rearrange {
            restricted_hunks(&input.text, &output, changed, true)?
        } else {
            rustpy_format::style_policy::changed_lines(&input.text, &output, changed)
                .map_err(|e| e.to_string())?
        };
    }
    Ok(output)
}

impl IdeApp {
    pub(super) fn silent_cleanup(&mut self) {
        let Some(index) = self.active.filter(|index| *index < self.tabs.len()) else {
            return;
        };
        if self.tabs[index].state.language != Language::Python {
            self.set_status("Code Cleanup is available for Python files");
            return;
        }
        if !self.refresh_file_permissions(index) || self.coordinated_quit_active() {
            return;
        }
        let Some(path) = self.tabs[index].state.path.clone() else {
            self.set_status("Save this buffer before Code Cleanup");
            return;
        };
        if self.code_style_excludes(&path).unwrap_or(true)
            || self
                .settings
                .editor
                .editorconfig_for(&path)
                .ide
                .formatter_enabled
                == Some(false)
        {
            self.set_status("File excluded by Code Style");
            return;
        }
        if self.selected_formatter() != "builtin" {
            self.request_cleanup();
            let Dialog::Reformat(mut state) = std::mem::replace(&mut self.dialog, Dialog::None)
            else {
                return;
            };
            state.silent = true;
            if let Err(error) = self.start_reformat_workflow(&state) {
                self.set_status(format!("Cleanup not applied: {error}"));
            }
            return;
        }
        self.sync_open_tabs_now();
        let Some(project) = self.analysis_handle() else {
            return;
        };
        let resolver = ModuleResolver::new(
            project.resolver.roots.clone(),
            project.resolver.search_paths.clone(),
            project.resolver.python_version,
        )
        .with_stub_roots(project.resolver.stub_roots.clone())
        .with_platform(&project.resolver.platform);
        let analysis = Analysis::new(resolver);
        analysis.set_analyze_doc_comments(project.analyze_doc_comments());
        let mut captured = 0usize;
        for file in project.open_files() {
            captured = captured.saturating_add(file.text.len());
            if captured > 32 * 1024 * 1024 {
                self.set_status("Open source capture exceeds 32 MiB");
                return;
            }
            analysis.set_file_text(&file.path, &file.text);
        }
        let original = self.tabs[index].state.doc.text();
        if original.len() > 1024 * 1024 {
            self.set_status("Cleanup source exceeds 1 MiB");
            return;
        }
        let style = self.settings.code_style.clone();
        let mut input = Input {
            path: path.clone(),
            text: original.clone(),
            options: options_for(
                &self.settings.editor,
                &style,
                &path,
                &original,
                style.keep_line_breaks,
            ),
            range: None,
            changed: None,
            command: None,
            markdown: false,
            ending: None,
            editorconfig: Some(Arc::new(self.settings.editor.editorconfig_for(&path))),
        };
        let choices = Choices {
            scope: Scope::File,
            directory: path.parent().unwrap_or(Path::new(".")).into(),
            masks: "*.py;*.pyi;*.pyw".into(),
            reformat: true,
            optimize: true,
            rearrange: false,
            cleanup: true,
            vcs: false,
            keep_breaks: style.keep_line_breaks,
        };
        let mut inspections = self.analysis_inputs().inspections;
        if self
            .analysis_inputs()
            .disabled_inspections
            .iter()
            .any(|id| id == "PyUnusedImports")
        {
            inspections.unused_imports = false;
        }
        match transform(
            &analysis,
            &mut input,
            &choices,
            &style,
            &inspections,
            &CancellationToken::new(),
        ) {
            Ok(output) => {
                let edits = document_markers::refine_formatter_edits(
                    &self.tabs[index].state.doc,
                    &[(whole(&original), output)],
                )
                .into_iter()
                .map(|edit| (edit.range, edit.text))
                .collect::<Vec<_>>();
                if edits.is_empty() {
                    self.set_status("Code Cleanup: no changes");
                    return;
                }
                match self.apply_cleanup_candidate(index, edits, "Code Cleanup") {
                    Ok(()) => self.set_status("Code Cleanup applied"),
                    Err(error) => self.set_status(format!("Cleanup not applied: {error}")),
                }
            }
            Err(error) => self.set_status(format!("Cleanup not applied: {error}")),
        }
    }
    fn apply_cleanup_candidate(
        &mut self,
        index: usize,
        edits: Vec<(TextRange, String)>,
        label: &str,
    ) -> Result<(), String> {
        let before = self.tabs[index].state.doc.version();
        let token = self.tabs[index]
            .state
            .begin_transaction(label)
            .map_err(|e| e.to_string())?;
        self.snapshot(index, "Before Code Cleanup");
        self.tabs[index].state.apply_text_edits(edits, label);
        if self.tabs[index].state.doc.version() == before {
            let _ = self.tabs[index].state.rollback_transaction(token);
            return Err("The document rejected the cleanup edit".into());
        }
        if let Err(error) = self.tabs[index].state.commit_transaction(token) {
            let _ = self.tabs[index].state.rollback_transaction(token);
            return Err(error.to_string());
        }
        self.reconcile_document_markers();
        self.sync_analysis_now();
        Ok(())
    }
    pub(super) fn request_reformat(&mut self, force_dialog: bool) {
        if self.web_format() {
            return;
        }
        if !force_dialog && !self.settings.code_style.show_reformat_dialog {
            if self
                .active
                .is_some_and(|index| self.tabs[index].state.language == Language::Markdown)
            {
                self.reformat_markdown_now();
            } else {
                self.reformat_code();
            }
            return;
        }
        let Some(index) = self.active else { return };
        let tab = &self.tabs[index];
        let Some(path) = tab.state.path.clone() else {
            self.set_status("Save this buffer before opening the file/directory formatting dialog");
            return;
        };
        let caret = tab.state.primary();
        let selection = caret.has_selection().then(|| caret.range());
        let state = ReformatState {
            path: path.clone(),
            selection,
            document: tab.state.doc.share(),
            version: tab.state.doc.version(),
            choices: Choices {
                scope: if selection.is_some() {
                    Scope::Selection
                } else {
                    Scope::File
                },
                directory: path.parent().unwrap_or(Path::new(".")).into(),
                masks: self.settings.code_style.reformat_file_masks.clone(),
                reformat: true,
                optimize: false,
                rearrange: false,
                cleanup: false,
                vcs: self.settings.code_style.reformat_scope == "vcs",
                keep_breaks: self.settings.code_style.keep_line_breaks,
            },
            silent: false,
            error: None,
        };
        self.dialog = Dialog::Reformat(Box::new(state));
    }
    pub(super) fn request_cleanup(&mut self) {
        self.request_reformat(true);
        if let Dialog::Reformat(state) = &mut self.dialog {
            state.choices.scope = Scope::File;
            state.choices.cleanup = true;
            state.choices.optimize = true;
        }
    }
    pub(super) fn request_rearrange(&mut self) {
        self.request_reformat(true);
        if let Dialog::Reformat(state) = &mut self.dialog {
            state.choices.scope = Scope::File;
            state.choices.rearrange = true;
            state.choices.reformat = false;
        }
    }
    pub(super) fn reformat_directory_dialog(&mut self, path: PathBuf, action: Action) {
        // A directory route must not open every file or change the active editor.
        let index = self.active;
        if let Some(index) = index {
            let tab = &self.tabs[index];
            self.dialog = Dialog::Reformat(Box::new(ReformatState {
                path: path.clone(),
                selection: None,
                document: tab.state.doc.share(),
                version: tab.state.doc.version(),
                choices: Choices {
                    scope: Scope::Directory,
                    directory: path,
                    masks: self.settings.code_style.reformat_file_masks.clone(),
                    reformat: action != Action::OptimizeImports && action != Action::RearrangeCode,
                    optimize: matches!(action, Action::OptimizeImports | Action::CodeCleanup),
                    rearrange: action == Action::RearrangeCode,
                    cleanup: action == Action::CodeCleanup,
                    vcs: self.settings.code_style.reformat_scope == "vcs",
                    keep_breaks: self.settings.code_style.keep_line_breaks,
                },
                silent: false,
                error: None,
            }));
        } else {
            // Directory formatting does not require an editor tab. A detached
            // document is only a dialog lifetime token; no buffer is published.
            self.dialog = Dialog::Reformat(Box::new(ReformatState {
                path: path.clone(),
                selection: None,
                document: Document::new(""),
                version: 0,
                choices: Choices {
                    scope: Scope::Directory,
                    directory: path,
                    masks: self.settings.code_style.reformat_file_masks.clone(),
                    reformat: action != Action::OptimizeImports && action != Action::RearrangeCode,
                    optimize: matches!(action, Action::OptimizeImports | Action::CodeCleanup),
                    rearrange: action == Action::RearrangeCode,
                    cleanup: action == Action::CodeCleanup,
                    vcs: self.settings.code_style.reformat_scope == "vcs",
                    keep_breaks: self.settings.code_style.keep_line_breaks,
                },
                silent: false,
                error: None,
            }));
        }
    }
    pub(super) fn draw_reformat_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<ReformatState>,
    ) {
        let (mut open, mut go, mut cancel) = (true, false, false);
        egui::Window::new("Reformat File / Directory").default_width(580.0).open(&mut open).show_modal(ctx,|ui|{
            ui.label(state.path.display().to_string());
            ui.horizontal(|ui|{
                ui.selectable_value(&mut state.choices.scope,Scope::File,"Whole file");
                ui.add_enabled_ui(state.selection.is_some(),|ui|{ui.selectable_value(&mut state.choices.scope,Scope::Selection,"Selection");});
                ui.selectable_value(&mut state.choices.scope,Scope::Directory,"Directory");
            });
            if state.choices.scope==Scope::Directory{let mut path=state.choices.directory.to_string_lossy().to_string();ui.label("Directory (recursive, project files)");if ui.text_edit_singleline(&mut path).changed(){state.choices.directory=path.into();}}
            ui.horizontal(|ui|{ui.label("File masks");ui.text_edit_singleline(&mut state.choices.masks);});
            ui.checkbox(&mut state.choices.reformat,"Reformat whitespace and wrapping");
            let file_scope=state.choices.scope!=Scope::Selection;
            ui.add_enabled_ui(file_scope,|ui|{
                ui.checkbox(&mut state.choices.optimize,"Optimize imports");ui.checkbox(&mut state.choices.rearrange,"Rearrange code");ui.checkbox(&mut state.choices.cleanup,"Code cleanup (safe inspection fixes)");
            });
            if !file_scope{state.choices.optimize=false;state.choices.rearrange=false;state.choices.cleanup=false;}
            ui.checkbox(&mut state.choices.vcs,"Only VCS changed text (HEAD)");
            let mut discard=!state.choices.keep_breaks;if ui.checkbox(&mut discard,"Do not keep line breaks").changed(){state.choices.keep_breaks = !discard;}
            ui.checkbox(&mut self.settings.code_style.show_reformat_dialog,"Show Reformat File dialog for Ctrl+Alt+L");
            ui.collapsing("Actions on Save",|ui|{
                ui.checkbox(&mut self.settings.code_style.format_on_save,"Reformat code");
                ui.checkbox(&mut self.settings.code_style.format_whole_file_on_save,"Whole file, including inactive editors (ignore selection on save)");ui.checkbox(&mut self.settings.code_style.optimize_imports_on_save,"Optimize imports");ui.checkbox(&mut self.settings.code_style.rearrange_on_save,"Rearrange code");ui.checkbox(&mut self.settings.code_style.cleanup_on_save,"Code cleanup");
                ui.label(format!("Formatter: {} (configured tool, trigger and masks are respected)",self.selected_formatter()));
                ui.label("EditorConfig and file-indent detection remain authoritative.");
            });
            if let Some(error)=&state.error{ui.colored_label(self.theme.error,error);}
            ui.label("Changes are prepared in the background and shown in the common checked preview. Cancel in Background Tasks; no file is rewritten by a formatter process.");
            ui.horizontal(|ui|{go=ui.button("Preview Changes").clicked();cancel=ui.button("Cancel").clicked()||ui.input(|i|i.key_pressed(egui::Key::Escape));});
        });
        if go {
            if state.choices.scope != Scope::Directory
                && self.tab_of(&state.path).is_none_or(|index| {
                    self.tabs[index].state.doc.identity() != state.document.identity()
                        || state.document.version() != state.version
                })
            {
                state.error = Some("The source editor changed; reopen Reformat File".into());
                self.dialog = Dialog::Reformat(state);
                return;
            }
            self.settings.code_style.reformat_file_masks = state.choices.masks.clone();
            if let Err(error) = self.start_reformat_workflow(&state) {
                state.error = Some(error);
                self.dialog = Dialog::Reformat(state);
            }
        } else if open && !cancel {
            self.dialog = Dialog::Reformat(state);
        } else {
            self.focus_editor = true;
        }
    }
    fn start_reformat_workflow(&mut self, state: &ReformatState) -> Result<(), String> {
        if self.coordinated_quit_active() {
            return Err("Finish the pending quit before formatting".into());
        }
        self.sync_open_tabs_now();
        let attached = self.project.is_some();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| state.path.parent().unwrap_or(Path::new(".")).to_owned());
        let project = self
            .analysis_handle()
            .unwrap_or_else(|| Arc::new(Analysis::for_project(&root)));
        let mut paths = if state.choices.scope == Scope::Directory {
            let directory = rustpy_core::vfs::normalize_path(&state.choices.directory);
            let scope = self
                .project_scope()
                .ok_or("Open a project for indexed directory formatting")?;
            if !scope.contains(&directory) || !directory.is_dir() {
                return Err("Choose a directory inside the project/content roots".into());
            }
            if self.project_index.files.is_building() || self.project_index.structure_changed {
                return Err("Wait for the project file index to finish rebuilding".into());
            }
            let files = self
                .project_index
                .files
                .files_for(&scope)
                .ok_or("Wait for the project file index before directory reformatting")?;
            files
                .iter()
                .filter(|path| path.starts_with(&directory))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            vec![state.path.clone()]
        };
        paths.sort();
        paths.dedup();
        paths.retain(|path| {
            super::formatter_tools::matches_masks(&state.choices.masks, path, Some(&root))
                && matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("py" | "pyi" | "pyw" | "md" | "markdown")
                )
        });
        if paths.is_empty() {
            return Err(
                "No indexed files match this scope/mask; wait for indexing or choose another scope"
                    .into(),
            );
        }
        if paths.len() > 128 {
            return Err(
                "Choose a smaller scope (at most 128 files per undoable transaction)".into(),
            );
        }
        let mut inputs = Vec::new();
        let mut guards = Vec::new();
        let mut total = 0usize;
        let style = self.settings.code_style.clone();
        let tool = self.selected_formatter().to_owned();
        for path in paths {
            if self.code_style_excludes(&path)?
                || self
                    .settings
                    .editor
                    .editorconfig_for(&path)
                    .ide
                    .formatter_enabled
                    == Some(false)
            {
                continue;
            }
            if path.ancestors().any(|path| {
                std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
            }) {
                return Err("Symlinked formatting targets are unsupported".into());
            }
            let tab = self.tab_of(&path);
            let (text, document, revision) = if let Some(index) = tab {
                if self.tabs[index].state.doc.is_read_only() {
                    return Err(format!("{} is read-only", path.display()));
                }
                let doc = &self.tabs[index].state.doc;
                (doc.text(), Some((doc.share(), doc.version())), None)
            } else {
                let snapshot = project
                    .vfs
                    .read(
                        &path,
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 1024 * 1024,
                            max_cached_bytes: 32 * 1024 * 1024,
                            max_cached_files: 256,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                let (text, _) =
                    rustpy_text::Encoding::decode(&snapshot.bytes).map_err(|e| e.to_string())?;
                (text, None, Some(snapshot.revision))
            };
            total = total
                .checked_add(text.len())
                .ok_or("Formatting size overflow")?;
            if text.len() > 1024 * 1024 || total > 16 * 1024 * 1024 {
                return Err("Formatting scope exceeds 1 MiB/file or 16 MiB total".into());
            }
            if std::fs::metadata(&path).is_ok_and(|m| m.permissions().readonly()) {
                return Err(format!("{} is read-only", path.display()));
            }
            let markdown = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("md" | "markdown")
            );
            let options = options_for(
                &self.settings.editor,
                &style,
                &path,
                &text,
                state.choices.keep_breaks,
            );
            let range = (state.choices.scope == Scope::Selection)
                .then_some(state.selection)
                .flatten();
            let changed = if state.choices.vcs {
                let git = self
                    .project
                    .as_ref()
                    .and_then(|p| p.git.as_ref())
                    .ok_or("VCS changed text requires a Git repository")?;
                Some(
                    git.line_changes(&path, &text)
                        .iter()
                        .map(|change| (change.start, change.count))
                        .collect(),
                )
            } else if style.reformat_scope == "changed" {
                let snapshot = project
                    .vfs
                    .read(&path, rustpy_core::vfs::ReadLimits::default())
                    .map_err(|e| e.to_string())?;
                let (saved, _) =
                    rustpy_text::Encoding::decode(&snapshot.bytes).map_err(|e| e.to_string())?;
                Some(
                    rustpy_vcs::line_changes(
                        &rustpy_text::LineEnding::normalize_to_lf(&saved),
                        &text,
                    )
                    .iter()
                    .map(|change| (change.start, change.count))
                    .collect(),
                )
            } else {
                None
            };
            let command = if state.choices.reformat && tool != "builtin" && !markdown {
                let config = style
                    .external_formatters
                    .iter()
                    .find(|config| config.tool == tool);
                if config.is_some_and(|config| {
                    !config.on_reformat
                        || !super::formatter_tools::matches_masks(
                            &config.file_masks,
                            &path,
                            Some(&root),
                        )
                }) {
                    continue;
                }
                if style.python != rustpy_settings::python_style::PythonStyle::default()
                    || options.formatter_tags.is_some()
                    || options.keep_simple_blocks
                    || options.keep_first_column_comments
                    || options.keep_line_breaks
                {
                    return Err("Protected/keep policy requires the builtin formatter".into());
                }
                Some(
                    self.external_formatter_command_for_path(Some(&path), &tool, false)
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let ending = if document.is_none() {
                let (ending, mixed) = rustpy_text::LineEnding::detect(&text);
                if mixed {
                    return Err(format!("{} has mixed line endings; open and normalize it before batch reformatting",path.display()));
                }
                Some(ending)
            } else {
                None
            };
            guards.push(Guard {
                path: path.clone(),
                text: text.clone(),
                document,
                revision,
            });
            inputs.push(Input {
                path: path.clone(),
                text,
                options,
                range,
                changed,
                command,
                markdown,
                ending,
                editorconfig: Some(Arc::new(self.settings.editor.editorconfig_for(&path))),
            });
        }
        if inputs.is_empty() {
            return Err("Every matching file is excluded by style/formatter policy".into());
        }
        let mut captured = Vec::new();
        let mut bytes = 0usize;
        for file in project.open_files() {
            bytes += file.text.len();
            if bytes > 32 * 1024 * 1024 {
                return Err("Open source capture exceeds 32 MiB".into());
            }
            captured.push((file.path.clone(), file.text.clone()));
        }
        let resolver = ModuleResolver::new(
            project.resolver.roots.clone(),
            project.resolver.search_paths.clone(),
            project.resolver.python_version,
        )
        .with_stub_roots(project.resolver.stub_roots.clone())
        .with_platform(&project.resolver.platform);
        let settings = style.clone();
        let analyze_doc_comments = project.analyze_doc_comments();
        let label = workflow_label(&state.choices).to_string();
        let choices = state.choices.clone();
        let inspections = self.analysis_inputs().inspections;
        if let Some(old) = self.bg.reformat.take() {
            drop(old);
        }
        drop(self.bg.external_formatter.take());
        let external = inputs.iter().any(|input| input.command.is_some());
        let epoch_path = root.clone();
        let epoch = project.revision_excluding(&epoch_path);
        let handle = TaskManager::global()
            .task("Reformat code")
            .spawn(move |progress| {
                let analysis = Analysis::new(resolver);
                analysis.set_analyze_doc_comments(analyze_doc_comments);
                for (path, text) in captured {
                    analysis.set_file_text(&path, &text);
                }
                let mut output = Output {
                    operations: Vec::new(),
                    lines: 0,
                    files: 0,
                    bytes: 0,
                };
                let count = inputs.len();
                for (index, mut input) in inputs.into_iter().enumerate() {
                    progress.check()?;
                    progress.set_step(index, count);
                    progress.set_details(input.path.display().to_string());
                    let text = transform(
                        &analysis,
                        &mut input,
                        &choices,
                        &settings,
                        &inspections,
                        progress.token(),
                    )
                    .map_err(task_error)?;
                    let text = if let Some(ending) = input.ending {
                        rustpy_text::LineEnding::normalize_to_lf(&text)
                            .replace('\n', ending.as_str())
                    } else {
                        text
                    };
                    if text.len() > 8 * 1024 * 1024 {
                        return Err(task_error("Formatted file exceeds 8 MiB"));
                    }
                    output.bytes = output
                        .bytes
                        .checked_add(text.len())
                        .ok_or_else(|| task_error("Formatted size overflow"))?;
                    if output.bytes > 32 * 1024 * 1024 {
                        return Err(task_error("Formatted scope exceeds 32 MiB"));
                    }
                    if text != input.text {
                        output.lines +=
                            crate::editor::save_actions::changed_lines(&input.text, &text)
                                .len()
                                .max(1);
                        output.files += 1;
                        let edits = if !input.text.contains('\r') {
                            let document = Document::new(&input.text);
                            document_markers::refine_formatter_edits(
                                &document,
                                &[(whole(&input.text), text)],
                            )
                            .into_iter()
                            .map(|edit| (edit.range, edit.text))
                            .collect()
                        } else {
                            vec![(whole(&input.text), text)]
                        };
                        output.operations.push(FileOp::Edit(input.path, edits));
                    }
                }
                progress.check()?;
                Ok(output)
            });
        self.bg.reformat = Some(Job {
            handle,
            guards,
            project,
            attached,
            interpreter: self
                .project
                .as_ref()
                .and_then(|p| p.interpreter.as_ref())
                .map(|i| i.path.clone()),
            style,
            editor: self.settings.editor.clone(),
            external,
            epoch_path,
            epoch,
            silent: state.silent,
            label,
        });
        self.set_status("Preparing reformat; cancel in Background Tasks");
        Ok(())
    }
    pub(super) fn poll_reformat_workflow(&mut self) {
        let stale = self.bg.reformat.as_ref().is_some_and(|job| {
            !(if job.attached {
                self.project.as_ref().is_some_and(|project| {
                    Arc::ptr_eq(&project.analysis, &job.project)
                        && project
                            .interpreter
                            .as_ref()
                            .map(|interpreter| interpreter.path.clone())
                            == job.interpreter
                })
            } else {
                self.project.is_none()
            }) || job.project.revision_excluding(&job.epoch_path) != job.epoch
                || self.settings.code_style != job.style
                || self.settings.editor != job.editor
                || (job.external && self.safe_mode)
                || self.coordinated_quit_active()
                || job.guards.iter().any(|guard| {
                    guard.document.as_ref().is_some_and(|(document, version)| {
                        self.tab_of(&guard.path).is_none_or(|index| {
                            self.tabs[index].state.doc.identity() != document.identity()
                                || document.version() != *version
                        })
                    })
                })
        });
        if stale {
            drop(self.bg.reformat.take());
            self.set_status("Cancelled stale reformat context");
            return;
        }
        let Some(job) = self.bg.reformat.as_mut() else {
            return;
        };
        let Some(result) = job.handle.try_result() else {
            return;
        };
        let job = self.bg.reformat.take().unwrap();
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                self.set_status(format!("Reformat not applied: {error}"));
                return;
            }
        };
        let current = if job.attached {
            self.project.as_ref().is_some_and(|p| {
                Arc::ptr_eq(&p.analysis, &job.project)
                    && p.interpreter.as_ref().map(|i| i.path.clone()) == job.interpreter
            })
        } else {
            self.project.is_none()
        };
        if !current
            || self.settings.code_style != job.style
            || self.settings.editor != job.editor
            || self.coordinated_quit_active()
        {
            self.set_status("Discarded stale reformat context");
            return;
        }
        for guard in &job.guards {
            if let Some((document, version)) = &guard.document {
                if self.tab_of(&guard.path).is_none_or(|index| {
                    self.tabs[index].state.doc.identity() != document.identity()
                        || document.version() != *version
                        || document.text() != guard.text
                }) {
                    self.set_status("An editor changed; reformat was not applied");
                    return;
                }
            } else if self.tab_of(&guard.path).is_some()
                || job
                    .project
                    .vfs
                    .read(&guard.path, rustpy_core::vfs::ReadLimits::default())
                    .map_or(true, |snapshot| Some(snapshot.revision) != guard.revision)
            {
                self.set_status("A closed file changed; reformat was not applied");
                return;
            }
        }
        if output.operations.is_empty() {
            self.set_status(format!("{}: no changes", job.label));
            return;
        }
        if job.silent {
            let [FileOp::Edit(path, edits)] = output.operations.as_slice() else {
                self.set_status("Silent cleanup requires one editor document");
                return;
            };
            let Some(index) = self.tab_of(path) else {
                return;
            };
            if !self.refresh_file_permissions(index) {
                return;
            }
            if let Err(error) = self.apply_cleanup_candidate(index, edits.clone(), &job.label) {
                self.set_status(format!("Cleanup not applied: {error}"));
            } else {
                self.set_status(format!("{}: {} lines changed", job.label, output.lines));
            }
            return;
        }
        if !matches!(self.dialog, Dialog::None) {
            self.set_status(
                "Formatting finished while another dialog was open; rerun to review it",
            );
            return;
        }
        let title = format!(
            "{} — {} lines changed in {} files",
            job.label, output.lines, output.files
        );
        self.preview_file_ops(output.operations, &title);
        self.editor_format_notification("Reformat preview prepared");
        self.set_status(format!(
            "Prepared {} changed lines; review before applying",
            output.lines
        ));
    }

    fn reformat_markdown_now(&mut self) {
        let Some(index) = self.active else { return };
        if !self.refresh_file_permissions(index) {
            return;
        }
        let state = &self.tabs[index].state;
        let text = state.doc.text();
        let path = state.path.as_deref().unwrap_or(Path::new("untitled.md"));
        if self.code_style_excludes(path).unwrap_or(true)
            || self
                .settings
                .editor
                .editorconfig_for(path)
                .ide
                .formatter_enabled
                == Some(false)
        {
            self.set_status("File excluded by Code Style");
            return;
        }
        let options = options_for(
            &self.settings.editor,
            &self.settings.code_style,
            path,
            &text,
            self.settings.code_style.keep_line_breaks,
        );
        let selection = state
            .primary()
            .has_selection()
            .then(|| state.primary().range());
        match markdown_edits(&text, &options, selection) {
            Ok(edits) => {
                let lines = edits
                    .iter()
                    .map(|(_, text)| text.lines().count())
                    .sum::<usize>();
                self.tabs[index]
                    .state
                    .apply_text_edits(edits, "Reformat Markdown code blocks");
                self.set_status(format!("{lines} lines reformatted in Markdown code blocks"));
            }
            Err(error) => self.set_status(error),
        }
    }
}

/// Extra transformations never cross a formatter-control region. A compound
/// import/class rewrite overlapping one is retained as a whole, not split.
pub(super) fn policy_edits(
    text: &str,
    edits: Vec<(TextRange, String)>,
    options: &FormatOptions,
) -> Vec<(TextRange, String)> {
    let Some(tags) = &options.formatter_tags else {
        return edits;
    };
    let mut start = None;
    let mut protected = Vec::new();
    for token in rustpy_syntax::tokenize(text).0 {
        if token.kind != rustpy_syntax::SyntaxKind::COMMENT {
            continue;
        }
        let comment = &text[token.range];
        if start.is_none() && !tags.off.is_empty() && comment.contains(&tags.off) {
            start = Some(token.range.start());
        } else if start.is_some() && !tags.on.is_empty() && comment.contains(&tags.on) {
            protected.push(TextRange::new(start.take().unwrap(), token.range.end()));
        }
    }
    if let Some(start) = start {
        protected.push(TextRange::new(start, TextSize::new(text.len() as u32)));
    }
    edits
        .into_iter()
        .filter(|(range, _)| {
            !protected.iter().any(|protected| {
                range.intersect(*protected).is_some_and(|r| !r.is_empty())
                    || (range.is_empty() && protected.contains_inclusive(range.start()))
            })
        })
        .collect()
}
/// Cleanup/import changes deliberately change the syntax fingerprint. Retain
/// complete zero-context diff hunks in HEAD-selected lines and revalidate syntax,
/// rather than pretending those actions are whitespace-only formatting.
fn restricted_hunks(
    source: &str,
    output: &str,
    lines: &[(usize, usize)],
    python: bool,
) -> Result<String, String> {
    let old: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .chain(std::iter::once(source.len()))
        .collect();
    let new: Vec<_> = std::iter::once(0)
        .chain(output.match_indices('\n').map(|(i, _)| i + 1))
        .chain(std::iter::once(output.len()))
        .collect();
    let old_lines: Vec<_> = source.split_inclusive('\n').collect();
    let new_lines: Vec<_> = output.split_inclusive('\n').collect();
    let diff = similar::TextDiff::from_lines(source, output);
    let mut edits = Vec::new();
    let mut push = |from: usize, end: usize, next: usize, next_end: usize| -> Result<(), String> {
        let allowed = (from..end.max(from + 1)).all(|line| {
            lines
                .iter()
                .any(|(start, count)| line >= *start && line < start.saturating_add(*count))
        });
        if allowed {
            edits.push((
                TextRange::new(
                    TextSize::new(*old.get(from).ok_or("Invalid diff start")? as u32),
                    TextSize::new(*old.get(end).ok_or("Invalid diff end")? as u32),
                ),
                output[*new.get(next).ok_or("Invalid result start")?
                    ..*new.get(next_end).ok_or("Invalid result end")?]
                    .to_owned(),
            ));
        }
        Ok(())
    };
    for group in diff.grouped_ops(0) {
        let Some(first) = group.first() else { continue };
        let Some(last) = group.last() else { continue };
        let (mut from, mut end, mut next, mut next_end) = (
            first.old_range().start,
            last.old_range().end,
            first.new_range().start,
            last.new_range().end,
        );
        // Peel only token-identical line pairs. Structural edits (import
        // removals/reorders or cleanup) remain one indivisible middle hunk.
        if python {
            while from < end
                && next < next_end
                && rustpy_format::significant_tokens(old_lines[from])
                    == rustpy_format::significant_tokens(new_lines[next])
            {
                push(from, from + 1, next, next + 1)?;
                from += 1;
                next += 1;
            }
            while from < end
                && next < next_end
                && rustpy_format::significant_tokens(old_lines[end - 1])
                    == rustpy_format::significant_tokens(new_lines[next_end - 1])
            {
                push(end - 1, end, next_end - 1, next_end)?;
                end -= 1;
                next_end -= 1;
            }
        }
        if from != end || next != next_end {
            push(from, end, next, next_end)?;
        }
    }
    let result = apply(source, &edits)?;
    if python && !rustpy_syntax::parse(&result).errors().is_empty() {
        return Err(
            "Changed-text transformations would split a Python construct; source left unchanged"
                .into(),
        );
    }
    Ok(result)
}
/// Token-preserving JSON layout: numbers, escapes and duplicate object keys are
/// never round-tripped through a lossy value serializer.
fn json_layout(text: &str) -> Result<String, String> {
    if text.len() > 128 * 1024 {
        return Err("Injected JSON exceeds 128 KiB".into());
    }
    let _: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let chars: Vec<_> = text.chars().collect();
    let mut output = String::new();
    let (mut depth, mut string, mut escape) = (0usize, false, false);
    for (index, ch) in chars.iter().copied().enumerate() {
        if output.len() > 1024 * 1024 {
            return Err("Formatted injected JSON exceeds 1 MiB".into());
        }
        if string {
            output.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                string = true;
                output.push(ch);
            }
            '{' | '[' => {
                output.push(ch);
                depth += 1;
                if depth > 64 {
                    return Err("Injected JSON nesting exceeds 64".into());
                }
                let empty = chars[index + 1..]
                    .iter()
                    .find(|ch| !ch.is_whitespace())
                    .is_some_and(|next| (*next == '}' && ch == '{') || (*next == ']' && ch == '['));
                if !empty {
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                let last = output.trim_end().chars().last();
                if !matches!(last, Some('{' | '[')) {
                    output.truncate(output.trim_end().len());
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
                output.push(ch);
            }
            ',' => {
                output.push_str(",\n");
                output.push_str(&"  ".repeat(depth));
            }
            ':' => output.push_str(": "),
            ch if ch.is_whitespace() => {}
            _ => output.push(ch),
        }
    }
    if text.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}
pub(super) fn format_fragment_editor(editor: &mut EditorState) -> Result<usize, String> {
    if editor.doc.is_read_only() {
        return Err("Injected host is read-only".into());
    }
    let text = editor.doc.text();
    let output = match editor.language {
        Language::Sql | Language::Html | Language::Yaml => {
            use rustpy_format::fragments::{self, Language as FragmentLanguage};
            let language = match editor.language {
                Language::Sql => FragmentLanguage::Sql,
                Language::Html => FragmentLanguage::Html,
                _ => FragmentLanguage::Yaml,
            };
            fragments::format(language, &text, fragments::Options::default())
                .map_err(|error| error.to_string())?
        }
        Language::Json => json_layout(&text)?,
        Language::Markdown => apply(&text, &markdown_edits(&text, &FormatOptions::pep8(), None)?)?,
        Language::Python => rustpy_format::format_source(&text, &FormatOptions::pep8())
            .map_err(|e| e.to_string())?,
        _ => {
            return Err(
                "No safe formatter is registered for this injected language; fragment unchanged"
                    .into(),
            )
        }
    };
    let lines = crate::editor::save_actions::changed_lines(&text, &output).len();
    editor.apply_text_edits(
        if output == text {
            Vec::new()
        } else {
            vec![(whole(&text), output)]
        },
        "Reformat injected fragment",
    );
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_sql_html_yaml_format_and_undo_without_touching_host() {
        for (language, source) in [
            (Language::Sql, "select a,b from table_name where a=1"),
            (Language::Html, "<div  title = 'hello'> text </div>"),
            (Language::Yaml, "key: [ 1 , 2 ]\n"),
        ] {
            let mut editor = EditorState::from_text(source, language);
            assert!(format_fragment_editor(&mut editor).unwrap() > 0);
            assert_ne!(editor.doc.text(), source);
            assert!(editor.doc.undo().is_some());
            assert_eq!(editor.doc.text(), source);
        }
    }
    fn choices() -> Choices {
        Choices {
            scope: Scope::File,
            directory: PathBuf::from("/project"),
            masks: "*.py".into(),
            reformat: true,
            optimize: false,
            rearrange: false,
            cleanup: false,
            vcs: false,
            keep_breaks: false,
        }
    }
    fn input(text: &str) -> Input {
        Input {
            path: PathBuf::from("/project/code.py"),
            text: text.into(),
            options: FormatOptions::pep8(),
            range: None,
            changed: None,
            command: None,
            markdown: false,
            ending: None,
            editorconfig: None,
        }
    }
    fn run(input: &mut Input, choices: &Choices) -> Result<String, String> {
        let analysis = Analysis::for_project(Path::new("/project"));
        transform(
            &analysis,
            input,
            choices,
            &Default::default(),
            &InspectionOptions::default(),
            &CancellationToken::new(),
        )
    }
    #[test]
    fn selection_and_vcs_ranges_preserve_unselected_source() {
        let mut selected = input("first=1\nsecond=2\n");
        selected.range = Some(TextRange::new(TextSize::new(0), TextSize::new(8)));
        assert_eq!(
            run(&mut selected, &choices()).unwrap(),
            "first = 1\nsecond=2\n"
        );
        let mut changed = input("first=1\nsecond=2\n");
        changed.changed = Some(vec![(1, 1)]);
        assert_eq!(
            run(&mut changed, &choices()).unwrap(),
            "first=1\nsecond = 2\n"
        );
    }
    #[test]
    fn cleanup_import_hunks_are_restricted_without_whitespace_equivalence_fiction() {
        let before = "import unused\nvalue=1\nuntouched=2\n";
        let after = "value = 1\nuntouched = 2\n";
        let output = restricted_hunks(before, after, &[(0, 2)], true).unwrap();
        assert!(!output.contains("import unused"), "{output}");
        assert!(output.contains("untouched=2"), "{output}");
        assert!(rustpy_syntax::parse(&output).errors().is_empty());
    }
    #[test]
    fn control_markers_protect_cleanup_rearrangement_and_import_ranges() {
        let text = "# @formatter:off\nkeep= 1\n# @formatter:on\nchange=2\n";
        let keep = text.find("keep").unwrap();
        let change = text.find("change").unwrap();
        let options = FormatOptions {
            formatter_tags: Some(rustpy_format::FormatterTags {
                off: "@formatter:off".into(),
                on: "@formatter:on".into(),
            }),
            ..FormatOptions::pep8()
        };
        let edits = policy_edits(
            text,
            vec![
                (
                    TextRange::new(TextSize::new(keep as u32), TextSize::new((keep + 7) as u32)),
                    "keep = 1".into(),
                ),
                (
                    TextRange::new(
                        TextSize::new(change as u32),
                        TextSize::new((change + 8) as u32),
                    ),
                    "change = 2".into(),
                ),
            ],
            &options,
        );
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].0.start(), TextSize::new(change as u32));
        assert!(policy_edits(text, vec![(whole(text), "".into())], &options).is_empty());
    }
    #[test]
    fn fenced_python_formatting_keeps_prose_fences_and_other_languages_exact() {
        let text = "π prose\n```python\nvalue=1\n```\n\n```javascript\nlet x=1;\n```\nlast prose\n";
        let edits = markdown_edits(text, &FormatOptions::pep8(), None).unwrap();
        let output = apply(text, &edits).unwrap();
        assert_eq!(
            output,
            "π prose\n```python\nvalue = 1\n```\n\n```javascript\nlet x=1;\n```\nlast prose\n"
        );
        assert!(markdown_edits(
            text,
            &FormatOptions::pep8(),
            Some(TextRange::new(TextSize::new(0), TextSize::new(5)))
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn injected_json_preserves_number_tokens_duplicate_keys_and_single_undo() {
        let text = "{\"n\":1.234567890123456789,\"n\":2,\"s\":\"π\\n\"}";
        let mut editor = EditorState::from_text(text, Language::Json);
        let lines = format_fragment_editor(&mut editor).unwrap();
        assert!(lines > 0);
        let output = editor.doc.text();
        assert!(output.contains("1.234567890123456789"));
        assert_eq!(output.matches("\"n\"").count(), 2);
        assert!(output.contains("π\\n"));
        assert!(editor.undo());
        assert_eq!(editor.doc.text(), text);
    }
    #[test]
    fn cancelled_candidate_never_changes_captured_source() {
        let analysis = Analysis::for_project(Path::new("/project"));
        let mut source = input("value=1\n");
        let token = CancellationToken::new();
        token.cancel();
        assert!(transform(
            &analysis,
            &mut source,
            &choices(),
            &Default::default(),
            &InspectionOptions::default(),
            &token
        )
        .is_err());
        assert_eq!(source.text, "value=1\n");
        assert!(analysis.file_for_path(&source.path).is_none());
    }
    fn app(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        settings.editor.detect_indents = false;
        let mut app = IdeApp::new(settings, Some(dir.path().into()));
        app.open_file(&path);
        (dir, app, path)
    }
    #[test]
    fn actual_reformat_file_dialog_exposes_scopes_and_options_without_publishing() {
        let source = "first=1\nsecond=2\n";
        let (_dir, mut app, _) = app(source);
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::new(8),
        )]);
        app.request_reformat(true);
        let Dialog::Reformat(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("dialog missing")
        };
        assert_eq!(state.choices.scope, Scope::Selection);
        assert_eq!(state.document.text(), source);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut state = Some(state);
        let mut shapes = Vec::new();
        // Preserve the production state through the invisible Area sizing pass.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| app.draw_reformat_dialog(ui.ctx(), state.take().unwrap()),
            );
            shapes = std::mem::take(&mut output.shapes);
            // No renderer consumes this fixture's atlas updates.
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::Reformat(retained) = std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("reformat dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.choices.scope, Scope::Selection);
                assert_eq!(retained.document.text(), source);
                state = Some(retained);
            }
        }
        for label in [
            "Whole file",
            "Selection",
            "Directory",
            "Optimize imports",
            "Rearrange code",
            "Code cleanup",
            "Only VCS changed text",
            "Do not keep line breaks",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text)if text.galley.text().contains(label))),"{label}");
        }
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn rearrange_action_does_not_implicitly_reformat_method_bodies() {
        let source =
            "class C:\n    def _private(self): return  1\n    def public(self): return  2\n";
        let (_dir, mut app, _) = app(source);
        app.request_rearrange();
        let Dialog::Reformat(state) = &app.dialog else {
            panic!("arrangement scope dialog");
        };
        assert!(state.choices.rearrange);
        assert!(!state.choices.reformat);
        assert_eq!(workflow_label(&state.choices), "Rearrange Code");
        let mut candidate = input(source);
        let output = run(&mut candidate, &state.choices).unwrap();
        assert!(output.find("def public").unwrap() < output.find("def _private").unwrap());
        assert!(output.contains("return  1"));
        assert!(output.contains("return  2"));
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn silent_cleanup_ignores_selection_and_publishes_one_undoable_candidate() {
        let source = "import os\nfirst=1\nsecond=2\n";
        let (_dir, mut app, path) = app(source);
        app.settings.code_style.formatter = "builtin".into();
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(10),
            TextSize::new(17),
        )]);
        let before = app.tabs[0].state.doc.undo_history().len();
        app.code_cleanup();
        let output = app.tabs[0].state.doc.text();
        assert!(!output.contains("import os"), "{output}");
        assert!(output.contains("first = 1\nsecond = 2"), "{output}");
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(app.tabs[0].state.doc.undo_history().len(), before + 1);
        assert_eq!(app.tabs[0].state.doc.transaction_depth(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn silent_cleanup_honors_editorconfig_exclusion_and_formatter_regions() {
        let source = "# @formatter:off\nimport os\nkeep= 1\n# @formatter:on\nchange=2\n";
        let (dir, mut app, _) = app(source);
        app.settings.code_style.formatter = "builtin".into();
        app.settings.code_style.formatter_markers = true;
        app.code_cleanup();
        let output = app.tabs[0].state.doc.text();
        assert!(output.contains("import os\nkeep= 1\n"), "{output}");
        assert!(output.contains("change = 2"), "{output}");
        assert!(app.tabs[0].state.undo());
        std::fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.py]\nij_formatter_enabled = false\n",
        )
        .unwrap();
        // The write bypasses the IDE save/watcher routes that invalidate this cache.
        rustpy_settings::editorconfig::invalidate_cache();
        assert_eq!(
            app.settings
                .editor
                .editorconfig_for(app.tabs[0].state.path.as_ref().unwrap())
                .ide
                .formatter_enabled,
            Some(false)
        );
        let before = app.tabs[0].state.doc.current_history();
        app.code_cleanup();
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.doc.current_history(), before);
    }
    #[test]
    fn cleanup_final_format_failure_preserves_source_and_history() {
        let source = "import os\nvalue=\n";
        let (_dir, mut app, _) = app(source);
        app.settings.code_style.formatter = "builtin".into();
        let before = app.tabs[0].state.doc.current_history();
        app.code_cleanup();
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.doc.current_history(), before);
        assert_eq!(app.tabs[0].state.doc.transaction_depth(), 0);
    }
    #[test]
    fn explicit_whole_file_save_policy_formats_inactive_file_despite_selection() {
        let (dir, mut app, path) = app("first=1\nsecond=2\n");
        app.settings.code_style.format_on_save = true;
        app.settings.code_style.format_whole_file_on_save = true;
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::new(8),
        )]);
        let other = dir.path().join("other.py");
        std::fs::write(&other, "other=3\n").unwrap();
        app.open_file(&other);
        assert!(app.save_tab(0));
        assert_eq!(app.tabs[0].state.doc.text(), "first = 1\nsecond = 2\n");
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "first = 1\nsecond = 2\n"
        );
        assert_eq!(app.tabs[1].state.doc.text(), "other=3\n");
    }
    #[test]
    fn typing_indent_and_paste_reformat_are_separate_single_editor_actions() {
        let mut editor = EditorState::from_text("if ready:", Language::Python);
        editor.set_carets(vec![crate::editor::Caret::at(editor.doc.len())]);
        let mut opts = crate::editor::EditOptions {
            indent_size: 4,
            auto_indent: true,
            ..Default::default()
        };
        editor.newline(&opts);
        assert_eq!(editor.doc.text(), "if ready:\n    ");
        let mut pasted = EditorState::from_text("", Language::Python);
        opts.paste_reformat = true;
        opts.paste_indent_block = true;
        pasted.paste("value=1\n", &opts);
        assert_eq!(pasted.doc.text(), "value = 1\n");
        assert!(pasted.undo());
        assert_eq!(pasted.doc.text(), "");
    }
}

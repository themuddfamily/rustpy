//! Notebook actions preserve the captured notebook authority and UTF-8 cell boundaries.
use super::*;
use crate::a11y::AccessibleCombo;
impl IdeApp {
    pub(super) fn notebook_process_labels(&self) -> Vec<String> {
        let mut owners = std::collections::HashSet::new();
        self.tabs
            .iter()
            .filter(|tab| {
                tab.notebook.as_ref().is_some_and(|notebook| {
                    notebook.owns_quit_process() && owners.insert(notebook.authority_id())
                })
            })
            .map(|tab| format!("Notebook '{}'", tab.state.title()))
            .collect()
    }
    pub(super) fn poll_notebook_retirement(&mut self, ctx: &egui::Context) -> bool {
        let mut owners = std::collections::HashSet::new();
        let mut pending = false;
        for notebook in self.tabs.iter_mut().filter_map(|tab| tab.notebook.as_mut()) {
            if owners.insert(notebook.authority_id()) {
                notebook.stop_quit_process();
                notebook.poll();
                pending |= notebook.owns_quit_process();
            }
        }
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        pending
    }
    pub(super) fn notebook_structure_view(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(index) = self.active.filter(|index| {
            self.tabs
                .get(*index)
                .is_some_and(|tab| tab.notebook.is_some())
        }) else {
            return false;
        };
        let model = self.tabs[index].notebook.as_ref().unwrap().model();
        ui.heading("Notebook headings");
        for (cell, depth, title) in rustpy_notebook::virtual_file::VirtualFile::headings(&model) {
            if ui
                .button(format!("{}{}", "  ".repeat(depth.saturating_sub(1)), title))
                .clicked()
            {
                if let Some(notebook) = self.tabs[index].notebook.as_mut() {
                    notebook.select(cell);
                }
                self.focus_editor = true;
            }
        }
        true
    }
    pub(super) fn notebook_global_shortcuts(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.editor_controls_available() {
            return false;
        }
        let Some(index) = self.active.filter(|index| {
            self.tabs
                .get(*index)
                .is_some_and(|tab| tab.notebook.is_some())
        }) else {
            return false;
        };
        let kind = if ui
            .input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Space))
        {
            Some("complete")
        } else if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F12)) {
            Some("definition")
        } else {
            None
        };
        if let Some(kind) = kind {
            if let Some(notebook) = self.tabs[index].notebook.as_mut() {
                notebook.request_semantic(kind);
            }
            return true;
        }
        false
    }
    pub(super) fn jupyter_settings_page(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.jupyter;
        ui.heading("Jupyter notebooks");
        ui.text_edit_singleline(&mut settings.default_kernel);
        egui::ComboBox::from_id_salt("jupyter-default-kind")
            .selected_text(&settings.connection_kind)
            .show_ui(ui, |ui| {
                for kind in ["embedded", "local", "managed", "remote", "ssh", "docker"] {
                    ui.selectable_value(
                        &mut settings.connection_kind,
                        kind.into(),
                        crate::i18n::tr_static(kind),
                    );
                }
            })
            .combo_named(crate::i18n::tr_static("Connection type"));
        for (label, value) in [
            ("Default server URL", &mut settings.server_url),
            ("CA PEM file", &mut settings.ca_file),
            ("SSH host alias", &mut settings.ssh_host),
            ("Docker container", &mut settings.docker_container),
            ("Remote Python", &mut settings.remote_python),
            ("Remote working directory", &mut settings.remote_cwd),
            ("Reachable debugger host", &mut settings.debug_host),
            ("Debugger bind address", &mut settings.debug_bind),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(value);
            });
        }
        ui.add(
            egui::DragValue::new(&mut settings.debug_port)
                .range(0..=65535)
                .prefix("Debugger port: "),
        );
        ui.checkbox(&mut settings.verify_ssl, "Verify server SSL");
        ui.label("Tokens/passwords are entered in each notebook connection and retained only by that notebook session.");
        ui.checkbox(&mut settings.show_line_numbers, "Show line numbers");
        ui.checkbox(&mut settings.autoscroll, "Autoscroll to executed cell");
        ui.checkbox(&mut settings.collapse_outputs, "Collapse outputs");
        ui.add(
            egui::DragValue::new(&mut settings.max_output_bytes)
                .range(1024..=64 * 1024 * 1024)
                .prefix("Maximum output bytes: "),
        );
        ui.checkbox(
            &mut settings.markdown_preview,
            "Markdown cells: show preview",
        );
        ui.checkbox(&mut settings.plots, "Jupyter plots / rich output viewers");
        ui.checkbox(
            &mut settings.split_console,
            "Split notebook editor with console",
        );
        ui.checkbox(
            &mut settings.clear_outputs_before_commit,
            "Clear all notebook outputs before commit",
        );
        if ui.button("Revoke all notebook output trust").clicked() {
            settings.trusted.clear();
        }
    }
    pub(super) fn notebook_event(
        &mut self,
        index: usize,
        path: &Path,
        event: crate::notebook_view::NotebookEvent,
    ) {
        if !self.builtin_plugin_enabled("jupyter") {
            self.set_status("Jupyter bundled capability is disabled in Plugins");
            return;
        }

        let project_safe_mode = self.project_safe_mode();
        use crate::notebook_view::NotebookEvent;
        match event {
            NotebookEvent::Breakpoints { path, lines } => {
                if let Some(session) = &mut self.debug {
                    session.set_breakpoints(
                        &path,
                        lines
                            .into_iter()
                            .map(rustpy_debug::Breakpoint::new)
                            .collect(),
                    );
                }
            }
            NotebookEvent::Debugger { host, port } => self.attach_notebook_debugger(host, port),
            NotebookEvent::Status(message) => self.set_status(message),
            NotebookEvent::Trust => {
                if let Some(notebook) = self.tabs.get(index).and_then(|tab| tab.notebook.as_ref()) {
                    let hash = notebook.trust_digest();
                    self.settings
                        .jupyter
                        .trusted
                        .insert(path.to_string_lossy().into_owned(), hash);
                    let _ = self.settings.save();
                }
            }
            NotebookEvent::Console(source) => {
                if !project_safe_mode && self.ensure_py_console() {
                    self.py_console.execute_fragment(&source);
                    self.bottom = Some(BottomTool::PythonConsole);
                }
            }
            NotebookEvent::Terminal => {
                if project_safe_mode {
                    self.notify_safe_mode("Jupyter terminal");
                    return;
                }
                let python = self
                    .project
                    .as_ref()
                    .and_then(|project| project.interpreter.as_ref())
                    .map(|interpreter| interpreter.path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "python3".into());
                let cwd = path.parent().unwrap_or(Path::new("."));
                match self.terminal_panel.open_command(
                    "Jupyter",
                    &python,
                    &[
                        "-m".into(),
                        "jupyter".into(),
                        "notebook".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                    cwd,
                    &[],
                ) {
                    Ok(()) => self.bottom = Some(BottomTool::Terminal),
                    Err(error) => self.set_status(error.to_string()),
                }
            }
            NotebookEvent::ImportPython(source) => {
                let entered = PathBuf::from(source);
                let path = if entered.is_absolute() {
                    entered
                } else {
                    path.parent().unwrap_or(Path::new(".")).join(entered)
                };
                match rustpy_core::vfs::Vfs::new().read(
                    &path,
                    rustpy_core::vfs::ReadLimits {
                        max_file_bytes: 4 * 1024 * 1024,
                        max_cached_bytes: 0,
                        max_cached_files: 0,
                    },
                ) {
                    Ok(snapshot) => match std::str::from_utf8(&snapshot.bytes) {
                        Ok(text) => {
                            let notebook = rustpy_notebook::Notebook::from_py(text);
                            if let Some(tab) = self
                                .tabs
                                .get_mut(index)
                                .and_then(|tab| tab.notebook.as_mut())
                            {
                                tab.replace_model(notebook);
                                tab.set_dirty(true);
                            }
                        }
                        Err(error) => self.set_status(error.to_string()),
                    },
                    Err(error) => self.set_status(error.to_string()),
                }
            }
            NotebookEvent::Data(page) => self.scientific_data(
                page.expression.clone(),
                crate::scientific_panel::Origin::Notebook(path.display().to_string()),
                serde_json::to_value(page).unwrap_or_default(),
            ),
            NotebookEvent::RichOutput {
                mime,
                data,
                trusted,
                title,
            } => {
                if trusted && !project_safe_mode {
                    self.scientific_plot(crate::scientific_panel::PlotPayload {
                        source: format!("Notebook {}", path.display()),
                        mime,
                        data,
                        title,
                    });
                } else {
                    self.set_status(
                        "Trust the project and notebook before opening active rich output",
                    );
                }
            }
            NotebookEvent::Semantic {
                kind,
                cell,
                offset,
                value,
            } => self.notebook_semantics(index, path, &kind, cell, offset, &value),
        }
    }
    fn notebook_semantics(
        &mut self,
        index: usize,
        path: &Path,
        kind: &str,
        cell: usize,
        offset: usize,
        value: &str,
    ) {
        let Some(notebook) = self
            .tabs
            .get(index)
            .and_then(|tab| tab.notebook.as_ref())
            .map(|notebook| notebook.model())
        else {
            return;
        };
        let Some(analysis) = self
            .project
            .as_ref()
            .map(|project| project.analysis.clone())
        else {
            self.set_status("Open a project for notebook code insight");
            return;
        };
        let virtual_file = rustpy_notebook::virtual_file::VirtualFile::new(&notebook);
        let virtual_path = path.with_extension("ipynb.py");
        analysis.set_file_text(&virtual_path, &virtual_file.text);
        let Some(file) = analysis.file_for_path(&virtual_path) else {
            return;
        };
        let Some(at) = virtual_file
            .offset(cell, offset)
            .map(|offset| TextSize::new(offset as u32))
        else {
            return;
        };
        let mut lines = Vec::new();
        let mut completions = Vec::new();
        match kind {
            "complete" => {
                // One reply to a notebook request: bounded like a lookup.
                completions = rustpy_semantic::features::complete_bounded(
                    &analysis,
                    &file,
                    at,
                    super::completion_worker::INFERENCE_BUDGET,
                    None,
                )
                .into_iter()
                .take(200)
                .map(|item| item.insert)
                .collect();
            }
            "inspect" => {
                for diagnostic in rustpy_semantic::features::diagnostics(&analysis, &file)
                    .into_iter()
                    .take(500)
                {
                    if let Some((cell, byte)) =
                        virtual_file.locate(usize::from(diagnostic.range.start()))
                    {
                        let line = notebook.cells[cell].source
                            [..byte.min(notebook.cells[cell].source.len())]
                            .lines()
                            .count();
                        lines.push(format!(
                            "Cell {}:{} · {}",
                            cell + 1,
                            line.max(1),
                            diagnostic.message
                        ));
                    }
                }
            }
            "definition" => {
                let locations = rustpy_semantic::features::definition_at(&analysis, &file, at);
                if let Some(location) = locations.first() {
                    if location.path == virtual_path {
                        if let Some((cell, _)) =
                            virtual_file.locate(usize::from(location.range.start()))
                        {
                            if let Some(tab) = self
                                .tabs
                                .get_mut(index)
                                .and_then(|tab| tab.notebook.as_mut())
                            {
                                tab.select(cell);
                            }
                        }
                    } else {
                        self.navigate_to(&location.path, location.range);
                    }
                } else {
                    lines.push("No declaration found at this cell position".into());
                }
            }
            "rename" => {
                match rustpy_refactor::rename::rename(
                    &analysis,
                    &file,
                    at,
                    value,
                    std::slice::from_ref(&virtual_path),
                ) {
                    Ok(edits) => {
                        let edits = edits
                            .into_iter()
                            .filter(|(path, _)| path == &virtual_path)
                            .flat_map(|(_, edits)| {
                                edits.into_iter().map(|(range, text)| {
                                    (usize::from(range.start())..usize::from(range.end()), text)
                                })
                            })
                            .collect::<Vec<_>>();
                        if let Some(tab) = self
                            .tabs
                            .get_mut(index)
                            .and_then(|tab| tab.notebook.as_mut())
                        {
                            if let Err(error) = tab.apply_virtual_edits(&edits) {
                                lines.push(error)
                            } else {
                                lines.push(format!("Renamed {} notebook occurrences", edits.len()));
                            }
                        }
                    }
                    Err(error) => lines.push(error),
                }
            }
            _ => {}
        }
        if let Some(tab) = self
            .tabs
            .get_mut(index)
            .and_then(|tab| tab.notebook.as_mut())
        {
            tab.semantic_result(lines, completions);
        }
    }
    pub(super) fn notebook_debug_breakpoints(
        &self,
    ) -> Vec<(PathBuf, Vec<rustpy_debug::Breakpoint>)> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.notebook.as_ref())
            .flat_map(|notebook| notebook.debug_breakpoints())
            .collect()
    }
    pub(super) fn navigate_notebook_debug_frame(
        &mut self,
        path: &Path,
        line: usize,
        column: usize,
        encoding: rustpy_debug::ColumnEncoding,
    ) -> bool {
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if tab
                .notebook
                .as_mut()
                .is_some_and(|notebook| notebook.focus_debug_frame(path, line, column, encoding))
            {
                self.active = Some(index);
                self.exec_point = Some((path.to_owned(), line));
                self.exec_point_column = Some(column);
                self.focus_editor = true;
                return true;
            }
        }
        false
    }
    pub(super) fn prepare_notebooks_for_commit(&mut self) -> Result<(), String> {
        if !self.settings.jupyter.clear_outputs_before_commit {
            return Ok(());
        }
        let Some(root) = self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
            .map(|repo| repo.root().to_owned())
        else {
            return Ok(());
        };
        let selected = self
            .git_panel
            .checked
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "ipynb")
            })
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(path)
                }
            })
            .collect::<Vec<_>>();
        if self.git_panel.commit_uses_index() {
            let repo = self
                .project
                .as_ref()
                .and_then(|project| project.git.as_ref())
                .unwrap();
            for path in selected {
                let original = repo
                    .workspace_index_text(&path)
                    .map_err(|error| error.to_string())?;
                let mut notebook = original
                    .parse::<rustpy_notebook::Notebook>()
                    .map_err(|error| error.to_string())?;
                notebook.clear_outputs();
                let replacement = notebook.to_string();
                self.snapshot_text(&path, &original, "Before notebook output cleanup (index)");
                repo.workspace_replace_index_text(&path, &original, &replacement)
                    .map_err(|error| error.to_string())?;
            }
            return Ok(());
        }
        for path in selected {
            if let Some(index) = self
                .tabs
                .iter()
                .position(|tab| tab.state.path.as_ref() == Some(&path))
            {
                if self.tabs[index].state.doc.is_read_only() {
                    return Err(format!("Notebook is read-only: {}", path.display()));
                }
                if self.tabs[index]
                    .notebook
                    .as_ref()
                    .is_some_and(|notebook| notebook.is_busy())
                {
                    return Err(format!(
                        "Stop the notebook before committing {}",
                        path.display()
                    ));
                }
                if let Some(notebook) = self.tabs[index].notebook.as_ref() {
                    self.snapshot_text(&path, &notebook.text(), "Before notebook output cleanup");
                }
                if let Some(notebook) = self.tabs[index].notebook.as_mut() {
                    notebook.clear_outputs_for_commit()?;
                }
                if !self.save_tab(index) {
                    return Err(format!(
                        "Cannot save notebook without outputs: {}",
                        path.display()
                    ));
                }
            } else if path.is_file() {
                let snapshot = rustpy_core::vfs::Vfs::new()
                    .read(
                        &path,
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 32 * 1024 * 1024,
                            max_cached_bytes: 0,
                            max_cached_files: 0,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                let source =
                    std::str::from_utf8(&snapshot.bytes).map_err(|error| error.to_string())?;
                let mut notebook = source
                    .parse::<rustpy_notebook::Notebook>()
                    .map_err(|error| error.to_string())?;
                self.snapshot_text(&path, source, "Before notebook output cleanup");
                notebook.clear_outputs();
                if std::fs::metadata(&path)
                    .map_err(|error| error.to_string())?
                    .permissions()
                    .readonly()
                {
                    return Err(format!("Notebook is read-only: {}", path.display()));
                }
                if std::fs::read(&path)
                    .map_err(|error| error.to_string())?
                    .as_slice()
                    != snapshot.bytes.as_ref()
                {
                    return Err(format!(
                        "Notebook changed while preparing commit: {}",
                        path.display()
                    ));
                }
                notebook.save(&path).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
}

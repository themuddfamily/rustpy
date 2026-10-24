//! Notebook tool consumers share the notebook authority, never the active editor's identity.
use super::*;
use crate::a11y::AccessibleCombo;
#[derive(Default)]
pub(super) struct Workspace {
    pub settings: rustpy_settings::jupyter::Settings,
    pub configured: bool,
    pub trusted: bool,
    pub execution_allowed: bool,
    pub connection: rustpy_notebook::Connection,
    pub show_server: bool,
    pub variables: Vec<rustpy_console::VarInfo>,
    /// Lists IPython-injected names (`In`, `Out`, `_ih`, …) in Jupyter Variables.
    pub show_special_variables: bool,
    pub logs: String,
    pub table: Option<rustpy_console::DataPage>,
    pub table_filter: String,
    pub table_sort: Option<(usize, bool)>,
    pub table_export: String,
    pub history: Vec<Notebook>,
    pub clipboard: Option<Cell>,
    pub selection: std::collections::BTreeSet<usize>,
    pub caret: usize,
    pub rename: String,
    pub semantic_lines: Vec<String>,
    pub completions: Vec<String>,
    pub completion_owner: Option<(u64, String, String, usize)>,
    pub export_path: String,
    pub export_kind: String,
    pub task: Option<rustpy_notebook::tasks::ProcessTask>,
    pub install_prompt: bool,
    pub json_mode: bool,
    pub json_text: Option<String>,
    pub debug_cursor: Option<(usize, usize, usize)>,
    pub debug_snapshot: Option<(String, usize)>,
    pub zen: bool,
    pub split_console: bool,
    pub console_input: String,
    pub console_log: String,
    pub console_seen: HashMap<String, usize>,
    pub pending_events: Vec<NotebookEvent>,
    pub trust_cache: Option<(u64, u64, String)>,
    /// Digest the app last confirmed as trusted and the model revision it covered.
    pub signed: Option<(String, u64)>,
    /// A re-signed digest still has to be persisted once execution is idle.
    pub resign_unsaved: bool,
    pub svg_jobs: HashMap<
        String,
        crossbeam_channel::Receiver<
            Result<crate::editor::svg_preview::Raster, crate::editor::svg_preview::SvgError>,
        >,
    >,
    pub svg_cache: HashMap<String, crate::editor::svg_preview::SvgTexture>,
    pub svg_errors: std::collections::HashSet<String>,
}
impl Workspace {
    /// Jupyter Variables rows; IPython internals stay hidden unless
    /// "Show special variables" is on.
    pub(crate) fn visible_variables(&self) -> Vec<rustpy_console::VarInfo> {
        self.variables
            .iter()
            .filter(|variable| self.show_special_variables || !variable.is_special())
            .cloned()
            .collect()
    }
}
impl NotebookTab {
    pub(crate) fn trust_digest(&self) -> String {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        if let Some((revision, output_revision, digest)) = &data.workspace.trust_cache {
            if *revision == data.revision && *output_revision == data.output_revision {
                return digest.clone();
            }
        }
        let text = if data.workspace.json_mode {
            data.workspace
                .json_text
                .clone()
                .unwrap_or_else(|| data.nb.to_string())
        } else {
            data.nb.to_string()
        };
        let digest = blake3::hash(text.as_bytes()).to_hex().to_string();
        data.workspace.trust_cache = Some((data.revision, data.output_revision, digest.clone()));
        digest
    }
    /// Checks the notebook against its `stored` trust signature. Returns whether
    /// it is trusted and, when this session's own outputs were re-signed, the
    /// new digest to store in place of `stored`.
    pub(crate) fn trust_against(&self, stored: Option<&str>) -> (bool, Option<String>) {
        let digest = self.trust_digest();
        let resigned = if stored == Some(digest.as_str()) {
            None
        } else if self.resign_own_outputs(stored) {
            Some(digest.clone())
        } else {
            return (false, None);
        };
        self.note_trusted(&digest);
        (true, resigned)
    }
    /// Records that `digest` is the notebook's trusted signature right now.
    fn note_trusted(&self, digest: &str) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        let revision = data.revision;
        if data
            .workspace
            .signed
            .as_ref()
            .is_none_or(|(signed, at)| signed != digest || *at != revision)
        {
            data.workspace.signed = Some((digest.to_owned(), revision));
        }
    }
    /// Jupyter's model: outputs produced by this session's own kernel while the
    /// notebook was trusted are re-signed, so running cells keeps it trusted.
    /// True only when the stored signature is the one this session confirmed and
    /// nothing but kernel outputs changed since (no edit, reload or JSON draft);
    /// outputs that arrive from disk always need explicit trust.
    fn resign_own_outputs(&self, stored: Option<&str>) -> bool {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        let resign = !(data.workspace.json_mode && data.workspace.json_text.is_some())
            && data
                .workspace
                .signed
                .as_ref()
                .is_some_and(|(signed, revision)| {
                    Some(signed.as_str()) == stored && *revision == data.revision
                });
        data.workspace.resign_unsaved |= resign;
        resign
    }
    /// Takes the "re-signed but not yet persisted" flag once execution is idle.
    pub(crate) fn take_resign_unsaved(&self) -> bool {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        if data.is_busy() {
            return false;
        }
        std::mem::take(&mut data.workspace.resign_unsaved)
    }
    pub(crate) fn clear_outputs_for_commit(&mut self) -> Result<(), String> {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        if let Some(text) = data
            .workspace
            .json_text
            .as_ref()
            .filter(|_| data.workspace.json_mode)
        {
            data.nb = text
                .parse::<Notebook>()
                .map_err(|error| error.to_string())?;
        }
        data.record_edit();
        data.nb.clear_outputs();
        if data.workspace.json_mode {
            data.workspace.json_text = Some(data.nb.to_string());
        }
        data.dirty = true;
        data.revision = data.revision.wrapping_add(1);
        Ok(())
    }
    pub fn append_markdown(&mut self, source: String) -> Result<(), String> {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        if let Some(text) = data
            .workspace
            .json_text
            .as_ref()
            .filter(|_| data.workspace.json_mode)
        {
            data.nb = text
                .parse::<Notebook>()
                .map_err(|error| error.to_string())?;
        }
        data.record_edit();
        let index = data.nb.cells.len();
        data.nb.insert_cell(index, Cell::markdown(source));
        data.workspace.json_mode = false;
        data.workspace.json_text = None;
        data.dirty = true;
        data.revision = data.revision.wrapping_add(1);
        Ok(())
    }
    pub(crate) fn debug_breakpoints(&self) -> Vec<(PathBuf, Vec<rustpy_debug::Breakpoint>)> {
        let data = self.data.lock().expect("notebook authority poisoned");
        let Some(kernel) = &data.kernel else {
            return Vec::new();
        };
        data.nb
            .cells
            .iter()
            .filter_map(|cell| {
                let id = cell.id.as_ref()?;
                let values = cell.metadata.get("rustpy_breakpoints")?.as_array()?;
                Some((
                    PathBuf::from(kernel.debug_source_path(id)),
                    values
                        .iter()
                        .filter_map(|value| {
                            value
                                .as_u64()
                                .map(|line| rustpy_debug::Breakpoint::new(line as usize))
                        })
                        .collect(),
                ))
            })
            .collect()
    }
    pub(crate) fn focus_debug_frame(
        &mut self,
        path: &Path,
        line: usize,
        column: usize,
        encoding: rustpy_debug::ColumnEncoding,
    ) -> bool {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        let Some(kernel) = &data.kernel else {
            return false;
        };
        let snapshot = data
            .kernel
            .as_ref()
            .and_then(|kernel| {
                kernel
                    .debug_sources
                    .get(&path.to_string_lossy().into_owned())
            })
            .cloned();
        let Some(index) = data.nb.cells.iter().position(|cell| {
            cell.id
                .as_ref()
                .is_some_and(|id| Path::new(&kernel.debug_source_path(id)) == path)
        }) else {
            if let Some(snapshot) = snapshot {
                data.workspace.debug_snapshot = Some((snapshot, line));
                return true;
            }
            return false;
        };
        if let Some(snapshot) = snapshot.filter(|snapshot| snapshot != &data.nb.cells[index].source)
        {
            data.workspace.debug_snapshot = Some((snapshot, line));
            self.presentation.selected = index;
            self.presentation.scroll_to_selected = true;
            return true;
        }
        data.workspace.debug_snapshot = None;
        self.presentation.selected = index;
        self.presentation.focus_cell = Some(index);
        self.presentation.scroll_to_selected = true;
        let source = &data.nb.cells[index].source;
        let line_text = source
            .lines()
            .nth(line.saturating_sub(1))
            .unwrap_or_default();
        let byte = encoding.byte_offset(line_text, column);
        let chars = source
            .split_inclusive('\n')
            .take(line.saturating_sub(1))
            .map(|line| line.chars().count())
            .sum::<usize>()
            + line_text[..byte].chars().count();
        data.workspace.debug_cursor = Some((index, chars, line));
        true
    }
    pub(crate) fn request_semantic(&mut self, kind: &str) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        let cell = self.presentation.selected;
        let offset = self.presentation.caret;
        data.workspace.pending_events.push(NotebookEvent::Semantic {
            kind: kind.into(),
            cell,
            offset,
            value: String::new(),
        });
    }
    pub fn configure_workspace(
        &mut self,
        settings: &rustpy_settings::jupyter::Settings,
        trusted: bool,
        execution_allowed: bool,
    ) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        data.workspace.trusted = trusted;
        data.workspace.execution_allowed = execution_allowed;
        data.workspace.settings.show_line_numbers = settings.show_line_numbers;
        data.workspace.settings.autoscroll = settings.autoscroll;
        data.workspace.settings.collapse_outputs = settings.collapse_outputs;
        data.workspace.settings.max_output_bytes = settings.max_output_bytes;
        data.workspace.settings.markdown_preview = settings.markdown_preview;
        data.workspace.settings.plots = settings.plots;
        if let Some(kernel) = &mut data.kernel {
            kernel.set_output_limit(settings.max_output_bytes);
        }
        if !data.workspace.configured {
            data.workspace.settings = settings.clone();
            data.workspace.settings.trusted.clear();
            data.workspace.connection = rustpy_notebook::Connection {
                kind: settings.connection_kind.clone(),
                kernel: settings.default_kernel.clone(),
                url: settings.server_url.clone(),
                verify_ssl: settings.verify_ssl,
                ca_file: settings.ca_file.clone(),
                host: settings.ssh_host.clone(),
                container: settings.docker_container.clone(),
                python: settings.remote_python.clone(),
                debug_host: settings.debug_host.clone(),
                debug_bind: settings.debug_bind.clone(),
                debug_port: settings.debug_port,
                cwd: settings.remote_cwd.clone(),
                ..Default::default()
            };
            data.workspace.split_console = settings.split_console;
            data.workspace.configured = true;
        }
    }
    pub fn semantic_result(&mut self, lines: Vec<String>, completions: Vec<String>) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        data.workspace.semantic_lines = lines;
        data.workspace.completions = completions;
        data.workspace.completion_owner =
            data.nb.cells.get(self.presentation.selected).map(|cell| {
                (
                    0,
                    cell_key(cell, self.presentation.selected),
                    cell.source.clone(),
                    self.presentation.caret,
                )
            });
    }
    pub fn apply_virtual_edits(
        &mut self,
        edits: &[(std::ops::Range<usize>, String)],
    ) -> Result<(), String> {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        let virtual_file = rustpy_notebook::virtual_file::VirtualFile::new(&data.nb);
        data.record_edit();
        virtual_file.apply_edits(&mut data.nb, edits)?;
        data.dirty = true;
        data.revision = data.revision.wrapping_add(1);
        Ok(())
    }
}
impl NotebookData {
    fn start_task(&mut self, mut task: rustpy_notebook::tasks::ProcessTask) {
        task.id = crate::app::window_lifecycle::next_process_owner();
        self.workspace.task = Some(task);
    }
    pub(super) fn cell_action(&mut self, index: usize, action: &str, python: &Path, cwd: &Path) {
        if index >= self.nb.cells.len() {
            return;
        }
        if self.selected != index {
            self.workspace.caret = self.nb.cells[index].source.len();
        }
        self.selected = index;
        match action {
            "run" => self.run(vec![index], python, cwd),
            "above" => self.run((0..index).collect(), python, cwd),
            "below" => self.run((index..self.nb.cells.len()).collect(), python, cwd),
            "advance" => {
                self.run(vec![index], python, cwd);
                if index + 1 >= self.nb.cells.len() {
                    self.insert(index + 1, CellKind::Code);
                } else {
                    self.selected = index + 1;
                    self.scroll_to_selected = true;
                }
            }
            "debug" => {
                if self.nb.cells[index].is_code()
                    && !self.nb.cells[index].source.trim().is_empty()
                    && self.ensure_kernel(python, cwd)
                {
                    if let Some(kernel) = &mut self.kernel {
                        kernel.debug_cell(&mut self.nb, index);
                    }
                }
            }
            "add-above" => self.insert(index, CellKind::Code),
            "add-below" => self.insert(index + 1, CellKind::Code),
            "delete" => self.delete_selected(),
            "split" => self.split_cell(),
            "merge" => self.merge_cell(),
            "code" => self.set_kind(CellKind::Code),
            "markdown" => self.set_kind(CellKind::Markdown),
            "raw" => self.set_kind(CellKind::Raw),
            "up" | "down" => {
                let target = if action == "up" {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(self.nb.cells.len().saturating_sub(1))
                };
                self.record_edit();
                self.nb.move_cell(index, target);
                self.selected = target;
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
            "clear" => {
                self.record_edit();
                self.nb.cells[index].clear_outputs();
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
            _ => {}
        }
    }
    pub(super) fn record_edit(&mut self) {
        self.workspace.history.push(self.nb.clone());
        while self.workspace.history.len() > 50
            || self
                .workspace
                .history
                .iter()
                .map(notebook_bytes)
                .sum::<usize>()
                > 32 * 1024 * 1024
        {
            self.workspace.history.remove(0);
            if self.workspace.history.is_empty() {
                break;
            }
        }
    }
    fn undo_cell_operation(&mut self) {
        if let Some(notebook) = self.workspace.history.pop() {
            self.nb = notebook;
            self.selected = self.selected.min(self.nb.cells.len().saturating_sub(1));
            self.dirty = true;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    fn split_cell(&mut self) {
        let Some(cell) = self.nb.cells.get(self.selected) else {
            return;
        };
        let mut offset = self.workspace.caret.min(cell.source.len());
        while !cell.source.is_char_boundary(offset) {
            offset = offset.saturating_sub(1)
        }
        let (kind, before, after) = (
            cell.kind,
            cell.source[..offset].to_owned(),
            cell.source[offset..].to_owned(),
        );
        self.record_edit();
        self.nb.cells[self.selected].source = before;
        self.nb.cells[self.selected].clear_outputs();
        self.nb
            .insert_cell(self.selected + 1, Cell::new(kind, after));
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }
    fn merge_cell(&mut self) {
        if self.selected + 1 >= self.nb.cells.len() {
            return;
        }
        self.record_edit();
        let next = self.nb.cells.remove(self.selected + 1);
        self.nb.cells[self.selected].source.push('\n');
        self.nb.cells[self.selected].source.push_str(&next.source);
        self.nb.cells[self.selected].clear_outputs();
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }
    pub(super) fn workspace_keys(&mut self, ui: &mut egui::Ui) {
        if !self.keys_enabled
            || self.editing.is_some()
            || ui.ctx().memory(|memory| memory.focused()).is_some()
        {
            return;
        }
        let pressed = |key| ui.input_mut(|input| input.consume_key(Modifiers::NONE, key));
        if pressed(Key::C) {
            self.workspace.clipboard = self.nb.cells.get(self.selected).cloned();
        } else if pressed(Key::X) {
            self.workspace.clipboard = self.nb.cells.get(self.selected).cloned();
            self.delete_selected();
        } else if pressed(Key::V) {
            if let Some(mut cell) = self.workspace.clipboard.clone() {
                cell.id = None;
                self.record_edit();
                self.nb.insert_cell(self.selected + 1, cell);
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
        } else if pressed(Key::Z) {
            self.undo_cell_operation();
        }
    }
    pub(super) fn workspace_toolbar(
        &mut self,
        ui: &mut egui::Ui,
        python: &Path,
        cwd: &Path,
        path: &Path,
        events: &mut Vec<NotebookEvent>,
    ) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if self.workspace.trusted {
                    "Trusted notebook"
                } else {
                    "Trust notebook"
                })
                .clicked()
            {
                events.push(NotebookEvent::Trust);
            }
            ui.checkbox(&mut self.workspace.zen, "Zen");
            let previous_json = self.workspace.json_mode;
            if ui
                .checkbox(&mut self.workspace.json_mode, "Open as JSON")
                .changed()
            {
                if previous_json {
                    if let Some(text) = &self.workspace.json_text {
                        match text.parse::<Notebook>() {
                            Ok(model) => {
                                self.nb = model;
                                self.workspace.json_text = None;
                            }
                            Err(error) => {
                                self.workspace.json_mode = true;
                                self.kernel_error = Some(format!(
                                    "Fix notebook JSON before returning to cells: {error}"
                                ));
                            }
                        }
                    }
                } else {
                    self.workspace.json_text = Some(self.nb.to_string());
                }
            }

            ui.checkbox(&mut self.workspace.split_console, "Split with console");
            if ui.button("Jupyter Server / kernel").clicked() {
                self.workspace.show_server = !self.workspace.show_server;
            }
            if ui.button("Split cell").clicked() {
                self.split_cell();
            }
            if ui.button("Merge below").clicked() {
                self.merge_cell();
            }
            if ui.button("Undo cell operation").clicked() {
                self.undo_cell_operation();
            }
            if ui.button("Run selected cells").clicked() {
                self.run(
                    self.workspace.selection.iter().copied().collect(),
                    python,
                    cwd,
                );
            }
            if ui.button("Restart & run all").clicked() {
                if let Some(kernel) = &mut self.kernel {
                    kernel.restart();
                }
                self.run((0..self.nb.cells.len()).collect(), python, cwd);
            }
            if ui.button("Cancel cell queue").clicked() {
                if let Some(kernel) = &mut self.kernel {
                    kernel.cancel_queued();
                }
            }
            if ui.button("Execute in console").clicked() {
                events.push(NotebookEvent::Console(
                    self.nb
                        .cells
                        .get(self.selected)
                        .map(|cell| cell.source.clone())
                        .unwrap_or_default(),
                ));
            }
        });
        if !self.workspace.zen {
            ui.horizontal_wrapped(|ui| {
                ui.label("Cell semantics");
                if ui.button("Kernel completion").clicked() {
                    if let Some(kernel) = &mut self.kernel {
                        if let Some(cell) = self.nb.cells.get(self.selected) {
                            let mut cursor = self.workspace.caret.min(cell.source.len());
                            while !cell.source.is_char_boundary(cursor) {
                                cursor = cursor.saturating_sub(1);
                            }
                            let id = kernel.complete(&cell.source, cursor);
                            self.workspace.completion_owner = Some((
                                id,
                                cell_key(cell, self.selected),
                                cell.source.clone(),
                                cursor,
                            ));
                        }
                    }
                }

                for (kind, label) in [
                    ("complete", "Complete"),
                    ("inspect", "Inspections"),
                    ("definition", "Go to definition"),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        events.push(NotebookEvent::Semantic {
                            kind: kind.into(),
                            cell: self.selected,
                            offset: self.workspace.caret,
                            value: String::new(),
                        });
                    }
                }
                ui.text_edit_singleline(&mut self.workspace.rename);
                if ui.button("Rename symbol").clicked() {
                    events.push(NotebookEvent::Semantic {
                        kind: "rename".into(),
                        cell: self.selected,
                        offset: self.workspace.caret,
                        value: self.workspace.rename.clone(),
                    });
                }
            });
            for message in &self.workspace.semantic_lines {
                ui.label(message);
            }
            let mut insertion = None;
            ui.horizontal_wrapped(|ui| {
                for completion in self.workspace.completions.iter().take(50) {
                    if ui.small_button(completion).clicked() {
                        insertion = Some(completion.clone());
                    }
                }
            });
            if let Some(insertion) = insertion {
                if let Some((_, id, source, cursor)) = &self.workspace.completion_owner {
                    if self.nb.cells.get(self.selected).is_none_or(|cell| {
                        cell_key(cell, self.selected) != *id || cell.source != *source
                    }) || self.workspace.caret != *cursor
                    {
                        self.workspace.completions.clear();
                        return;
                    }
                }
                if let Some(cell) = self.nb.cells.get_mut(self.selected) {
                    let mut end = self.workspace.caret.min(cell.source.len());
                    while !cell.source.is_char_boundary(end) {
                        end -= 1
                    }
                    let start = cell.source[..end]
                        .rfind(|character: char| !character.is_alphanumeric() && character != '_')
                        .map(|offset| offset + 1)
                        .unwrap_or(0);
                    cell.source.replace_range(start..end, &insertion);
                    self.dirty = true;
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            egui::CollapsingHeader::new("Structure / table of contents").show(ui, |ui| {
                for (cell, depth, title) in
                    rustpy_notebook::virtual_file::VirtualFile::headings(&self.nb)
                {
                    if ui
                        .button(format!("{}{}", "  ".repeat(depth.saturating_sub(1)), title))
                        .clicked()
                    {
                        self.selected = cell;
                        self.scroll_to_selected = true;
                    }
                }
            });
            self.variable_ui(ui, events, cwd);
            self.export_ui(ui, python, cwd, path, events);
        }
        if self.workspace.show_server {
            ui.add_enabled_ui(self.workspace.execution_allowed, |ui| {
                self.server_ui(ui, python, cwd)
            });
        }
        if let Some(task) = &mut self.workspace.task {
            task.poll();
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} · {:.1}s",
                    task.label,
                    task.started.elapsed().as_secs_f32()
                ));
                if task.running() {
                    crate::a11y::spinner(ui);
                    if ui.button("Cancel operation").clicked() {
                        task.cancel();
                    }
                } else if let Some(result) = &task.result {
                    ui.label(match result {
                        Ok(log) | Err(log) => log,
                    });
                }
            });
        }
    }
    fn server_ui(&mut self, ui: &mut egui::Ui, python: &Path, cwd: &Path) {
        egui::CollapsingHeader::new("Jupyter Server tool window").default_open(true).show(ui,|ui|{
        let connection=&mut self.workspace.connection;egui::ComboBox::from_id_salt("notebook_connection").selected_text(&connection.kind).show_ui(ui,|ui|{for kind in ["embedded","local","managed","remote","ssh","docker"]{ui.selectable_value(&mut connection.kind,kind.into(),crate::i18n::tr_static(kind));}}).combo_named(crate::i18n::tr_static("Connection type"));
        ui.label("Kernel name / registered kernels");ui.text_edit_singleline(&mut connection.kernel);if ["local","ssh","docker"].contains(&connection.kind.as_str())&&ui.button("Use selected interpreter's IPykernel").clicked(){connection.kernel="project".into();}
if let Some(kernel)=&self.kernel{egui::ComboBox::from_id_salt("notebook_kernels").selected_text(&connection.kernel).show_ui(ui,|ui|{for name in &kernel.kernels{ui.selectable_value(&mut connection.kernel,name.clone(),name);}}).combo_named(crate::i18n::tr_static("Registered kernels"));}
        for(label,value)in [("Server URL",&mut connection.url),("SSH host alias",&mut connection.host),("Docker container",&mut connection.container),("Remote Python",&mut connection.python),("Remote working directory",&mut connection.cwd),("CA PEM path",&mut connection.ca_file),("Forwarded/reachable debugger host",&mut connection.debug_host),("Debugger bind address",&mut connection.debug_bind)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
        if connection.kind == "ssh" {
            for (label, selected) in [
                ("Local SSH executable (optional)", &mut connection.ssh_program),
                ("Local SSH config file (optional)", &mut connection.ssh_config_file),
            ] {
                let mut value = selected.as_ref().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr_static(label));
                    if ui.text_edit_singleline(&mut value).changed() {
                        *selected = (!value.is_empty()).then(|| PathBuf::from(value));
                    }
                });
            }
        }
        ui.add(egui::DragValue::new(&mut connection.debug_port).range(0..=65535).prefix("Debugger port (0 = automatic): "));ui.label("Remote debugging requires a reachable host and listener/published port or an existing SSH forward.");
        ui.checkbox(&mut connection.verify_ssl,"Verify server SSL certificate");ui.horizontal(|ui|{ui.label("Token (session only)");ui.add(egui::TextEdit::singleline(&mut connection.token).password(true));});ui.horizontal(|ui|{ui.label("Password (session only)");ui.add(egui::TextEdit::singleline(&mut connection.password).password(true));});
        ui.horizontal(|ui|{if ui.button("Start / connect selected kernel").clicked(){if self.kernel.as_ref().is_some_and(|kernel|kernel.owns_process()){self.shutdown();self.kernel_error=Some("Previous kernel is stopping. Connect again after it retires.".into());}else{self.kernel=None;self.ensure_kernel(python,cwd);}}
if ui.button("Stop server / kernel").clicked(){self.shutdown();}
if ui.button("Install Jupyter dependencies…").clicked(){self.workspace.install_prompt=true;}});
        if self.workspace.install_prompt{ui.label(format!("Install jupyter_server, jupyter_client, ipykernel, websocket-client, debugpy, nbconvert and ipywidgets into {}?",python.display()));ui.horizontal(|ui|{if ui.add_enabled(!self.workspace.task.as_ref().is_some_and(|task|task.running()),egui::Button::new("Install into selected interpreter")).clicked(){self.start_task(rustpy_notebook::tasks::ProcessTask::start(python,vec!["-m".into(),"pip".into(),"install".into(),"jupyter_server".into(),"jupyter_client".into(),"ipykernel".into(),"websocket-client".into(),"debugpy".into(),"nbconvert".into(),"ipywidgets".into()],cwd,"Install Jupyter dependencies"));self.workspace.install_prompt=false;}
if ui.button("Cancel installation").clicked(){self.workspace.install_prompt=false;}});}
        if let Some(kernel)=&self.kernel{if let Some((url,token))=&kernel.server_info{ui.label(format!("Managed server: {url}"));if ui.button("Copy managed server URL/token").clicked(){ui.ctx().copy_text(format!("{url}?token={token}"));}}ui.label(format!("Kernel: {:?}",kernel.state()));if let Some((memory,cpu))=kernel.resources{ui.label(format!("Resources: {:.1} MiB · {cpu:.2} CPU seconds",memory as f64/1048576.0));}}
        egui::ScrollArea::vertical().max_height(160.0).show(ui,|ui|{ui.monospace(&self.workspace.logs);});
    });
    }
    fn export_ui(
        &mut self,
        ui: &mut egui::Ui,
        python: &Path,
        cwd: &Path,
        _path: &Path,
        events: &mut Vec<NotebookEvent>,
    ) {
        egui::CollapsingHeader::new("Export / convert / terminal").show(ui, |ui| {
            ui.horizontal(|ui| {
                for kind in ["py", "html", "pdf", "ipynb"] {
                    ui.selectable_value(
                        &mut self.workspace.export_kind,
                        kind.into(),
                        crate::i18n::tr_static(kind),
                    );
                }
                ui.text_edit_singleline(&mut self.workspace.export_path);
                if ui.button("Export (new file)").clicked() {
                    let entered = PathBuf::from(&self.workspace.export_path);
                    let destination = if entered.is_absolute() {
                        entered
                    } else {
                        cwd.join(entered)
                    };
                    let result = match self.workspace.export_kind.as_str() {
                        "py" => rustpy_notebook::tasks::write_export(
                            &destination,
                            self.nb.to_py().as_bytes(),
                        ),
                        "html" => rustpy_notebook::tasks::write_export(
                            &destination,
                            rustpy_notebook::tasks::export_html(&self.nb, self.workspace.trusted)
                                .as_bytes(),
                        ),
                        "ipynb" => rustpy_notebook::tasks::write_export(
                            &destination,
                            self.nb.to_string().as_bytes(),
                        ),
                        "pdf" => {
                            if !self.workspace.execution_allowed {
                                Err("Trust the project before running nbconvert".into())
                            } else if self
                                .workspace
                                .task
                                .as_ref()
                                .is_some_and(|task| task.running())
                            {
                                Err(
                                    "Wait for the current notebook operation before exporting PDF"
                                        .into(),
                                )
                            } else if destination
                                .extension()
                                .is_none_or(|extension| extension != "pdf")
                            {
                                Err("PDF export destination must end in .pdf".into())
                            } else if destination.exists() {
                                Err("Export destination already exists".into())
                            } else {
                                let directory = rustpy_settings::Settings::config_dir()
                                    .join("notebook-exports");
                                let _ = std::fs::create_dir_all(&directory);
                                let snapshot = directory.join(format!(
                                    "snapshot-{}-{}.ipynb",
                                    std::process::id(),
                                    crate::app::window_lifecycle::next_process_owner()
                                ));
                                if let Err(error) = self.nb.save(&snapshot) {
                                    events.push(NotebookEvent::Status(error.to_string()));
                                    return;
                                }
                                self.start_task(
                                    rustpy_notebook::tasks::ProcessTask::start_with_cleanup(
                                        python,
                                        vec![
                                            "-m".into(),
                                            "nbconvert".into(),
                                            "--to".into(),
                                            "pdf".into(),
                                            "--output".into(),
                                            destination
                                                .file_stem()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .into_owned(),
                                            "--output-dir".into(),
                                            destination
                                                .parent()
                                                .unwrap_or(cwd)
                                                .to_string_lossy()
                                                .into_owned(),
                                            snapshot.to_string_lossy().into_owned(),
                                        ],
                                        cwd,
                                        "Export notebook with nbconvert",
                                        vec![snapshot],
                                    ),
                                );
                                Ok(())
                            }
                        }
                        _ => Err("Choose an export format".into()),
                    };
                    events.push(NotebookEvent::Status(
                        result
                            .err()
                            .unwrap_or_else(|| "Notebook export prepared".into()),
                    ));
                }
            });
            if ui.button("Convert Python #%% script → notebook").clicked() {
                events.push(NotebookEvent::ImportPython(
                    self.workspace.export_path.clone(),
                ));
            }
            if ui.button("Run jupyter in terminal").clicked() {
                events.push(NotebookEvent::Terminal);
            }
        });
    }
    fn variable_ui(&mut self, ui: &mut egui::Ui, events: &mut Vec<NotebookEvent>, cwd: &Path) {
        egui::CollapsingHeader::new("Jupyter Variables / DataFrame and array viewers").show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Refresh variables").clicked() {
                        if let Some(kernel) = &mut self.kernel {
                            kernel.request_vars();
                        }
                    }
                    ui.checkbox(
                        &mut self.workspace.show_special_variables,
                        "Show special variables",
                    );
                });
                for variable in self.workspace.visible_variables() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{}: {} = {}",
                            variable.name, variable.type_name, variable.value
                        ));
                        if ui.small_button("Children").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.inspect(&variable.expr);
                            }
                        }
                        if ui.small_button("View DataFrame / array").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.request_data(&variable.expr, 0, 0);
                            }
                        }
                    });
                }
                if let Some(table) = self.workspace.table.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} · {} × {}",
                            table.expression, table.total_rows, table.total_cols
                        ));
                        ui.text_edit_singleline(&mut self.workspace.table_filter);
                        if ui.button("Open viewer").clicked() {
                            events.push(NotebookEvent::Data(table.clone()));
                        }
                        if ui.button("Previous page").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.request_data(
                                    &table.expression,
                                    table.row_start.saturating_sub(100),
                                    table.col_start,
                                );
                            }
                        }
                        if ui.button("Previous columns").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.request_data(
                                    &table.expression,
                                    table.row_start,
                                    table.col_start.saturating_sub(20),
                                );
                            }
                        }
                        if ui.button("Next columns").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.request_data(
                                    &table.expression,
                                    table.row_start,
                                    (table.col_start + 20).min(table.total_cols.saturating_sub(1)),
                                );
                            }
                        }
                        if ui.button("Next page").clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.request_data(
                                    &table.expression,
                                    (table.row_start + 100).min(table.total_rows.saturating_sub(1)),
                                    table.col_start,
                                );
                            }
                        }
                    });
                    let mut rows = table.rows.clone();
                    rows.retain(|row| {
                        self.workspace.table_filter.is_empty()
                            || row
                                .iter()
                                .any(|cell| cell.contains(&self.workspace.table_filter))
                    });
                    if let Some((column, ascending)) = self.workspace.table_sort {
                        rows.sort_by(|left, right| {
                            let order = left.get(column).cmp(&right.get(column));
                            if ascending {
                                order
                            } else {
                                order.reverse()
                            }
                        });
                    }
                    egui::ScrollArea::both().max_height(200.0).show(ui, |ui| {
                        egui::Grid::new("notebook_dataframe")
                            .striped(true)
                            .show(ui, |ui| {
                                for (column, label) in table.columns.iter().enumerate() {
                                    if ui.button(label).clicked() {
                                        let ascending =
                                            self.workspace.table_sort != Some((column, true));
                                        self.workspace.table_sort = Some((column, ascending));
                                    }
                                }
                                ui.end_row();
                                for row in &rows {
                                    for cell in row {
                                        ui.label(cell);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.workspace.table_export);
                        if ui.button("Export displayed page CSV").clicked() {
                            let quote = |text: &str| format!("\"{}\"", text.replace('"', "\"\""));
                            let text = std::iter::once(table.columns.clone())
                                .chain(rows)
                                .map(|row| {
                                    row.iter()
                                        .map(|cell| quote(cell))
                                        .collect::<Vec<_>>()
                                        .join(",")
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            events.push(NotebookEvent::Status(
                                rustpy_notebook::tasks::write_export(
                                    &if Path::new(&self.workspace.table_export).is_absolute() {
                                        PathBuf::from(&self.workspace.table_export)
                                    } else {
                                        cwd.join(&self.workspace.table_export)
                                    },
                                    text.as_bytes(),
                                )
                                .err()
                                .unwrap_or_else(|| "Exported displayed table page".into()),
                            ));
                        }
                    });
                }
            },
        );
    }
}
impl NotebookData {
    pub(super) fn rich_output_controls(
        &mut self,
        ui: &mut egui::Ui,
        data: &rustpy_notebook::MimeBundle,
        cell: usize,
        events: &mut Vec<NotebookEvent>,
    ) {
        if let Some(widget) = data
            .get("application/vnd.jupyter.widget-view+json")
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|value| value["model_id"].as_str().map(str::to_owned))
        {
            if !self.workspace.trusted {
                ui.label("Trust notebook to enable live widgets");
            } else if let Some(state) = self
                .kernel
                .as_ref()
                .and_then(|kernel| kernel.widgets.get(&widget))
                .cloned()
            {
                let model = state["_model_name"].as_str().unwrap_or("");
                let description = state["description"].as_str().unwrap_or("");
                let mut update = None;
                match model {
                    "IntSliderModel" | "FloatSliderModel" => {
                        let mut value = state["value"].as_f64().unwrap_or(0.0);
                        let min = state["min"].as_f64().unwrap_or(0.0);
                        let max = state["max"].as_f64().unwrap_or(100.0);
                        if ui
                            .add(
                                egui::Slider::new(&mut value, min..=max.max(min)).text(description),
                            )
                            .changed()
                        {
                            update = Some(
                                serde_json::json!({"value":if model=="IntSliderModel"{serde_json::json!(value.round()as i64)}else{serde_json::json!(value)}}),
                            );
                        }
                    }
                    "CheckboxModel" => {
                        let mut value = state["value"].as_bool().unwrap_or(false);
                        if ui.checkbox(&mut value, description).changed() {
                            update = Some(serde_json::json!({"value":value}));
                        }
                    }
                    "TextModel" | "TextareaModel" => {
                        let mut value = state["value"].as_str().unwrap_or("").to_owned();
                        ui.label(description);
                        if ui.text_edit_singleline(&mut value).changed() {
                            update = Some(serde_json::json!({"value":value}));
                        }
                    }
                    "DropdownModel" | "SelectModel" => {
                        let mut index = state["index"].as_u64().unwrap_or(0) as usize;
                        let labels = state["_options_labels"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        egui::ComboBox::from_id_salt(("nb_widget", &widget))
                            .selected_text(
                                labels
                                    .get(index)
                                    .and_then(|value| value.as_str())
                                    .unwrap_or(description),
                            )
                            .show_ui(ui, |ui| {
                                for (i, label) in labels.iter().enumerate() {
                                    if ui
                                        .selectable_value(
                                            &mut index,
                                            i,
                                            label.as_str().unwrap_or(""),
                                        )
                                        .changed()
                                    {
                                        update = Some(serde_json::json!({"index":index}));
                                    }
                                }
                            })
                            .combo_named(description);
                    }
                    "IntProgressModel" | "FloatProgressModel" => {
                        let value = state["value"].as_f64().unwrap_or(0.0);
                        let min = state["min"].as_f64().unwrap_or(0.0);
                        let max = state["max"].as_f64().unwrap_or(100.0);
                        ui.add(
                            egui::ProgressBar::new(
                                ((value - min) / (max - min).max(1.0)).clamp(0.0, 1.0) as f32,
                            )
                            .text(description),
                        );
                    }
                    "ButtonModel" => {
                        if ui.button(description).clicked() {
                            if let Some(kernel) = &mut self.kernel {
                                kernel.widget_event(&widget, serde_json::json!({"event":"click"}));
                            }
                        }
                    }
                    "LabelModel" | "HTMLModel" => {
                        ui.label(html_to_text(state["value"].as_str().unwrap_or(description)));
                    }
                    _ => {
                        ui.label(format!("Widget {model}: {description}"));
                    }
                }
                if let Some(update) = update {
                    if let Some(kernel) = &mut self.kernel {
                        kernel.widget_update(&widget, update.clone());
                        if let Some(state) = kernel
                            .widgets
                            .get_mut(&widget)
                            .and_then(|state| state.as_object_mut())
                        {
                            state.extend(update.as_object().unwrap().clone());
                        }
                    }
                }
            } else {
                ui.label("Widget model is not connected to this kernel; re-run the cell");
            }
        }
        if let Some(page) = data
            .get("application/vnd.rustpy.table+json")
            .or_else(|| data.get("application/json"))
            .and_then(|text| serde_json::from_str::<rustpy_console::DataPage>(text).ok())
        {
            if ui
                .button("View result DataFrame / sort / filter / export")
                .clicked()
            {
                self.workspace.table = Some(page.clone());
                events.push(NotebookEvent::Data(page));
            }
        }
        for mime in [
            "image/png",
            "image/jpeg",
            "image/svg+xml",
            "text/html",
            "text/latex",
            "application/vnd.plotly.v1+json",
            "application/vnd.bokehjs_exec.v0+json",
            "application/vnd.vegalite.v5+json",
        ] {
            if let Some(payload) = data.get(mime) {
                if ui
                    .add_enabled(
                        self.workspace.trusted && self.workspace.settings.plots,
                        egui::Button::new(format!("Open in SciView / web viewer ({mime})")),
                    )
                    .clicked()
                {
                    let actual_mime = if mime == "text/latex" {
                        "text/html"
                    } else {
                        mime
                    };
                    let bytes = if mime == "text/latex" {
                        format!(r#"<!doctype html><meta charset=utf-8><script async src="https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-svg.js"></script><div>\[{}\]</div>"#,payload.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;")).into_bytes()
                    } else if matches!(mime, "image/png" | "image/jpeg") {
                        base64::engine::general_purpose::STANDARD
                            .decode(payload)
                            .unwrap_or_default()
                    } else {
                        payload.as_bytes().to_vec()
                    };
                    events.push(NotebookEvent::RichOutput {
                        mime: actual_mime.into(),
                        data: bytes,
                        trusted: self.workspace.trusted,
                        title: format!("Notebook cell {}", cell + 1),
                    });
                }
            }
        }
        if let Some(latex) = data.get("text/latex") {
            ui.monospace(latex);
        }
        if let Some(svg) = data
            .get("image/svg+xml")
            .filter(|_| self.workspace.settings.plots)
        {
            let key = blake3::hash(svg.as_bytes()).to_hex().to_string();
            if let Some(result) = self
                .workspace
                .svg_jobs
                .get(&key)
                .and_then(|job| job.try_recv().ok())
            {
                self.workspace.svg_jobs.remove(&key);
                match result {
                    Ok(raster) => {
                        let mut texture = crate::editor::svg_preview::SvgTexture::default();
                        texture.publish(ui.ctx(), format!("notebook-svg-{key}"), raster);
                        if self.workspace.svg_cache.len() >= 16 {
                            self.workspace.svg_cache.clear();
                        }
                        self.workspace.svg_cache.insert(key.clone(), texture);
                    }
                    Err(_) => {
                        self.workspace.svg_errors.insert(key.clone());
                    }
                }
            }
            if let Some(texture) = self.workspace.svg_cache.get(&key) {
                texture.show(ui);
            } else if !self.workspace.svg_jobs.contains_key(&key)
                && !self.workspace.svg_errors.contains(&key)
                && self.workspace.svg_jobs.len() < 2
            {
                let source = svg.clone();
                let (tx, rx) = crossbeam_channel::bounded(1);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let mut fonts = resvg::usvg::fontdb::Database::new();
                    fonts.load_font_data(crate::theme::hack_regular().to_vec());
                    let result = crate::editor::svg_preview::rasterize_cancellable(
                        &source,
                        std::sync::Arc::new(fonts),
                        rustpy_core::CancellationToken::new(),
                    );
                    let _ = tx.send(result);
                    ctx.request_repaint();
                });
                self.workspace.svg_jobs.insert(key, rx);
                crate::a11y::spinner(ui);
            }

            if let Some(text) = data.get("text/plain") {
                ui.label(text);
            }
            ui.label(format!(
                "SVG output: {} bytes; open the trusted plot viewer to render",
                svg.len()
            ));
        }
    }
}
pub(super) fn ansi_job(
    text: &str,
    font: egui::FontId,
    default: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut state = rustpy_run::ansi::SgrState::default();
    for line in text.split_inclusive('\n') {
        for span in rustpy_run::ansi::parse_line(line, &mut state) {
            let color = span
                .fg_rgb
                .map(|rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                .or_else(|| {
                    span.fg.map(|index| match index % 8 {
                        0 => egui::Color32::DARK_GRAY,
                        1 => egui::Color32::LIGHT_RED,
                        2 => egui::Color32::LIGHT_GREEN,
                        3 => egui::Color32::YELLOW,
                        4 => egui::Color32::LIGHT_BLUE,
                        5 => egui::Color32::LIGHT_RED,
                        6 => egui::Color32::LIGHT_BLUE,
                        _ => egui::Color32::WHITE,
                    })
                })
                .unwrap_or(default);
            job.append(
                &span.text,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color,
                    italics: span.italic,
                    underline: if span.underline {
                        egui::Stroke::new(1.0, color)
                    } else {
                        egui::Stroke::NONE
                    },
                    ..Default::default()
                },
            );
        }
    }
    job
}

fn notebook_bytes(notebook: &Notebook) -> usize {
    notebook
        .cells
        .iter()
        .map(|cell| {
            cell.source.len()
                + cell
                    .outputs
                    .iter()
                    .map(|output| match output {
                        Output::Stream { text, .. } => text.len(),
                        Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => {
                            data.values().map(String::len).sum()
                        }
                        Output::Error { traceback, .. } => traceback.iter().map(String::len).sum(),
                        Output::Unknown(value) => serde_json::to_vec(value)
                            .map(|encoded| encoded.len())
                            .unwrap_or(usize::MAX),
                    })
                    .sum::<usize>()
        })
        .sum()
}
pub(super) fn markdown_web(source: &str) -> String {
    let mut html = String::from(
        r#"<!doctype html><meta charset="utf-8"><script>window.MathJax={tex:{inlineMath:[['$','$'],['\\(','\\)']]}};</script><script async src="https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-svg.js"></script>"#,
    );
    pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(source));
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_execution_outputs_keep_trust_but_external_outputs_revoke_it() {
        let stream = |text: &str| Output::Stream {
            name: "stdout".into(),
            text: text.into(),
            extra: Default::default(),
        };
        let mut tab = NotebookTab::from_notebook(rustpy_notebook::new_notebook());
        let signed = tab.trust_digest();
        assert_eq!(tab.trust_against(None), (false, None));
        assert_eq!(tab.trust_against(Some(&signed)), (true, None));
        {
            let mut data = tab.data.lock().unwrap();
            data.nb.cells[0].outputs.push(stream("own execution\n"));
            data.note_output_changes(&[0]);
        }
        let (trusted, resigned) = tab.trust_against(Some(&signed));
        assert!(
            trusted,
            "own kernel outputs keep a trusted notebook trusted"
        );
        let resigned = resigned.expect("own outputs are re-signed");
        assert_ne!(resigned, signed);
        assert!(tab.take_resign_unsaved());
        assert!(!tab.take_resign_unsaved());
        assert_eq!(tab.trust_against(Some(&resigned)), (true, None));
        assert_eq!(tab.trust_against(Some("unrelated")), (false, None));
        // Outputs changed on disk (reloaded model) need explicit trust again,
        // including for later kernel outputs on top of them.
        let mut external = tab.model();
        external.cells[0]
            .outputs
            .push(stream("<script>external</script>"));
        tab.replace_model(external.clone());
        assert_eq!(tab.trust_against(Some(&resigned)), (false, None));
        {
            let mut data = tab.data.lock().unwrap();
            data.nb.cells[0].outputs.push(stream("own execution\n"));
            data.note_output_changes(&[0]);
        }
        assert_eq!(tab.trust_against(Some(&resigned)), (false, None));
        let reopened = NotebookTab::from_notebook(external);
        assert_eq!(reopened.trust_against(Some(&resigned)), (false, None));
    }
    #[test]
    fn jupyter_variables_hide_ipython_internals_unless_special_variables_are_shown() {
        let mut workspace = Workspace::default();
        for (name, type_name) in [
            ("In", "list"),
            ("Out", "dict"),
            ("quit", "ExitAutocall"),
            ("_oh", "dict"),
            ("_i2", "str"),
            ("frame", "DataFrame"),
        ] {
            workspace.variables.push(rustpy_console::VarInfo {
                name: name.into(),
                expr: name.into(),
                type_name: type_name.into(),
                value: String::new(),
                is_callable: name == "quit",
                is_module: false,
                has_children: false,
            });
        }
        let names = |workspace: &Workspace| {
            workspace
                .visible_variables()
                .into_iter()
                .map(|variable| variable.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&workspace), ["frame"]);
        workspace.show_special_variables = true;
        assert_eq!(names(&workspace).len(), 6);
    }
    #[test]
    fn split_merge_and_undo_preserve_shared_cell_authority_without_kernel() {
        let mut data = NotebookData::from_notebook(rustpy_notebook::new_notebook());
        data.nb.cells[0].source = "left\nright".into();
        data.workspace.caret = 5;
        data.split_cell();
        assert_eq!(data.nb.cells.len(), 2);
        assert_eq!(data.nb.cells[1].source, "right");
        data.merge_cell();
        assert_eq!(data.nb.cells.len(), 1);
        data.undo_cell_operation();
        assert_eq!(data.nb.cells.len(), 2);
        assert!(!data.owns_quit_process());
    }
    #[test]
    fn append_markdown_updates_splits_and_preserves_invalid_json_draft() {
        let mut original = NotebookTab::from_notebook(rustpy_notebook::new_notebook());
        let split = original.split_view();
        original.append_markdown("# Heading".into()).unwrap();
        assert_eq!(split.model().cells.len(), 2);
        {
            let mut data = original.data.lock().unwrap();
            data.workspace.json_mode = true;
            data.workspace.json_text = Some("{ invalid".into());
        }
        assert!(original
            .append_markdown("must not replace draft".into())
            .is_err());
        assert_eq!(original.text(), "{ invalid");
    }
    #[test]
    fn notebook_metadata_cannot_grant_active_output_trust() {
        let mut notebook = rustpy_notebook::new_notebook();
        notebook.metadata.insert("trusted".into(), true.into());
        let data = NotebookData::from_notebook(notebook);
        assert!(!data.workspace.trusted);
        assert!(!data.workspace.execution_allowed);
    }
}

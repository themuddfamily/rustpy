//! Jupyter notebook editor (`.ipynb`): code/Markdown/raw cells with
//! highlighted editors, outputs (text, images, errors), a kernel per notebook
//! and Jupyter-style keys (Shift+Enter, Ctrl+Enter, Alt+Enter; in command mode
//! A/B insert, D D delete, M/Y change type, J/K or arrows move, Enter edits).

use crate::a11y::AccessibleCombo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use egui::{Color32, Key, Modifiers, RichText};
use rustpy_highlight::{ColorScheme, HighlightTag};
use rustpy_notebook::{Cell, CellKind, CellRunner, CellStatus, KernelState, Notebook, Output};

use crate::theme::{rgb, UiTheme};
mod workspace;
use workspace::{ansi_job, Workspace};

/// One notebook model/kernel authority, independently presented by split editors.
pub struct NotebookTab {
    data: std::sync::Arc<std::sync::Mutex<NotebookData>>,
    presentation: NotebookPresentation,
    view_id: egui::Id,
    seen_revision: u64,
    seen_output_revision: u64,
    pub keys_enabled: bool,
    pub editable: bool,
}
#[derive(Default)]
struct NotebookPresentation {
    selected: usize,
    caret: usize,
    editing: Option<usize>,
    focus_cell: Option<usize>,
    textures: HashMap<(String, usize), egui::TextureHandle>,
    pending_d: bool,
    scroll_to_selected: bool,
}
impl NotebookPresentation {
    fn swap(&mut self, data: &mut NotebookData) {
        std::mem::swap(&mut self.selected, &mut data.selected);
        std::mem::swap(&mut self.caret, &mut data.workspace.caret);
        std::mem::swap(&mut self.editing, &mut data.editing);
        std::mem::swap(&mut self.focus_cell, &mut data.focus_cell);
        std::mem::swap(&mut self.textures, &mut data.textures);
        std::mem::swap(&mut self.pending_d, &mut data.pending_d);
        std::mem::swap(&mut self.scroll_to_selected, &mut data.scroll_to_selected);
    }
}
fn notebook_view_id() -> egui::Id {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    egui::Id::new((
        "notebook_instance",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ))
}
impl NotebookTab {
    pub(crate) fn service_snapshot(&self) -> Option<(u64, KernelState)> {
        let data = self.data.lock().expect("notebook authority poisoned");
        if let Some(task) = data.workspace.task.as_ref().filter(|task| task.running()) {
            return Some((task.id, KernelState::Busy));
        }
        let kernel = data.kernel.as_ref()?;
        kernel
            .owns_process()
            .then_some((data.process_epoch, kernel.state()))
    }
    pub(crate) fn restart_service(&mut self) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        data.debugger_endpoint = None;
        if let Some(kernel) = &mut data.kernel {
            kernel.restart();
        }
    }
    pub fn from_notebook(nb: Notebook) -> Self {
        Self {
            data: std::sync::Arc::new(std::sync::Mutex::new(NotebookData::from_notebook(nb))),
            presentation: NotebookPresentation::default(),
            view_id: notebook_view_id(),
            seen_revision: 0,
            seen_output_revision: 0,
            keys_enabled: true,
            editable: true,
        }
    }
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let data = NotebookData::open(path)?;
        Ok(Self {
            data: std::sync::Arc::new(std::sync::Mutex::new(data)),
            presentation: NotebookPresentation::default(),
            view_id: notebook_view_id(),
            seen_revision: 0,
            seen_output_revision: 0,
            keys_enabled: true,
            editable: true,
        })
    }
    pub fn split_view(&self) -> Self {
        Self {
            data: std::sync::Arc::clone(&self.data),
            presentation: NotebookPresentation {
                selected: self.presentation.selected,
                ..Default::default()
            },
            view_id: notebook_view_id(),
            seen_revision: self.seen_revision,
            seen_output_revision: self.seen_output_revision,
            keys_enabled: self.keys_enabled,
            editable: self.editable,
        }
    }
    /// Stable per-instance owner of a cell editor, independent of tab indices.
    pub fn cell_editor_id(&self, path: &Path, index: usize) -> egui::Id {
        egui::Id::new(("nb_cell", self.view_id, path, index))
    }
    pub fn shares_authority(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.data, &other.data)
    }
    pub fn authority_id(&self) -> usize {
        std::sync::Arc::as_ptr(&self.data) as usize
    }
    pub fn model(&self) -> Notebook {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .nb
            .clone()
    }
    pub fn text(&self) -> String {
        let data = self.data.lock().expect("notebook authority poisoned");
        if data.workspace.json_mode {
            if let Some(text) = &data.workspace.json_text {
                return text.clone();
            }
        }
        data.nb.to_string()
    }
    pub fn to_json(&self) -> serde_json::Value {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .nb
            .to_json()
    }
    pub fn is_dirty(&self) -> bool {
        self.data.lock().expect("notebook authority poisoned").dirty
    }
    pub fn set_dirty(&mut self, dirty: bool) {
        self.data.lock().expect("notebook authority poisoned").dirty = dirty;
    }
    pub fn synced_version(&self) -> Option<u64> {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .synced_version
    }
    pub fn set_synced_version(&mut self, version: Option<u64>) {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .synced_version = version;
    }
    pub fn set_cell_source(&mut self, index: usize, source: String) {
        let mut data = self.data.lock().expect("notebook authority poisoned");
        if let Some(cell) = data.nb.cells.get_mut(index) {
            cell.source = source;
            data.dirty = true;
            data.revision = data.revision.wrapping_add(1);
        }
    }
    pub fn selected(&self) -> usize {
        self.presentation.selected
    }
    pub fn select(&mut self, index: usize) {
        self.presentation.scroll_to_selected = true;
        self.presentation.selected = index.min(
            self.data
                .lock()
                .expect("notebook authority poisoned")
                .nb
                .cells
                .len()
                .saturating_sub(1),
        );
    }
    pub fn replace_model(&mut self, nb: Notebook) {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .replace_model(nb);
    }
    pub fn is_busy(&self) -> bool {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .is_busy()
    }
    pub(crate) fn take_debugger_endpoint(&mut self) -> Option<Result<(String, u16), String>> {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .debugger_endpoint
            .take()
    }
    pub fn poll(&mut self) -> bool {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .poll()
    }
    pub(crate) fn quit_process_state(&self) -> String {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .quit_process_state()
    }
    pub(crate) fn stop_quit_process(&mut self) {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .stop_quit_process();
    }
    pub(crate) fn owns_quit_process(&self) -> bool {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .owns_quit_process()
    }
    pub fn shutdown(&mut self) {
        self.data
            .lock()
            .expect("notebook authority poisoned")
            .shutdown();
    }
    fn refresh_presentation(&mut self, data: &NotebookData) {
        if self.seen_revision != data.revision {
            self.presentation.editing = None;
            self.presentation.focus_cell = None;
        }
        if self.seen_revision != data.revision || self.seen_output_revision != data.output_revision
        {
            self.presentation.textures.clear();
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        scheme: &ColorScheme,
        mono: egui::FontId,
        interpreter: Option<&Path>,
        cwd: &Path,
        path: &Path,
    ) -> Vec<NotebookEvent> {
        let authority = std::sync::Arc::clone(&self.data);
        let mut data = authority.lock().expect("notebook authority poisoned");
        self.refresh_presentation(&data);
        self.presentation.swap(&mut data);
        data.view_id = self.view_id;
        data.keys_enabled = self.keys_enabled;
        let events = ui
            .push_id(self.view_id, |ui| {
                ui.add_enabled_ui(self.editable, |ui| {
                    data.ui(ui, theme, scheme, mono, interpreter, cwd, path)
                })
                .inner
            })
            .inner;
        self.seen_revision = data.revision;
        self.seen_output_revision = data.output_revision;
        self.presentation.swap(&mut data);
        events
    }
}

struct NotebookData {
    workspace: Workspace,
    pub nb: Notebook,
    revision: u64,
    output_revision: u64,
    view_id: egui::Id,
    pub selected: usize,
    /// Cell whose editor has keyboard focus (edit mode); `None` = command mode.
    editing: Option<usize>,
    focus_cell: Option<usize>,
    kernel: Option<CellRunner>,
    process_epoch: u64,
    debugger_endpoint: Option<Result<(String, u16), String>>,
    pub kernel_error: Option<String>,
    /// The model changed since the document text was last synced.
    pub dirty: bool,
    /// Decoded image outputs by (cell id or index, output index).
    textures: HashMap<(String, usize), egui::TextureHandle>,
    pending_d: bool,
    scroll_to_selected: bool,
    /// Set by the app: false while a dialog or popup owns the keyboard.
    pub keys_enabled: bool,
    /// Version of the tab's document the model matches (the app reloads the
    /// model when undo/redo or a reload changes the document).
    pub synced_version: Option<u64>,
}

pub enum NotebookEvent {
    Breakpoints {
        path: PathBuf,
        lines: Vec<usize>,
    },
    Trust,
    Console(String),
    Terminal,
    ImportPython(String),
    Data(rustpy_console::DataPage),
    RichOutput {
        mime: String,
        data: Vec<u8>,
        trusted: bool,
        title: String,
    },
    Semantic {
        kind: String,
        cell: usize,
        offset: usize,
        value: String,
    },

    Debugger {
        host: String,
        port: u16,
    },
    Status(String),
}

fn cell_key(cell: &Cell, index: usize) -> String {
    cell.id.clone().unwrap_or_else(|| format!("#{index}"))
}

/// Strips ANSI escape sequences (tracebacks are colored by IPython-style formatters).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Plain text of an HTML snippet (tags removed, common entities decoded).
fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn highlight_job(
    text: &str,
    scheme: &ColorScheme,
    font: &egui::FontId,
    wrap: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let spans = rustpy_highlight::highlight_python(text);
    let default = egui::TextFormat {
        font_id: font.clone(),
        color: rgb(scheme.editor.foreground),
        ..Default::default()
    };
    let fmt = |tag: HighlightTag| {
        let st = scheme.style(tag);
        egui::TextFormat {
            font_id: font.clone(),
            color: st.fg.map(rgb).unwrap_or(default.color),
            italics: st.italic,
            ..Default::default()
        }
    };
    let mut pos = 0;
    for s in spans {
        let (a, b) = (
            usize::from(s.range.start()).min(text.len()),
            usize::from(s.range.end()).min(text.len()),
        );
        if a < pos || b <= a {
            continue;
        }
        if a > pos {
            job.append(&text[pos..a], 0.0, default.clone());
        }
        job.append(&text[a..b], 0.0, fmt(s.tag));
        pos = b;
    }
    if pos < text.len() {
        job.append(&text[pos..], 0.0, default.clone());
    }
    job.wrap.max_width = wrap;
    job
}

impl NotebookData {
    /// Observational identity of the retained process owner; does not poll it.
    pub(crate) fn quit_process_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.process_epoch,
                (
                    self.kernel.as_ref().map(|owner| owner as *const _),
                    self.workspace
                        .task
                        .as_ref()
                        .filter(|task| task.running())
                        .map(|task| (task.id, &task.label))
                )
            )
        )
    }
    pub(crate) fn stop_quit_process(&mut self) {
        self.shutdown();
    }
    pub(crate) fn owns_quit_process(&self) -> bool {
        self.kernel
            .as_ref()
            .is_some_and(|kernel| kernel.owns_process())
            || self
                .workspace
                .task
                .as_ref()
                .is_some_and(|task| task.running())
    }

    pub fn open(path: &Path) -> anyhow::Result<NotebookData> {
        let mut nb = Notebook::load(path)?;
        if nb.cells.is_empty() {
            nb.insert_cell(0, Cell::code(""));
        }
        Ok(Self::from_notebook(nb))
    }

    /// Replaces the model after the document changed underneath (undo/redo).
    pub fn replace_model(&mut self, nb: Notebook) {
        self.nb = nb;
        self.revision = self.revision.wrapping_add(1);
        self.selected = self.selected.min(self.nb.cells.len().saturating_sub(1));
        self.editing = None;
        self.focus_cell = None;
        self.textures.clear();
    }

    pub fn from_notebook(nb: Notebook) -> NotebookData {
        NotebookData {
            workspace: Workspace::default(),
            nb,
            revision: 0,
            output_revision: 0,
            view_id: egui::Id::new("notebook_unmounted"),
            selected: 0,
            editing: None,
            focus_cell: None,
            kernel: None,
            process_epoch: 0,
            debugger_endpoint: None,
            kernel_error: None,
            dirty: false,
            textures: HashMap::new(),
            pending_d: false,
            scroll_to_selected: false,
            keys_enabled: true,
            synced_version: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.kernel
            .as_ref()
            .is_some_and(|kernel| kernel.is_busy() || kernel.state() == KernelState::Starting)
            || self
                .workspace
                .task
                .as_ref()
                .is_some_and(|task| task.running())
    }

    /// Drains kernel events into the model; returns true when cells changed.
    pub fn poll(&mut self) -> bool {
        let task_changed = self.workspace.task.as_mut().is_some_and(|task| task.poll());
        let Some(k) = &mut self.kernel else {
            return task_changed;
        };
        let changed = k.poll(&mut self.nb);
        for event in k.take_events() {
            match event {
                rustpy_console::ConsoleEvent::Debugger {
                    host, port, error, ..
                } => {
                    self.debugger_endpoint = Some(match error {
                        Some(error) => Err(error),
                        None => Ok((host, port)),
                    })
                }
                rustpy_console::ConsoleEvent::Vars { items, .. }
                | rustpy_console::ConsoleEvent::Children { items, .. } => {
                    self.workspace.variables = items
                }
                rustpy_console::ConsoleEvent::Data { table, error, .. } => {
                    self.workspace.table = table;
                    if let Some(error) = error {
                        self.workspace.logs.push_str(&error);
                    }
                }
                rustpy_console::ConsoleEvent::Completions { id, items, .. } => {
                    if self.workspace.completion_owner.as_ref().is_some_and(
                        |(request, key, source, _)| {
                            *request == id
                                && self.nb.cells.iter().enumerate().any(|(index, cell)| {
                                    cell_key(cell, index) == *key && cell.source == *source
                                })
                        },
                    ) {
                        self.workspace.completions =
                            items.into_iter().map(|item| item.name).collect();
                    }
                }
                rustpy_console::ConsoleEvent::Stdout(text)
                | rustpy_console::ConsoleEvent::Stderr(text) => {
                    self.workspace.logs.push_str(&text);
                    self.workspace.console_log.push_str(&text);
                }
                _ => {}
            }
        }
        if self.workspace.logs.len() > 256 * 1024 {
            let mut start = self.workspace.logs.len() - 256 * 1024;
            while !self.workspace.logs.is_char_boundary(start) {
                start += 1;
            }
            self.workspace.logs.drain(..start);
        }
        for index in &changed {
            if let Some(cell) = self.nb.cells.get(*index) {
                let id = cell_key(cell, *index);
                let text = cell
                    .outputs
                    .iter()
                    .map(|output| match output {
                        Output::Stream { text, .. } => text.clone(),
                        Output::ExecuteResult { data, .. } | Output::DisplayData { data, .. } => {
                            data.get("text/plain").cloned().unwrap_or_default()
                        }
                        Output::Error { traceback, .. } => traceback.join("\n"),
                        _ => String::new(),
                    })
                    .collect::<String>();
                let seen = self.workspace.console_seen.entry(id).or_default();
                let mut start = (*seen).min(text.len());
                while !text.is_char_boundary(start) {
                    start -= 1;
                }
                self.workspace.console_log.push_str(&text[start..]);
                *seen = text.len();
            }
        }
        if self.workspace.console_log.len() > 256 * 1024 {
            let mut start = self.workspace.console_log.len() - 256 * 1024;
            while !self.workspace.console_log.is_char_boundary(start) {
                start += 1;
            }
            self.workspace.console_log.drain(..start);
        }
        if !changed.is_empty() && self.workspace.settings.autoscroll {
            if let Some(index) = k.running_cell(&self.nb) {
                self.selected = index;
                self.scroll_to_selected = true;
            }
        }
        self.note_output_changes(&changed) || task_changed
    }

    fn note_output_changes(&mut self, changed: &[usize]) -> bool {
        if !changed.is_empty() {
            self.dirty = true;
            self.output_revision = self.output_revision.wrapping_add(1);
            for i in changed {
                if let Some(c) = self.nb.cells.get(*i) {
                    let key = cell_key(c, *i);
                    self.textures.retain(|(k, _), _| k != &key);
                }
            }
            return true;
        }
        false
    }

    fn ensure_kernel(&mut self, interpreter: &Path, cwd: &Path) -> bool {
        if !self.workspace.execution_allowed {
            self.kernel_error =
                Some("Trust the project before starting notebook kernels or servers".into());
            return false;
        }
        if let Some(kernel) = &self.kernel {
            if kernel.state() != KernelState::Dead {
                return true;
            }
            if kernel.owns_process() {
                self.kernel_error = Some("Kernel is stopping; wait before starting another".into());
                return false;
            }
        }
        self.kernel = None;
        let env = rustpy_net::proxy_env_vars();
        match CellRunner::start_with_connection(
            interpreter,
            cwd,
            &env,
            "",
            self.workspace.connection.clone(),
        ) {
            Ok(mut k) => {
                k.set_output_limit(self.workspace.settings.max_output_bytes);
                self.process_epoch = crate::app::window_lifecycle::next_process_owner();
                self.kernel = Some(k);
                self.kernel_error = None;
                true
            }
            Err(e) => {
                self.kernel_error = Some(format!("Cannot start the kernel: {e}"));
                false
            }
        }
    }

    fn run(&mut self, indices: Vec<usize>, interpreter: &Path, cwd: &Path) {
        if !indices.iter().any(|index| {
            self.nb
                .cells
                .get(*index)
                .is_some_and(|cell| cell.is_code() && !cell.source.trim().is_empty())
        }) {
            for index in indices {
                if let Some(cell) = self.nb.cells.get_mut(index).filter(|cell| cell.is_code()) {
                    cell.clear_outputs();
                    self.dirty = true;
                }
            }
            self.editing = None;
            return;
        }
        if !self.ensure_kernel(interpreter, cwd) {
            return;
        }
        if let Some(k) = &mut self.kernel {
            k.run_cells(&mut self.nb, indices);
            self.dirty = true;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn insert(&mut self, at: usize, kind: CellKind) {
        self.record_edit();
        let at = at.min(self.nb.cells.len());
        self.nb.insert_cell(at, Cell::new(kind, ""));
        self.selected = at;
        self.focus_cell = Some(at);
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        self.scroll_to_selected = true;
    }

    fn delete_selected(&mut self) {
        self.record_edit();
        if self.nb.cells.is_empty() {
            self.insert(0, CellKind::Code);
            return;
        }
        if self.nb.cells.len() <= 1 {
            self.nb.cells[0] = Cell::code("");
        } else {
            self.nb.delete_cell(self.selected);
            self.selected = self.selected.min(self.nb.cells.len().saturating_sub(1));
        }
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    fn set_kind(&mut self, kind: CellKind) {
        self.record_edit();
        if let Some(c) = self.nb.cells.get_mut(self.selected) {
            if c.kind != kind {
                let source = std::mem::take(&mut c.source);
                *c = Cell::new(kind, source);
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    fn image_texture(
        &mut self,
        ctx: &egui::Context,
        key: (String, usize),
        b64: &str,
    ) -> Option<egui::TextureHandle> {
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        if b64.len() > self.workspace.settings.max_output_bytes.saturating_mul(4) / 3 + 1024 {
            return None;
        }
        let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(clean)
            .ok()?;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let img = reader.decode().ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        let tex = ctx.load_texture(
            format!("nb-img-{}-{}", key.0, key.1),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(key, tex.clone());
        Some(tex)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        scheme: &ColorScheme,
        mono: egui::FontId,
        interpreter: Option<&Path>,
        cwd: &Path,
        path: &Path,
    ) -> Vec<NotebookEvent> {
        let mut events = std::mem::take(&mut self.workspace.pending_events);
        if let Some(endpoint) = self.debugger_endpoint.take() {
            match endpoint {
                Ok((host, port)) => events.push(NotebookEvent::Debugger { host, port }),
                Err(error) => events.push(NotebookEvent::Status(error)),
            }
        }
        let python: PathBuf = interpreter
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
        if self.is_busy() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        let n = self.nb.cells.len();
        self.selected = self.selected.min(n.saturating_sub(1));

        self.workspace_toolbar(ui, &python, cwd, path, &mut events);
        if let Some((source, line)) = &self.workspace.debug_snapshot {
            egui::CollapsingHeader::new(format!(
                "Captured debugger source · line {line} (live cell changed)"
            ))
            .default_open(true)
            .show(ui, |ui| {
                for (index, text) in source
                    .lines()
                    .enumerate()
                    .filter(|(index, _)| index.abs_diff(line.saturating_sub(1)) <= 3)
                {
                    ui.monospace(format!(
                        "{} {:>4} {}",
                        if index + 1 == *line { "▶" } else { " " },
                        index + 1,
                        text
                    ));
                }
            });
        }

        self.workspace_keys(ui);
        if self.workspace.json_mode {
            let mut text = self
                .workspace
                .json_text
                .clone()
                .unwrap_or_else(|| self.nb.to_string());
            if ui
                .add(
                    egui::TextEdit::multiline(&mut text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(30),
                )
                .changed()
            {
                self.workspace.json_text = Some(text.clone());
                self.workspace.trust_cache = None;
                self.dirty = true;
                match text.parse() {
                    Ok(model) => {
                        self.nb = model;
                        self.dirty = true;
                        self.revision = self.revision.wrapping_add(1);
                    }
                    Err(error) => {
                        events.push(NotebookEvent::Status(format!("Notebook JSON: {error}")))
                    }
                }
            }
            return events;
        }
        // ---- keyboard ----------------------------------------------------------
        let run_next =
            self.keys_enabled && ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter));
        let run_here =
            self.keys_enabled && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
        let run_insert =
            self.keys_enabled && ui.input_mut(|i| i.consume_key(Modifiers::ALT, Key::Enter));
        if run_next || run_here || run_insert {
            let i = self.selected;
            if self.nb.cells.get(i).is_some_and(|c| c.is_code()) {
                self.run(vec![i], &python, cwd);
            } else {
                self.editing = None;
            }
            if run_insert {
                self.insert(i + 1, CellKind::Code);
            } else if run_next {
                if i + 1 >= self.nb.cells.len() {
                    self.insert(i + 1, CellKind::Code);
                } else {
                    self.selected = i + 1;
                    self.scroll_to_selected = true;
                    if self.editing.is_some() {
                        self.focus_cell = Some(i + 1);
                    }
                }
            }
        }
        // Command-mode keys only when no text field anywhere has the keyboard
        // (another tool window's input or a dialog must keep its typing).
        let any_focus = ui.ctx().memory(|m| m.focused()).is_some();
        if self.keys_enabled
            && self.editing.is_some()
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.editing = None;
            if let Some(focused) = ui.ctx().memory(|m| m.focused()) {
                ui.ctx().memory_mut(|m| m.surrender_focus(focused));
            }
        }
        if self.keys_enabled && !any_focus && self.editing.is_none() {
            let pressed = |k: Key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, k));
            if pressed(Key::A) {
                self.insert(self.selected, CellKind::Code);
            } else if pressed(Key::B) {
                self.insert(self.selected + 1, CellKind::Code);
            } else if pressed(Key::M) {
                self.set_kind(CellKind::Markdown);
            } else if pressed(Key::Y) {
                self.set_kind(CellKind::Code);
            } else if pressed(Key::R) {
                self.set_kind(CellKind::Raw);
            } else if pressed(Key::D) {
                if self.pending_d {
                    self.delete_selected();
                    self.pending_d = false;
                } else {
                    self.pending_d = true;
                }
            } else if pressed(Key::J) || pressed(Key::ArrowDown) {
                self.selected = (self.selected + 1).min(self.nb.cells.len().saturating_sub(1));
                self.scroll_to_selected = true;
            } else if pressed(Key::K) || pressed(Key::ArrowUp) {
                self.selected = self.selected.saturating_sub(1);
                self.scroll_to_selected = true;
            } else if pressed(Key::Enter) {
                self.focus_cell = Some(self.selected);
            }
        }

        // ---- toolbar -----------------------------------------------------------
        let state = self.kernel.as_ref().map(|k| k.state());
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("▶ Run").color(theme.success))
                .on_hover_text("Run the selected cell (Shift+Enter runs and advances)")
                .clicked()
            {
                let i = self.selected;
                if self.nb.cells.get(i).is_some_and(|c| c.is_code()) {
                    self.run(vec![i], &python, cwd);
                }
            }
            if ui
                .add_enabled(
                    !self.is_busy()
                        && self
                            .nb
                            .cells
                            .get(self.selected)
                            .is_some_and(|cell| cell.is_code() && !cell.source.trim().is_empty()),
                    egui::Button::new("Debug Cell"),
                )
                .clicked()
                && self.ensure_kernel(&python, cwd)
            {
                if let Some(kernel) = &mut self.kernel {
                    kernel.debug_cell(&mut self.nb, self.selected);
                }
            }
            if ui.button("Run All").clicked() {
                let all: Vec<usize> = (0..self.nb.cells.len()).collect();
                self.run(all, &python, cwd);
            }
            if ui.button("Run Above").clicked() {
                let above: Vec<usize> = (0..self.selected).collect();
                self.run(above, &python, cwd);
            }
            if ui.button("Run Below").clicked() {
                let below: Vec<usize> = (self.selected..self.nb.cells.len()).collect();
                self.run(below, &python, cwd);
            }
            if ui
                .add_enabled(self.is_busy(), egui::Button::new("Interrupt"))
                .clicked()
            {
                if let Some(k) = &mut self.kernel {
                    k.interrupt();
                }
            }
            if ui
                .button("Restart")
                .on_hover_text("Restart the kernel (variables are lost)")
                .clicked()
            {
                self.debugger_endpoint = None;
                if let Some(k) = &mut self.kernel {
                    k.restart();
                    events.push(NotebookEvent::Status("Kernel restarted".into()));
                }
            }
            if ui.button("Clear Outputs").clicked() {
                self.nb.clear_outputs();
                self.textures.clear();
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
            ui.separator();
            if ui.button("+ Code").clicked() {
                self.insert(self.selected + 1, CellKind::Code);
            }
            if ui.button("+ Markdown").clicked() {
                self.insert(self.selected + 1, CellKind::Markdown);
            }
            if ui.button("Delete").clicked() {
                self.delete_selected();
            }
            if ui.button("Up").clicked() && self.selected > 0 {
                self.nb.move_cell(self.selected, self.selected - 1);
                self.selected -= 1;
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
            if ui.button("Down").clicked() && self.selected + 1 < self.nb.cells.len() {
                self.nb.move_cell(self.selected, self.selected + 1);
                self.selected += 1;
                self.dirty = true;
                self.revision = self.revision.wrapping_add(1);
            }
            let cur = self
                .nb
                .cells
                .get(self.selected)
                .map(|c| c.kind)
                .unwrap_or(CellKind::Code);
            egui::ComboBox::from_id_salt(("nb_kind", path))
                .selected_text(match cur {
                    CellKind::Code => "Code",
                    CellKind::Markdown => "Markdown",
                    CellKind::Raw => "Raw",
                })
                .show_ui(ui, |ui| {
                    for (label, k) in [
                        ("Code", CellKind::Code),
                        ("Markdown", CellKind::Markdown),
                        ("Raw", CellKind::Raw),
                    ] {
                        if ui
                            .selectable_label(cur == k, crate::i18n::tr_static(label))
                            .clicked()
                        {
                            self.set_kind(k);
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Cell type"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, color) = match state {
                    None => ("Kernel: not started".to_string(), theme.text_dim),
                    Some(KernelState::Starting) => ("Kernel: starting…".to_string(), theme.warning),
                    Some(KernelState::Idle) => (
                        format!(
                            "Kernel: idle · {}",
                            self.kernel
                                .as_ref()
                                .and_then(|k| k.kernel_version())
                                .unwrap_or("Python")
                        ),
                        theme.success,
                    ),
                    Some(KernelState::Busy) => ("Kernel: busy".to_string(), theme.warning),
                    Some(KernelState::Dead) => ("Kernel: stopped".to_string(), theme.error),
                };
                ui.label(RichText::new(label).color(color).small());
            });
        });
        if let Some(e) = &self.kernel_error {
            ui.colored_label(theme.error, e);
        }
        ui.separator();

        // ---- cells -------------------------------------------------------------
        let wrap = (ui.available_width() - 90.0).max(200.0);
        let mut select: Option<usize> = None;
        let mut edit_started: Option<usize> = None;
        let mut edited = false;
        let mut cell_action = None;
        egui::ScrollArea::vertical().id_salt(("nb_scroll", path)).auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.nb.cells.len() {
                let status = self.kernel.as_ref().map(|k| k.cell_status(&self.nb.cells[i])).unwrap_or(CellStatus::Idle);
                let is_sel = i == self.selected;
                let stroke = if is_sel { egui::Stroke::new(1.5, if self.editing == Some(i) { theme.success } else { theme.accent }) } else { egui::Stroke::new(1.0, theme.border) };
                let frame = egui::Frame::new().stroke(stroke).corner_radius(4.0).inner_margin(egui::Margin::same(6)).fill(if is_sel { theme.panel_bg } else { Color32::TRANSPARENT });
                let resp = frame.show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let cell = &self.nb.cells[i];
                        let prompt = match (cell.kind, status) {
                            (CellKind::Code, CellStatus::Running) => "[*]".to_string(),
                            (CellKind::Code, CellStatus::Queued) => "[…]".to_string(),
                            (CellKind::Code, _) => format!("[{}]", cell.execution_count.map(|n| n.to_string()).unwrap_or_else(|| " ".into())),
                            (CellKind::Markdown, _) => "md".to_string(),
                            (CellKind::Raw, _) => "raw".to_string(),
                        };
                        ui.add_sized([44.0, 18.0], egui::Label::new(RichText::new(prompt).font(mono.clone()).color(if status == CellStatus::Running { theme.warning } else if cell.outputs.iter().any(|output|matches!(output,Output::Error{..})){theme.error}else { theme.text_dim })));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui|{ui.menu_button("Cell toolbar",|ui|{for(label,action)in [("Run cell","run"),("Run and advance","advance"),("Run above","above"),("Run below","below"),("Debug cell","debug"),("Add above","add-above"),("Add below","add-below"),("Delete","delete"),("Move up","up"),("Move down","down"),("Split at cursor","split"),("Merge below","merge"),("Convert to code","code"),("Convert to Markdown","markdown"),("Convert to raw","raw"),("Clear outputs","clear")]{if ui.button(crate::i18n::tr_static(label)).clicked(){cell_action=Some((i,action));ui.close();}}});let mut selected=self.workspace.selection.contains(&i);if ui.checkbox(&mut selected,"Selected").changed(){if selected{self.workspace.selection.insert(i);}else{self.workspace.selection.remove(&i);}}let collapsed=self.nb.cells[i].metadata.get("collapsed").and_then(|value|value.as_bool()).unwrap_or(false);if ui.small_button(if collapsed{"Expand input"}else{"Collapse input"}).clicked(){self.nb.cells[i].metadata.insert("collapsed".into(),(!collapsed).into());self.dirty=true;}let hidden=self.nb.cells[i].metadata.get("outputs_hidden").and_then(|value|value.as_bool()).unwrap_or(self.workspace.settings.collapse_outputs);if ui.small_button(if hidden{"Expand output"}else{"Collapse output"}).clicked(){self.nb.cells[i].metadata.insert("outputs_hidden".into(),(!hidden).into());self.dirty=true;}
if ui.small_button("Clear cell output").clicked(){self.nb.cells[i].clear_outputs();self.dirty=true;}});
                            let input_collapsed=self.nb.cells[i].metadata.get("collapsed").and_then(|value|value.as_bool()).unwrap_or(false);
                            let kind = self.nb.cells[i].kind;
                            let editing_this = self.editing == Some(i) || self.focus_cell == Some(i);
                            if input_collapsed {ui.label("Input collapsed");} else if kind == CellKind::Markdown && self.workspace.settings.markdown_preview && !editing_this && !self.nb.cells[i].source.trim().is_empty() {
                                // A clickable response may consume Enter itself. Observe the
                                // still-unclaimed key before registering this cell interaction.
                                let edit_key = self.keys_enabled && ui.input(|input|
                                    input.modifiers.is_none() && input.key_pressed(Key::Enter));
                                // Enter can activate the clickable response and retire its
                                // focus during interaction. Retain the exact owner first.
                                let focus_before_interaction = ui.ctx().memory(|memory| memory.focused());
                                let cell = &self.nb.cells[i];
                                let r = ui.push_id(("notebook-markdown", path, cell_key(cell, i), &cell.source),
                                    |ui| crate::markdown_view::render_markdown(ui, theme, &cell.source, 13.0))
                                    .response.interact(egui::Sense::click());
                                if self.workspace.trusted && (cell.source.contains('$')||cell.source.contains("\\("))&&ui.small_button("Typeset Markdown / LaTeX in web viewer").clicked(){events.push(NotebookEvent::RichOutput{mime:"text/html".into(),data:workspace::markdown_web(&cell.source).into_bytes(),trusted:true,title:format!("Notebook Markdown cell {}",i+1)});}
                                let label = format!("Markdown cell {}. Enter edits. {}", i + 1,
                                    cell.source.chars().take(160).collect::<String>());
                                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button,
                                    self.keys_enabled, &label));
                                let keyboard_edit = edit_key && focus_before_interaction == Some(r.id);
                                if self.keys_enabled && r.clicked() {
                                    select = Some(i);
                                }
                                if (self.keys_enabled && r.double_clicked()) || keyboard_edit {
                                    select = Some(i);
                                    self.focus_cell = Some(i);
                                }
                                if keyboard_edit {
                                    ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
                                }
                            } else {
                                let scheme_c = scheme.clone();
                                let mono_c = mono.clone();
                                let mut layouter = move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                    let job = if kind == CellKind::Code {
                                        highlight_job(text.as_str(), &scheme_c, &mono_c, wrap_width)
                                    } else {
                                        let mut j = egui::text::LayoutJob::simple(text.as_str().to_string(), mono_c.clone(), rgb(scheme_c.editor.foreground), wrap_width);
                                        j.wrap.max_width = wrap_width;
                                        j
                                    };
                                    ui.fonts_mut(|f| f.layout_job(job))
                                };
                                let te = egui::TextEdit::multiline(&mut self.nb.cells[i].source).id(egui::Id::new(("nb_cell", self.view_id, path, i))).code_editor().interactive(self.keys_enabled).desired_width(wrap).desired_rows(1).lock_focus(true).layouter(&mut layouter);
                                let mut output=te.show(ui);if let Some((cell,cursor,line))=self.workspace.debug_cursor.filter(|(cell,_,_)|*cell==i){output.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(cursor))));output.state.store(ui.ctx(),output.response.id);ui.label(RichText::new(format!("▶ Debugger stopped at cell line {line}")).color(theme.warning));self.workspace.debug_cursor=None;let _=cell;}let r=output.response;if r.has_focus(){if let Some(cursor)=output.cursor_range{let char_offset=cursor.primary.index;self.workspace.caret=self.nb.cells[i].source.char_indices().nth(char_offset.0).map(|(offset,_)|offset).unwrap_or(self.nb.cells[i].source.len());}}
                                if self.workspace.settings.show_line_numbers {ui.horizontal_wrapped(|ui|{for line in 1..=self.nb.cells[i].source.lines().count().max(1){let active=self.nb.cells[i].metadata.get("rustpy_breakpoints").and_then(|value|value.as_array()).is_some_and(|values|values.iter().any(|value|value.as_u64()==Some(line as u64)));if ui.small_button(format!("{}{}",if active{"●"}else{""},line)).on_hover_text("Toggle cell breakpoint").clicked(){let value=self.nb.cells[i].metadata.entry("rustpy_breakpoints").or_insert_with(||serde_json::json!([]));if !value.is_array(){*value=serde_json::json!([]);}let values=value.as_array_mut().unwrap();if active{values.retain(|value|value.as_u64()!=Some(line as u64));}else{values.push((line as u64).into());}let lines=values.iter().filter_map(|value|value.as_u64().map(|line|line as usize)).collect();if let Some(kernel)=&self.kernel{events.push(NotebookEvent::Breakpoints{path:PathBuf::from(kernel.debug_source_path(self.nb.cells[i].id.as_deref().unwrap_or_default())),lines});}self.dirty=true;}}});}
                                if self.keys_enabled && self.focus_cell == Some(i) {
                                    r.request_focus();
                                    edit_started = Some(i);
                                    self.focus_cell = None;
                                }
                                if r.gained_focus() || r.has_focus() {
                                    edit_started = Some(i);
                                }
                                if r.changed() {
                                    edited = true;
                                }
                            }
                            // Outputs.
                            let outputs = self.nb.cells[i].outputs.clone();
                            let key = cell_key(&self.nb.cells[i], i);
                            let outputs_hidden=self.nb.cells[i].metadata.get("outputs_hidden").and_then(|value|value.as_bool()).unwrap_or(self.workspace.settings.collapse_outputs);
                            for (oi, out) in outputs.iter().enumerate().filter(|_|!outputs_hidden) {
                                match out {
                                    Output::Stream { name, text, .. } => {
                                        let color = if name == "stderr" { theme.error } else { theme.text };
                                        ui.label(ansi_job(text,mono.clone(),color));
                                    }
                                    Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                                        self.rich_output_controls(ui,data,i,&mut events);
                                        if let Some(png) = data.get("image/png").or_else(|| data.get("image/jpeg")).filter(|_|self.workspace.settings.plots) {
                                            if let Some(tex) = self.image_texture(ui.ctx(), (key.clone(), oi), png) {
                                                let size = tex.size_vec2();
                                                let scale = (wrap / size.x).min(1.0);
                                                ui.image((tex.id(), size * scale));
                                                continue;
                                            }
                                        }
                                        let text = data.get("text/plain").cloned().or_else(|| data.get("text/markdown").cloned()).or_else(|| data.get("text/html").map(|h| if h.len()>65536{format!("HTML output ({} bytes)",h.len())}else{html_to_text(h)})).unwrap_or_default();
                                        if let Some(md) = data.get("text/markdown") {
                                            crate::markdown_view::render_markdown(ui, theme, md, 13.0);
                                        } else {
                                            ui.label(RichText::new(text.trim_end_matches('\n')).font(mono.clone()).color(theme.text));
                                        }
                                    }
                                    Output::Error { ename, evalue, traceback, .. } => {
                                        egui::Frame::new().fill(theme.error.gamma_multiply(0.12)).corner_radius(3.0).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                                            ui.label(RichText::new(format!("{ename}: {evalue}")).font(mono.clone()).color(theme.error).strong());
                                            let tb: Vec<String> = traceback.iter().map(|l| strip_ansi(l)).collect();
                                            if !tb.is_empty() {
                                                ui.label(RichText::new(tb.join("\n")).font(mono.clone()).color(theme.text_dim));
                                            }
                                        });
                                    }
                                    Output::Unknown(_) => {
                                        ui.label(RichText::new("(unsupported output)").color(theme.text_dim).small());
                                    }
                                }
                            }
                            if let Some(d) = self.kernel.as_ref().and_then(|k| k.cell_duration(&self.nb.cells[i])) {
                                if self.nb.cells[i].is_code() && status == CellStatus::Idle && self.nb.cells[i].execution_count.is_some() {
                                    ui.label(RichText::new(format!("{:.2} s", d.as_secs_f32())).color(theme.text_dim).small());
                                }
                            }
                        });
                    });
                });
                // Select on a click anywhere in the cell, without a click-sensing
                // widget on top of it (that took the press from the cell's editor
                // and from the Markdown double-click).
                let r = &resp.response;
                if ui.input(|inp| inp.pointer.primary_clicked() && inp.pointer.interact_pos().is_some_and(|p| r.rect.contains(p))) {
                    select = Some(i);
                }
                if is_sel && self.scroll_to_selected {
                    r.scroll_to_me(Some(egui::Align::Center));
                    self.scroll_to_selected = false;
                }
                ui.add_space(6.0);
            }
            // Add-cell affordance at the end.
            ui.horizontal(|ui| {
                ui.add_space(50.0);
                if ui.small_button("+ Code cell").clicked() {
                    self.insert(self.nb.cells.len(), CellKind::Code);
                }
                if ui.small_button("+ Markdown cell").clicked() {
                    self.insert(self.nb.cells.len(), CellKind::Markdown);
                }
            });
        });
        if let Some((index, action)) = cell_action {
            self.cell_action(index, action, &python, cwd);
            select = None;
            edit_started = None;
        }
        if let Some(i) = edit_started {
            self.editing = Some(i);
            self.selected = i;
        } else if self.editing.is_some() && ui.ctx().memory(|m| m.focused()).is_none() {
            self.editing = None;
        }
        if let Some(i) = select {
            self.selected = i;
            self.pending_d = false;
        }
        if edited {
            self.dirty = true;
            self.revision = self.revision.wrapping_add(1);
        }
        if self.workspace.split_console {
            ui.separator();
            ui.label("Notebook console (same kernel)");
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .show(ui, |ui| {
                    ui.monospace(&self.workspace.console_log);
                });
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.workspace.console_input);
                if ui.button("Execute in notebook kernel").clicked() {
                    let source = self.workspace.console_input.clone();
                    self.record_edit();
                    let index = self.nb.insert_cell(self.nb.cells.len(), Cell::code(source));
                    self.run(vec![index], &python, cwd);
                    self.workspace.console_input.clear();
                }
            });
        }
        events
    }

    /// Kernel shutdown when the tab closes.
    pub fn shutdown(&mut self) {
        if let Some(task) = &self.workspace.task {
            task.cancel();
        }
        if let Some(k) = &mut self.kernel {
            k.shutdown();
        }
    }
}

impl Drop for NotebookData {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_and_html_helpers() {
        assert_eq!(
            strip_ansi("\u{1b}[0;31mValueError\u{1b}[0m: bad"),
            "ValueError: bad"
        );
        assert_eq!(
            html_to_text("<table><tr><td>1 &amp; 2</td></tr></table>"),
            "1 & 2"
        );
    }

    #[test]
    fn cell_operations_mark_dirty() {
        let mut t = NotebookData::from_notebook(rustpy_notebook::new_notebook());
        assert!(!t.dirty);
        t.insert(1, CellKind::Markdown);
        assert_eq!(t.nb.cells.len(), 2);
        assert_eq!(t.selected, 1);
        assert!(t.dirty);
        t.set_kind(CellKind::Code);
        assert!(t.nb.cells[1].is_code());
        t.delete_selected();
        assert_eq!(t.nb.cells.len(), 1);
        t.delete_selected();
        assert_eq!(t.nb.cells.len(), 1, "the last cell is cleared, not removed");
    }
    #[test]
    fn split_notebooks_share_model_but_not_selection_or_view_identity() {
        let mut first = NotebookTab::from_notebook(rustpy_notebook::new_notebook());
        first.data.lock().unwrap().insert(1, CellKind::Markdown);
        first.select(0);
        let mut second = first.split_view();
        second.select(1);
        assert!(first.shares_authority(&second));
        assert_eq!(first.authority_id(), second.authority_id());
        assert_ne!(first.view_id, second.view_id);
        assert_eq!(first.selected(), 0);
        assert_eq!(second.selected(), 1);
        second.set_cell_source(1, "π🙂".into());
        assert_eq!(first.model().cells[1].source, "π🙂");
        assert!(first.is_dirty());
        first.set_synced_version(Some(7));
        assert_eq!(second.synced_version(), Some(7));
        drop(first);
        assert_eq!(second.model().cells[1].source, "π🙂");
    }
    #[test]
    fn shared_kernel_outputs_preserve_other_cell_edit_owner_but_model_replacement_retires_it() {
        let mut first = NotebookTab::from_notebook(rustpy_notebook::new_notebook());
        first.data.lock().unwrap().insert(1, CellKind::Markdown);
        first.seen_revision = first.data.lock().unwrap().revision;
        let mut second = first.split_view();
        second.presentation.editing = Some(1);
        second.presentation.focus_cell = Some(1);
        let authority = std::sync::Arc::clone(&first.data);
        {
            let mut data = authority.lock().unwrap();
            data.nb.cells[0].outputs.push(Output::Stream {
                name: "stdout".into(),
                text: "π🙂\n".into(),
                extra: Default::default(),
            });
            assert!(data.note_output_changes(&[0]));
            second.refresh_presentation(&data);
            assert_eq!(second.presentation.editing, Some(1));
            assert_eq!(second.presentation.focus_cell, Some(1));
            assert_eq!(data.nb.cells[0].outputs.len(), 1);
            data.replace_model(rustpy_notebook::new_notebook());
            second.refresh_presentation(&data);
        }
        assert_eq!(second.presentation.editing, None);
        assert_eq!(second.presentation.focus_cell, None);
    }
}

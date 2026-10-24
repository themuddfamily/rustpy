//! Scientific layout, snapshot routing and session-owned refresh requests.
use super::*;
use crate::scientific_panel::{Event, Origin, PlotPayload, Workspace};
use base64::Engine;
use std::collections::HashMap;
#[derive(Default)]
pub(super) struct State {
    pub view: Workspace,
    debug_stamps: HashMap<u64, (u64, u64)>,
    plot_stops: HashMap<u64, u64>,
    plot_preferences: HashMap<u64, bool>,
    web_files: Vec<PathBuf>,
    captures: Vec<(PathBuf, u64)>,
    pub initialized: bool,
    last_document: Option<rustpy_text::DocumentId>,
}
impl Drop for State {
    fn drop(&mut self) {
        for path in &self.web_files {
            let _ = std::fs::remove_file(path);
        }
        for (path, _) in &self.captures {
            let _ = std::fs::remove_file(path);
        }
    }
}
impl IdeApp {
    pub(super) fn scientific_data(
        &mut self,
        name: String,
        origin: Origin,
        page: serde_json::Value,
    ) {
        self.scientific
            .view
            .publish(name, origin, page, &self.settings.scientific);
    }
    pub(super) fn scientific_plot(&mut self, mut payload: PlotPayload) {
        if payload.mime == "text/html" && !self.settings.scientific.capture_interactive {
            return;
        }
        if payload.mime == "image/svg+xml" {
            if let Ok(svg) = std::str::from_utf8(&payload.data) {
                payload.data = format!(
                    "<!doctype html><meta charset=\"utf-8\"><title>Scientific SVG</title>{svg}"
                )
                .into_bytes();
                payload.mime = "text/html".into();
            }
        }
        if self.settings.scientific.show_plots {
            self.scientific
                .view
                .plot(payload, self.settings.scientific.max_plots);
        }
    }
    pub(super) fn prepare_scientific_debug(&mut self, cfg: &mut RunConfig) {
        if !self.settings.scientific.show_plots {
            return;
        }
        let directory = Settings::system_dir().join("scientific/plots");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        let path = directory.join(format!(
            "debug-{}-{}-{}.jsonl",
            std::process::id(),
            self.next_session_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        cfg.env.insert("MPLBACKEND".into(), "Agg".into());
        cfg.env.insert(
            "RUSTPY_SCIENTIFIC_INTERACTIVE".into(),
            if self.settings.scientific.capture_interactive {
                "1"
            } else {
                "0"
            }
            .into(),
        );
        cfg.env.insert(
            "RUSTPY_SCIENTIFIC_OUTPUT".into(),
            path.to_string_lossy().into_owned(),
        );
        self.scientific.captures.push((path, 0));
    }
    pub(super) fn poll_scientific(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        for tab in &mut self.tabs {
            tab.view.scientific_cell_separators = self.settings.scientific.cell_separators;
        }
        self.py_console.scientific_capture(
            self.settings.scientific.show_plots,
            self.settings.scientific.capture_interactive,
        );
        if !project_safe_mode {
            while let Some((owner, host, port, code)) = self.py_console.scientific_debug_ready() {
                let setup = self.debug_launch_setup();
                match DebugSession::attach_endpoint(&host, port, setup.options, setup.breakpoints) {
                    Ok(mut session) => {
                        session.id = self.next_session_id;
                        self.next_session_id += 1;
                        session.name = format!("Scientific console {owner}");
                        self.retain_debug_session();
                        self.debug = Some(session);
                        self.py_console.scientific_execute(owner, &code);
                        self.bottom = Some(BottomTool::Debug);
                    }
                    Err(error) => self.set_status(format!("Cannot debug cell: {error}")),
                }
            }
        }
        let (tables, plots) = self.py_console.scientific_outputs();
        for (name, origin, page) in tables {
            self.scientific_data(name, origin, page);
        }
        for plot in plots {
            self.scientific_plot(plot);
        }
        let mut tables = Vec::new();
        let mut plots = Vec::new();
        for session in self
            .debug
            .iter_mut()
            .chain(self.debug_sessions.iter_mut().map(|(session, _)| session))
        {
            if let Some(page) = &session.backend.data_view {
                let stamp = (session.stop_epoch, session.backend.scientific_revision);
                if self.scientific.debug_stamps.get(&session.id) != Some(&stamp) {
                    self.scientific.debug_stamps.insert(session.id, stamp);
                    tables.push((
                        session.backend.data_expression.clone(),
                        Origin::Debug {
                            session: session.id,
                            epoch: session.stop_epoch,
                        },
                        page.clone(),
                    ));
                }
            }
            let enabled = self.settings.scientific.show_plots;
            if !project_safe_mode
                && session.is_stopped()
                && session.current_frame().is_some()
                && !session.backend.pending_view
                && (self.scientific.plot_preferences.get(&session.id) != Some(&enabled)
                    || enabled
                        && self.scientific.plot_stops.get(&session.id) != Some(&session.stop_epoch))
            {
                self.scientific
                    .plot_stops
                    .insert(session.id, session.stop_epoch);
                self.scientific.plot_preferences.insert(session.id, enabled);
                session.inspect_value("None", if enabled { "plots" } else { "plots_disabled" });
            }
            for plot in std::mem::take(&mut session.backend.scientific_plots) {
                if let (Some(mime), Some(encoded)) =
                    (plot["mime"].as_str(), plot["data_base64"].as_str())
                {
                    if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                        plots.push(PlotPayload {
                            source: format!("Debugger {}", session.name),
                            mime: mime.into(),
                            data,
                            title: plot["title"].as_str().unwrap_or("Debug figure").into(),
                        });
                    }
                }
            }
        }
        for (name, origin, page) in tables {
            self.scientific_data(name, origin, page);
        }
        for plot in plots {
            self.scientific_plot(plot);
        }
        let mut files = Vec::new();
        for (path, offset) in &mut self.scientific.captures {
            use std::io::{Read, Seek, SeekFrom};
            if let Ok(mut file) = std::fs::File::open(path) {
                if file.seek(SeekFrom::Start(*offset)).is_ok() {
                    let mut bytes = Vec::new();
                    if file.take(32 * 1024 * 1024).read_to_end(&mut bytes).is_ok() {
                        if let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') {
                            *offset += (end + 1) as u64;
                            for line in bytes[..=end].split(|byte| *byte == b'\n') {
                                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line)
                                {
                                    files.push(value);
                                }
                            }
                        }
                    }
                }
            }
        }
        for value in files {
            if let (Some(mime), Some(encoded)) =
                (value["mime"].as_str(), value["data_base64"].as_str())
            {
                if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                    self.scientific_plot(PlotPayload {
                        source: "Debugger".into(),
                        mime: mime.into(),
                        data,
                        title: value["title"].as_str().unwrap_or("Figure").into(),
                    });
                }
            }
        }
        if self.settings.scientific.mode == rustpy_settings::scientific::Mode::Automatic
            && self.settings.scientific.layout
        {
            if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                let document = tab.state.doc.identity();
                if self.scientific.last_document != Some(document) {
                    self.scientific.last_document = Some(document);
                    let source = tab.state.doc.text();
                    if tab.state.language == Language::Python
                        && (source.contains("#%%")
                            || source.contains("# %%")
                            || [
                                "import numpy",
                                "import pandas",
                                "import matplotlib",
                                "import torch",
                                "import polars",
                            ]
                            .iter()
                            .any(|name| source.contains(name)))
                    {
                        self.bottom = Some(BottomTool::SciView);
                    }
                }
            }
        }
        let now = ctx.input(|input| input.time);
        let mut refresh = Vec::new();
        for (index, tab) in self.scientific.view.data.iter_mut().enumerate() {
            tab.stale = match tab.origin {
                Origin::Console(owner) => !self.py_console.scientific_owner_running(owner),
                Origin::Debug { session, epoch } => !self
                    .debug
                    .iter()
                    .chain(self.debug_sessions.iter().map(|(session, _)| session))
                    .any(|owner| {
                        owner.id == session && owner.is_stopped() && owner.stop_epoch == epoch
                    }),
                _ => false,
            };
            if tab.auto_refresh
                && !project_safe_mode
                && now - tab.last_refresh
                    >= self.settings.scientific.refresh_seconds.clamp(1, 60) as f64
            {
                tab.last_refresh = now;
                refresh.push(Event::Refresh(index));
            }
        }
        for event in refresh {
            self.scientific_event(event, ctx);
        }
        if std::mem::take(&mut self.scientific.view.request_open) {
            self.bottom = Some(BottomTool::SciView);
        }
        if self.scientific.view.data.iter().any(|tab| tab.auto_refresh)
            || !self.scientific.captures.is_empty()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if !self.scientific.initialized {
            self.scientific.initialized = true;
            if self.settings.scientific.mode == rustpy_settings::scientific::Mode::Scientific {
                self.bottom = Some(BottomTool::SciView);
            }
        }
    }
    pub(super) fn scientific_event(&mut self, event: Event, _ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        match event {
            Event::Settings => {
                self.settings_ui.page = settings_ui::SettingsPage::Scientific;
                self.dialog = Dialog::Settings;
            }
            Event::ExportData { path, bytes } => match std::fs::write(&path, bytes) {
                Ok(()) => self.set_status(format!("Exported {}", path.display())),
                Err(error) => self.set_status(format!("Export failed: {error}")),
            },
            Event::OpenInteractive(index) => {
                if let Some(plot) = self.scientific.view.interactive(index) {
                    if plot.mime != "text/html" {
                        return;
                    }
                    let directory = Settings::system_dir().join("scientific/web");
                    if std::fs::create_dir_all(&directory).is_ok() {
                        let path = directory.join(format!(
                            "plot-{}-{index}-{}.html",
                            std::process::id(),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos()
                        ));
                        match std::fs::write(&path, &plot.data) {
                            Ok(()) => {
                                self.scientific.web_files.push(path.clone());
                                while self.scientific.web_files.len() > 64 {
                                    let old = self.scientific.web_files.remove(0);
                                    let _ = std::fs::remove_file(old);
                                }
                                self.open_web_file(&path, None, true);
                            }
                            Err(error) => {
                                self.set_status(format!("Cannot open interactive plot: {error}"))
                            }
                        }
                    }
                }
            }
            Event::Refresh(index) => {
                // Safe mode never contacts a kernel or debugger, but the tab still
                // explains why nothing changed: its snapshot stays readable.
                if project_safe_mode {
                    if let Some(tab) = self.scientific.view.data.get_mut(index) {
                        tab.message = Some(
                            "Refresh is disabled in safe mode. Snapshots remain available.".into(),
                        );
                    }
                    return;
                }
                let Some(tab) = self.scientific.view.data.get(index) else {
                    return;
                };
                let name = tab.name.clone();
                let options = tab.options();
                let origin = tab.origin.clone();
                let refreshed = match origin {
                    Origin::Console(owner) => {
                        self.py_console.scientific_refresh(owner, &name, options)
                    }
                    Origin::Debug { session, .. } => {
                        if let Some(owner) = self
                            .debug
                            .iter_mut()
                            .chain(self.debug_sessions.iter_mut().map(|(session, _)| session))
                            .find(|owner| {
                                owner.id == session
                                    && owner.is_stopped()
                                    && !owner.backend.pending_view
                            })
                        {
                            owner.backend.offset = tab.offset;
                            owner.backend.rows = tab.rows;
                            owner.backend.columns = tab.columns;
                            owner.backend.scientific_column_offset = tab.col_start;
                            owner.backend.slice = tab.slice.clone();
                            owner.inspect_value(&name, "table");
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if let Some(tab) = self.scientific.view.data.get_mut(index) {
                    tab.message=(!refreshed).then(||"Refresh requires the original idle console or suspended debugger. Snapshots remain available.".into());
                }
            }
        }
    }
    pub(super) fn scientific_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        let layout = self.settings.scientific.layout
            && self.settings.scientific.mode != rustpy_settings::scientific::Mode::Standard;
        let mut events = Vec::new();
        if layout && self.settings.scientific.documentation {
            egui::Panel::right("scientific_documentation")
                .resizable(true)
                .default_size(280.0)
                .show(ui, |ui| {
                    ui.heading("Documentation");
                    self.documentation.docked = true;
                    self.refresh_documentation_tool();
                    self.documentation_tool_ui(ui);
                });
        }
        let console = layout.then_some(|app: &mut Self, ui: &mut egui::Ui| {
            app.python_console_tabs(ui);
            if ui.button("Start Python console").clicked() {
                app.ensure_py_console();
            }
            let mono = egui::FontId::new(
                app.settings.editor.font_size.max(10.0),
                crate::theme::mono_family(false, false),
            );
            app.py_console.show_variables = app.settings.scientific.variables;
            app.py_console.ui(ui, theme, mono);
            app.consume_python_console_requests();
        });
        scientific_split(ui, self, console, |app, ui| {
            events.extend(app.scientific.view.ui(ui))
        });
        for event in events {
            self.scientific_event(event, ui.ctx());
        }
    }
    pub(super) fn scientific_floating(&mut self, ctx: &egui::Context) {
        let events = self.scientific.view.floating(ctx);
        for event in events {
            self.scientific_event(event, ctx);
        }
    }
    pub(super) fn scientific_toggle(&mut self) {
        let enabled =
            self.settings.scientific.mode != rustpy_settings::scientific::Mode::Scientific;
        self.change_setting_now(|settings| {
            settings.scientific.layout = enabled;
            settings.scientific.mode = if enabled {
                rustpy_settings::scientific::Mode::Scientific
            } else {
                rustpy_settings::scientific::Mode::Standard
            };
        });
        if enabled {
            self.bottom = Some(BottomTool::SciView);
        }
    }
    pub(super) fn scientific_settings(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.scientific;
        ui.heading("Python Scientific");
        ui.checkbox(&mut settings.show_plots, "Show plots in tool window");
        ui.checkbox(&mut settings.layout, "Scientific mode layout");
        egui::ComboBox::from_label("Mode selection")
            .selected_text(format!("{:?}", settings.mode))
            .show_ui(ui, |ui| {
                for mode in [
                    rustpy_settings::scientific::Mode::Automatic,
                    rustpy_settings::scientific::Mode::Scientific,
                    rustpy_settings::scientific::Mode::Standard,
                ] {
                    ui.selectable_value(&mut settings.mode, mode, format!("{mode:?}"));
                }
            });
        ui.checkbox(
            &mut settings.documentation,
            "Show documentation viewer in scientific layout",
        );
        ui.checkbox(
            &mut settings.variables,
            "Show console variables in scientific layout",
        );
        ui.checkbox(
            &mut settings.auto_refresh,
            "Auto-refresh data on variable change",
        );
        ui.add(
            egui::DragValue::new(&mut settings.refresh_seconds)
                .range(1..=60)
                .suffix(" seconds"),
        );
        ui.label("Preview rectangle");
        ui.add(
            egui::DragValue::new(&mut settings.page_rows)
                .range(1..=1000)
                .suffix(" rows"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.page_columns)
                .range(1..=200)
                .suffix(" columns"),
        );
        ui.label("Default number format (.6g, .3f, .2e)");
        ui.text_edit_singleline(&mut settings.number_format);
        ui.checkbox(&mut settings.heatmap, "Heatmap by default");
        egui::ComboBox::from_label("Color map")
            .selected_text(&settings.color_map)
            .show_ui(ui, |ui| {
                for map in ["Viridis", "Blue–Red", "Grayscale"] {
                    ui.selectable_value(
                        &mut settings.color_map,
                        map.into(),
                        crate::i18n::tr_static(map),
                    );
                }
            });
        ui.add(
            egui::DragValue::new(&mut settings.max_plots)
                .range(1..=256)
                .suffix(" gallery plots"),
        );
        ui.checkbox(
            &mut settings.capture_interactive,
            "Capture Plotly/Bokeh interactive HTML",
        );
        ui.checkbox(&mut settings.cell_separators, "Show code cell separators");
        ui.label("HTML plots open explicitly in the system browser web view. Exports contain the loaded, filtered rectangle; statistics describe the loaded source page.");
    }
    pub(super) fn scientific_run_above(&mut self, index: usize, line: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let text = self.tabs[index].state.doc.text();
        let Some(cell) = crate::editor::console_code::cell_at(&text, line) else {
            return;
        };
        let code = text
            .lines()
            .take(cell.marker.unwrap_or(0))
            .collect::<Vec<_>>()
            .join("\n");
        if !code.trim().is_empty() && self.ensure_py_console() {
            self.py_console.execute_fragment(&code);
            self.bottom = Some(BottomTool::PythonConsole);
        }
    }
    pub(super) fn scientific_debug_cell(&mut self, index: usize, line: usize) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode || index >= self.tabs.len() || !self.save_all() {
            return;
        }
        let tab = &self.tabs[index];
        let Some(path) = tab.state.path.clone() else {
            self.set_status("Save the Python file before debugging its cell");
            return;
        };
        let text = tab.state.doc.text();
        let Some(cell) = crate::editor::console_code::cell_at(&text, line) else {
            return;
        };
        let code = text
            .lines()
            .skip(cell.lines.start)
            .take(cell.lines.len())
            .collect::<Vec<_>>()
            .join("\n");
        if code.trim().is_empty() {
            self.set_status("The cell is empty");
            return;
        }
        let padded = if let Some(marker) = cell.marker {
            format!(
                "{}__import__('debugpy').breakpoint()\n{}",
                "\n".repeat(marker),
                code
            )
        } else {
            code
        };
        let script=format!("import debugpy as _rustpy_debugpy, linecache as _rustpy_linecache, threading as _rustpy_threading\n_rustpy_timer = _rustpy_threading.Timer(15.0, _rustpy_debugpy.wait_for_client.cancel)\n_rustpy_timer.daemon = True\n_rustpy_timer.start()\ntry:\n    _rustpy_debugpy.wait_for_client()\nfinally:\n    _rustpy_timer.cancel()\nif not _rustpy_debugpy.is_client_connected():\n    raise RuntimeError('Cell debugger did not connect before the timeout')\n_rustpy_linecache.cache[{}] = ({}, None, {}.splitlines(True), {})\nexec(compile({}, {}, 'exec'), globals())\n",serde_json::to_string(&path.to_string_lossy()).unwrap(),text.len(),serde_json::to_string(&text).unwrap(),serde_json::to_string(&path.to_string_lossy()).unwrap(),serde_json::to_string(&padded).unwrap(),serde_json::to_string(&path.to_string_lossy()).unwrap());
        if !self.ensure_py_console() {
            return;
        }
        let owner = self.py_console.service_id().unwrap_or_default();
        let name = format!("Scientific console {owner}");
        if let Some(session) = self
            .debug
            .iter()
            .chain(self.debug_sessions.iter().map(|(session, _)| session))
            .find(|session| session.name == name && session.is_active())
        {
            if session.is_stopped() {
                self.set_status("Resume the console debugger before starting another cell");
                return;
            }
            self.py_console.execute_fragment(&script);
            self.bottom = Some(BottomTool::Debug);
        } else if self.py_console.scientific_debug(script) {
            self.bottom = Some(BottomTool::PythonConsole);
            self.set_status("Attaching the debugger to the cell's Python console");
        } else {
            self.set_status("Wait for the console to become idle before debugging a cell");
        }
    }
}

/// Smallest console height kept in the scientific split.
const SCIENTIFIC_CONSOLE_MIN: f32 = 72.0;
/// Space always left for SciView below the toolbar so its tabs stay reachable.
const SCIENTIFIC_VIEW_MIN: f32 = 120.0;

/// Lays out the scientific tool window: an optional resizable console docked at
/// the bottom, and SciView confined (and clipped) to whatever remains. Returns
/// the console and SciView rectangles.
pub(super) fn scientific_split<T>(
    ui: &mut egui::Ui,
    state: &mut T,
    console: Option<impl FnOnce(&mut T, &mut egui::Ui)>,
    view: impl FnOnce(&mut T, &mut egui::Ui),
) -> (Option<egui::Rect>, egui::Rect) {
    let available = ui.available_rect_before_wrap().height();
    let console_rect = console.map(|console| {
        let max =
            (available - SCIENTIFIC_VIEW_MIN).max(SCIENTIFIC_CONSOLE_MIN.min(available * 0.5));
        let min = SCIENTIFIC_CONSOLE_MIN.min(max);
        egui::Panel::bottom("scientific_console")
            .resizable(true)
            .default_size(180.0)
            .size_range(min..=max)
            .show(ui, |ui| {
                // Console content taller than the split must not grow the panel
                // (a bottom panel grows upwards over SciView): lay it out in an
                // unallocated child confined and clipped to the panel.
                let rect = ui.max_rect();
                let mut content = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                content.set_clip_rect(rect.intersect(ui.clip_rect()));
                console(state, &mut content);
            })
            .response
            .rect
    });
    let rect = ui.available_rect_before_wrap();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            egui::ScrollArea::vertical()
                .id_salt("scientific_view_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| view(state, ui));
        },
    );
    (console_rect, rect)
}

#[cfg(all(test, target_os = "linux"))]
#[path = "scientific_ui/configured_producer_tests.rs"]
mod configured_producer_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the scientific split headlessly; returns the console and SciView
    /// rectangles plus every text shape with its clip rectangle.
    fn render(
        size: egui::Vec2,
        view: &mut Workspace,
    ) -> (egui::Rect, egui::Rect, Vec<(String, egui::Rect)>) {
        let ctx = egui::Context::default();
        let mut rects = None;
        let mut texts = Vec::new();
        for frame in 0..3 {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 * 0.1),
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| {
                    let console = |_: &mut Workspace, ui: &mut egui::Ui| {
                        for line in 0..60 {
                            ui.label(format!("console line {line}"));
                        }
                    };
                    rects = Some(scientific_split(ui, view, Some(console), |view, ui| {
                        view.ui(ui);
                    }));
                },
            );
            texts = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => {
                        Some((text.galley.job.text.clone(), clipped.clip_rect))
                    }
                    _ => None,
                })
                .collect();
            output.drop_without_applying_deltas();
        }
        let (console, view) = rects.unwrap();
        (console.unwrap(), view, texts)
    }

    #[test]
    fn console_and_sciview_table_stay_in_their_own_rectangles() {
        let mut view = Workspace::default();
        let columns = (0..30)
            .map(|column| format!("col{column}"))
            .collect::<Vec<_>>();
        let rows = (0..80)
            .map(|row| {
                (0..30)
                    .map(|column| format!("{}", row * 100 + column))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        view.publish(
            "df".into(),
            Origin::Console(1),
            serde_json::json!({"columns": columns, "rows": rows}),
            &Default::default(),
        );
        for size in [
            egui::vec2(900.0, 300.0),
            egui::vec2(700.0, 180.0),
            egui::vec2(1200.0, 800.0),
        ] {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let (console, sciview, texts) = render(size, &mut view);
            assert!(
                screen.expand(0.5).contains_rect(console),
                "{size:?}: {console:?}"
            );
            assert!(
                screen.expand(0.5).contains_rect(sciview),
                "{size:?}: {sciview:?}"
            );
            assert!(
                console.min.y >= sciview.max.y - 0.5,
                "{size:?}: console overlaps SciView"
            );
            if size.y >= SCIENTIFIC_VIEW_MIN + SCIENTIFIC_CONSOLE_MIN {
                assert!(
                    sciview.height() >= SCIENTIFIC_VIEW_MIN - 0.5,
                    "{size:?}: {sciview:?}"
                );
                assert!(
                    console.height() >= SCIENTIFIC_CONSOLE_MIN - 0.5,
                    "{size:?}: {console:?}"
                );
            }
            assert!(texts.iter().any(|(text, _)| text == "Data"));
            assert!(texts
                .iter()
                .any(|(text, _)| text.starts_with("console line")));
            for (text, clip) in &texts {
                let visible = clip.intersect(screen);
                if visible.width() <= 0.0 || visible.height() <= 0.0 {
                    continue;
                }
                let owner = if text.starts_with("console line") {
                    console
                } else {
                    sciview
                };
                assert!(
                    owner.expand(1.0).contains_rect(visible),
                    "{size:?}: {text:?} drawn outside its container {owner:?}: {visible:?}"
                );
            }
        }
    }

    #[test]
    fn safe_mode_refresh_keeps_snapshot_and_explains_without_contacting_kernel() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.safe_mode = true;
        let page = serde_json::json!({"columns": ["a"], "rows": [["1"]]});
        app.scientific.view.publish(
            "df".into(),
            Origin::Console(7),
            page.clone(),
            &Default::default(),
        );
        app.scientific_event(Event::Refresh(0), &egui::Context::default());
        let tab = &app.scientific.view.data[0];
        assert_eq!(tab.page, page);
        assert!(tab
            .message
            .as_deref()
            .is_some_and(|message| message.contains("Snapshots remain available")));
        assert!(
            app.py_console.service_id().is_none(),
            "safe mode must not start a console"
        );
    }
}

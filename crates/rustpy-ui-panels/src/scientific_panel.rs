//! SciView retains immutable pages; the originating session owns evaluation.
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use serde_json::Value;
use std::path::PathBuf;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Console(u64),
    Debug { session: u64, epoch: u64 },
    Notebook(String),
    Snapshot,
}
#[derive(Clone, Debug)]
pub struct PlotPayload {
    pub source: String,
    pub mime: String,
    pub data: Vec<u8>,
    pub title: String,
}
#[derive(Clone, Debug)]
pub enum Event {
    Refresh(usize),
    OpenInteractive(usize),
    ExportData { path: PathBuf, bytes: Vec<u8> },
    Settings,
}
pub struct DataTab {
    pub name: String,
    pub origin: Origin,
    pub page: Value,
    pub offset: usize,
    pub columns: usize,
    pub rows: usize,
    pub col_start: usize,
    pub slice: String,
    pub filter: String,
    pub sort: Option<(usize, bool)>,
    pub transpose: bool,
    pub format: String,
    pub column_formats: std::collections::BTreeMap<String, String>,
    pub format_column: String,
    pub color_map: String,
    pub heatmap: bool,
    pub auto_refresh: bool,
    pub stale: bool,
    pub floating: bool,
    pub column_width: f32,
    pub export_path: String,
    pub message: Option<String>,
    pub last_refresh: f64,
}
impl DataTab {
    pub fn options(&self) -> Value {
        serde_json::json!({"offset":self.offset,"rows":self.rows,"columns":self.columns,"col_start":self.col_start,"slice":self.slice})
    }
    fn new(
        name: String,
        origin: Origin,
        page: Value,
        prefs: &rustpy_settings::scientific::ScientificSettings,
    ) -> Self {
        let offset = page["offset"]
            .as_u64()
            .or_else(|| page["row_start"].as_u64())
            .unwrap_or_default() as usize;
        let col_start = page["col_start"].as_u64().unwrap_or_default() as usize;
        Self {
            name,
            origin,
            page,
            offset,
            columns: prefs.page_columns.clamp(1, 200),
            rows: prefs.page_rows.clamp(1, 1000),
            col_start,
            slice: String::new(),
            filter: String::new(),
            sort: None,
            transpose: false,
            format: prefs.number_format.clone(),
            column_formats: Default::default(),
            format_column: String::new(),
            color_map: prefs.color_map.clone(),
            heatmap: prefs.heatmap,
            auto_refresh: prefs.auto_refresh,
            stale: false,
            floating: false,
            column_width: 120.0,
            export_path: String::new(),
            message: None,
            last_refresh: 0.0,
        }
    }
    pub fn rectangle(&self) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
        let mut columns = self.page["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .map(display)
            .collect::<Vec<_>>();
        let mut rows = self.page["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                row.as_array()
                    .into_iter()
                    .flatten()
                    .map(display)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if let Some(index) = self.page["index"]
            .as_array()
            .filter(|index| index.len() == rows.len())
        {
            columns.insert(0, "Index".into());
            for (row, label) in rows.iter_mut().zip(index) {
                row.insert(0, display(label));
            }
        }
        if !self.filter.trim().is_empty() {
            let predicate = Predicate::parse(&self.filter, &columns)?;
            rows.retain(|row| predicate.matches(row));
        }
        if let Some((column, descending)) = self.sort {
            rows.sort_by(|a, b| {
                let a = a.get(column).map(String::as_str).unwrap_or_default();
                let b = b.get(column).map(String::as_str).unwrap_or_default();
                let order = match (a.parse::<f64>(), b.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a.total_cmp(&b),
                    _ => a.cmp(b),
                };
                if descending {
                    order.reverse()
                } else {
                    order
                }
            });
        }
        if self.transpose {
            let mut transposed = Vec::new();
            for (c, column) in columns.iter().enumerate() {
                let mut row = vec![column.clone()];
                row.extend(
                    rows.iter()
                        .map(|row| row.get(c).cloned().unwrap_or_default()),
                );
                transposed.push(row);
            }
            columns = std::iter::once("Column".into())
                .chain((0..rows.len()).map(|r| (self.offset + r).to_string()))
                .collect();
            rows = transposed;
        }
        Ok((columns, rows))
    }
}
fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
struct Predicate {
    clauses: Vec<(usize, String, String)>,
}
impl Predicate {
    fn parse(expression: &str, columns: &[String]) -> Result<Self, String> {
        let mut clauses = Vec::new();
        for clause in expression.split("&&") {
            let clause = clause.trim();
            let Some((position, operator)) = [">=", "<=", "!=", "==", ">", "<"]
                .iter()
                .filter_map(|op| clause.find(op).map(|position| (position, *op)))
                .min_by_key(|(position, _)| *position)
            else {
                return Err("Use column > 1, column == \"text\"; combine with &&".into());
            };
            let name = clause[..position].trim().trim_matches('`');
            let column = columns
                .iter()
                .position(|column| column == name)
                .or_else(|| {
                    name.parse::<usize>()
                        .ok()
                        .filter(|index| *index < columns.len())
                })
                .ok_or_else(|| format!("Unknown column: {name}"))?;
            let value = clause[position + operator.len()..]
                .trim()
                .trim_matches(['\'', '"'])
                .to_owned();
            clauses.push((column, operator.into(), value));
        }
        Ok(Self { clauses })
    }
    fn matches(&self, row: &[String]) -> bool {
        self.clauses.iter().all(|(column, op, right)| {
            let left = row.get(*column).map(String::as_str).unwrap_or_default();
            let order = match (left.parse::<f64>(), right.parse::<f64>()) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b),
                _ => Some(left.cmp(right)),
            };
            match op.as_str() {
                "==" => order == Some(std::cmp::Ordering::Equal),
                "!=" => order != Some(std::cmp::Ordering::Equal),
                ">" => order == Some(std::cmp::Ordering::Greater),
                "<" => order == Some(std::cmp::Ordering::Less),
                ">=" => matches!(
                    order,
                    Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
                ),
                "<=" => matches!(
                    order,
                    Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Less)
                ),
                _ => false,
            }
        })
    }
}
struct Plot {
    payload: PlotPayload,
    texture: Option<egui::TextureHandle>,
    full_texture: Option<egui::TextureHandle>,
    zoom: f32,
    save_path: String,
    error: Option<String>,
}
#[derive(Default)]
pub struct Workspace {
    pub data: Vec<DataTab>,
    pub selected: usize,
    pub tab: usize,
    plots: Vec<Plot>,
    selected_plot: usize,
    pub request_open: bool,
}
impl Workspace {
    pub fn publish(
        &mut self,
        name: String,
        origin: Origin,
        page: Value,
        prefs: &rustpy_settings::scientific::ScientificSettings,
    ) {
        let mut opened = false;
        if let Some(tab) = self
            .data
            .iter_mut()
            .find(|tab| tab.name == name && same_owner(&tab.origin, &origin))
        {
            tab.page = page;
            tab.origin = origin;
            tab.stale = false;
        } else {
            opened = true;
            self.data.push(DataTab::new(name, origin, page, prefs));
            self.selected = self.data.len() - 1;
            if self.data.len() > 64 {
                self.data.remove(0);
                self.selected = self.selected.saturating_sub(1);
            }
        }
        if opened {
            self.tab = 0;
            self.request_open = true;
        }
    }
    pub fn plot(&mut self, payload: PlotPayload, maximum: usize) {
        if payload.data.len() > 16 * 1024 * 1024
            || self.plots.last().is_some_and(|plot| {
                plot.payload.source == payload.source && plot.payload.data == payload.data
            })
        {
            return;
        }
        self.plots.push(Plot {
            payload,
            texture: None,
            full_texture: None,
            zoom: 1.0,
            save_path: String::new(),
            error: None,
        });
        while self.plots.len() > maximum.clamp(1, 256)
            || self
                .plots
                .iter()
                .map(|plot| plot.payload.data.len())
                .sum::<usize>()
                > 64 * 1024 * 1024
        {
            self.plots.remove(0);
        }
        self.selected_plot = self.plots.len().saturating_sub(1);
        self.tab = 1;
        self.request_open = true;
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Vec<Event> {
        let mut events = Vec::new();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, 0, "Data");
            ui.selectable_value(&mut self.tab, 1, "Plots");
            if ui.button("Settings…").clicked() {
                events.push(Event::Settings);
            }
        });
        if self.tab == 0 {
            ui.horizontal_wrapped(|ui| {
                for (index, tab) in self.data.iter().enumerate() {
                    if ui
                        .selectable_label(index == self.selected, &tab.name)
                        .clicked()
                    {
                        self.selected = index;
                    }
                }
            });
            let mut close = false;
            let mut duplicate = false;
            if let Some(tab) = self.data.get_mut(self.selected) {
                ui.horizontal(|ui| {
                    if ui.button("Open in new tab").clicked() {
                        duplicate = true;
                    }
                    ui.checkbox(&mut tab.floating, "Floating");
                    if ui.button("Close tab").clicked() {
                        close = true;
                    }
                });
                events.extend(data_ui(ui, tab, self.selected));
            } else {
                ui.label("Choose View as Array/DataFrame from the console or debugger variables.");
            }
            if duplicate {
                if let Some(tab) = self.data.get(self.selected) {
                    let mut copy = DataTab::new(
                        format!("{} (snapshot)", tab.name),
                        Origin::Snapshot,
                        tab.page.clone(),
                        &Default::default(),
                    );
                    copy.format = tab.format.clone();
                    self.data.push(copy);
                    self.selected = self.data.len() - 1;
                    if self.data.len() > 64 {
                        self.data.remove(0);
                        self.selected = self.selected.saturating_sub(1);
                    }
                }
            }
            if close {
                self.data.remove(self.selected);
                self.selected = self.selected.min(self.data.len().saturating_sub(1));
            }
        } else {
            self.plots_ui(ui, &mut events);
        }
        events
    }
    pub fn floating(&mut self, ctx: &egui::Context) -> Vec<Event> {
        let mut events = Vec::new();
        for (index, tab) in self.data.iter_mut().enumerate() {
            if tab.floating {
                let mut open = true;
                egui::Window::new(format!("SciView — {}", tab.name))
                    .id(egui::Id::new(("scientific_float", index)))
                    .open(&mut open)
                    .resizable(true)
                    .default_width(800.0)
                    .show_modeless(ctx, |ui| events.extend(data_ui(ui, tab, index)));
                tab.floating = open;
            }
        }
        events
    }
    fn plots_ui(&mut self, ui: &mut egui::Ui, events: &mut Vec<Event>) {
        ui.horizontal(|ui| {
            if ui.button("Clear plots").clicked() {
                self.plots.clear();
                self.selected_plot = 0;
            }
            ui.label(format!("{} captured figures", self.plots.len()));
        });
        egui::ScrollArea::horizontal()
            .id_salt("scientific_gallery")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, plot) in self.plots.iter_mut().enumerate() {
                        ensure_texture(ui.ctx(), plot, index);
                        ui.vertical(|ui| {
                            if let Some(texture) = &plot.texture {
                                if ui
                                    .add(
                                        egui::Button::image(
                                            egui::Image::new(texture)
                                                .fit_to_exact_size(egui::vec2(120.0, 80.0)),
                                        )
                                        .selected(index == self.selected_plot),
                                    )
                                    .clicked()
                                {
                                    self.selected_plot = index;
                                }
                            } else if ui
                                .selectable_label(index == self.selected_plot, &plot.payload.title)
                                .clicked()
                            {
                                self.selected_plot = index;
                            }
                            ui.small(&plot.payload.source);
                        });
                    }
                });
            });
        for (index, plot) in self.plots.iter_mut().enumerate() {
            if index != self.selected_plot {
                plot.full_texture = None;
            }
        }
        let Some(plot) = self.plots.get_mut(self.selected_plot) else {
            ui.label(
                "Plots from Python consoles, debugger sessions and notebook cells appear here.",
            );
            return;
        };
        ensure_full_texture(ui.ctx(), plot, self.selected_plot);
        ui.horizontal(|ui| {
            ui.label(&plot.payload.title);
            ui.add(egui::Slider::new(&mut plot.zoom, 0.1..=8.0).text("Zoom"));
            if ui.button("Fit").clicked() {
                plot.zoom = 1.0;
            }
            if plot.payload.mime == "text/html" && ui.button("Open interactive web view").clicked()
            {
                events.push(Event::OpenInteractive(self.selected_plot));
            }
            if ui.button("Copy data URL").clicked() {
                use base64::Engine;
                ui.ctx().copy_text(format!(
                    "data:{};base64,{}",
                    plot.payload.mime,
                    base64::engine::general_purpose::STANDARD.encode(&plot.payload.data)
                ));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Save path");
            ui.text_edit_singleline(&mut plot.save_path);
            if ui
                .add_enabled(!plot.save_path.is_empty(), egui::Button::new("Save plot"))
                .clicked()
            {
                events.push(Event::ExportData {
                    path: plot.save_path.clone().into(),
                    bytes: plot.payload.data.clone(),
                });
            }
        });
        if let Some(error) = &plot.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(texture) = plot.full_texture.as_ref().or(plot.texture.as_ref()) {
            egui::ScrollArea::both()
                .id_salt("scientific_plot_zoom")
                .show(ui, |ui| {
                    let size = texture.size_vec2();
                    let width = size.x.min(ui.available_width()) * plot.zoom;
                    ui.add(
                        egui::Image::new(texture)
                            .fit_to_exact_size(egui::vec2(width, width * size.y / size.x)),
                    );
                });
        } else if plot.payload.mime == "text/html" {
            ui.label("Interactive Plotly/Bokeh output runs in the system browser web view. Open it to pan, zoom and use its controls.");
        } else if let Ok(source) = std::str::from_utf8(&plot.payload.data) {
            egui::ScrollArea::both()
                .max_height(ui.available_height().clamp(60.0, 400.0))
                .show(ui, |ui| {
                    ui.monospace(source.chars().take(65536).collect::<String>());
                });
        }
    }
    pub fn interactive(&self, index: usize) -> Option<&PlotPayload> {
        self.plots.get(index).map(|plot| &plot.payload)
    }
}
fn same_owner(a: &Origin, b: &Origin) -> bool {
    match (a, b) {
        (Origin::Console(a), Origin::Console(b)) => a == b,
        (Origin::Debug { session: a, .. }, Origin::Debug { session: b, .. }) => a == b,
        (Origin::Notebook(a), Origin::Notebook(b)) => a == b,
        _ => false,
    }
}
fn plot_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let format = image::guess_format(bytes).map_err(|error| error.to_string())?;
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|error| error.to_string())?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 8 * 1024 * 1024
    {
        return Err("Plot exceeds the 8 megapixel display budget; save the original bytes to view externally".into());
    }
    image::load_from_memory_with_format(bytes, format).map_err(|error| error.to_string())
}
fn ensure_texture(ctx: &egui::Context, plot: &mut Plot, index: usize) {
    if plot.texture.is_some() || plot.error.is_some() || !plot.payload.mime.starts_with("image/") {
        return;
    }
    match plot_image(&plot.payload.data) {
        Ok(image) => {
            let image = image.thumbnail(160, 120).to_rgba8();
            plot.texture = Some(ctx.load_texture(
                format!("scientific_thumbnail_{index}_{}", plot.payload.source),
                egui::ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                ),
                Default::default(),
            ));
        }
        Err(error) => plot.error = Some(error),
    }
}
fn ensure_full_texture(ctx: &egui::Context, plot: &mut Plot, index: usize) {
    if plot.full_texture.is_some()
        || plot.error.is_some()
        || !plot.payload.mime.starts_with("image/")
    {
        return;
    }
    match plot_image(&plot.payload.data) {
        Ok(image) => {
            let image = image.to_rgba8();
            plot.full_texture = Some(ctx.load_texture(
                format!("scientific_selected_{index}_{}", plot.payload.source),
                egui::ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                ),
                Default::default(),
            ));
        }
        Err(error) => plot.error = Some(error),
    }
}

fn data_ui(ui: &mut egui::Ui, tab: &mut DataTab, index: usize) -> Vec<Event> {
    let mut events = Vec::new();
    ui.heading(&tab.name);
    ui.label(format!(
        "{} · dtype {} · shape {} · {} total rows",
        tab.page["type"]
            .as_str()
            .or_else(|| tab.page["kind"].as_str())
            .unwrap_or("Data"),
        tab.page["dtype"],
        tab.page["shape"],
        tab.page["total_rows"]
            .as_u64()
            .or_else(|| tab.page["total"].as_u64())
            .unwrap_or_default()
    ));
    ui.label(format!(
        "Loaded {} rows × {} columns. Paging uses the returned bounded page size.",
        tab.page["rows"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        tab.page["columns"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
    ));
    if tab.stale {
        ui.colored_label(ui.visuals().warn_fg_color,"Snapshot: its originating session is running, stopped or at another suspension. Refresh only when that owner is available.");
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Offset");
        ui.add(egui::DragValue::new(&mut tab.offset));
        ui.label("Rows");
        ui.add(egui::DragValue::new(&mut tab.rows).range(1..=1000));
        ui.label("Column offset");
        ui.add(egui::DragValue::new(&mut tab.col_start));
        ui.label("Columns");
        ui.add(egui::DragValue::new(&mut tab.columns).range(1..=200));
        if ui.button("Previous").clicked() {
            tab.offset = tab.offset.saturating_sub(
                tab.page["rows"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(tab.rows)
                    .max(1),
            );
            events.push(Event::Refresh(index));
        }
        if ui.button("Next").clicked() {
            tab.offset = tab.offset.saturating_add(
                tab.page["rows"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(tab.rows)
                    .max(1),
            );
            events.push(Event::Refresh(index));
        }
        if ui.button("Refresh").clicked() {
            events.push(Event::Refresh(index));
        }
        ui.checkbox(&mut tab.auto_refresh, "Auto-refresh on variable change");
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Tensor slice");
        ui.add(
            egui::TextEdit::singleline(&mut tab.slice)
                .hint_text("0, :, 0:100")
                .desired_width(140.0),
        );
        ui.label("Filter loaded page");
        ui.add(
            egui::TextEdit::singleline(&mut tab.filter)
                .hint_text("column > 1 && other == 'value'")
                .desired_width(220.0),
        );
        ui.checkbox(&mut tab.transpose, "Transpose");
        ui.label("Format");
        ui.add(egui::TextEdit::singleline(&mut tab.format).desired_width(70.0));
        ui.checkbox(&mut tab.heatmap, "Heatmap");
        egui::ComboBox::from_id_salt(("scientific_map", index))
            .selected_text(&tab.color_map)
            .show_ui(ui, |ui| {
                for map in ["Viridis", "Blue–Red", "Grayscale"] {
                    ui.selectable_value(
                        &mut tab.color_map,
                        map.into(),
                        crate::i18n::tr_static(map),
                    );
                }
            })
            .combo_named(crate::i18n::tr_static("Color map"));
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("scientific_column_format", index))
            .selected_text(if tab.format_column.is_empty() {
                "Column format override"
            } else {
                &tab.format_column
            })
            .show_ui(ui, |ui| {
                for column in tab.page["columns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    ui.selectable_value(&mut tab.format_column, column.into(), column);
                }
            })
            .combo_named(crate::i18n::tr_static("Column format"));
        if !tab.format_column.is_empty() {
            ui.text_edit_singleline(
                tab.column_formats
                    .entry(tab.format_column.clone())
                    .or_insert_with(|| tab.format.clone()),
            );
            if ui.small_button("Reset override").clicked() {
                tab.column_formats.remove(&tab.format_column);
                tab.format_column.clear();
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Resize columns");
        ui.add(egui::Slider::new(&mut tab.column_width, 40.0..=400.0));
        if ui.button("Fit columns").clicked() {
            tab.column_width = (ui.available_width()
                / tab.page["columns"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or(1)
                    .max(1) as f32)
                .clamp(40.0, 400.0);
        }
    });
    match tab.rectangle() {
        Err(error) => {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        Ok((columns, rows)) => {
            ui.horizontal(|ui| {
                if ui.button("Copy TSV").clicked() {
                    ui.ctx()
                        .copy_text(crate::scientific_export::csv(&columns, &rows, '\t'));
                }
                ui.label("Export loaded rectangle");
                ui.text_edit_singleline(&mut tab.export_path);
                for (label, excel) in [("CSV", false), ("Excel .xlsx", true)] {
                    if ui
                        .add_enabled(!tab.export_path.is_empty(), egui::Button::new(label))
                        .clicked()
                    {
                        let path = PathBuf::from(&tab.export_path).with_extension(if excel {
                            "xlsx"
                        } else {
                            "csv"
                        });
                        let bytes = if excel {
                            crate::scientific_export::xlsx(&columns, &rows)
                        } else {
                            crate::scientific_export::csv(&columns, &rows, ',').into_bytes()
                        };
                        events.push(Event::ExportData { path, bytes });
                    }
                }
            });
            let numbers = rows
                .iter()
                .flatten()
                .filter_map(|cell| cell.parse::<f64>().ok())
                .filter(|number| number.is_finite())
                .collect::<Vec<_>>();
            let low = numbers.iter().copied().fold(f64::INFINITY, f64::min);
            let high = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            egui::ScrollArea::both()
                .id_salt(("scientific_data", index))
                // Stay inside the containing split/window rather than a fixed height.
                .max_height(ui.available_height().clamp(60.0, 500.0))
                .show(ui, |ui| {
                    egui::Grid::new(("scientific_grid", index))
                        .striped(true)
                        .show(ui, |ui| {
                            for (column, name) in columns.iter().enumerate() {
                                if ui
                                    .add_sized([tab.column_width, 22.0], egui::Button::new(name))
                                    .clicked()
                                {
                                    tab.sort = Some((
                                        column,
                                        tab.sort.is_some_and(|(old, descending)| {
                                            old == column && !descending
                                        }),
                                    ));
                                }
                            }
                            ui.end_row();
                            for row in &rows {
                                for (column, cell) in row.iter().enumerate() {
                                    let number = cell
                                        .parse::<f64>()
                                        .ok()
                                        .filter(|number| number.is_finite());
                                    let formatted = number
                                        .map(|value| {
                                            format_number(
                                                value,
                                                columns
                                                    .get(column)
                                                    .and_then(|name| tab.column_formats.get(name))
                                                    .unwrap_or(&tab.format),
                                            )
                                        })
                                        .unwrap_or_else(|| cell.clone());
                                    let mut text = egui::RichText::new(formatted).monospace();
                                    if tab.heatmap {
                                        if let Some(value) = number {
                                            text = text.background_color(color(
                                                &tab.color_map,
                                                value,
                                                low,
                                                high,
                                            ));
                                        }
                                    }
                                    ui.add_sized(
                                        [tab.column_width, 20.0],
                                        egui::Label::new(text).truncate(),
                                    )
                                    .on_hover_text(cell);
                                }
                                ui.end_row();
                            }
                        });
                });
        }
    }
    ui.collapsing("Column statistics (loaded source page)", |ui| {
        if let Some(stats) = tab.page["stats"].as_array() {
            for column in stats {
                ui.label(format!(
                    "{}: count {}, numeric {}, missing {}, min {}, max {}, mean {}, stddev {}",
                    column["column"],
                    column["count"],
                    column["numeric"],
                    column["missing"],
                    column["minimum"],
                    column["maximum"],
                    column["mean"],
                    column["stddev"]
                ));
            }
        }
    });
    if let Some(message) = &tab.message {
        ui.label(message);
    }
    events
}
fn format_number(value: f64, format: &str) -> String {
    let format = format.trim().trim_start_matches("{:").trim_end_matches('}');
    let precision = format
        .strip_prefix('.')
        .and_then(|rest| rest.trim_end_matches(['f', 'e', 'g']).parse::<usize>().ok())
        .unwrap_or(6)
        .min(16);
    if format.ends_with('e') {
        format!("{value:.precision$e}")
    } else if format.ends_with('f') {
        format!("{value:.precision$}")
    } else {
        let precision = precision.max(1);
        let exponent = if value == 0.0 {
            0
        } else {
            value.abs().log10().floor() as i32
        };
        if exponent < -4 || exponent >= precision as i32 {
            let decimals = precision.saturating_sub(1);
            format!("{value:.decimals$e}")
        } else {
            let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
            let text = format!("{value:.decimals$}");
            if text.contains('.') {
                text.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                text
            }
        }
    }
}
fn color(map: &str, value: f64, low: f64, high: f64) -> egui::Color32 {
    let t = if high > low {
        ((value - low) / (high - low)).clamp(0.0, 1.0)
    } else {
        0.5
    };
    match map {
        "Grayscale" => egui::Color32::from_gray((40.0 + t * 190.0) as u8),
        "Blue–Red" => egui::Color32::from_rgb((t * 220.0) as u8, 40, ((1.0 - t) * 220.0) as u8),
        _ => egui::Color32::from_rgb(
            (68.0 + t * 185.0) as u8,
            (15.0 + t * 215.0) as u8,
            (90.0 + (1.0 - t) * 75.0) as u8,
        ),
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn filtered_numeric_sort_and_transpose_are_deterministic() {
        let mut tab = DataTab::new(
            "frame".into(),
            Origin::Snapshot,
            serde_json::json!({"columns":["amount","name"],"rows":[["10","a"],["2","b"],["0","c"]]}),
            &Default::default(),
        );
        tab.filter = "amount > 1".into();
        tab.sort = Some((0, false));
        let (_, rows) = tab.rectangle().unwrap();
        assert_eq!(rows[0][0], "2");
        tab.transpose = true;
        let (cols, rows) = tab.rectangle().unwrap();
        assert_eq!(cols.len(), 3);
        assert_eq!(rows[0], vec!["amount", "2", "10"]);
        tab.filter = "missing > 1".into();
        assert!(tab.rectangle().is_err());
    }
    #[test]
    fn new_suspension_updates_same_owner_without_creating_parallel_authority() {
        let mut view = Workspace::default();
        for epoch in [1, 2] {
            view.publish(
                "a".into(),
                Origin::Debug { session: 9, epoch },
                serde_json::json!({"rows":[]}),
                &Default::default(),
            );
        }
        assert_eq!(view.data.len(), 1);
        assert_eq!(
            view.data[0].origin,
            Origin::Debug {
                session: 9,
                epoch: 2
            }
        );
    }
}

//! Enabled-plugin CSV/TSV editing through the shared Document undo transaction.
use super::csv_table::{self, CsvError, Table};
use super::*;
use crate::a11y::AccessibleCombo;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
pub(super) struct CsvUi {
    parsed: Option<Result<Table, CsvError>>,
    stamp: Option<(rustpy_text::DocumentId, u64, u64)>,
    oversized: bool,
    source_mode: bool,
    delimiter: Option<char>,
    header: bool,
    initialized: bool,
    filter: String,
    sort: Option<(usize, bool)>,
    widths: Vec<f32>,
    draft: Option<Table>,
    editing: bool,
    dirty: bool,
}
impl IdeApp {
    pub(super) fn csv_registration(&self, index: usize) -> Option<(u64, String)> {
        let tab = self.tabs.get(index)?;
        if tab.image.is_some()
            || tab.notebook.is_some()
            || tab.state.is_large_file()
            || self.is_large_file_placeholder(index)
            || self.permission_viewers.contains(&tab.state.doc.identity())
        {
            return None;
        }
        let extension = tab.state.path.as_ref()?.extension()?.to_str()?;
        let registration = crate::plugin_host::table_editors()
            .into_iter()
            .find(|entry| {
                entry.kind == rustpy_plugins::TableEditorKind::Csv
                    && (entry
                        .extensions
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(extension))
                        || extension.eq_ignore_ascii_case("tsv")
                            && entry.extensions.iter().any(|candidate| candidate == "csv"))
            })?;
        Some((crate::plugin_host::generation(), registration.name))
    }
    #[cfg(test)]
    pub(super) fn csv_source_mode(&self, view: egui::Id) -> bool {
        self.csv_routes
            .get(&view)
            .is_some_and(|cache| cache.source_mode)
    }
    pub(super) fn csv_table_owns_input(&self) -> bool {
        self.active
            .is_some_and(|index| self.csv_table_owns_input_for(index))
    }
    pub(super) fn csv_table_owns_input_for(&self, index: usize) -> bool {
        self.csv_registration(index).is_some()
            && self
                .csv_routes
                .get(&self.tabs[index].view.id)
                .is_none_or(|cache| !cache.source_mode)
    }
    pub(super) fn csv_table_area(&mut self, ui: &mut egui::Ui, index: usize) -> bool {
        let Some((generation, name)) = self.csv_registration(index) else {
            if let Some(tab) = self.tabs.get(index) {
                self.csv_routes.remove(&tab.view.id);
            }
            return false;
        };
        self.csv_routes
            .retain(|id, _| self.tabs.iter().any(|tab| tab.view.id == *id));
        let tab = &self.tabs[index];
        let id = tab.view.id;
        let stamp = (
            tab.state.doc.identity(),
            tab.state.doc.version(),
            generation,
        );
        let readonly = tab.state.doc.is_read_only();
        let cache = self.csv_routes.entry(id).or_default();
        if !cache.initialized {
            cache.initialized = true;
            cache.header = self.settings.data_files.csv_header;
        }
        if cache.stamp != Some(stamp) {
            cache.oversized = usize::from(tab.state.doc.len()) > MAX_SOURCE_BYTES;
            cache.draft = None;
            cache.dirty = false;
            if !cache.oversized {
                let source = tab.state.doc.text();
                let delimiter = *cache
                    .delimiter
                    .get_or_insert_with(|| csv_table::delimiter(&source));
                cache.parsed = Some(if delimiter == ',' {
                    csv_table::parse(&source)
                } else {
                    csv_table::parse_delimited(&source, delimiter)
                });
            }
            cache.stamp = Some(stamp);
        }
        let old_source = cache.source_mode;
        let mut reparse = false;
        let mut apply = false;
        ui.horizontal(|ui| {
            ui.label(name);
            ui.selectable_value(&mut cache.source_mode, false, "Table");
            ui.selectable_value(&mut cache.source_mode, true, "Show as text");
            ui.checkbox(&mut cache.header, "Header row");
            egui::ComboBox::from_id_salt(id.with("delimiter"))
                .selected_text(match cache.delimiter.unwrap_or(',') {
                    '\t' => "Tab",
                    ';' => "Semicolon",
                    '|' => "Pipe",
                    _ => "Comma",
                })
                .show_ui(ui, |ui| {
                    for (label, delimiter) in [
                        ("Comma", ','),
                        ("Tab", '\t'),
                        ("Semicolon", ';'),
                        ("Pipe", '|'),
                    ] {
                        if ui
                            .selectable_label(
                                cache.delimiter == Some(delimiter),
                                crate::i18n::tr_static(label),
                            )
                            .clicked()
                        {
                            cache.delimiter = Some(delimiter);
                            reparse = true;
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Delimiter"));
            if ui.button("Detect delimiter").clicked() {
                cache.delimiter = None;
                reparse = true;
            }
        });
        if reparse {
            cache.stamp = None;
            cache.draft = None;
            cache.dirty = false;
        }
        if cache.source_mode {
            if !old_source {
                self.focus_editor = true;
            }
            return false;
        }
        if cache.oversized {
            ui.label("CSV exceeds the 8 MiB table limit; choose Show as text");
            return true;
        }
        if let Some(Err(error)) = &cache.parsed {
            ui.label(format!("CSV at byte {}: {}", error.byte, error.message));
            return true;
        }
        if cache.draft.is_none() {
            cache.draft = cache
                .parsed
                .as_ref()
                .and_then(|table| table.as_ref().ok())
                .cloned();
        }
        let Some(table) = &mut cache.draft else {
            return true;
        };
        while cache.widths.len() < table.columns {
            cache.widths.push(120.0);
        }
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} records · {} columns",
                table.rows.len(),
                table.columns
            ));
            ui.checkbox(&mut cache.editing, "Edit cells");
            ui.label("Filter");
            ui.text_edit_singleline(&mut cache.filter);
            if ui
                .add_enabled(
                    cache.dirty && !readonly,
                    egui::Button::new("Apply table edits"),
                )
                .clicked()
            {
                apply = true;
            }
            if ui.button("Add row").clicked() && !readonly {
                table.rows.push(vec![String::new(); table.columns]);
                cache.dirty = true;
            }
        });
        if readonly {
            ui.label("Read-only document");
        }
        ui.collapsing("Column widths", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (index, width) in cache.widths.iter_mut().enumerate() {
                    ui.add(
                        egui::DragValue::new(width)
                            .range(40.0..=600.0)
                            .prefix(format!("{}: ", index + 1)),
                    );
                }
            });
        });
        let first = usize::from(cache.header && !table.rows.is_empty());
        let mut rows = (first..table.rows.len())
            .filter(|row| {
                cache.filter.is_empty()
                    || table.rows[*row]
                        .iter()
                        .any(|cell| cell.to_lowercase().contains(&cache.filter.to_lowercase()))
            })
            .collect::<Vec<_>>();
        if let Some((column, descending)) = cache.sort {
            rows.sort_by(|a, b| {
                let a = table.rows[*a]
                    .get(column)
                    .map(String::as_str)
                    .unwrap_or_default();
                let b = table.rows[*b]
                    .get(column)
                    .map(String::as_str)
                    .unwrap_or_default();
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
        let mut delete = None;
        egui::ScrollArea::both()
            .id_salt(id.with("csv_grid"))
            .show_rows(ui, 28.0, rows.len(), |ui, visible| {
                egui::Grid::new(id.with("cells"))
                    .striped(true)
                    .show(ui, |ui| {
                        for column in 0..table.columns {
                            let label = if first == 1 {
                                table.rows[0].get(column).cloned().unwrap_or_default()
                            } else {
                                format!("Column {}", column + 1)
                            };
                            if ui
                                .add_sized([cache.widths[column], 24.0], egui::Button::new(label))
                                .clicked()
                            {
                                cache.sort = Some((
                                    column,
                                    cache.sort.is_some_and(|(old, descending)| {
                                        old == column && !descending
                                    }),
                                ));
                            }
                        }
                        ui.end_row();
                        for position in visible {
                            let row = rows[position];
                            while table.rows[row].len() < table.columns {
                                table.rows[row].push(String::new());
                            }
                            for column in 0..table.columns {
                                if cache.editing && !readonly {
                                    cache.dirty |= ui
                                        .add_sized(
                                            [cache.widths[column], 24.0],
                                            egui::TextEdit::singleline(
                                                &mut table.rows[row][column],
                                            ),
                                        )
                                        .changed();
                                } else {
                                    let cell = &table.rows[row][column];
                                    // Paint a bounded single-line preview;
                                    // multiline fields remain literal in the
                                    // draft, hover, shared Document and Undo.
                                    let text: String = cell
                                        .chars()
                                        .take(240)
                                        .map(|c| if c == '\n' || c == '\r' { '↵' } else { c })
                                        .collect();
                                    let response = ui.add_sized(
                                        [cache.widths[column], 24.0],
                                        egui::Label::new(text).truncate(),
                                    );
                                    if cell.len() <= 4096 {
                                        response.on_hover_text(cell);
                                    }
                                }
                            }
                            if ui.small_button("Delete row").clicked() && !readonly {
                                delete = Some(row);
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(row) = delete {
            table.rows.remove(row);
            cache.dirty = true;
        }
        let candidate = apply.then(|| {
            let source = self.tabs[index].state.doc.text();
            csv_table::serialize(
                table,
                cache.delimiter.unwrap_or(','),
                if source.contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                },
            )
        });
        if let Some(candidate) = candidate {
            let len = self.tabs[index].state.doc.len();
            self.tabs[index].state.apply_text_edits(
                vec![(TextRange::new(TextSize::new(0), len), candidate)],
                "Edit CSV table",
            );
        }
        true
    }
}

use super::{catalog::write_new, workspace::*};
use crate::a11y::AccessibleCombo;
use rustpy_db::{workspace::*, Value};
use std::path::Path;
impl DatabaseWorkspace {
    pub(super) fn grid_ui(&mut self, ui: &mut egui::Ui) {
        let Some(result) = self.current_result().cloned() else {
            return;
        };
        ui.separator();
        if matches!(self.current_dialect(), Dialect::Mongo | Dialect::Redis) {
            ui.label("Native key/document operations use their captured native editor; SQL rows cannot be submitted as native mutations.");
            return;
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} · offset {}", result.summary(), self.offset));
            if ui.button("Previous page").clicked() {
                self.offset = self.offset.saturating_sub(self.model.settings.page_size);
                self.page();
            }
            if ui.button("Next page").clicked() {
                self.offset = self.offset.saturating_add(self.model.settings.page_size);
                self.page();
            }
            if ui.button("Aggregate view (database)").clicked() {
                if let Some(object) = self.result_object().cloned() {
                    let d = self.current_dialect();
                    let mut columns = vec!["COUNT(*) AS row_count".to_string()];
                    for c in &object.columns {
                        let ty = c.data_type.to_ascii_uppercase();
                        if ["INT", "REAL", "FLOA", "DOUB", "DEC", "NUM"]
                            .iter()
                            .any(|part| ty.contains(part))
                        {
                            for function in ["SUM", "AVG", "MIN", "MAX"] {
                                columns.push(format!(
                                    "{function}({}) AS {}",
                                    d.quote(&c.name),
                                    d.quote(&format!(
                                        "{}_{}",
                                        c.name,
                                        function.to_ascii_lowercase()
                                    ))
                                ));
                            }
                        }
                    }
                    let sql = format!(
                        "SELECT {} FROM {}{}",
                        columns.join(","),
                        d.qualified(&object.qualified()),
                        if self.filter.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" WHERE ({})", self.filter)
                        }
                    );
                    self.table = None;
                    self.execute(sql);
                }
            }
            ui.checkbox(&mut self.transpose, "Transpose");
            ui.label("Find in loaded data");
            ui.text_edit_singleline(&mut self.find);
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("WHERE expression"));
            ui.add(egui::TextEdit::singleline(&mut self.order).hint_text("ORDER BY expression"));
            if ui.button("Apply filter / sort").clicked() {
                self.offset = 0;
                self.page();
            }
        });
        if self.table.is_none() {
            self.table = self.result_object().cloned();
        }
        egui::CollapsingHeader::new("Columns: visibility / ordering").show(ui, |ui| {
            for name in &result.columns {
                let mut show = !self.hidden.contains(name);
                if ui.checkbox(&mut show, name).changed() {
                    if show {
                        self.hidden.remove(name);
                    } else {
                        self.hidden.insert(name.clone());
                    }
                }
            }
            let mut order = self.column_order.join(",");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut order)
                        .hint_text("Column order: comma separated names"),
                )
                .changed()
            {
                self.column_order = order
                    .split(',')
                    .map(str::trim)
                    .map(str::to_string)
                    .collect();
            }
            ui.label(
                "Click a column header to sort; ORDER BY accepts multiple columns and ASC/DESC.",
            );
        });
        let mut shown: Vec<_> = result
            .columns
            .iter()
            .enumerate()
            .filter(|(_, n)| !self.hidden.contains(*n))
            .collect();
        shown.sort_by_key(|(_, name)| {
            self.column_order
                .iter()
                .position(|n| n == *name)
                .unwrap_or(usize::MAX)
        });
        let mut clicked = None;
        egui::ScrollArea::both()
            .id_salt("database-grid")
            .max_height(260.0)
            .show(ui, |ui| {
                egui::Grid::new("db-data-grid")
                    .striped(true)
                    .show(ui, |ui| {
                        if self.transpose {
                            for (index, name) in &shown {
                                ui.strong(*name);
                                for (row, values) in result.rows.iter().enumerate() {
                                    let value =
                                        values.get(*index).map(Value::display).unwrap_or_default();
                                    if ui
                                        .selectable_label(
                                            self.selected_cell == Some((row, *index)),
                                            &value,
                                        )
                                        .clicked()
                                    {
                                        clicked = Some((row, *index, value));
                                    }
                                }
                                ui.end_row();
                            }
                        } else {
                            for (_, name) in &shown {
                                if ui.button(*name).clicked() {
                                    self.order =
                                        format!("{} ASC", self.current_dialect().quote(name));
                                }
                            }
                            ui.end_row();
                            for (row, values) in result.rows.iter().enumerate() {
                                if !self.find.is_empty()
                                    && !values.iter().any(|v| {
                                        v.display()
                                            .to_lowercase()
                                            .contains(&self.find.to_lowercase())
                                    })
                                {
                                    continue;
                                }
                                for (index, _) in &shown {
                                    let text =
                                        values.get(*index).map(Value::display).unwrap_or_default();
                                    if ui
                                        .selectable_label(
                                            self.selected_cell == Some((row, *index)),
                                            &text,
                                        )
                                        .clicked()
                                    {
                                        clicked = Some((row, *index, text));
                                    }
                                }
                                ui.end_row();
                            }
                        }
                    });
            });
        if let Some((row, col, text)) = clicked {
            self.selected_cell = Some((row, col));
            self.cell_text = text;
        }
        if let Some((row, col)) = self.selected_cell {
            if let Some(value) = result.rows.get(row).and_then(|r| r.get(col)) {
                egui::CollapsingHeader::new("Value editor · text / JSON / binary / image")
                    .default_open(true)
                    .show(ui, |ui| {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&self.cell_text)
                        {
                            if ui.button("Format JSON").clicked() {
                                self.cell_text =
                                    serde_json::to_string_pretty(&json).unwrap_or_default();
                            }
                        }
                        ui.add(
                            egui::TextEdit::multiline(&mut self.cell_text)
                                .desired_rows(3)
                                .code_editor(),
                        );
                        ui.label(format!(
                            "{} · {}",
                            result.columns.get(col).cloned().unwrap_or_default(),
                            rustpy_db::workspace::statistics(&result, col)
                        ));
                        if let Value::Binary(bytes) = value {
                            ui.label(format!("{} binary bytes (hex editor)", bytes.len()));
                            if bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xff, 0xd8]) {
                                ui.add(
                                    egui::Image::from_bytes(
                                        format!(
                                            "bytes://database-{}-{row}-{col}",
                                            self.active.unwrap_or(0)
                                        ),
                                        bytes.clone(),
                                    )
                                    .max_width(400.0),
                                );
                            }
                            if ui.button("Save binary value").clicked() {
                                use std::io::Write;
                                match std::fs::OpenOptions::new()
                                    .create_new(true)
                                    .write(true)
                                    .open(&self.output_path)
                                {
                                    Ok(mut file) => {
                                        if let Err(e) = file.write_all(bytes) {
                                            self.last_error = e.to_string()
                                        }
                                    }
                                    Err(e) => self.last_error = e.to_string(),
                                }
                            }
                        }
                        if let Value::Text(text) = value {
                            if text.starts_with("data:image/") {
                                ui.add(egui::Image::new(text.clone()).max_width(400.0));
                            }
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Queue cell edit").clicked() {
                                if let Some(object) = self.result_object().cloned() {
                                    match row_key(&object, &result, row) {
                                        Ok(key) => {
                                            let column = result.columns[col].clone();
                                            let ty = object
                                                .columns
                                                .iter()
                                                .find(|c| c.name == column)
                                                .map(|c| c.data_type.as_str())
                                                .unwrap_or("");
                                            self.queue(Change::Update {
                                                table: object.qualified(),
                                                key,
                                                column,
                                                value: if matches!(value, Value::Binary(_)) {
                                                    match rustpy_db::injection::parse_hex(
                                                        &self.cell_text,
                                                    ) {
                                                        Ok(bytes) => Value::Binary(bytes),
                                                        Err(e) => {
                                                            self.last_error = e.to_string();
                                                            return;
                                                        }
                                                    }
                                                } else {
                                                    Value::parse_literal(&self.cell_text, ty)
                                                },
                                            });
                                        }
                                        Err(e) => self.last_error = e,
                                    }
                                } else {
                                    self.last_error =
                                        "Only a table page with a complete primary key is editable"
                                            .into();
                                }
                            }
                            if ui.button("Queue delete row").clicked() {
                                if let Some(object) = self.result_object().cloned() {
                                    match row_key(&object, &result, row) {
                                        Ok(key) => self.queue(Change::Delete {
                                            table: object.qualified(),
                                            key,
                                        }),
                                        Err(e) => self.last_error = e,
                                    }
                                }
                            }
                            if ui.button("Aggregate / Column statistics").clicked() {
                                self.output = rustpy_db::workspace::statistics(&result, col);
                            }
                            if ui.button("Foreign key navigation").clicked() {
                                if let Some(object) = self.result_object().cloned() {
                                    if let Some(fk) = object
                                        .keys
                                        .iter()
                                        .find(|k| Some(&k.column) == result.columns.get(col))
                                    {
                                        if let Some(target) = self
                                            .active
                                            .and_then(|id| self.objects.get(&id))
                                            .and_then(|objects| {
                                                objects.iter().find(|o| {
                                                    o.qualified() == fk.target
                                                        || o.name == fk.target
                                                })
                                            })
                                            .cloned()
                                        {
                                            let d = self.current_dialect();
                                            match dialect_literal(value, self.current_dialect()) {
                                                Ok(v) => {
                                                    self.filter = format!(
                                                        "{} = {v}",
                                                        d.quote(&fk.target_column)
                                                    );
                                                    self.table = Some(target);
                                                    self.offset = 0;
                                                    self.page();
                                                }
                                                Err(e) => self.last_error = e.to_string(),
                                            }
                                        }
                                    }
                                }
                            }
                        });
                    });
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add row from value JSON").clicked() {
                match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                    &self.cell_text,
                ) {
                    Ok(values) => {
                        if let Some(object) = self.result_object().cloned() {
                            let values = values
                                .into_iter()
                                .map(|(k, v)| (k, json_value(v)))
                                .collect();
                            self.queue(Change::Insert {
                                table: object.qualified(),
                                values,
                            });
                        }
                    }
                    Err(e) => self.last_error = e.to_string(),
                }
            }
            if ui.button("Copy with headers").clicked() {
                ui.ctx().copy_text(
                    rustpy_db::workspace::export(&result, "TSV", "", self.current_dialect())
                        .unwrap_or_default(),
                );
            }
            if ui
                .button("Compare loaded data with clipboard JSON")
                .clicked()
            {
                self.output = "Paste a JSON array into Import text, then press Compare data".into();
            }
            if ui.button("Compare data").clicked() {
                let current =
                    rustpy_db::workspace::export(&result, "JSON", "", self.current_dialect())
                        .unwrap_or_default();
                self.output = match (
                    serde_json::from_str::<serde_json::Value>(&current),
                    serde_json::from_str::<serde_json::Value>(&self.import_text),
                ) {
                    (Ok(a), Ok(b)) => {
                        if a == b {
                            "Loaded datasets are equal".into()
                        } else {
                            format!(
                                "Loaded data differs\nCurrent:\n{current}\nReference:\n{}",
                                self.import_text
                            )
                        }
                    }
                    _ => "Reference must be a JSON array matching exported columns".into(),
                };
            }
        });
        egui::CollapsingHeader::new("Import / Export / Extractors").show(ui,|ui|{egui::ComboBox::from_id_salt("db-export-format").selected_text(&self.export_format).show_ui(ui,|ui|{for format in ["CSV","TSV","JSON","SQL","Markdown","HTML"]{ui.selectable_value(&mut self.export_format,format.into(),crate::i18n::tr_static(format));}}).combo_named(crate::i18n::tr_static("Export format"));ui.text_edit_singleline(&mut self.output_path);ui.horizontal(|ui|{if ui.button("Export loaded result").clicked(){let text=rustpy_db::workspace::export(&result,&self.export_format,self.result_object().map(Object::qualified).as_deref().unwrap_or("table"),self.current_dialect());match text{Ok(text)=>if let Err(e)=write_new(&self.output_path,&text){self.last_error=e},Err(e)=>self.last_error=e.to_string()}}
if ui.button("Copy extracted result").clicked(){match rustpy_db::workspace::export(&result,&self.export_format,"table",self.current_dialect()){Ok(s)=>ui.ctx().copy_text(s),Err(e)=>self.last_error=e.to_string()}}});
            ui.label("Import CSV/TSV or paste clipboard (nothing is submitted until Database changes → Submit)");if ui.button("Load CSV / TSV file from output path").clicked(){match std::fs::metadata(&self.output_path){Ok(metadata)if metadata.len()<=32*1024*1024=>match std::fs::read_to_string(&self.output_path){Ok(text)=>self.import_text=text,Err(e)=>self.last_error=e.to_string()},Ok(_)=>self.last_error="Import exceeds 32 MiB".into(),Err(e)=>self.last_error=e.to_string()}}ui.checkbox(&mut self.import_tsv,"TSV");ui.checkbox(&mut self.import_headers,"First row is headers");ui.add(egui::TextEdit::multiline(&mut self.import_text).desired_rows(3));ui.add(egui::TextEdit::singleline(&mut self.import_mapping).hint_text("Mapping: input-index=column, e.g. 0=id,1=name"));if ui.button("Queue mapped import").clicked(){if let Some(object)=self.result_object().cloned(){let mapping=self.import_mapping.split(',').map(|pair|{let(index,name)=pair.trim().split_once('=').ok_or("Invalid mapping")?;Ok((index.parse::<usize>().map_err(|_|"Invalid input index")?,name.trim().to_string()))}).collect::<Result<Vec<_>,&str>>();match mapping{Ok(mapping)=>match rustpy_db::schema::import_delimited(&self.import_text,if self.import_tsv{b'\t'}else{b','},&object.qualified(),&mapping,self.import_headers){Ok(changes)=>for change in changes{self.queue(change)},Err(e)=>self.last_error=e.to_string()},Err(e)=>self.last_error=e.into()}}}
            ui.label("Excel/XLSX and Big Data Tools require plugins; no built-in connector is claimed.");for extractor in self.model.settings.extractors.clone(){if ui.button(format!("Run extractor: {}",extractor.name)).clicked(){if self.extractor_process.is_some(){self.last_error="An extractor is already running".into();continue;}let input=rustpy_db::workspace::export(&result,"JSON","",self.current_dialect()).unwrap_or_default();match rustpy_db::extractor::start(&extractor,&input,&self.output_path,self.root.as_deref().unwrap_or(Path::new("."))){Ok(child)=>self.extractor_process=Some(child),Err(e)=>self.last_error=e.to_string()}}}
        });
    }
}
fn row_key(
    object: &Object,
    result: &rustpy_db::QueryResult,
    row: usize,
) -> Result<Vec<(String, Value)>, String> {
    let mut key = vec![];
    for column in object.columns.iter().filter(|c| c.primary) {
        let index = result
            .columns
            .iter()
            .position(|n| *n == column.name)
            .ok_or("Primary key column absent from result")?;
        key.push((
            column.name.clone(),
            result
                .rows
                .get(row)
                .and_then(|r| r.get(index))
                .ok_or("Row no longer exists")?
                .clone(),
        ));
    }
    if key.is_empty() {
        return Err("This object has no primary key; unsafe row edits are refused".into());
    }
    Ok(key)
}
fn json_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Real))
            .unwrap_or(Value::Null),
        serde_json::Value::String(s) => Value::Text(s),
        other => Value::Text(other.to_string()),
    }
}

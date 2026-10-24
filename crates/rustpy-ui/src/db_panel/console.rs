use super::workspace::*;
use crate::a11y::AccessibleCombo;
use rustpy_db::session::Request;
use rustpy_db::Value;
impl DatabaseWorkspace {
    pub(super) fn console_ui(&mut self, ui: &mut egui::Ui) {
        if self.active.is_none() {
            return;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.full_text)
                    .hint_text("Full-text search in selected database"),
            );
            if ui.button("Search database text columns").clicked() {
                self.table = None;
                self.send(Request::Search(self.full_text.clone()));
            }
        });
        let active = self.active;
        let mut execute = None;
        let mut request = None;
        let mut params = false;
        if let Some(console) = self.owners.iter_mut().find(|c| Some(c.owner.id) == active) {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Execute statement").clicked() {
                    execute = Some((1, console.sql.clone()));
                }
                if ui.button("Execute selection").clicked() {
                    execute = Some((2, console.sql.clone()));
                }
                if ui.button("Execute script").clicked() {
                    execute = Some((0, console.sql.clone()));
                }
                if ui.button("Explain (text / visual)").clicked() {
                    execute = Some((3, console.sql.clone()));
                }
                if ui.button("Format SQL").clicked() {
                    match rustpy_format::fragments::format(
                        rustpy_format::fragments::Language::Sql,
                        &console.sql,
                        Default::default(),
                    ) {
                        Ok(sql) => console.sql = sql,
                        Err(e) => self.last_error = e.to_string(),
                    }
                }
                let mut manual = console.manual;
                if ui.checkbox(&mut manual, "Manual transaction").changed() {
                    request = Some(Request::AutoCommit(!manual));
                }
                if ui.button("Commit").clicked() {
                    request = Some(Request::Commit);
                }
                if ui.button("Rollback").clicked() {
                    request = Some(Request::Rollback);
                }
                if ui.button("Parameters").clicked() {
                    params = true;
                }
            });
            let output = egui::TextEdit::multiline(&mut console.sql)
                .id(egui::Id::new(("database-console", console.owner.id)))
                .code_editor()
                .desired_rows(6)
                .show(ui);
            if let Some(position) = console.navigate_error.take() {
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), output.response.id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(
                            egui::text::CCursor::new(position.min(console.sql.chars().count())),
                        )));
                    state.store(ui.ctx(), output.response.id);
                    ui.memory_mut(|m| m.request_focus(output.response.id));
                }
            }
            if let Some((kind, sql)) = execute.take() {
                let selected = output
                    .cursor_range
                    .map(|range| {
                        let start = sql
                            .char_indices()
                            .nth(usize::from(range.sorted_cursors()[0].index))
                            .map(|(i, _)| i)
                            .unwrap_or(sql.len());
                        let end = sql
                            .char_indices()
                            .nth(usize::from(range.sorted_cursors()[1].index))
                            .map(|(i, _)| i)
                            .unwrap_or(sql.len());
                        (start, end)
                    })
                    .unwrap_or((0, 0));
                let caret = selected.1;
                let text = match kind {
                    2 => {
                        if selected.0 == selected.1 {
                            self.last_error = "Select SQL text before executing a selection".into();
                            String::new()
                        } else {
                            sql[selected.0..selected.1].into()
                        }
                    }
                    1 => rustpy_db::split_statements(&sql)
                        .into_iter()
                        .find(|(range, _)| {
                            usize::from(range.start()) <= caret && caret <= usize::from(range.end())
                        })
                        .map(|(_, s)| s)
                        .unwrap_or_default(),
                    3 => sql,
                    _ => sql,
                };
                execute = Some((kind, text));
            }
            if console.owner.busy {
                ui.label("Executing on the captured session… Cancel closes this session after the driver returns; driver timeout support varies.");
            }
            let completions = rustpy_db::injection::completions(
                &console.sql,
                self.objects
                    .get(&console.owner.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            egui::CollapsingHeader::new("Schema completion / SQL inspections").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for word in completions.into_iter().take(60) {
                        if ui.small_button(&word).clicked() {
                            console.sql.push_str(&word);
                        }
                    }
                });
                for issue in rustpy_db::injection::inspect_sql(
                    &console.sql,
                    self.objects
                        .get(&console.owner.id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ) {
                    ui.colored_label(egui::Color32::YELLOW, issue);
                }
            });
        }
        if let Some(request) = request {
            self.send(request);
        }
        if let Some((kind, sql)) = execute {
            if !sql.trim().is_empty() {
                self.table = None;
                if kind == 3 {
                    if let Some(c) = self
                        .owners
                        .iter_mut()
                        .find(|c| Some(c.owner.id) == self.active)
                    {
                        c.running_sql = format!("EXPLAIN {sql}");
                    }
                    self.send(Request::Explain(sql));
                } else {
                    self.execute(sql);
                }
            }
        }
        if params || !self.parameters.is_empty() {
            egui::CollapsingHeader::new("Parameters (values are SQL literals: 'text', 42, NULL)")
                .default_open(true)
                .show(ui, |ui| {
                    for (name, value) in &mut self.parameters {
                        ui.horizontal(|ui| {
                            ui.label(name);
                            ui.text_edit_singleline(value);
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.param_name).hint_text("Name"));
                        ui.text_edit_singleline(&mut self.param_value);
                        if ui.button("Add parameter").clicked() {
                            self.parameters
                                .insert(self.param_name.clone(), self.param_value.clone());
                        }
                    });
                });
        }
        ui.horizontal_wrapped(|ui| {
            let isolation_label = ui.label("Isolation");
            egui::ComboBox::from_id_salt("database-isolation")
                .selected_text(&self.isolation)
                .show_ui(ui, |ui| {
                    for level in [
                        "READ UNCOMMITTED",
                        "READ COMMITTED",
                        "REPEATABLE READ",
                        "SERIALIZABLE",
                    ] {
                        ui.selectable_value(
                            &mut self.isolation,
                            level.into(),
                            crate::i18n::tr_static(level),
                        );
                    }
                })
                .combo_labelled(&isolation_label);
            if ui.button("Apply isolation").clicked() {
                self.send(Request::Isolation(self.isolation.clone()));
            }
            if ui.button("Toggle read-only").clicked() {
                let value = self
                    .owners
                    .iter()
                    .find(|c| Some(c.owner.id) == self.active)
                    .is_some_and(|c| !c.read_only);
                self.send(Request::ReadOnly(value));
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.script_path)
                    .hint_text("SQL script file (relative to Scripts directory)"),
            );
            if ui.button("Load script").clicked() {
                if let Some(root) = &self.root {
                    let path = root
                        .join(&self.model.settings.scripts_directory)
                        .join(&self.script_path);
                    match std::fs::read_to_string(&path) {
                        Ok(sql) => {
                            if let Some(c) = self
                                .owners
                                .iter_mut()
                                .find(|c| Some(c.owner.id) == self.active)
                            {
                                c.sql = sql;
                            }
                        }
                        Err(e) => self.last_error = e.to_string(),
                    }
                }
            }
        });
        egui::CollapsingHeader::new("Query history").show(ui, |ui| {
            for record in self.model.history.clone().into_iter().rev().take(100) {
                if ui
                    .selectable_label(
                        false,
                        format!(
                            "{} · {} ms · {}",
                            record.session,
                            record.elapsed_ms,
                            record.sql.lines().next().unwrap_or("")
                        ),
                    )
                    .clicked()
                {
                    if let Some(c) = self
                        .owners
                        .iter_mut()
                        .find(|c| Some(c.owner.id) == self.active)
                    {
                        c.sql = record.sql;
                    }
                }
            }
            if ui.button("Save history").clicked() {
                self.save();
            }
        });
        if let Some(c) = self
            .owners
            .iter_mut()
            .find(|c| Some(c.owner.id) == self.active)
        {
            egui::CollapsingHeader::new("Output (messages / errors)").show(ui, |ui| {
                for message in &c.messages {
                    if ui
                        .link(message)
                        .on_hover_text("Navigate to the captured SQL statement")
                        .clicked()
                    {
                        c.sql = c.running_sql.clone();
                        c.navigate_error = Some(error_position(message, &c.sql));
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (index, (_, result)) in c.results.iter().enumerate() {
                    ui.selectable_value(
                        &mut c.result,
                        index,
                        format!("Result {} · {}", index + 1, result.summary()),
                    );
                }
            });
            if let Some((sql, result)) = c.results.get(c.result) {
                if sql.trim_start().to_uppercase().starts_with("EXPLAIN")
                    || c.running_sql.starts_with("EXPLAIN ")
                {
                    super::plan::show(ui, result);
                }
                if result
                    .columns
                    .iter()
                    .any(|name| ["detail", "QUERY PLAN", "PLAN", "parent"].contains(&name.as_str()))
                {
                    egui::CollapsingHeader::new("Query plan viewer").show(ui, |ui| {
                        for row in &result.rows {
                            let depth = row
                                .get(1)
                                .and_then(|v| v.display().parse::<usize>().ok())
                                .unwrap_or(0)
                                .min(16);
                            ui.horizontal(|ui| {
                                ui.add_space(depth as f32 * 8.0);
                                ui.label(
                                    row.iter()
                                        .map(Value::display)
                                        .collect::<Vec<_>>()
                                        .join(" · "),
                                );
                            });
                        }
                    });
                }
            }
        }
    }
}

fn error_position(message: &str, sql: &str) -> usize {
    let lower = message.to_ascii_lowercase();
    for marker in ["at character ", "position: "] {
        if let Some(at) = lower.find(marker) {
            let number = lower[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(index) = number.parse::<usize>() {
                return index.saturating_sub(1);
            }
        }
    }
    if let Some(at) = lower.find("line ") {
        let number = lower[at + 5..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if let Ok(line) = number.parse::<usize>() {
            return sql
                .split_inclusive('\n')
                .take(line.saturating_sub(1))
                .map(|s| s.chars().count())
                .sum();
        }
    }
    0
}

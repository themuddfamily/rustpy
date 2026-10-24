use super::workspace::*;
use rustpy_db::{session::Request, workspace::*};
impl DatabaseWorkspace {
    pub(super) fn catalog_ui(&mut self, ui: &mut egui::Ui) {
        let objects = self
            .active
            .and_then(|id| self.objects.get(&id))
            .cloned()
            .unwrap_or_default();
        if !self.graph.is_empty() {
            self.er_diagram_ui(ui, &objects);
        }
        egui::CollapsingHeader::new(format!("Database objects ({})", objects.len()))
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("db-catalog")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for object in &objects {
                            if !self.search.is_empty()
                                && !object
                                    .qualified()
                                    .to_lowercase()
                                    .contains(&self.search.to_lowercase())
                            {
                                continue;
                            }
                            egui::CollapsingHeader::new(format!(
                                "{} · {} ({} columns)",
                                object.qualified(),
                                object.kind,
                                object.columns.len()
                            ))
                            .id_salt((object.qualified(), object.routine.as_ref().map(|r| &r.specific_name)))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Open editor / Data").clicked() {
                                        self.table = Some(object.clone());
                                        self.offset = 0;
                                        self.hidden.clear();
                                        self.page();
                                    }
                                    if ui.button("DDL").clicked() {
                                        self.output = object.ddl.clone()
                                            + ";\n"
                                            + &object.related_ddl.join(";\n");
                                    }
                                    if ui
                                        .button("Inspect indexes / keys / checks / partitions")
                                        .clicked()
                                    {
                                        self.table = None;
                                        self.send(Request::ObjectDetails(Box::new(object.clone())));
                                    }
                                    if ui.button("Modify table").clicked() {
                                        self.design = serde_json::to_string_pretty(
                                            &rustpy_db::schema::TableDesign {
                                                name: object.qualified(),
                                                columns: object.columns.clone(),
                                                primary: object
                                                    .columns
                                                    .iter()
                                                    .filter(|c| c.primary)
                                                    .map(|c| c.name.clone())
                                                    .collect(),
                                                foreign_keys: object.keys.clone(),
                                                ..Default::default()
                                            },
                                        )
                                        .unwrap_or_default();
                                        self.design_open = true;
                                    }
                                    if object.kind == "ROUTINE" {
                                        if let Some(signature) = &object.routine {
                                            ui.label(format!("{:?} · {:?}", signature.kind, signature.result_shape));
                                            for parameter in &signature.parameters {
                                                ui.label(format!("{} · {:?} {}: {}{}", parameter.ordinal,
                                                    parameter.mode, parameter.name, parameter.data_type,
                                                    if parameter.has_default { " (default available)" } else { "" }));
                                            }
                                            for omit_defaults in [false, true] {
                                                if omit_defaults && !signature.parameters.iter().any(|p| p.has_default) { continue; }
                                                let label = if omit_defaults { "Run routine with defaults…" } else { "Run routine…" };
                                                if ui.button(label).clicked() {
                                                    match rustpy_db::routine::invocation_with_prefix(self.current_dialect(), &object.schema, &object.name, signature, omit_defaults, &self.model.settings.parameter_prefix) {
                                                        Ok(invocation) => {
                                                            if let Some(c) = self.owners.iter_mut().find(|c| Some(c.owner.id) == self.active) { c.sql = invocation.sql; }
                                                            self.parameters.clear();
                                                            for name in invocation.parameters { self.parameters.insert(name, String::new()); }
                                                        }
                                                        Err(error) => self.last_error = error.to_string(),
                                                    }
                                                }
                                            }
                                        } else {
                                            ui.label("Routine signature is unavailable from this provider; enter its invocation in the query console.");
                                        }
                                    }
                                });
                                for column in &object.columns {
                                    ui.label(format!(
                                        "{}: {}{}{}",
                                        column.name,
                                        column.data_type,
                                        if column.primary { " · PK" } else { "" },
                                        if column.nullable { "" } else { " · NOT NULL" }
                                    ));
                                }
                                for fk in &object.keys {
                                    if ui
                                        .link(format!(
                                            "{} → {}.{}",
                                            fk.column, fk.target, fk.target_column
                                        ))
                                        .clicked()
                                    {
                                        if let Some(target) = objects.iter().find(|o| {
                                            o.qualified() == fk.target || o.name == fk.target
                                        }) {
                                            self.table = Some(target.clone());
                                            self.offset = 0;
                                            self.page();
                                        }
                                    }
                                }
                            });
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("New → Table / View / Column / Index…").clicked() {
                        self.design = serde_json::to_string_pretty(
                            &rustpy_db::schema::TableDesign::default(),
                        )
                        .unwrap_or_default();
                        self.design_open = true;
                    }
                    if ui.button("ER diagram").clicked() {
                        self.graph = rustpy_db::workspace::diagram(&objects);
                        self.output = self.graph.clone();
                    }
                    if ui.button("Capture schema for comparison").clicked() {
                        self.schema_reference =
                            serde_json::to_string_pretty(&objects).unwrap_or_default();
                    }
                    if ui.button("Compare schemas").clicked() {
                        match serde_json::from_str::<Vec<Object>>(&self.schema_reference) {
                            Ok(old) => {
                                self.comparison = rustpy_db::workspace::schema_diff(
                                    &old,
                                    &objects,
                                    self.current_dialect(),
                                );
                                self.output = self.comparison.clone();
                            }
                            Err(e) => self.last_error = e.to_string(),
                        }
                    }
                    if ui.button("Generate DDL").clicked() {
                        self.output = objects.iter().map(|o| o.ddl.clone() + ";\n").collect();
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.output_path)
                        .hint_text("Export diagram / schema snapshot / output path"),
                );
                if ui.button("Export displayed schema / diagram").clicked() {
                    if let Err(e) = write_new(&self.output_path, &self.output) {
                        self.last_error = e;
                    }
                }
                if !self.schema_reference.is_empty() {
                    egui::CollapsingHeader::new(
                        "Reference schema JSON (paste another source snapshot)",
                    )
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.schema_reference)
                                .code_editor()
                                .desired_rows(3),
                        );
                    });
                }
            });
    }
    pub(super) fn design_ui(&mut self, ui: &mut egui::Ui) {
        if !self.design_open {
            return;
        }
        egui::CollapsingHeader::new("Modify / New object — SQL preview").default_open(true).show(ui,|ui|{
            ui.label("Edit columns, keys, indexes, foreign keys, checks, partition clause or view query. Inspect attributes to find existing constraint/index names before dropping them.");
            ui.add(egui::TextEdit::multiline(&mut self.design).code_editor().desired_rows(8));
            match serde_json::from_str::<rustpy_db::schema::TableDesign>(&self.design){
                Ok(design)=>{
                    let d=self.current_dialect();
                    let existing=self.active.and_then(|id|self.objects.get(&id)).and_then(|objects|objects.iter().find(|o|o.qualified()==design.name||o.name==design.name)).cloned();
                    let preview=if let Some(old)=&existing{rustpy_db::schema::alter_sql(old,&design,d)}else{design.create_sql(d)};
                    match preview{
                        Ok(mut sql)=>{ui.add(egui::TextEdit::multiline(&mut sql).code_editor().desired_rows(5));if ui.button("Queue reviewed SQL in Database changes").clicked(){self.queue(Change::Sql(sql));}},
                        Err(error)=>{
                            ui.label(error.to_string());
                            if d==Dialect::Sqlite{if let Some(old)=existing{
                                match rustpy_db::schema::sqlite_rebuild_preview(&old,&design){
                                    Ok(mut sql)=>{ui.label("Atomic rebuild retains existing data, indexes and triggers, removes dropped columns and validates foreign keys. Submit separately in auto-commit mode.");ui.add(egui::TextEdit::multiline(&mut sql).code_editor().desired_rows(5));if ui.button("Queue reviewed SQLite rebuild").clicked(){self.queue(Change::SqliteRebuild{old,design:Box::new(design.clone())});}},
                                    Err(error)=>{ui.label(error.to_string());}
                                }
                            }}
                        }
                    }
                },
                Err(error)=>{ui.label(error.to_string());}
            }
            if ui.button("Close object editor").clicked(){self.design_open=false;}
        });
    }
    pub(super) fn pending_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(format!("Database changes ({} pending)", self.changes.len()))
            .show(ui, |ui| {
                let dialect = self.current_dialect();
                for change in &self.changes {
                    ui.monospace(change.sql(dialect).unwrap_or_else(|e| e.to_string()));
                }
                let submitting = self.owners.iter().any(|c| {
                    Some(c.owner.id) == self.change_owner && c.owner.busy && c.submitted > 0
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!submitting, egui::Button::new("Submit"))
                        .clicked()
                    {
                        self.send(Request::Submit(self.changes.clone()));
                    }
                    if ui
                        .add_enabled(!submitting, egui::Button::new("Discard queued edits"))
                        .clicked()
                    {
                        self.changes.clear();
                    }
                });
            });
    }
}
pub(super) fn write_new(path: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    if path.trim().is_empty() {
        return Err("Choose an output path".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

impl DatabaseWorkspace {
    pub(super) fn er_diagram_ui(&mut self, ui: &mut egui::Ui, objects: &[Object]) {
        egui::CollapsingHeader::new("ER diagram canvas")
            .default_open(true)
            .show(ui, |ui| {
                let objects = &objects[..objects.len().min(100)];
                let columns = (objects.len() as f32).sqrt().ceil().max(1.0) as usize;
                egui::ScrollArea::both()
                    .id_salt("database-er")
                    .max_height(400.0)
                    .show(ui, |ui| {
                        let size = egui::vec2(
                            columns as f32 * 240.0,
                            (objects.len().div_ceil(columns)) as f32 * 150.0,
                        );
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let painter = ui.painter_at(rect);
                        let positions = objects
                            .iter()
                            .enumerate()
                            .map(|(i, _)| {
                                rect.min
                                    + egui::vec2(
                                        (i % columns) as f32 * 240.0,
                                        (i / columns) as f32 * 150.0,
                                    )
                            })
                            .collect::<Vec<_>>();
                        for (i, object) in objects.iter().enumerate() {
                            for key in &object.keys {
                                if let Some(j) = objects.iter().position(|o| {
                                    o.qualified() == key.target || o.name == key.target
                                }) {
                                    painter.arrow(
                                        positions[i] + egui::vec2(200.0, 35.0),
                                        positions[j] + egui::vec2(0.0, 35.0)
                                            - (positions[i] + egui::vec2(200.0, 35.0)),
                                        egui::Stroke::new(1.0, egui::Color32::LIGHT_BLUE),
                                    );
                                }
                            }
                        }
                        for (i, object) in objects.iter().enumerate() {
                            let node =
                                egui::Rect::from_min_size(positions[i], egui::vec2(200.0, 130.0));
                            painter.rect_filled(node, 4.0, egui::Color32::from_gray(35));
                            painter.text(
                                node.min + egui::vec2(8.0, 8.0),
                                egui::Align2::LEFT_TOP,
                                format!(
                                    "{}\n{}",
                                    object.qualified(),
                                    object
                                        .columns
                                        .iter()
                                        .take(5)
                                        .map(|c| format!(
                                            "{}{}: {}",
                                            if c.primary { "🔑 " } else { "" },
                                            c.name,
                                            c.data_type
                                        ))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                ),
                                egui::FontId::monospace(11.0),
                                egui::Color32::WHITE,
                            );
                            if ui
                                .interact(
                                    node,
                                    egui::Id::new(("er-object", object.qualified())),
                                    egui::Sense::click(),
                                )
                                .double_clicked()
                            {
                                self.table = Some(object.clone());
                                self.page();
                            }
                        }
                    });
            });
    }
    pub(super) fn migrations_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Migrations awareness (Alembic / Django)").show(ui,|ui|{if ui.button("Refresh project migration sources").clicked()&&self.migration_job.is_none(){if let Some(root)=self.root.clone(){let(tx,rx)=std::sync::mpsc::sync_channel(1);self.migration_job=Some(rx);let _=std::thread::Builder::new().name("database-migrations".into()).spawn(move||{let migrations=rustpy_db::injection::migrations(&root);let _=tx.send((root,migrations));});}}for migration in &self.migrations{if ui.link(format!("{} · {} · {}",migration.framework,migration.revision,migration.path.display())).clicked(){self.events.push(super::DbEvent::Navigate(migration.path.clone(),1));}for dependency in &migration.dependencies{ui.monospace(dependency);}}ui.label("Source awareness never imports settings or runs migrations. Use the framework task runner for an explicitly selected migration.");});
    }
}

#[cfg(test)]
mod connected_routine_tests {
    use super::*;
    use crate::db_panel::properties::ui_fixture::Fixture;
    use rustpy_db::routine::{
        ParameterMode, Routine, RoutineKind, RoutineParameter, RoutineResultShape,
    };

    #[test]
    fn published_routine_and_defaults_fill_the_active_parameter_pane_and_execute_owned_sqlite() {
        let mut f = Fixture::new();
        let source = Source {
            id: 1,
            name: "Local SQL".into(),
            file: f.root.path().join("local.sqlite"),
            ..Default::default()
        };
        f.panel.workspace.model.sources.push(source);
        f.panel.workspace.selected = Some(1);
        f.panel.workspace.model.settings.parameter_prefix = "@".into();
        f.frame(vec![]);
        f.frame(vec![]);
        f.activate("Connect / New console");
        assert!(
            f.panel.workspace.owners.is_empty(),
            "untrusted panel cannot admit a session"
        );
        f.panel.set_database_execution_allowed(true);
        f.activate("Connect / New console");
        f.ready();
        assert_eq!(f.panel.workspace.owners.len(), 1);
        let active = f.panel.workspace.active.unwrap();
        f.activate("Connect / New console");
        f.ready();
        let inactive = f.panel.workspace.active.unwrap();
        assert_ne!(active, inactive);
        f.panel
            .workspace
            .owners
            .iter_mut()
            .find(|c| c.owner.id == inactive)
            .unwrap()
            .sql = "SELECT 'inactive stays unchanged'".into();
        f.activate(&format!("Local SQL / console {active}"));
        assert_eq!(f.panel.workspace.active, Some(active));
        // Isolate the real catalog-button consumer from provider discovery.
        // SQLite's actual builtin accepts both substr(text,start,length) and
        // substr(text,start); it independently checks the generated calls below.
        let parameters: Vec<_> = (1..=3)
            .map(|ordinal| RoutineParameter {
                name: format!("input{ordinal}"),
                data_type: if ordinal == 1 { "text" } else { "integer" }.into(),
                mode: ParameterMode::In,
                ordinal,
                has_default: ordinal == 3,
                variadic: false,
                type_schema: String::new(),
                type_name: String::new(),
            })
            .collect();
        let routine: Routine = serde_json::from_value(serde_json::json!({
            "kind": RoutineKind::Function,
            "result_shape": RoutineResultShape::Scalar,
            "specific_name": "sqlite-builtin-substr",
            "parameters": parameters,
            "allowed_default_input_counts": [2]
        }))
        .unwrap();
        let object = Object {
            routine: Some(routine),
            related_ddl: vec![],
            catalog: String::new(),
            schema: String::new(),
            name: "substr".into(),
            kind: "ROUTINE".into(),
            columns: vec![],
            keys: vec![],
            ddl: String::new(),
        };
        f.panel.workspace.objects.insert(active, vec![object]);
        f.frame(vec![]);
        f.activate("substr · ROUTINE (0 columns)");
        f.panel
            .workspace
            .parameters
            .insert("stale_from_previous_query".into(), "99".into());
        f.activate("Run routine…");
        let console = f
            .panel
            .workspace
            .owners
            .iter()
            .find(|c| c.owner.id == active)
            .unwrap();
        assert_eq!(console.sql, "SELECT \"substr\"(@arg_1, @arg_2, @arg_3)");
        assert_eq!(
            f.panel
                .workspace
                .parameters
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["arg_1", "arg_2", "arg_3"]
        );
        f.replace_parameter("arg_1", "'héllo'");
        f.replace_parameter("arg_2", "2");
        f.replace_parameter("arg_3", "2");
        assert_eq!(f.panel.workspace.parameters["arg_1"], "'héllo'");
        f.activate("Execute script");
        f.ready();
        let console = f
            .panel
            .workspace
            .owners
            .iter()
            .find(|c| c.owner.id == active)
            .unwrap();
        assert_eq!(console.results[0].1.rows[0][0].display(), "él");
        f.activate("Run routine with defaults…");
        let console = f
            .panel
            .workspace
            .owners
            .iter()
            .find(|c| c.owner.id == active)
            .unwrap();
        assert_eq!(console.sql, "SELECT \"substr\"(@arg_1, @arg_2)");
        assert_eq!(
            f.panel
                .workspace
                .parameters
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["arg_1", "arg_2"]
        );
        f.replace_parameter("arg_1", "'héllo'");
        f.replace_parameter("arg_2", "2");
        f.activate("Execute script");
        f.ready();
        let console = f
            .panel
            .workspace
            .owners
            .iter()
            .find(|c| c.owner.id == active)
            .unwrap();
        assert_eq!(console.results[0].1.rows[0][0].display(), "éllo");
        assert_eq!(
            f.panel
                .workspace
                .owners
                .iter()
                .find(|c| c.owner.id == inactive)
                .unwrap()
                .sql,
            "SELECT 'inactive stays unchanged'"
        );
        f.retire();
    }
}

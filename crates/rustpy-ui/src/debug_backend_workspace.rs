//! Explicit suspension-owned value views. Opening a renderer executes in the selected frame.
use rustpy_debug::DebugSession;
pub fn show(ui: &mut egui::Ui, session: &mut DebugSession) -> Option<(std::path::PathBuf, usize)> {
    let mut navigate = None;
    ui.collapsing("Python value views", |ui| {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(session.is_stopped(), egui::Button::new("Async tasks"))
                .clicked()
            {
                session.inspect_async_tasks();
            }
            if ui
                .add_enabled(session.is_stopped(), egui::Button::new("Force Step Into"))
                .clicked()
            {
                session.force_step_into();
            }
            if ui.button("Export threads").clicked() {
                ui.ctx().copy_text(
                    session
                        .threads
                        .iter()
                        .map(|(id, name)| format!("{id}: {name}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            if ui.button("Copy stack").clicked() {
                ui.ctx().copy_text(session.copy_stack());
            }
            ui.checkbox(
                &mut session.options.backend.show_special,
                "Special variables",
            );
            ui.checkbox(
                &mut session.options.backend.show_protected,
                "Protected variables",
            );
            ui.checkbox(
                &mut session.options.backend.sort_variables,
                "Sort variables",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Rows");
            ui.add(egui::DragValue::new(&mut session.backend.rows).range(1..=1000));
            ui.label("Offset");
            ui.add(egui::DragValue::new(&mut session.backend.offset));
            ui.label("Slice");
            ui.text_edit_singleline(&mut session.backend.slice);
            ui.label("Filter");
            ui.text_edit_singleline(&mut session.backend.filter);
        });
        if let Some(info) = session.protocol.custom_results.get("pydevdSystemInfo") {
            ui.collapsing("Debugger acceleration", |ui| {
                ui.label(format!(
                    "Cython: {} · Frame evaluation: {}",
                    info["pydevd"]["usingCython"], info["pydevd"]["usingFrameEval"]
                ));
            });
        }
        if session.backend.pending_view {
            crate::a11y::spinner(ui);
        }
        if let Some((expression, kind, text)) = session.backend.value_view.clone() {
            ui.label(format!("{expression} ({kind})"));
            if ui.button("Copy rendered value").clicked() {
                ui.ctx().copy_text(text.clone());
            }
            if matches!(kind.as_str(), "source" | "type_source") {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(path) = value["path"].as_str() {
                        if ui.button("Open source").clicked() {
                            navigate = Some((
                                session.local_source_path(path),
                                value["line"].as_u64().unwrap_or(1) as usize,
                            ));
                        }
                    }
                }
            }
            if kind == "image" {
                use base64::Engine as _;
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                text.hash(&mut hash);
                let hash = hash.finish();
                let key = egui::Id::new("debug_value_image");
                let mut cached = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<(u64, egui::TextureHandle)>(key));
                if cached
                    .as_ref()
                    .is_none_or(|(previous, _)| *previous != hash)
                {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&text) {
                        let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes))
                            .with_guessed_format()
                            .ok()
                            .and_then(|reader| reader.into_dimensions().ok());
                        if dimensions.is_some_and(|(width, height)| {
                            u64::from(width) * u64::from(height) <= 16 * 1024 * 1024
                        }) {
                            if let Ok(image) = image::load_from_memory(&bytes) {
                                let rgba = image.to_rgba8();
                                let image = egui::ColorImage::from_rgba_unmultiplied(
                                    [rgba.width() as usize, rgba.height() as usize],
                                    rgba.as_raw(),
                                );
                                let texture = ui.ctx().load_texture(
                                    "debug_value_image",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                );
                                cached = Some((hash, texture));
                                if let Some(cached) = cached.clone() {
                                    ui.ctx().data_mut(|data| data.insert_temp(key, cached));
                                }
                            }
                        }
                    }
                }
                if let Some((_, texture)) = cached {
                    ui.add(egui::Image::new(&texture).max_size(egui::vec2(640.0, 400.0)));
                } else {
                    ui.label("Image preview unavailable or exceeds pixel budget");
                }
            } else {
                egui::ScrollArea::both().max_height(240.0).show(ui, |ui| {
                    ui.monospace(text);
                });
            }
        }
        if session.backend.data_view.is_some() {
            if ui.button("Open in SciView").clicked() {
                session.backend.scientific_requested = true;
            }
            show_table(ui, session);
        }
        if let Some(tasks) = &session.backend.async_tasks {
            match serde_json::from_str::<Vec<serde_json::Value>>(tasks) {
                Ok(tasks) => {
                    for (index, task) in tasks.iter().enumerate() {
                        ui.push_id(("async_task", index), |ui| {
                            ui.collapsing(
                                format!(
                                    "{} — {}",
                                    task["name"].as_str().unwrap_or("Task"),
                                    if task["cancelled"].as_bool().unwrap_or(false) {
                                        "cancelled"
                                    } else if task["done"].as_bool().unwrap_or(false) {
                                        "done"
                                    } else {
                                        "awaiting"
                                    }
                                ),
                                |ui| {
                                    if let Some(frames) = task["await_chain"].as_array() {
                                        for frame in frames {
                                            let path = frame["path"].as_str().unwrap_or("");
                                            let line = frame["line"].as_u64().unwrap_or(1) as usize;
                                            if ui
                                                .button(format!(
                                                    "{} — {path}:{line}",
                                                    frame["name"].as_str().unwrap_or("frame")
                                                ))
                                                .clicked()
                                            {
                                                navigate =
                                                    Some((session.local_source_path(path), line));
                                            }
                                        }
                                    }
                                },
                            );
                        });
                    }
                }
                Err(_) => {
                    ui.label(tasks);
                }
            }
        }
        for (label, expression) in &session.backend.labels {
            ui.label(format!("{label}: {expression}"));
        }
    });
    navigate
}

pub fn show_table(ui: &mut egui::Ui, session: &mut DebugSession) {
    let Some(table) = session.backend.data_view.clone() else {
        return;
    };
    ui.heading(&session.backend.data_expression);
    ui.horizontal(|ui| {
        ui.label("Sort column");
        ui.add(egui::DragValue::new(&mut session.backend.sort_column));
        ui.checkbox(&mut session.backend.heatmap, "Heatmap");
        ui.label("Decimal places");
        ui.add(egui::DragValue::new(&mut session.backend.number_precision).range(0..=16));
        if ui
            .add_enabled(
                session.is_stopped(),
                egui::Button::new("Refresh slice / page"),
            )
            .clicked()
        {
            let expression = session.backend.data_expression.clone();
            session.inspect_value(&expression, "table");
        }
    });
    ui.label(format!(
        "{} — {} rows; page starts at {}",
        table["type"], table["total"], table["offset"]
    ));
    let mut rows = table["rows"].as_array().cloned().unwrap_or_default();
    let column = session.backend.sort_column;
    rows.sort_by(|a, b| {
        let a = a[column].as_str().unwrap_or("");
        let b = b[column].as_str().unwrap_or("");
        match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(a), Ok(b)) => a.total_cmp(&b),
            _ => a.cmp(b),
        }
    });
    let numbers = rows
        .iter()
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(|value| value.as_str()?.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    let low = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let high = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    egui::ScrollArea::both().max_height(420.0).show(ui, |ui| {
        egui::Grid::new("debug_scientific_table")
            .striped(true)
            .show(ui, |ui| {
                if let Some(columns) = table["columns"].as_array() {
                    for column in columns {
                        ui.strong(column.as_str().unwrap_or(""));
                    }
                    ui.end_row();
                }
                for row in rows {
                    if let Some(cells) = row.as_array() {
                        if !session.backend.filter.is_empty()
                            && !cells.iter().any(|cell| {
                                cell.as_str()
                                    .unwrap_or("")
                                    .contains(&session.backend.filter)
                            })
                        {
                            continue;
                        }
                        for cell in cells {
                            let text = cell.as_str().unwrap_or("");
                            let number = text.parse::<f64>().ok().filter(|value| value.is_finite());
                            let display = number
                                .map(|value| {
                                    format!("{:.*}", session.backend.number_precision, value)
                                })
                                .unwrap_or_else(|| text.into());
                            let mut label = egui::RichText::new(display).monospace();
                            if session.backend.heatmap {
                                if let Some(number) = number {
                                    let intensity = if high > low {
                                        ((number - low) / (high - low) * 180.0) as u8
                                    } else {
                                        90
                                    };
                                    label = label.background_color(egui::Color32::from_rgb(
                                        intensity,
                                        35,
                                        180 - intensity,
                                    ));
                                }
                            }
                            ui.label(label);
                        }
                        ui.end_row();
                    }
                }
            });
    });
}

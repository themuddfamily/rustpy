//! Endpoints tool window: Flask / FastAPI / Django / Pyramid routes of the project.

use std::path::PathBuf;

use egui::Color32;
use rustpy_semantic::endpoints::{Endpoint, ParamLocation};

use crate::theme::UiTheme;

#[derive(Default)]
pub struct EndpointsPanel {
    accessibility_blocked: bool,
    pub endpoints: Vec<Endpoint>,
    pub scanned: bool,
    filter: String,
    selected: Option<usize>,
    pub files_scanned: usize,
    spec_path: String,
    imported: Vec<rustpy_http::HttpRequest>,
    import_error: String,
}

pub enum EndpointsEvent {
    ImportedRequests(String),
    Refresh,
    Navigate(PathBuf, usize),
    GenerateRequest(Endpoint),
    OpenInBrowser(Endpoint),
    CopyUrl(Endpoint),
}

/// Default development server URL per framework.
pub fn base_url(framework: &str) -> &'static str {
    match framework {
        "Flask" => "http://127.0.0.1:5000",
        _ => "http://127.0.0.1:8000",
    }
}

/// Compact parameter summary for the Parameters column: `{id}: int`, `?q`, `body: Item`.
pub fn params_summary(e: &Endpoint) -> String {
    e.params
        .iter()
        .map(|p| {
            let name = match p.location {
                ParamLocation::Path => format!("{{{}}}", p.name),
                ParamLocation::Query => format!("?{}", p.name),
                ParamLocation::Body => format!("body {}", p.name),
            };
            if p.annotation.is_empty() {
                name
            } else {
                format!("{name}: {}", p.annotation)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Hover details: one parameter per line, then the JSON body skeleton.
fn params_details(e: &Endpoint) -> String {
    let mut lines: Vec<String> = e
        .params
        .iter()
        .map(|p| {
            format!(
                "{} ({}){}",
                p.name,
                p.location.as_str(),
                if p.annotation.is_empty() {
                    String::new()
                } else {
                    format!(": {}", p.annotation)
                }
            )
        })
        .collect();
    if let Some(body) = &e.body {
        lines.push(format!("Request body:\n{body}"));
    }
    lines.join("\n")
}

fn method_color(m: &str, theme: &UiTheme) -> Color32 {
    if theme.high_contrast {
        return theme.text;
    }
    match m {
        "GET" => Color32::from_rgb(0x4C, 0xAF, 0x50),
        "POST" => Color32::from_rgb(0xE0, 0x9B, 0x3D),
        "PUT" | "PATCH" => Color32::from_rgb(0x4A, 0x90, 0xD9),
        "DELETE" => Color32::from_rgb(0xD9, 0x53, 0x4F),
        _ => theme.text_dim,
    }
}

impl EndpointsPanel {
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        root: &std::path::Path,
    ) -> Vec<EndpointsEvent> {
        let mut events = Vec::new();
        ui.horizontal(|ui| {
            ui.label("OpenAPI spec import (JSON / YAML)");
            ui.text_edit_singleline(&mut self.spec_path);
            if ui.button("Import spec").clicked() {
                let result = rustpy_core::vfs::Vfs::new()
                    .read(
                        std::path::Path::new(&self.spec_path),
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 2 * 1024 * 1024,
                            max_cached_bytes: 0,
                            max_cached_files: 0,
                        },
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|snapshot| {
                        std::str::from_utf8(&snapshot.bytes)
                            .map_err(|error| error.to_string())
                            .and_then(rustpy_http::workspace::openapi)
                    });
                match result {
                    Ok(requests) => {
                        self.imported = requests;
                        self.import_error.clear();
                    }
                    Err(error) => self.import_error = error,
                }
            }
        });
        if !self.import_error.is_empty() {
            ui.colored_label(theme.error, &self.import_error);
        }
        for request in &self.imported {
            ui.horizontal(|ui| {
                ui.label(format!("{} {}", request.method, request.url));
                if ui
                    .button("Generate request / Open in HTTP Client")
                    .clicked()
                {
                    events.push(EndpointsEvent::ImportedRequests(
                        rustpy_http::workspace::to_http(request),
                    ));
                }
            });
        }

        if !self.scanned {
            events.push(EndpointsEvent::Refresh);
        }
        ui.horizontal(|ui| {
            if ui.button("⟳").on_hover_text("Refresh").clicked() {
                events.push(EndpointsEvent::Refresh);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter by path, method, parameter or handler")
                    .desired_width(260.0),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} endpoint(s) in {} file(s)",
                    self.endpoints.len(),
                    self.files_scanned
                ))
                .color(theme.text_dim),
            );
        });
        ui.separator();
        if self.scanned && self.endpoints.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No Flask, FastAPI, Django or Pyramid routes found in this project.",
                )
                .color(theme.text_dim),
            );
            return events;
        }
        let f = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("endpoints_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("endpoints_grid")
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for h in [
                            "Method",
                            "Path",
                            "Parameters",
                            "Handler",
                            "Framework",
                            "Location",
                        ] {
                            ui.label(egui::RichText::new(h).strong());
                        }
                        ui.end_row();
                        for (i, e) in self.endpoints.iter().enumerate() {
                            let methods = e.methods.join(" ");
                            let params = params_summary(e);
                            if !f.is_empty()
                                && !(e.path.to_lowercase().contains(&f)
                                    || methods.to_lowercase().contains(&f)
                                    || params.to_lowercase().contains(&f)
                                    || e.handler.to_lowercase().contains(&f))
                            {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                for m in &e.methods {
                                    ui.label(
                                        egui::RichText::new(m)
                                            .monospace()
                                            .color(method_color(m, theme)),
                                    );
                                }
                            });
                            let resp = ui
                                .push_id(
                                    (
                                        "endpoint",
                                        &e.file,
                                        e.line,
                                        e.framework,
                                        &e.handler,
                                        &e.methods,
                                        &e.path,
                                    ),
                                    |ui| {
                                        ui.selectable_label(
                                            self.selected == Some(i),
                                            egui::RichText::new(&e.path).monospace(),
                                        )
                                    },
                                )
                                .inner;
                            let params_label = ui.label(
                                egui::RichText::new(&params)
                                    .monospace()
                                    .color(theme.text_dim),
                            );
                            if !e.params.is_empty() || e.body.is_some() {
                                params_label.on_hover_text(params_details(e));
                            }
                            ui.label(&e.handler);
                            ui.label(egui::RichText::new(e.framework).color(theme.text_dim));
                            let rel = e
                                .file
                                .strip_prefix(root)
                                .unwrap_or(&e.file)
                                .display()
                                .to_string();
                            ui.label(
                                egui::RichText::new(format!("{rel}:{}", e.line + 1))
                                    .color(theme.text_dim),
                            );
                            ui.end_row();
                            if !self.accessibility_blocked && resp.clicked() {
                                self.selected = Some(i);
                            }
                            if !self.accessibility_blocked && resp.double_clicked() {
                                events.push(EndpointsEvent::Navigate(e.file.clone(), e.line));
                            }
                            crate::a11y::keyboard_context_menu_fallback(
                                &resp,
                                !self.accessibility_blocked,
                            )
                            .show(|ui| {
                                if ui.button("Jump to Source").clicked() {
                                    events.push(EndpointsEvent::Navigate(e.file.clone(), e.line));
                                    ui.close();
                                }
                                if ui.button("Generate HTTP Request").clicked() {
                                    events.push(EndpointsEvent::GenerateRequest(e.clone()));
                                    ui.close();
                                }
                                if ui.button("Open in Browser").clicked() {
                                    events.push(EndpointsEvent::OpenInBrowser(e.clone()));
                                    ui.close();
                                }
                                if ui.button("Copy URL").clicked() {
                                    events.push(EndpointsEvent::CopyUrl(e.clone()));
                                    ui.close();
                                }
                            });
                        }
                    });
            });
        events
    }
}

#[cfg(test)]
mod params_column_tests {
    use super::*;

    #[test]
    fn parameter_summary_and_details_cover_every_location() {
        let e = rustpy_semantic::endpoints::scan_project(&[(
            PathBuf::from("main.py"),
            "from fastapi import FastAPI\nfrom pydantic import BaseModel\nclass Item(BaseModel):\n    name: str\napp = FastAPI()\n@app.post('/items/{item_id}')\ndef create(item_id: int, item: Item, q: str = ''):\n    pass\n"
                .to_string(),
        )])
        .remove(0);
        assert_eq!(
            params_summary(&e),
            "{item_id}: int, body item: Item, ?q: str"
        );
        let details = params_details(&e);
        assert!(details.starts_with("item_id (path): int\nitem (body): Item\nq (query): str\n"));
        assert!(
            details.ends_with("Request body:\n{\n  \"name\": \"string\"\n}"),
            "{details}"
        );
    }
}

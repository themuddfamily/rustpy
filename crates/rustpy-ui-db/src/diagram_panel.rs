//! UML class diagram (Diagrams → Show Diagram): classes of a file with
//! members and inheritance edges, laid out in layers by depth.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_core::TextRange;

use crate::theme::UiTheme;
pub(crate) mod models;
mod workspace;

#[derive(Clone, Debug)]
pub struct DiagramClass {
    /// Stable physical class identity, independent of its display name.
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub range: TextRange,
    pub bases: Vec<String>,
    pub fields: Vec<String>,
    pub methods: Vec<String>,
}

pub enum DiagramEvent {
    Navigate(PathBuf, TextRange),
    DatabaseObject(String, String),
}

#[derive(Default)]
pub struct DiagramPanel {
    workspace: workspace::Workspace,
    pub title: String,
    pub classes: Vec<DiagramClass>,
    pub show_members: bool,
    pub show_fields: bool,
    pub show_methods: bool,
    pub show_dependencies: bool,
    pub show_implementations: bool,
    pub dependencies: Vec<(String, String)>,
    pub implementations: Vec<(String, String)>,
    export_message: Option<String>,
    pub zoom: f32,
    pub pan: Vec2,
    pub positions: HashMap<String, Pos2>,
    pub dragging: Option<String>,
    selected: usize,
    focus_requested: bool,
}

impl DiagramPanel {
    pub fn set(&mut self, title: String, classes: Vec<DiagramClass>) {
        if let Some(job) = &self.workspace.job {
            job.cancelled
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.workspace.issues.clear();
        self.workspace.relations.clear();
        self.workspace.er_source = None;
        self.workspace.er_nodes.clear();
        self.workspace.fingerprints.clear();
        self.selected = 0;
        self.focus_requested = true;
        self.title = title;
        self.classes = classes;
        self.positions.clear();
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.show_members = true;
        self.show_fields = self.workspace.preferences.fields;
        self.show_methods = self.workspace.preferences.methods;
        self.show_dependencies = self.workspace.preferences.dependencies;
        self.show_implementations = self.workspace.preferences.implementations;
        self.dependencies.clear();
        self.implementations.clear();
        self.export_message = None;
        self.layout();
    }

    /// Keyboard activation can enter the panel without a pointer click.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// Layered layout: depth = longest path to a root (class without known bases).
    fn layout(&mut self) {
        let names: Vec<String> = self.classes.iter().map(|c| c.id.clone()).collect();
        let mut depth: HashMap<String, usize> = HashMap::new();
        fn depth_of(
            name: &str,
            classes: &[DiagramClass],
            memo: &mut HashMap<String, usize>,
            guard: usize,
        ) -> usize {
            if let Some(d) = memo.get(name) {
                return *d;
            }
            if guard > 50 {
                return 0;
            }
            let d = classes
                .iter()
                .find(|c| c.id == name)
                .map(|c| {
                    c.bases
                        .iter()
                        .filter(|b| classes.iter().any(|x| &x.id == *b))
                        .map(|b| depth_of(b, classes, memo, guard + 1) + 1)
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            memo.insert(name.to_string(), d);
            d
        }
        for n in &names {
            depth_of(n, &self.classes, &mut depth, 0);
        }
        let mut layers: Vec<Vec<String>> = Vec::new();
        for n in &names {
            let d = depth[n];
            while layers.len() <= d {
                layers.push(Vec::new());
            }
            layers[d].push(n.clone());
        }
        for (li, layer) in layers.iter().enumerate() {
            for (ci, n) in layer.iter().enumerate() {
                let x = 30.0 + ci as f32 * 230.0;
                let y = 20.0 + li as f32 * 380.0;
                self.positions.insert(n.clone(), Pos2::new(x, y));
            }
        }
    }

    /// Standalone vector image of the current layout, with the same relationship/member filters.
    pub fn svg_image(&self) -> String {
        fn escape(text: &str) -> String {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        }
        let max_x = self
            .positions
            .values()
            .map(|pos| pos.x + 240.0)
            .fold(300.0_f32, f32::max);
        let max_y = self
            .positions
            .values()
            .map(|pos| pos.y + 380.0)
            .fold(200.0_f32, f32::max);
        let min_x = self
            .positions
            .values()
            .map(|pos| pos.x - 10.0)
            .fold(0.0_f32, f32::min);
        let min_y = self
            .positions
            .values()
            .map(|pos| pos.y - 10.0)
            .fold(0.0_f32, f32::min);
        let mut image = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{min_x} {min_y} {} {}\"><title>{}</title><defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"10\" refY=\"5\" markerWidth=\"8\" markerHeight=\"8\" orient=\"auto-start-reverse\"><path d=\"M0 0 L10 5 L0 10 z\" fill=\"white\" stroke=\"#333\"/></marker></defs>", max_x-min_x, max_y-min_y, escape(&self.title));
        for class in &self.classes {
            let from = self.positions.get(&class.id).copied().unwrap_or_default();
            for base in &class.bases {
                let implementation = self
                    .implementations
                    .contains(&(class.id.clone(), base.clone()));
                if implementation && !self.show_implementations {
                    continue;
                }
                if let Some(to) = self.positions.get(base) {
                    image.push_str(&format!("<path d=\"M{} {} L{} {}\" fill=\"none\" stroke=\"#333\" {} marker-end=\"url(#arrow)\"/>", from.x+105.0, from.y, to.x+105.0, to.y+26.0, if implementation { "stroke-dasharray=\"5 3\"" } else { "" }));
                }
            }
        }
        if self.show_dependencies {
            for (source, target) in &self.dependencies {
                if let (Some(from), Some(to)) =
                    (self.positions.get(source), self.positions.get(target))
                {
                    image.push_str(&format!("<path d=\"M{} {} L{} {}\" stroke=\"#4682b4\" stroke-dasharray=\"3 3\" marker-end=\"url(#arrow)\"/>",from.x+210.0,from.y+13.0,to.x,to.y+13.0));
                }
            }
        }
        for relation in self
            .workspace
            .relations
            .iter()
            .filter(|_| self.workspace.preferences.relations)
        {
            if let (Some(from), Some(to)) = (
                self.positions.get(&relation.source),
                self.positions.get(&relation.target),
            ) {
                image.push_str(&format!("<path d=\"M{} {} L{} {}\" stroke=\"#267080\" fill=\"none\" marker-end=\"url(#arrow)\"/>",from.x+210.0,from.y+20.0,to.x,to.y+20.0));
                if self.workspace.preferences.labels {
                    image.push_str(&format!(
                        "<text x=\"{}\" y=\"{}\" font-size=\"11\">{} [{}]</text>",
                        (from.x + to.x + 210.0) / 2.0,
                        (from.y + to.y) / 2.0,
                        escape(&relation.label),
                        escape(&relation.cardinality)
                    ));
                }
            }
        }
        for class in &self.classes {
            let pos = self.positions.get(&class.id).copied().unwrap_or_default();
            let fields = if self.show_members && self.show_fields {
                class.fields.iter().take(8).collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let methods = if self.show_members && self.show_methods {
                class.methods.iter().take(10).collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            image.push_str(&format!("<rect x=\"{}\" y=\"{}\" width=\"210\" height=\"{}\" fill=\"white\" stroke=\"#333\"/><g font-family=\"sans-serif\" font-size=\"12\" fill=\"#111\"><text x=\"{}\" y=\"{}\" font-weight=\"bold\">{}</text>",pos.x,pos.y,32+16*(fields.len()+methods.len()),pos.x+8.0,pos.y+18.0,escape(&class.name)));
            for (index, member) in fields
                .into_iter()
                .map(|name| format!("- {name}"))
                .chain(methods.into_iter().map(|name| format!("+ {name}()")))
                .enumerate()
            {
                image.push_str(&format!(
                    "<text x=\"{}\" y=\"{}\">{}</text>",
                    pos.x + 8.0,
                    pos.y + 38.0 + index as f32 * 16.0,
                    escape(&member)
                ));
            }
            image.push_str("</g>");
        }
        image.push_str("</svg>");
        image
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) -> Vec<DiagramEvent> {
        let mut events = Vec::new();
        self.workspace_toolbar(ui);
        if self.classes.is_empty() {
            ui.label(egui::RichText::new("Diagram: open a Python file and use Navigate → Show Diagram (Ctrl+Alt+Shift+U).").color(theme.text_dim));
            return events;
        }
        self.selected = self.selected.min(self.classes.len() - 1);
        let mut keyboard_navigation = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&self.title).strong());
            ui.checkbox(&mut self.show_fields, "Fields");
            ui.checkbox(&mut self.show_methods, "Methods");
            ui.checkbox(&mut self.show_dependencies, "Dependencies");
            ui.checkbox(&mut self.show_implementations, "Implementations");
            ui.add(
                egui::TextEdit::singleline(&mut self.workspace.preferences.export_path)
                    .hint_text("Export new SVG or .dot file path"),
            );
            if ui.button("Export image / Graphviz").clicked() {
                use std::io::Write;
                let path = self.workspace.preferences.export_path.clone();
                let content = if path.ends_with(".dot") {
                    self.graphviz()
                } else {
                    self.svg_image()
                };
                self.export_message = Some(
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .and_then(|mut file| file.write_all(content.as_bytes()))
                    {
                        Ok(()) => format!("Exported {path}"),
                        Err(error) => format!("Export failed: {error}"),
                    },
                );
            }
            if ui.button("Fit").clicked() {
                let min = self.positions.values().fold(Pos2::new(0.0, 0.0), |p, n| {
                    Pos2::new(p.x.min(n.x), p.y.min(n.y))
                });
                let max = self
                    .positions
                    .values()
                    .fold(Pos2::new(240.0, 380.0), |p, n| {
                        Pos2::new(p.x.max(n.x + 240.0), p.y.max(n.y + 380.0))
                    });
                let size = ui.available_size();
                self.zoom = ((size.x - 20.0) / (max.x - min.x).max(1.0))
                    .min((size.y - 80.0) / (max.y - min.y).max(1.0))
                    .clamp(0.01, 3.5);
                self.pan = Vec2::new(10.0 - min.x * self.zoom, 10.0 - min.y * self.zoom);
            }
            ui.label(
                egui::RichText::new("Drag boxes to rearrange · double-click to open")
                    .color(theme.text_dim)
                    .small(),
            );
        });
        if let Some(message) = &self.export_message {
            ui.label(message);
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Previous class").clicked() {
                self.selected = (self.selected + self.classes.len() - 1) % self.classes.len();
            }
            if ui.button("Next class").clicked() {
                self.selected = (self.selected + 1) % self.classes.len();
            }
            ui.label(format!(
                "Class {} of {}: {}",
                self.selected + 1,
                self.classes.len(),
                self.classes[self.selected].name
            ));
            let open = ui.button("Open selected class");
            if std::mem::take(&mut self.focus_requested) {
                open.request_focus();
            }
            keyboard_navigation = open.clicked();
            ui.add(egui::Slider::new(&mut self.zoom, 0.01..=3.5).text("Diagram zoom"));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Diagram pan");
            ui.add(
                egui::DragValue::new(&mut self.pan.x)
                    .range(-100_000.0..=100_000.0)
                    .prefix("Horizontal: "),
            );
            ui.add(
                egui::DragValue::new(&mut self.pan.y)
                    .range(-100_000.0..=100_000.0)
                    .prefix("Vertical: "),
            );
            let name = &self.classes[self.selected].id;
            if let Some(pos) = self.positions.get_mut(name) {
                ui.label("Selected class position");
                ui.add(
                    egui::DragValue::new(&mut pos.x)
                        .range(-100_000.0..=100_000.0)
                        .prefix("X: "),
                );
                ui.add(
                    egui::DragValue::new(&mut pos.y)
                        .range(-100_000.0..=100_000.0)
                        .prefix("Y: "),
                );
            }
        });
        if keyboard_navigation {
            events.push(self.navigation_event(self.selected));
        }
        ui.separator();
        let avail = ui.available_rect_before_wrap();
        let (rect, resp) = ui.allocate_exact_size(avail.size(), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, theme.panel_bg_darker);
        if resp.dragged() && self.dragging.is_none() {
            self.pan += resp.drag_delta();
        }
        let zoom = self.zoom;
        let origin = rect.min + self.pan;
        let box_w = 210.0 * zoom;
        let row_h = 16.0 * zoom;
        let painter = ui.painter().with_clip_rect(rect);
        // Box rects.
        let mut rects: HashMap<String, Rect> = HashMap::new();
        for c in &self.classes {
            let pos = self
                .positions
                .get(&c.id)
                .copied()
                .unwrap_or(Pos2::new(40.0, 40.0));
            let rows = if self.show_members {
                1 + if self.show_fields {
                    c.fields.len().min(8)
                } else {
                    0
                } + if self.show_methods {
                    c.methods.len().min(10)
                } else {
                    0
                } + 2
            } else {
                1
            };
            let h = row_h * rows as f32 + 8.0 * zoom;
            rects.insert(
                c.id.clone(),
                Rect::from_min_size(origin + pos.to_vec2() * zoom, Vec2::new(box_w, h)),
            );
        }
        // Edges (inheritance arrows from subclass top to base bottom).
        for c in &self.classes {
            for b in &c.bases {
                if self.implementations.contains(&(c.id.clone(), b.clone()))
                    && !self.show_implementations
                {
                    continue;
                }
                let (Some(r), Some(br)) = (rects.get(&c.id), rects.get(b)) else {
                    continue;
                };
                let from = Pos2::new(r.center().x, r.top());
                let to = Pos2::new(br.center().x, br.bottom());
                if self.implementations.contains(&(c.id.clone(), b.clone())) {
                    let length = from.distance(to);
                    let direction = (to - from).normalized();
                    for segment in 0..(length / 8.0).ceil() as usize {
                        let start = segment as f32 * 8.0;
                        painter.line_segment(
                            [
                                from + direction * start,
                                from + direction * (start + 4.0).min(length),
                            ],
                            Stroke::new(1.5, theme.text_dim),
                        );
                    }
                } else {
                    painter.line_segment([from, to], Stroke::new(1.5, theme.text_dim));
                }
                let dir = (to - from).normalized();
                let tip = to;
                let left = tip - dir * 10.0 + dir.rot90() * 5.0;
                let right = tip - dir * 10.0 - dir.rot90() * 5.0;
                painter.add(egui::Shape::convex_polygon(
                    vec![tip, left, right],
                    theme.panel_bg_darker,
                    Stroke::new(1.5, theme.text_dim),
                ));
            }
        }
        if self.show_dependencies {
            for (source, target) in &self.dependencies {
                let (Some(from), Some(to)) = (rects.get(source), rects.get(target)) else {
                    continue;
                };
                painter.arrow(
                    from.right_center(),
                    to.left_center() - from.right_center(),
                    Stroke::new(1.0, theme.accent),
                );
            }
        }
        for relation in self
            .workspace
            .relations
            .iter()
            .filter(|_| self.workspace.preferences.relations)
        {
            if let (Some(from), Some(to)) =
                (rects.get(&relation.source), rects.get(&relation.target))
            {
                let color = match relation.kind {
                    models::Kind::ManyToMany => theme.accent,
                    models::Kind::OneToOne => Color32::LIGHT_GREEN,
                    _ => theme.text_dim,
                };
                let a = from.right_center();
                let b = to.left_center();
                if relation.source == relation.target {
                    let center = a + Vec2::new(35.0, -25.0);
                    painter.line_segment([a, center], Stroke::new(1.5, color));
                    painter.arrow(center, from.right_top() - center, Stroke::new(1.5, color));
                } else {
                    painter.arrow(a, b - a, Stroke::new(1.5, color));
                }
                if self.workspace.preferences.labels {
                    painter.text(
                        a.lerp(b, 0.5),
                        egui::Align2::CENTER_BOTTOM,
                        format!("{} [{}]", relation.label, relation.cardinality),
                        egui::FontId::proportional(11.0 * zoom),
                        color,
                    );
                }
            }
        }
        // Boxes.
        let mut navigate: Option<(PathBuf, TextRange)> = None;
        let mut drag_update: Option<(String, Vec2)> = None;
        for c in &self.classes {
            let r = rects[&c.id];
            let id = ui.id().with(("uml", &c.id));
            let box_resp = ui.interact(r, id, Sense::click_and_drag());
            box_resp.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Other,
                    true,
                    format!(
                        "Class {}. Bases: {}. Fields: {}. Methods: {}",
                        c.name,
                        c.bases
                            .iter()
                            .map(|base| {
                                self.classes
                                    .iter()
                                    .find(|class| &class.id == base)
                                    .map_or(base.as_str(), |class| class.name.as_str())
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                        c.fields.join(", "),
                        c.methods.join(", ")
                    ),
                )
            });
            if box_resp.drag_started() {
                self.dragging = Some(c.id.clone());
            }
            if box_resp.dragged() {
                drag_update = Some((c.id.clone(), box_resp.drag_delta() / zoom));
            }
            if box_resp.drag_stopped() {
                self.dragging = None;
            }
            if box_resp.double_clicked() {
                navigate = Some((c.path.clone(), c.range));
                self.selected = self
                    .classes
                    .iter()
                    .position(|node| node.id == c.id)
                    .unwrap_or(0);
            }
            painter.rect_filled(
                r,
                4.0,
                if !self.workspace.preferences.search.is_empty()
                    && c.name
                        .to_lowercase()
                        .contains(&self.workspace.preferences.search.to_lowercase())
                {
                    theme.accent.gamma_multiply(0.3)
                } else {
                    theme.panel_bg
                },
            );
            painter.rect_stroke(
                r,
                4.0,
                Stroke::new(
                    1.0,
                    if box_resp.hovered() {
                        theme.accent
                    } else {
                        theme.border
                    },
                ),
                egui::StrokeKind::Outside,
            );
            let header = Rect::from_min_size(r.min, Vec2::new(r.width(), row_h + 6.0 * zoom));
            painter.rect_filled(
                header,
                4.0,
                if theme.high_contrast {
                    theme.panel_bg
                } else {
                    Color32::from_rgb(0x4A, 0x88, 0xC7)
                },
            );
            painter.text(
                header.center(),
                egui::Align2::CENTER_CENTER,
                &c.name,
                egui::FontId::proportional(13.0 * zoom),
                if theme.high_contrast {
                    theme.text
                } else {
                    Color32::WHITE
                },
            );
            if self.show_members {
                let mut y = header.bottom() + 2.0 * zoom;
                for f in c.fields.iter().take(if self.show_fields { 8 } else { 0 }) {
                    painter.text(
                        Pos2::new(r.left() + 8.0 * zoom, y + row_h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        format!("- {f}"),
                        egui::FontId::proportional(11.5 * zoom),
                        theme.text,
                    );
                    y += row_h;
                }
                if self.show_fields
                    && self.show_methods
                    && !c.fields.is_empty()
                    && !c.methods.is_empty()
                {
                    painter.line_segment(
                        [Pos2::new(r.left(), y + 2.0), Pos2::new(r.right(), y + 2.0)],
                        Stroke::new(1.0, theme.border),
                    );
                    y += 4.0 * zoom;
                }
                for m in c
                    .methods
                    .iter()
                    .take(if self.show_methods { 10 } else { 0 })
                {
                    painter.text(
                        Pos2::new(r.left() + 8.0 * zoom, y + row_h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        format!("+ {m}()"),
                        egui::FontId::proportional(11.5 * zoom),
                        theme.text,
                    );
                    y += row_h;
                }
                let more = if self.show_fields {
                    c.fields.len().saturating_sub(8)
                } else {
                    0
                } + if self.show_methods {
                    c.methods.len().saturating_sub(10)
                } else {
                    0
                };
                if more > 0 {
                    painter.text(
                        Pos2::new(r.left() + 8.0 * zoom, y + row_h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        format!("… {more} more"),
                        egui::FontId::proportional(11.0 * zoom),
                        theme.text_dim,
                    );
                }
            }
        }
        if let Some((name, delta)) = drag_update {
            if let Some(p) = self.positions.get_mut(&name) {
                *p += delta;
            }
        }
        if navigate.is_some() {
            events.push(self.navigation_event(self.selected));
        }
        // Zoom with the wheel.
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if resp.hovered() && scroll != 0.0 {
            self.zoom = (self.zoom * if scroll > 0.0 { 1.1 } else { 0.9 }).clamp(0.01, 3.5);
        }
        events
    }
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    #[test]
    fn actual_keyboard_button_opens_unicode_class_and_painted_nodes_are_named() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        let range = TextRange::new(rustpy_core::TextSize::new(0), rustpy_core::TextSize::new(8));
        let mut panel = DiagramPanel::default();
        panel.set(
            "Unicode diagram".into(),
            vec![DiagramClass {
                id: "unicode-class".into(),
                name: "πClass".into(),
                path: PathBuf::from("π.py"),
                range,
                bases: vec!["Base".into()],
                fields: vec!["value".into()],
                methods: vec!["method".into()],
            }],
        );
        let mut got = Vec::new();
        let mut nodes = Vec::new();
        for (time, key) in [(0.0, false), (0.1, false), (0.2, true)] {
            let events = if key {
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]
            } else {
                vec![]
            };
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                    ..Default::default()
                },
                |ui| got.extend(panel.ui(ui, &UiTheme::high_contrast())),
            );
            nodes.extend(
                output
                    .platform_output
                    .accesskit_update
                    .clone()
                    .unwrap()
                    .nodes,
            );
            output.drop_without_applying_deltas();
        }
        assert!(
            matches!(&got[..], [DiagramEvent::Navigate(path, r)] if path == &PathBuf::from("π.py") && *r == range)
        );
        assert!(nodes
            .iter()
            .any(|(_, n)| n.label()
                == Some("Class πClass. Bases: Base. Fields: value. Methods: method")));
        assert!(nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Open selected class")));
        assert_eq!(panel.classes[0].range, range);
        assert_eq!(panel.zoom, 1.0);
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn diagram_export_preserves_physical_identity_filters_and_xml_escaping() {
        let class = |id: &str| DiagramClass {
            id: id.into(),
            name: "Same<&".into(),
            path: PathBuf::from(format!("{id}.py")),
            range: TextRange::empty(0.into()),
            bases: Vec::new(),
            fields: vec!["payload".into()],
            methods: vec!["run".into()],
        };
        let mut panel = DiagramPanel::default();
        let mut child = class("child");
        child.bases.push("base".into());
        panel.set("Diagram<&".into(), vec![class("base"), child]);
        panel.dependencies.push(("base".into(), "child".into()));
        panel.implementations.push(("child".into(), "base".into()));
        assert_eq!(panel.positions.len(), 2);
        let image = panel.svg_image();
        assert!(image.contains("Same&lt;&amp;"));
        assert!(image.contains("payload"));
        assert!(image.contains("run()"));
        assert!(image.contains("marker-end"));
        panel.show_fields = false;
        panel.show_methods = false;
        panel.show_dependencies = false;
        panel.show_implementations = false;
        let image = panel.svg_image();
        assert!(!image.contains("payload"));
        assert!(!image.contains("run()"));
        assert!(!image.contains("marker-end="));
    }
}

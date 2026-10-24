use super::*;
impl ProfilerPanel {
    pub(super) fn poll_files(&mut self) -> bool {
        let Some(rx) = &self.file_rx else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                FileResult::Failed("Profiler file operation ended without a result".into())
            }
        };
        self.file_rx = None;
        match result {
            FileResult::Imported(report, compare) => {
                if compare {
                    self.baseline = Some(report);
                    self.view = ProfileView::Comparison;
                } else {
                    self.report = Some(report);
                    self.selected = None;
                    self.graph_root = None;
                    self.show_log = false;
                }
            }
            FileResult::Saved(path) => self.log.push((format!("Saved {}", path.display()), false)),
            FileResult::Failed(error) => {
                self.log.push((error, true));
                self.show_log = true;
            }
        }
        true
    }
    pub(super) fn file_controls(&mut self, ui: &mut egui::Ui, root: &Path) {
        if self.file_path.is_empty() {
            self.file_path = root.join("profile.pstat").display().to_string();
        }
        let mut operation = None;
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.file_path)
                    .hint_text(".pstat / .prof / .json / .svg path"),
            );
            for (label, action) in [
                ("Open pstat file", 0),
                ("Save as…", 1),
                ("Compare pstat…", 2),
                ("Export graph image (SVG)", 3),
            ] {
                if ui
                    .add_enabled(
                        self.file_rx.is_none()
                            && (action == 0 || action == 2 || self.report.is_some()),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    operation = Some(action);
                }
            }
            if self.file_rx.is_some() {
                crate::a11y::spinner(ui);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
        let Some(action) = operation else { return };
        let path = PathBuf::from(self.file_path.trim());
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        let root = if root.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_default()
        } else {
            root.to_path_buf()
        };
        let python = self.python.clone().unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let report = self.report.clone();
        let graph_root = self.graph_root;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = match action {
                0 | 2 => rustpy_run::profile_tools::import_report(&path, &python, &root)
                    .map(|report| FileResult::Imported(report, action == 2)),
                1 => {
                    rustpy_run::profile_tools::save_report(&report.unwrap(), &path, &python, &root)
                        .map(|()| FileResult::Saved(path))
                }
                _ => rustpy_run::profile_tools::export_svg(&report.unwrap(), graph_root, &path)
                    .map(|()| FileResult::Saved(path)),
            };
            let _ = tx.send(result.unwrap_or_else(|error| FileResult::Failed(error.to_string())));
        });
        self.file_rx = Some(rx);
    }
    pub(super) fn views_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        report: &ProfileReport,
        project_root: &Path,
        events: &mut Vec<ProfilerEvent>,
    ) {
        if self.view == ProfileView::Comparison {
            let Some(baseline) = &self.baseline else {
                ui.label("Open a baseline with Compare pstat to show time and call-count changes.");
                return;
            };
            if report.sampling != baseline.sampling {
                ui.label("The baseline and current profile use different count units (samples versus calls). Time deltas remain available.");
            }
            egui::ScrollArea::both()
                .id_salt("profile_comparison")
                .show(ui, |ui| {
                    egui::Grid::new("profile_comparison_grid")
                        .striped(true)
                        .show(ui, |ui| {
                            for title in ["Function", "Count Δ", "Own time Δ", "Total time Δ"] {
                                ui.label(crate::i18n::tr_static(title));
                            }
                            ui.end_row();
                            for delta in rustpy_run::profile_tools::compare(report, baseline) {
                                if self.project_only
                                    && !Path::new(&delta.function.file).starts_with(project_root)
                                {
                                    continue;
                                }
                                if !self.filter.is_empty()
                                    && !delta
                                        .function
                                        .display_name()
                                        .to_lowercase()
                                        .contains(&self.filter.to_lowercase())
                                {
                                    continue;
                                }
                                let response =
                                    ui.selectable_label(false, delta.function.display_name());
                                if response.double_clicked() && !delta.function.is_builtin() {
                                    events.push(ProfilerEvent::Navigate(
                                        PathBuf::from(delta.function.file),
                                        delta.function.line,
                                    ));
                                }
                                ui.label(if report.sampling != baseline.sampling {
                                    "—".into()
                                } else {
                                    format!("{:+}", delta.calls_delta)
                                });
                                ui.colored_label(
                                    if delta.own_delta > 0.0 {
                                        theme.error
                                    } else {
                                        theme.success
                                    },
                                    format!("{:+.6}s", delta.own_delta),
                                );
                                ui.label(format!("{:+.6}s", delta.total_delta));
                                ui.end_row();
                            }
                        });
                });
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("All roots").clicked() {
                self.graph_root = None;
            }
            if ui.button("Focus selected function").clicked() {
                self.graph_root = self.selected;
            }
            ui.add(egui::Slider::new(&mut self.graph_zoom, 0.5..=6.0).text("Zoom"));
        });
        let root = self.graph_root.or_else(|| {
            if self.project_only {
                report.sorted_by_cumtime().into_iter().find(|index| {
                    Path::new(&report.functions[*index].file).starts_with(project_root)
                })
            } else {
                None
            }
        });
        let nodes = rustpy_run::profile_tools::call_tree(report, root, 20, 1500);
        if self.view == ProfileView::CallTree {
            egui::ScrollArea::both()
                .id_salt("profile_tree")
                .show(ui, |ui| {
                    for node in &nodes {
                        let function = &report.functions[node.function];
                        if self.project_only && !Path::new(&function.file).starts_with(project_root)
                        {
                            continue;
                        }
                        if !self.filter.is_empty()
                            && !function
                                .display_name()
                                .to_lowercase()
                                .contains(&self.filter.to_lowercase())
                        {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(node.depth as f32 * 18.0);
                            let response = ui.selectable_label(
                                self.selected == Some(node.function),
                                format!(
                                    "{}{} · {} calls · {:.6}s own / {:.6}s total",
                                    function.display_name(),
                                    if node.recursive { " [recursive]" } else { "" },
                                    function.ncalls,
                                    function.tottime,
                                    function.cumtime
                                ),
                            );
                            if response.clicked() {
                                self.selected = Some(node.function);
                            }
                            if response.double_clicked() && !function.is_builtin() {
                                events.push(ProfilerEvent::Navigate(
                                    PathBuf::from(&function.file),
                                    function.line,
                                ));
                            }
                        });
                    }
                });
            return;
        }
        let width = (ui.available_width().max(600.0) * self.graph_zoom).max(300.0);
        let levels = nodes.iter().map(|node| node.depth).max().unwrap_or(0) + 1;
        let height = levels as f32 * 48.0;
        egui::ScrollArea::both()
            .id_salt("profile_graph")
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                let mut rectangles = Vec::new();
                for (index, node) in nodes.iter().enumerate() {
                    let function = &report.functions[node.function];
                    let bounds = egui::Rect::from_min_size(
                        rect.min + egui::vec2(node.x * width, node.depth as f32 * 48.0),
                        egui::vec2((node.width * width - 2.0).max(1.0), 38.0),
                    );
                    let ratio = if report.total_time > 0.0 {
                        (function.tottime / report.total_time).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let color = egui::Color32::from_rgb(230, (180.0 * (1.0 - ratio)) as u8, 60);
                    if self.view == ProfileView::CallGraph {
                        if let Some(parent) = node
                            .parent
                            .and_then(|parent| rectangles.get(parent))
                            .copied()
                        {
                            let parent: egui::Rect = parent;
                            ui.painter().line_segment(
                                [parent.center_bottom(), bounds.center_top()],
                                egui::Stroke::new(1.0, theme.text_dim),
                            );
                        }
                    }
                    ui.painter().rect_filled(bounds, 3.0, color);
                    let response = ui.interact(
                        bounds,
                        ui.id().with(("profile_graph_node", index)),
                        egui::Sense::click(),
                    );
                    ui.painter().with_clip_rect(bounds).text(
                        bounds.left_center() + egui::vec2(4.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &function.name,
                        egui::FontId::proportional(12.0),
                        egui::Color32::BLACK,
                    );
                    if response.clicked() {
                        self.selected = Some(node.function);
                    }
                    if response.double_clicked() {
                        self.graph_root = Some(node.function);
                    }
                    response.on_hover_text(format!(
                        "{}\n{} calls; {:.6}s own; {:.6}s total{}",
                        function.display_name(),
                        function.ncalls,
                        function.tottime,
                        function.cumtime,
                        if node.recursive {
                            "\nRecursive edge (expansion stopped)"
                        } else {
                            ""
                        }
                    ));
                    rectangles.push(bounds);
                }
            });
        ui.label("Widths show aggregate caller time; repeated functions and recursive edges are bounded. Double-click a node to focus it.");
    }
}

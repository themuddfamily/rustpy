//! Main-window chrome. Actions use the ordinary dispatcher and keymap authority.
use super::*;
use rustpy_settings::chrome::Element;

fn registered(id: &str) -> Option<Action> {
    Action::ALL.iter().copied().find(|action| action.id() == id)
}
fn icon(action: Action) -> &'static str {
    match action {
        Action::SearchEverywhere => "⌕",
        Action::Settings => "⚙",
        Action::Run => "▶",
        Action::Debug => "🐞",
        Action::Stop => "■",
        Action::Save => "▣",
        _ => "•",
    }
}

impl IdeApp {
    pub(super) fn chrome_frame_start(&mut self, ctx: &egui::Context) {
        let compact = self.settings.appearance.compact_mode;
        let font_size = self.settings.appearance.ui_font_size.clamp(10.0, 24.0);
        ctx.global_style_mut(|style| {
            // Derive from the standard metrics each pass so repeated toggles,
            // different project viewports and font changes cannot compound.
            let base = egui::style::Spacing::default();
            let factor = if compact { 0.75 } else { 1.0 };
            style.spacing.item_spacing = base.item_spacing * factor;
            style.spacing.button_padding = base.button_padding * factor;
            style.spacing.indent = base.indent * factor;
            style.spacing.interact_size = base.interact_size;
            style.spacing.interact_size.y = (base.interact_size.y * factor)
                .max(font_size + 2.0 * style.spacing.button_padding.y);
        });
        let scale = self.settings.chrome.effective_scale();
        if (ctx.zoom_factor() - scale).abs() > 0.001 {
            ctx.set_zoom_factor(scale);
        }
        let native = self.settings.chrome.native_header;
        let key = egui::Id::new("rustpy-native-header");
        if ctx.data(|data| data.get_temp::<bool>(key)) != Some(native) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(native));
            ctx.data_mut(|data| data.insert_temp(key, native));
        }
    }
    pub(super) fn configure_chrome(&mut self) {
        self.settings_ui.page = settings_ui::SettingsPage::WindowChrome;
        self.dialog = Dialog::Settings;
    }
    pub(super) fn custom_window_header(&mut self, ui: &mut egui::Ui) {
        if self.settings.chrome.native_header || !self.shows_chrome() {
            return;
        }
        egui::Panel::top("custom-window-header")
            .resizable(false)
            .show(ui, |ui| {
                let merge = !self.settings.chrome.separate_main_menu && self.shows_menu_bar();
                ui.horizontal(|ui| {
                    let controls = 112.0;
                    let width = (ui.available_width() - controls).max(80.0);
                    ui.allocate_ui(egui::vec2(width, 26.0), |ui| {
                        if merge {
                            self.menu_bar(ui);
                        } else {
                            let response = ui.add(
                                egui::Label::new(&self.window_title).sense(Sense::click_and_drag()),
                            );
                            if response.drag_started() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                            }
                            if response.double_clicked() {
                                let maximized =
                                    ui.input(|input| input.viewport().maximized.unwrap_or(false));
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(
                                    !maximized,
                                ));
                            }
                        }
                    });
                    if ui.button("−").icon_hint("Minimize window").clicked() {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    if ui
                        .button("□")
                        .icon_hint("Maximize or restore window")
                        .clicked()
                    {
                        let maximized =
                            ui.input(|input| input.viewport().maximized.unwrap_or(false));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                    if ui.button("×").icon_hint("Close project window").clicked() {
                        // Native close always goes through the shared document/process guard.
                        self.request_exit();
                    }
                });
            });
    }

    pub(super) fn chrome_toolbar(&mut self, ui: &mut egui::Ui) {
        let elements = self.settings.chrome.toolbar.clone();
        let mut pending = Vec::new();
        let response = ui
            .horizontal_wrapped(|ui| {
                for (index, element) in elements.iter().take(64).enumerate() {
                    ui.push_id(("custom-toolbar", index), |ui| match element {
                        Element::Project => {
                            let title = self
                                .project
                                .as_ref()
                                .and_then(|project| project.root.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "Open project".into());
                            ui.menu_button(format!("{title} ▾"), |ui| {
                                for action in
                                    [Action::NewProject, Action::OpenProject, Action::GetFromVcs]
                                {
                                    if ui.button(action.text()).clicked() {
                                        pending.push(action);
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                for path in
                                    self.settings.recent_projects.clone().into_iter().take(20)
                                {
                                    if ui.button(path.display().to_string()).clicked() {
                                        self.request_open_project(&path);
                                        ui.close();
                                    }
                                }
                            });
                        }
                        Element::Vcs => {
                            let branch = self
                                .project
                                .as_ref()
                                .and_then(|project| project.git_branch.as_deref())
                                .unwrap_or("Version control");
                            if ui
                                .button(format!("{branch} ▾"))
                                .icon_hint("Git branches and repository operations")
                                .clicked()
                            {
                                self.dialog = Dialog::Branches;
                            }
                        }
                        Element::Run => {
                            self.run_toolbar_widget(ui);
                        }
                        Element::Action { id } => {
                            if let Some(action) = registered(id) {
                                let label = if self.settings.chrome.new_ui {
                                    icon(action)
                                } else {
                                    action.text()
                                };
                                if ui
                                    .button(label)
                                    .icon_hint(&{
                                        // Unbound or gesture-only (Shift Shift) actions have
                                        // no shortcut text: no empty "()" in name or tooltip.
                                        let keys = self.keymap.shortcut_text(action);
                                        if keys.is_empty() {
                                            action.text().to_string()
                                        } else {
                                            format!("{} ({keys})", action.text())
                                        }
                                    })
                                    .clicked()
                                {
                                    pending.push(action);
                                }
                            }
                        }
                        Element::Separator => {
                            ui.separator();
                        }
                        Element::Group { label, actions } => {
                            ui.menu_button(label, |ui| {
                                for id in actions.iter().take(128) {
                                    if let Some(action) = registered(id) {
                                        if ui
                                            .add(
                                                egui::Button::new(action.text()).shortcut_text(
                                                    self.keymap.shortcut_text(action),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            pending.push(action);
                                            ui.close();
                                        }
                                    }
                                }
                            });
                        }
                    });
                }
                self.navigation_toolbar(ui);
            })
            .response;
        response.context_menu(|ui| {
            if ui.button("Customize Toolbar…").clicked() {
                self.configure_chrome();
                ui.close();
            }
        });
        let ctx = ui.ctx().clone();
        for action in pending {
            self.handle_action(action, &ctx);
        }
    }

    pub(super) fn window_chrome_settings(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.chrome;
        ui.heading("Window and interface");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut settings.new_ui, true, "New UI");
            ui.selectable_value(&mut settings.new_ui, false, "Classic UI");
        });
        ui.checkbox(&mut settings.native_header, "Use native window header");
        ui.checkbox(&mut settings.show_main_menu, "Show main menu");
        ui.add_enabled_ui(!settings.native_header, |ui| {
            ui.checkbox(
                &mut settings.separate_main_menu,
                "Show main menu separately from custom header",
            );
        });
        ui.horizontal(|ui| {
            ui.label("IDE scale");
            ui.add(egui::Slider::new(&mut settings.scale, 0.5..=3.0).step_by(0.1));
            if ui.button("100%").clicked() {
                settings.scale = 1.0;
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut settings.title_project, "Project in title");
            ui.checkbox(&mut settings.title_file, "File in title");
            ui.checkbox(&mut settings.title_full_path, "Full file path");
            ui.checkbox(&mut settings.title_branch, "Branch in title");
        });
        ui.checkbox(
            &mut settings.tool_window_numbers,
            "Show tool-window shortcut numbers",
        );
        ui.horizontal(|ui| {
            use rustpy_settings::chrome::NavigationLocation;
            ui.label("Navigation bar:");
            let visible = self.settings.appearance.show_navigation_bar;
            for (label, location) in [
                ("In editor", NavigationLocation::Editor),
                ("In main toolbar", NavigationLocation::Toolbar),
            ] {
                if ui
                    .selectable_label(
                        visible && settings.navigation_location == location,
                        crate::i18n::tr_static(label),
                    )
                    .clicked()
                {
                    settings.navigation_location = location;
                    self.settings.appearance.show_navigation_bar = true;
                }
            }
            if ui.selectable_label(!visible, "Hidden").clicked() {
                self.settings.appearance.show_navigation_bar = false;
            }
        });
        ui.checkbox(
            &mut settings.navigation_members,
            "Show members in navigation bar",
        );
        ui.collapsing("Status Bar Widgets", |ui| {
            for &(id, label) in rustpy_settings::chrome::STATUS_WIDGETS {
                let mut visible = settings.status_visible(id);
                if ui.checkbox(&mut visible, label).changed() {
                    if visible {
                        settings.hidden_status_widgets.remove(id);
                    } else {
                        settings.hidden_status_widgets.insert(id.into());
                    }
                }
            }
            ui.checkbox(
                &mut self.settings.appearance.show_memory_indicator,
                "Enable memory sampling",
            );
        });
        ui.separator();
        ui.heading("Main toolbar");
        let mut movement = None;
        let mut remove = None;
        for (index, element) in settings.toolbar.iter_mut().take(64).enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    ui.label(element.label());
                    if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                        movement = Some((index, index - 1));
                    }
                    if ui.small_button("↓").clicked() {
                        movement = Some((index, index + 1));
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
                if let Element::Group { label, actions } = element {
                    ui.text_edit_singleline(label);
                    let mut text = actions.join("\n");
                    if ui
                        .add(
                            egui::TextEdit::multiline(&mut text)
                                .desired_rows(3)
                                .hint_text("One registered action ID per line"),
                        )
                        .changed()
                    {
                        *actions = text
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .take(128)
                            .map(str::to_owned)
                            .collect();
                    }
                    for action in actions.iter().filter(|id| registered(id).is_none()) {
                        ui.colored_label(
                            self.theme.warning,
                            format!("Unavailable action: {action}"),
                        );
                    }
                }
            });
        }
        if let Some(index) = remove {
            settings.toolbar.remove(index);
        } else if let Some((from, to)) = movement {
            if to < settings.toolbar.len() {
                settings.toolbar.swap(from, to);
            }
        }
        if settings.toolbar.len() < 64 {
            ui.horizontal_wrapped(|ui| {
                for (label, element) in [
                    ("Add project", Element::Project),
                    ("Add VCS", Element::Vcs),
                    ("Add run widget", Element::Run),
                    ("Add separator", Element::Separator),
                    (
                        "Add group",
                        Element::Group {
                            label: "Custom".into(),
                            actions: Vec::new(),
                        },
                    ),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        settings.toolbar.push(element);
                    }
                }
                ui.menu_button("Add action", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for action in Action::ALL {
                                if ui.button(action.text()).clicked() {
                                    settings.toolbar.push(Element::Action {
                                        id: action.id().into(),
                                    });
                                    ui.close();
                                }
                            }
                        });
                });
            });
        }
        if ui.button("Restore default toolbar").clicked() {
            settings.toolbar = rustpy_settings::chrome::Settings::default().toolbar;
        }
        ui.label("Toolbar actions use the current keymap and ordinary project-trust checks. Configure shortcuts in Keymap.");
        ui.separator();
        self.native_ui_settings(ui);
    }
}

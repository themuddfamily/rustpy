//! Status controls share the ordinary action and per-file preference authorities.
use super::*;

struct StatusPlan {
    inline: Vec<(&'static str, f32)>,
    overflow: Vec<&'static str>,
}
fn status_width(id: &str) -> f32 {
    match id {
        "notifications" => 42.0,
        "position" => 100.0,
        "encoding" => 82.0,
        "line-endings" => 58.0,
        "indent" => 90.0,
        "git" => 160.0,
        "interpreter" => 190.0,
        "status" => 220.0,
        "task" => 260.0,
        "highlighting" => 150.0,
        "progress" => 220.0,
        "indexing" => 160.0,
        "language" | "read-only" => 110.0,
        "memory" => 84.0,
        "zoom" => 64.0,
        _ => 110.0,
    }
}
fn status_plan(width: f32, available: &[&'static str]) -> StatusPlan {
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let total: f32 = available.iter().map(|id| status_width(id) + 4.0).sum();
    let mut budget = (width - if total > width { 36.0 } else { 0.0 }).max(0.0);
    let mut inline = Vec::new();
    // Preserve the current position and attention indicator before optional widgets.
    for id in [
        "notifications",
        "position",
        "status",
        // A running background task (and its cancel button) beats optional widgets.
        "progress",
        "git",
        "encoding",
        "line-endings",
        "indent",
        "interpreter",
        "task",
        "language",
        "read-only",
        "indexing",
        "memory",
        "highlighting",
        "zoom",
        "reader",
        "column",
        "power-save",
        "collaboration",
        "updates",
        "changes",
        "intentions",
    ] {
        if available.contains(&id) && status_width(id) + 4.0 <= budget {
            inline.push((id, status_width(id)));
            budget -= status_width(id) + 4.0;
        }
    }
    inline.sort_by_key(|(id, _)| {
        available
            .iter()
            .position(|key| key == id)
            .unwrap_or(usize::MAX)
    });
    let overflow = available
        .iter()
        .copied()
        .filter(|id| !inline.iter().any(|(key, _)| key == id))
        .collect();
    StatusPlan { inline, overflow }
}

impl IdeApp {
    pub(super) fn status_widgets_menu(&mut self, ui: &mut egui::Ui) {
        ui.label("Status Bar Widgets");
        for &(id, label) in rustpy_settings::chrome::STATUS_WIDGETS {
            let mut visible = self.settings.chrome.status_visible(id);
            if ui.checkbox(&mut visible, label).changed() {
                if visible {
                    self.settings.chrome.hidden_status_widgets.remove(id);
                } else {
                    self.settings.chrome.hidden_status_widgets.insert(id.into());
                }
            }
        }
        ui.separator();
        ui.checkbox(
            &mut self.settings.appearance.show_memory_indicator,
            "Enable memory indicator",
        );
        if ui.button("Restore widget defaults").clicked() {
            self.settings.chrome.hidden_status_widgets.clear();
        }
    }

    fn extra_status_widget(&mut self, ui: &mut egui::Ui, id: &str, pending: &mut Vec<Action>) {
        if id == "zoom" {
            ui.menu_button(
                format!("{:.0}%", self.settings.chrome.effective_scale() * 100.0),
                |ui| {
                    for action in [Action::ZoomIdeIn, Action::ZoomIdeOut, Action::ZoomIdeReset] {
                        if ui.button(action.text()).clicked() {
                            pending.push(action);
                            ui.close();
                        }
                    }
                },
            )
            .response
            .on_hover_text("IDE zoom");
        }
        let reader = self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| crate::editor::doc_render::reader_mode(&tab.state));
        let column = self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.state.column_mode);
        for (widget_id, label, selected, action) in [
            (
                "power-save",
                "Power Save",
                self.settings.power_save_mode,
                Action::TogglePowerSaveMode,
            ),
            (
                "collaboration",
                "Collaboration",
                self.collaboration_running(),
                Action::Collaborate,
            ),
            ("reader", "Reader", reader, Action::ToggleReaderMode),
            (
                "column",
                "Column",
                column,
                Action::ToggleColumnSelectionMode,
            ),
            (
                "updates",
                "Updates",
                self.pending_update_restart(),
                Action::CheckForUpdates,
            ),
            ("intentions", "💡", false, Action::ShowIntentionActions),
        ] {
            if id == widget_id
                && ui
                    .selectable_label(selected, crate::i18n::tr_static(label))
                    .icon_hint(action.text())
                    .clicked()
            {
                pending.push(action);
            }
        }
        if id == "highlighting" {
            if let Some(path) = self
                .active
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.state.path.clone())
            {
                use rustpy_settings::inspection_profiles::HighlightingLevel;
                let before = self.settings.inspections.profiles.level(&path);
                let mut level = before;
                ui.menu_button(format!("Highlight: {level:?}"), |ui| {
                    for value in [
                        HighlightingLevel::None,
                        HighlightingLevel::Syntax,
                        HighlightingLevel::All,
                    ] {
                        if ui
                            .selectable_value(&mut level, value, format!("{value:?}"))
                            .clicked()
                        {
                            ui.close();
                        }
                    }
                });
                if before != level {
                    self.settings
                        .inspections
                        .profiles
                        .file_levels
                        .insert(path.to_string_lossy().into_owned(), level);
                    if let Some(root) = self.project.as_ref().map(|project| project.root.clone()) {
                        if let Err(error) = self.settings.inspections.profiles.save_project(&root) {
                            self.set_status(format!(
                                "Cannot save highlighting preference: {error}"
                            ));
                        }
                    }
                    self.sync_open_tabs_now();
                }
            }
        }
    }
    fn status_control_available(&self, id: &str) -> bool {
        if !self.settings.chrome.status_visible(id) {
            return false;
        }
        match id {
            "status" => self.status.is_some() || self.project.is_some(),
            "task" => self
                .tasks
                .as_ref()
                .and_then(|store| store.active_task())
                .is_some_and(|task| task.id != rustpy_settings::tasks::DEFAULT_TASK_ID),
            "position" | "line-endings" | "encoding" | "indent" | "read-only" | "language"
            | "reader" | "column" | "intentions" => {
                self.active.and_then(|i| self.tabs.get(i)).is_some()
            }
            "highlighting" => self
                .active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| tab.state.path.is_some()),
            "changes" => !self.git_panel.entries.is_empty(),
            "git" => {
                self.settings.vcs.preferences.show_status_branch
                    && self
                        .project
                        .as_ref()
                        .is_some_and(|p| p.git_branch.is_some())
            }
            "memory" => self.settings.appearance.show_memory_indicator,
            // Only reserve the slot while a task is shown (or Power Save is on).
            "progress" => {
                self.settings.power_save_mode
                    || !crate::progress_ui::shown_tasks(
                        &rustpy_core::tasks::TaskManager::global().running(),
                    )
                    .is_empty()
            }
            _ => true,
        }
    }
    fn status_control(&mut self, ui: &mut egui::Ui, id: &str, pending: &mut Vec<Action>) {
        let theme = self.theme.clone();
        match id {
            "status" => {
                let text = self
                    .status
                    .as_ref()
                    .map(|(message, _)| message.clone())
                    .or_else(|| self.project.as_ref().map(|p| p.root.display().to_string()))
                    .unwrap_or_default();
                ui.add(
                    egui::Label::new(egui::RichText::new(&text).color(theme.text_dim)).truncate(),
                )
                .on_hover_text(text);
            }
            "task" => {
                if let Some(task) = self.tasks.as_ref().and_then(|s| s.active_task()) {
                    let label = format!(
                        "Task: {} · {}",
                        task.summary,
                        rustpy_settings::tasks::format_duration(task.time_spent)
                    );
                    if ui
                        .add(
                            egui::Label::new(egui::RichText::new(&label).color(theme.accent))
                                .truncate()
                                .sense(Sense::click()),
                        )
                        .on_hover_text(label)
                        .clicked()
                    {
                        self.open_switch_task_popup();
                    }
                }
                ui.menu_button("Time tracking", |ui| self.task_status_controls(ui));
            }
            "notifications" => self.status_notification_widget(ui),
            "memory" => self.status_memory_widget(ui),
            "changes" => {
                if ui
                    .link(format!(
                        "Changes: {} / {}",
                        self.git_panel.checked.len(),
                        self.git_panel.entries.len()
                    ))
                    .clicked()
                {
                    self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
                    self.bottom = Some(BottomTool::Git);
                }
            }
            "position" => {
                if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                    let pos = tab.state.virtual_caret(0).head;
                    let selected = crate::status_text::selected_codepoints(
                        &tab.state.doc,
                        tab.state.carets.iter().map(|caret| caret.range()),
                    );
                    let mut text = pos.to_string();
                    if tab.state.sticky_selection {
                        text.push_str(" · Sticky selection");
                    }
                    if selected > 0 {
                        text.push_str(&format!(" ({selected} chars)"));
                    }
                    if tab.state.carets.len() > 1 {
                        text.push_str(&format!(" [{} carets]", tab.state.carets.len()));
                    }
                    if ui
                        .add(egui::Label::new(text).truncate().sense(Sense::click()))
                        .on_hover_text("Go to Line:Column (Ctrl+G)")
                        .clicked()
                    {
                        self.dialog = Dialog::GoToLine(String::new());
                    }
                }
            }
            "line-endings" => self.line_separator_widget(ui),
            "encoding" => self.encoding_widget(ui),
            "indent" => self.indent_status_widget(ui),
            "read-only" => {
                if let Some(index) = self.active {
                    let readonly = self.tabs[index].state.doc.is_read_only();
                    if ui
                        .add(
                            egui::Label::new(if readonly {
                                "🔒 read-only"
                            } else {
                                "🔓 writable"
                            })
                            .truncate()
                            .sense(Sense::click()),
                        )
                        .on_hover_text(
                            "Toggle the disk read-only attribute; writable requires confirmation",
                        )
                        .clicked()
                    {
                        self.toggle_file_read_only(index);
                    }
                }
            }
            "language" => {
                if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                    let mut label = tab.state.language_label();
                    if tab.state.overwrite_mode {
                        label.push_str(" · OVR");
                    }
                    ui.add(egui::Label::new(label).truncate());
                }
            }
            "git" => {
                if let Some(branch) = self.project.as_ref().and_then(|p| p.git_branch.clone()) {
                    // Reading refs and walking the graph is repository IO:
                    // refresh the counts every few seconds, not every frame.
                    let repo = self.project.as_ref().and_then(|p| p.git.as_ref());
                    let key = egui::Id::new((
                        "status-git-ahead-behind",
                        repo.map(|repo| repo.root().to_path_buf()),
                        &branch,
                    ));
                    let now = ui.input(|input| input.time);
                    let cached = ui
                        .data(|data| data.get_temp::<(f64, Option<(usize, usize)>)>(key))
                        .filter(|(at, _)| now - at < 3.0 && *at <= now);
                    let ahead_behind = match cached {
                        Some((_, counts)) => counts,
                        None => {
                            let counts = repo.and_then(GitRepo::ahead_behind);
                            ui.data_mut(|data| data.insert_temp(key, (now, counts)));
                            counts
                        }
                    };
                    let counts = ahead_behind
                        .map(|(outgoing, incoming)| format!(" ↓{incoming} ↑{outgoing}"))
                        .unwrap_or_default();
                    let label = format!("Git: {branch}{counts}");
                    if ui.add(egui::Label::new(&label).truncate().sense(Sense::click())).on_hover_text(format!("{label} — branch operations (Alt+`)\nCounts use the last fetched upstream state")).clicked() {
                        self.dialog=Dialog::Branches;
                    }
                }
            }
            "interpreter" => {
                let label = self
                    .project
                    .as_ref()
                    .and_then(|p| p.interpreter.as_ref())
                    .map(|i| i.label())
                    .unwrap_or_else(|| "No interpreter".into());
                if ui
                    .add(egui::Label::new(&label).truncate().sense(Sense::click()))
                    .on_hover_text(format!("{label} — click to change Python interpreter"))
                    .clicked()
                {
                    self.dialog = Dialog::InterpreterPicker;
                }
                self.interpreter_configuration_banner(ui);
            }
            "indexing" => self.index_status_widget(ui),
            "progress" => self.status_bar_tasks(ui),
            _ => self.extra_status_widget(ui, id, pending),
        }
    }
    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        if !self.status_control_available("memory") {
            self.layout.memory = None;
        }
        let available: Vec<_> = rustpy_settings::chrome::STATUS_WIDGETS
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| self.status_control_available(id))
            .collect();
        if !available.contains(&"progress")
            && rustpy_core::tasks::TaskManager::global()
                .running()
                .iter()
                .any(|task| !task.hidden)
        {
            // Young tasks become visible after SHOW_AFTER without other repaints.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let plan = status_plan(ui.available_width(), &available);
        let mut pending = Vec::new();
        let response = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Allocate fixed slots before drawing children: even an unusually long
                // path, task or interpreter cannot displace the overflow button.
                for &(id, width) in &plan.inline {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(width, ui.spacing().interact_size.y),
                        Sense::hover(),
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("status-slot", id))
                            .max_rect(rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    child.set_clip_rect(rect.intersect(ui.clip_rect()));
                    self.status_control(&mut child, id, &mut pending);
                }
                if !plan.overflow.is_empty() {
                    ui.menu_button("⋯", |ui| {
                        ui.set_width((ui.ctx().content_rect().width() - 24.0).clamp(24.0, 440.0));
                        egui::ScrollArea::vertical()
                            .max_height((ui.ctx().content_rect().height() - 80.0).max(80.0))
                            .show(ui, |ui| {
                                for &id in &plan.overflow {
                                    let label = rustpy_settings::chrome::STATUS_WIDGETS
                                        .iter()
                                        .find(|(key, _)| *key == id)
                                        .map(|(_, label)| *label)
                                        .unwrap_or(id);
                                    ui.label(egui::RichText::new(label).small().weak());
                                    ui.horizontal_wrapped(|ui| {
                                        self.status_control(ui, id, &mut pending)
                                    });
                                    ui.separator();
                                }
                                ui.collapsing("Status Bar Widgets", |ui| {
                                    self.status_widgets_menu(ui)
                                });
                            });
                    })
                    .response
                    .icon_hint("More status controls");
                }
            })
            .response;
        response.context_menu(|ui| self.status_widgets_menu(ui));
        let ctx = ui.ctx().clone();
        for action in pending {
            // Status controls also expose editor commands (Reader, Column).
            // Use the same editor authority as menus instead of the IDE-only
            // action handler, whose fallback cannot execute those commands.
            if crate::editor::view::is_editor_action(action) {
                self.dispatch_editor_action(action);
                self.focus_editor = true;
                self.learn_observe(action);
                self.macro_record_action(action);
            } else {
                self.handle_action(action, &ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_widgets_do_not_render_or_sample_and_can_be_restored_individually() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.settings.appearance.show_memory_indicator = true;
        app.settings.chrome.hidden_status_widgets = rustpy_settings::chrome::STATUS_WIDGETS
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        let ctx = egui::Context::default();
        let hidden = ctx.run_ui(egui::RawInput::default(), |ui| app.status_bar(ui));
        assert!(!hidden
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(_))));
        assert!(
            app.layout.memory.is_none(),
            "hiding the memory widget also suppresses sampling"
        );
        hidden.drop_without_applying_deltas();
        app.settings
            .chrome
            .hidden_status_widgets
            .remove("interpreter");
        let visible = ctx.run_ui(egui::RawInput::default(), |ui| app.status_bar(ui));
        assert!(visible.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text()=="No interpreter")));
        assert!(app.layout.memory.is_none());
        visible.drop_without_applying_deltas();
    }

    #[test]
    fn running_task_progress_outranks_optional_widgets() {
        // The widgets of an open Python file in a 1300 px window: before, the
        // progress slot (with its cancel button) fell into the "⋯" overflow.
        let ids = [
            "status",
            "position",
            "line-endings",
            "encoding",
            "indent",
            "read-only",
            "language",
            "interpreter",
            "indexing",
            "highlighting",
            "progress",
            "notifications",
            "zoom",
        ];
        let plan = status_plan(1284.0, &ids);
        assert!(plan.inline.iter().any(|(id, _)| *id == "progress"));
        assert!(plan.overflow.contains(&"highlighting"));
    }
    #[test]
    fn narrow_plan_keeps_every_enabled_widget_in_exactly_one_destination() {
        let ids: Vec<_> = rustpy_settings::chrome::STATUS_WIDGETS
            .iter()
            .map(|(id, _)| *id)
            .collect();
        for width in [0.0, 80.0, 160.0, 320.0, 1024.0, 4096.0] {
            let plan = status_plan(width, &ids);
            let used: f32 = plan.inline.iter().map(|(_, width)| width + 4.0).sum();
            assert!(
                used + if plan.overflow.is_empty() { 0.0 } else { 36.0 } <= width.max(36.0) + 0.01
            );
            for id in &ids {
                assert_eq!(
                    usize::from(plan.inline.iter().any(|(key, _)| key == id))
                        + usize::from(plan.overflow.contains(id)),
                    1,
                    "{id} at {width}"
                );
            }
        }
        assert!(status_plan(f32::NAN, &ids).inline.is_empty());
    }
    #[test]
    fn actual_overflow_reader_action_preserves_visibility_preferences_and_document() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.push_tab(EditorState::from_text("value = 'λ'\n", Language::Python));
        app.settings.chrome.hidden_status_widgets = rustpy_settings::chrome::STATUS_WIDGETS
            .iter()
            .map(|(id, _)| id.to_string())
            .filter(|id| id != "reader" && id != "column")
            .collect();
        let hidden = app.settings.chrome.hidden_status_widgets.clone();
        let before = app.tabs[0].state.doc.text();
        let ctx = egui::Context::default();
        let frame = |app: &mut IdeApp, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(240.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_max_width(100.0);
                    app.status_bar(ui);
                },
            );
            let shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            shapes
        };
        let text_center = |shapes: &[egui::epaint::ClippedShape], text: &str| {
            shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(shape) if shape.galley.text() == text => {
                        Some(shape.pos + shape.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing rendered control: {text}"))
        };
        let click = |point| {
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        // Settle layout before locating controls, then deliver real pointer
        // movement, press and release as separate input frames.
        frame(&mut app, vec![]);
        let initial = frame(&mut app, vec![]);
        let more = text_center(&initial, "⋯");
        for event in click(more) {
            frame(&mut app, vec![event]);
        }
        frame(&mut app, vec![]);
        let opened = frame(&mut app, vec![]);
        let reader = text_center(&opened, "Reader");
        for event in click(reader) {
            frame(&mut app, vec![event]);
        }
        assert!(crate::editor::doc_render::reader_mode(&app.tabs[0].state));
        assert_eq!(app.settings.chrome.hidden_status_widgets, hidden);
        assert_eq!(app.tabs[0].state.doc.text(), before);
    }

    #[test]
    fn ide_zoom_dispatch_preserves_editor_font_and_unsaved_document() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.push_tab(EditorState::from_text("value = 'λ'\n", Language::Python));
        let index = app.active.unwrap();
        let before = app.tabs[index].state.doc.text();
        let font_size = app.settings.editor.font_size;
        let ctx = egui::Context::default();
        app.handle_action(Action::ZoomIdeIn, &ctx);
        // chrome_frame_start requests egui's deferred zoom update; the next
        // pass applies it. Both are actual frames, with no renderer in this test.
        for _ in 0..2 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                app.chrome_frame_start(ui.ctx())
            })
            .drop_without_applying_deltas();
        }
        assert!((ctx.zoom_factor() - 1.1).abs() < 0.001);
        assert_eq!(app.settings.editor.font_size, font_size);
        assert_eq!(app.tabs[index].state.doc.text(), before);
        app.handle_action(Action::ZoomIdeReset, &ctx);
        assert_eq!(app.settings.chrome.scale, 1.0);
    }
}

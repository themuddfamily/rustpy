//! Keyboard equivalents of tool-panel resize, float/dock and maximize handles.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum ToolDimension {
    LeftWidth,
    BottomHeight,
}

impl IdeApp {
    /// Annotate egui's real panel handles, preserving their pointer interaction
    /// and Tab order while giving their focused keyboard owner resize commands.
    pub(super) fn tool_panel_resize_accessibility(&mut self, ctx: &egui::Context) {
        let keyboard = self.editor_keyboard()
            && self.component_menu_owner_available(false)
            && !egui::Popup::is_any_open(ctx);
        for (panel, dimension, visible, label, orientation, increase, decrease) in [
            (
                "project_panel",
                ToolDimension::LeftWidth,
                self.layout.left_visible,
                "Left tool window width",
                egui::accesskit::Orientation::Vertical,
                egui::Key::ArrowRight,
                egui::Key::ArrowLeft,
            ),
            (
                "bottom_tools",
                ToolDimension::BottomHeight,
                self.bottom.is_some() && !self.floating_bottom && !self.bottom_maximized,
                "Bottom tool window height",
                egui::accesskit::Orientation::Horizontal,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
            ),
        ] {
            if !visible {
                continue;
            }
            let id = egui::Id::new(panel).with("__resize");
            let Some(response) = ctx.read_response(id) else {
                continue;
            };
            if keyboard && response.has_focus() {
                ctx.memory_mut(|memory| {
                    memory.set_focus_lock_filter(
                        id,
                        egui::EventFilter {
                            horizontal_arrows: matches!(dimension, ToolDimension::LeftWidth),
                            vertical_arrows: matches!(dimension, ToolDimension::BottomHeight),
                            ..Default::default()
                        },
                    )
                });
                for (key, increase) in [(increase, true), (decrease, false)] {
                    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
                        self.resize_tool_window_keyboard(dimension, increase, ctx);
                        // Cancel cardinal traversal already queued for this key.
                        ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                    }
                }
            }
            let size = match dimension {
                ToolDimension::LeftWidth => self.layout.left_width,
                ToolDimension::BottomHeight => self.layout.bottom_height,
            };
            ctx.accesskit_node_builder(id, |node| {
                node.set_role(egui::accesskit::Role::Splitter);
                node.set_label(label);
                node.set_orientation(orientation);
                node.set_numeric_value(f64::from(size));
                node.set_description(match dimension {
                    ToolDimension::LeftWidth => {
                        "Use Left and Right arrow keys to resize; Tab moves to the next control."
                    }
                    ToolDimension::BottomHeight => {
                        "Use Up and Down arrow keys to resize; Tab moves to the next control."
                    }
                });
            });
        }
    }

    pub(super) fn resize_tool_window_keyboard(
        &mut self,
        dimension: ToolDimension,
        increase: bool,
        ctx: &egui::Context,
    ) {
        if self.layout.view_mode != layout::ViewMode::Normal {
            self.set_status("Leave the view mode before resizing tool windows");
            return;
        }
        let delta = if increase { 40.0 } else { -40.0 };
        match dimension {
            ToolDimension::LeftWidth => {
                if !self.layout.left_visible || self.project.is_none() {
                    self.set_status("Open a left tool window before resizing it");
                    return;
                }
                self.prepare_tool_panel_sizes(ctx);
                let max = (ctx.content_rect().width() - 140.0).clamp(140.0, 1200.0);
                let width = (self.layout.left_width + delta).clamp(140.0, max);
                let id = egui::Id::new("project_panel");
                let mut rect = egui::PanelState::load(ctx, id)
                    .map(|state| state.outer_rect)
                    .unwrap_or_else(|| ctx.content_rect());
                rect.max.x = rect.min.x + width;
                ctx.data_mut(|data| {
                    data.insert_persisted(id, egui::PanelState { outer_rect: rect })
                });
                self.layout.left_width = width;
                self.set_status(format!("Left tool window width: {width:.0} pixels"));
            }
            ToolDimension::BottomHeight => {
                if self.bottom.is_none() {
                    self.set_status("Open a bottom tool window before resizing it");
                    return;
                }
                if self.floating_bottom || self.bottom_maximized {
                    self.set_status("Dock and restore the tool window before resizing it");
                    return;
                }
                self.prepare_tool_panel_sizes(ctx);
                let max = (ctx.content_rect().height() - 80.0).clamp(80.0, 1600.0);
                let height = (self.layout.bottom_height + delta).clamp(80.0, max);
                bottom_dock::set_keyboard_height(ctx, height);
                self.layout.bottom_height = height;
                self.set_status(format!("Bottom tool window height: {height:.0} pixels"));
            }
        }
        ctx.request_repaint();
    }

    pub(super) fn toggle_tool_window_floating_keyboard(&mut self, ctx: &egui::Context) {
        if self.tool_window_action(Action::ToggleToolWindowFloating, ctx) {
            return;
        }
        if self.bottom.is_none() {
            self.set_status("Open a bottom tool window before changing its mode");
            return;
        }
        self.floating_bottom = !self.floating_bottom;
        if let Some(tool) = self.bottom {
            self.tool_mode(
                layout::ToolRef::Bottom(tool),
                if self.floating_bottom {
                    rustpy_settings::tool_windows::Mode::Float
                } else {
                    rustpy_settings::tool_windows::Mode::DockPinned
                },
            );
        }
        self.bottom_maximized = false;
        self.set_status(if self.floating_bottom {
            "Tool window floating"
        } else {
            "Tool window docked"
        });
        ctx.request_repaint();
    }

    pub(super) fn toggle_tool_window_maximized_keyboard(&mut self, ctx: &egui::Context) {
        if self.tool_window_action(Action::ToggleToolWindowMaximized, ctx) {
            return;
        }
        if self.bottom.is_none() {
            self.set_status("Open a bottom tool window before changing its mode");
            return;
        }
        self.bottom_maximized = !self.bottom_maximized;
        self.set_status(if self.bottom_maximized {
            "Tool window maximized"
        } else {
            "Tool window restored"
        });
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "π = 1\n").unwrap();
        let mut settings = Settings::default();
        settings.completion.show_as_you_type = false;
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        app.bottom = Some(BottomTool::Problems);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        (root, app, ctx)
    }

    // Real App controls, ProjectTree and bottom dock. Geometry comes from the
    // same persisted PanelState captured by layout_frame_end, not a test model.
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                app.layout_frame_start(ui.ctx());
                app.global_shortcuts(ui);
                let keyboard = app.editor_keyboard();
                if app.layout.left_visible {
                    egui::Panel::left("project_panel")
                        .resizable(true)
                        .default_size(app.layout.left_width)
                        .min_size(140.0)
                        .show(ui, |ui| {
                            if let Some(project) = &mut app.project {
                                project.tree.ui(ui, &app.theme, &app.keymap, keyboard);
                            }
                        });
                }
                if app.bottom.is_some() && !app.floating_bottom {
                    let maximized = app.bottom_maximized;
                    bottom_dock::show_bottom_dock(ui, maximized, app.layout.bottom_height, |ui| {
                        app.bottom_panel(ui)
                    });
                }
                if !app.editor_behind_tool_window() {
                    egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                }
                app.layout_frame_end(ui.ctx());
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }

    #[test]
    fn resize_changes_real_panel_geometry_and_saved_layout_without_editing() {
        let (_root, mut app, ctx) = fixture();
        let before = app.tabs[0].state.doc.text();
        let left = app.layout.left_width;
        let bottom = app.layout.bottom_height;
        app.resize_tool_window_keyboard(ToolDimension::LeftWidth, true, &ctx);
        app.resize_tool_window_keyboard(ToolDimension::BottomHeight, true, &ctx);
        frame(&mut app, &ctx, 0.2, vec![]);
        let left_rect = egui::PanelState::load(&ctx, egui::Id::new("project_panel"))
            .unwrap()
            .outer_rect;
        let bottom_rect = egui::PanelState::load(&ctx, egui::Id::new("bottom_tools"))
            .unwrap()
            .outer_rect;
        assert!((left_rect.width() - left - 40.0).abs() < 2.0);
        assert!((bottom_rect.height() - bottom - 40.0).abs() < 2.0);
        let saved = app.tool_window_layout();
        assert_eq!(saved.project_width, app.layout.left_width);
        assert_eq!(saved.bottom_height, app.layout.bottom_height);
        for _ in 0..50 {
            app.resize_tool_window_keyboard(ToolDimension::LeftWidth, true, &ctx);
            app.resize_tool_window_keyboard(ToolDimension::BottomHeight, false, &ctx);
        }
        assert_eq!(app.layout.left_width, 860.0);
        assert_eq!(app.layout.bottom_height, 80.0);
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert!(!app.tabs[0].state.doc.can_undo());
    }

    #[test]
    fn explicit_resize_after_restore_supersedes_old_maximize_height() {
        let (_root, mut app, ctx) = fixture();
        let original = app.layout.bottom_height;
        app.toggle_tool_window_maximized_keyboard(&ctx);
        frame(&mut app, &ctx, 0.2, vec![]);
        assert!(app.bottom_maximized);
        app.resize_tool_window_keyboard(ToolDimension::BottomHeight, true, &ctx);
        assert_eq!(
            app.layout.bottom_height, original,
            "maximized geometry is not a docked resize"
        );
        app.toggle_tool_window_maximized_keyboard(&ctx);
        app.resize_tool_window_keyboard(ToolDimension::BottomHeight, true, &ctx);
        frame(&mut app, &ctx, 0.3, vec![]);
        let height = egui::PanelState::load(&ctx, egui::Id::new("bottom_tools"))
            .unwrap()
            .outer_rect
            .height();
        assert!((height - original - 40.0).abs() < 2.0, "{height}");
        app.bottom = None;
        app.resize_tool_window_keyboard(ToolDimension::BottomHeight, false, &ctx);
        assert_eq!(app.layout.bottom_height, (original + 40.0).round());
    }
    fn command(key: egui::Key) -> Vec<egui::Event> {
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::ALT;
        key_events(key, modifiers)
    }

    fn key_events(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }

    #[test]
    fn actual_panel_resize_handles_have_names_and_keyboard_adjustment() {
        let (root, mut app, ctx) = fixture();
        let identity = app.tabs[0].state.doc.identity();
        let version = app.tabs[0].state.doc.version();
        let original = std::fs::read(root.path().join("π.py")).unwrap();
        frame(
            &mut app,
            &ctx,
            0.2,
            key_events(egui::Key::Num1, egui::Modifiers::ALT),
        );
        frame(&mut app, &ctx, 0.3, vec![]);
        assert!(ProjectTree::has_focus(&ctx));
        let mut time = 0.4;
        for (panel, label, orientation, increase, decrease, dimension) in [
            (
                "project_panel",
                "Left tool window width",
                egui::accesskit::Orientation::Vertical,
                egui::Key::ArrowRight,
                egui::Key::ArrowLeft,
                ToolDimension::LeftWidth,
            ),
            (
                "bottom_tools",
                "Bottom tool window height",
                egui::accesskit::Orientation::Horizontal,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                ToolDimension::BottomHeight,
            ),
        ] {
            let handle = egui::Id::new(panel).with("__resize");
            let mut reached = false;
            let mut cycle_start = None;
            // The real dock may contain more than 32 toolbar/tab stops. Walk
            // one actual focus cycle instead of assuming its control count.
            // Keep a hard cap to diagnose traversal that never forms a cycle.
            for _ in 0..256 {
                let update = frame(
                    &mut app,
                    &ctx,
                    time,
                    key_events(egui::Key::Tab, egui::Modifiers::NONE),
                );
                time += 0.1;
                if ctx.memory(|memory| memory.focused()) == Some(handle) {
                    let node = update
                        .nodes
                        .iter()
                        .find(|(id, _)| *id == handle.accesskit_id())
                        .map(|(_, node)| node)
                        .expect("actual focused resize handle is exposed");
                    assert_eq!(update.focus, handle.accesskit_id());
                    assert_eq!(node.role(), egui::accesskit::Role::Splitter);
                    assert_eq!(node.label(), Some(label));
                    assert_eq!(node.orientation(), Some(orientation));
                    assert!(node.description().unwrap().contains("arrow keys"));
                    reached = true;
                    break;
                }
                if let Some(focused) = ctx.memory(|memory| memory.focused()) {
                    if cycle_start == Some(focused) {
                        break;
                    }
                    cycle_start.get_or_insert(focused);
                }
            }
            assert!(
                reached,
                "a complete real Tab cycle must reach {panel} resize handle"
            );
            let size = |app: &IdeApp| match dimension {
                ToolDimension::LeftWidth => app.layout.left_width,
                ToolDimension::BottomHeight => app.layout.bottom_height,
            };
            let before = size(&app);
            frame(
                &mut app,
                &ctx,
                time,
                key_events(increase, egui::Modifiers::NONE),
            );
            time += 0.1;
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(handle));
            assert!((size(&app) - before - 40.0).abs() < 2.0);
            let persisted = egui::PanelState::load(&ctx, egui::Id::new(panel)).unwrap();
            let persisted_size = match dimension {
                ToolDimension::LeftWidth => persisted.outer_rect.width(),
                ToolDimension::BottomHeight => persisted.outer_rect.height(),
            };
            assert!((persisted_size - size(&app)).abs() < 2.0);
            frame(
                &mut app,
                &ctx,
                time,
                key_events(decrease, egui::Modifiers::NONE),
            );
            time += 0.1;
            assert!((size(&app) - before).abs() < 2.0);
        }
        assert_eq!(app.tabs[0].state.doc.identity(), identity);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert_eq!(std::fs::read(root.path().join("π.py")).unwrap(), original);
    }

    #[test]
    fn actual_resize_handle_does_not_consume_find_in_files_input() {
        let (_root, mut app, ctx) = fixture();
        frame(
            &mut app,
            &ctx,
            0.2,
            key_events(egui::Key::Num1, egui::Modifiers::ALT),
        );
        frame(&mut app, &ctx, 0.3, vec![]);
        frame(
            &mut app,
            &ctx,
            0.4,
            key_events(egui::Key::Tab, egui::Modifiers::NONE),
        );
        let handle = egui::Id::new("project_panel").with("__resize");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(handle));
        frame(
            &mut app,
            &ctx,
            0.5,
            key_events(egui::Key::F, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
        );
        assert!(
            app.find_in_files.visible,
            "real shortcut opens Find in Files"
        );
        let width = app.layout.left_width;
        let document = app.tabs[0].state.doc.text();
        frame(
            &mut app,
            &ctx,
            0.6,
            key_events(egui::Key::ArrowRight, egui::Modifiers::NONE),
        );
        assert_eq!(app.layout.left_width, width);
        assert!(app.find_in_files.visible);
        assert_eq!(app.tabs[0].state.doc.text(), document);
        assert!(!app.tabs[0].state.doc.can_undo());
    }

    #[test]
    fn actual_bound_keyboard_commands_resize_dock_and_name_toolbar_controls() {
        let (_root, mut app, ctx) = fixture();
        let original = app.tabs[0].state.doc.text();
        let width = app.layout.left_width;
        let height = app.layout.bottom_height;
        for (chord, action) in [
            ("Ctrl+Alt+F7", Action::IncreaseLeftToolWidth),
            ("Ctrl+Alt+F8", Action::IncreaseBottomToolHeight),
            ("Ctrl+Alt+F9", Action::ToggleToolWindowFloating),
            ("Ctrl+Alt+F10", Action::ToggleToolWindowMaximized),
        ] {
            app.keymap
                .bind(crate::actions::KeyChord::parse(chord).unwrap(), action);
        }
        let mut nodes = frame(&mut app, &ctx, 0.2, command(egui::Key::F7)).nodes;
        assert!((app.layout.left_width - width - 40.0).abs() < 2.0);
        nodes.extend(frame(&mut app, &ctx, 0.3, command(egui::Key::F8)).nodes);
        assert!((app.layout.bottom_height - height - 40.0).abs() < 2.0);
        nodes.extend(frame(&mut app, &ctx, 0.4, command(egui::Key::F9)).nodes);
        assert!(app.floating_bottom);
        assert!(app.tool_window_layout().bottom_floating);
        nodes.extend(frame(&mut app, &ctx, 0.5, command(egui::Key::F9)).nodes);
        assert!(!app.floating_bottom);
        nodes.extend(frame(&mut app, &ctx, 0.6, command(egui::Key::F10)).nodes);
        assert!(app.bottom_maximized);
        nodes.extend(frame(&mut app, &ctx, 0.7, command(egui::Key::F10)).nodes);
        assert!(!app.bottom_maximized);
        for time in [0.8, 0.9] {
            nodes.extend(frame(&mut app, &ctx, time, vec![]).nodes);
        }
        assert!(nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Float the tool window in its own window")));
        assert!(nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Maximize / restore the tool window")));
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}

//! Persistent tool-window edge bars. Reserve these panels before drawing docks,
//! after the menu/toolbar/status panels, so bars surround their docked content.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StripeTool {
    Left(LeftTool),
    Bottom(BottomTool),
    Ai,
    Learn,
}

const LEFT: &[(StripeTool, &str, Action)] = &[
    (
        StripeTool::Left(LeftTool::Project),
        "Project",
        Action::ToggleProjectView,
    ),
    (
        StripeTool::Left(LeftTool::Structure),
        "Structure",
        Action::ToggleStructureView,
    ),
    (
        StripeTool::Left(LeftTool::Bookmarks),
        "Bookmarks",
        Action::ToggleBookmarksView,
    ),
];
const RIGHT: &[(StripeTool, &str, Action)] = &[
    (StripeTool::Ai, "AI Assistant", Action::ToggleAiAssistant),
    (StripeTool::Learn, "Learn", Action::LearnIdeFeatures),
];
const BOTTOM: &[(BottomTool, Action)] = &[
    (BottomTool::Terminal, Action::ToggleTerminal),
    (BottomTool::PythonConsole, Action::TogglePythonConsole),
    (BottomTool::SciView, Action::ToggleSciView),
    (BottomTool::Documentation, Action::ToggleDocumentationView),
    (BottomTool::Run, Action::ToggleRunView),
    (BottomTool::Debug, Action::ToggleDebugView),
    (BottomTool::Tests, Action::ToggleTestsView),
    (BottomTool::Packages, Action::TogglePythonPackages),
    (BottomTool::Services, Action::ToggleServicesView),
    (BottomTool::Git, Action::ToggleGitView),
    (BottomTool::Branches, Action::ToggleBranchesView),
    (BottomTool::OtherVcs, Action::ToggleOtherVcsView),
    (BottomTool::PullRequests, Action::GitHubPullRequests),
    (BottomTool::Find, Action::ToggleFindView),
    (BottomTool::Problems, Action::ToggleProblemsView),
    (BottomTool::Todo, Action::ToggleTodoView),
    (BottomTool::Endpoints, Action::ToggleEndpointsView),
    (BottomTool::Notifications, Action::ToggleNotificationsView),
    (BottomTool::Hierarchy, Action::ToggleHierarchyView),
    (BottomTool::Coverage, Action::ToggleCoverageView),
    (BottomTool::Profiler, Action::ToggleProfilerView),
    (BottomTool::Concurrency, Action::ToggleConcurrencyView),
    (BottomTool::WebPreview, Action::ToggleWebPreview),
    (BottomTool::HttpClient, Action::ToggleHttpClientView),
    (BottomTool::Database, Action::ToggleDatabaseView),
    (BottomTool::Diagram, Action::ToggleDiagramView),
];

impl IdeApp {
    fn stripe_caption(&self, name: &str, action: Action) -> String {
        let caption = if self.settings.chrome.new_ui {
            match action {
                Action::ToggleProjectView => "▧",
                Action::ToggleStructureView => "≡",
                Action::ToggleBookmarksView => "⚑",
                Action::ToggleTerminal => ">_",
                Action::TogglePythonConsole => "Py",
                Action::ToggleSciView => "∿",
                Action::ToggleRunView => "▶",
                Action::ToggleDebugView => "🐞",
                Action::ToggleTestsView => "✓",
                Action::TogglePythonPackages => "◈",
                Action::ToggleServicesView => "▦",
                Action::ToggleGitView | Action::ToggleBranchesView => "⑂",
                Action::ToggleOtherVcsView => "V",
                Action::GitHubPullRequests => "⇄",
                Action::ToggleFindView => "⌕",
                Action::ToggleProblemsView => "!",
                Action::ToggleTodoView => "☑",
                Action::ToggleEndpointsView => "⇥",
                Action::ToggleNotificationsView => "♧",
                Action::ToggleHierarchyView => "♜",
                Action::ToggleCoverageView => "%",
                Action::ToggleProfilerView => "◴",
                Action::ToggleConcurrencyView => "⋈",
                Action::ToggleWebPreview => "◎",
                Action::ToggleHttpClientView => "↔",
                Action::ToggleDatabaseView => "▤",
                Action::ToggleDiagramView => "◇",
                Action::ToggleAiAssistant => "✦",
                Action::LearnIdeFeatures => "?",
                _ => "•",
            }
            .to_string()
        } else {
            crate::i18n::tr(name).into_owned()
        };
        if self.settings.chrome.tool_window_numbers {
            let shortcut = self.keymap.shortcut_text(action);
            if let Some(digit) = shortcut
                .strip_prefix("Alt+")
                .filter(|value| value.len() == 1 && matches!(value.as_bytes()[0], b'1'..=b'9'))
            {
                return format!("{digit} {caption}");
            }
        }
        caption
    }
    pub(super) fn framework_tool_for_action(action: Action) -> Option<layout::ToolRef> {
        LEFT.iter()
            .chain(RIGHT)
            .find(|(_, _, candidate)| *candidate == action)
            .and_then(|(tool, _, _)| Self::stripe_id(*tool))
            .and_then(|id| layout::tool_ref_by_id(&id))
            .or_else(|| {
                BOTTOM
                    .iter()
                    .find(|(_, candidate)| *candidate == action)
                    .map(|(tool, _)| layout::ToolRef::Bottom(*tool))
            })
    }
    fn placed_stripe_tools(&self, edge: usize) -> Vec<(StripeTool, &'static str, Action)> {
        LEFT.iter()
            .copied()
            .chain(RIGHT.iter().copied())
            .chain(
                BOTTOM
                    .iter()
                    .map(|(tool, action)| (StripeTool::Bottom(*tool), tool.title(), *action)),
            )
            .filter(|(tool, _, _)| {
                Self::stripe_id(*tool).is_some_and(|id| {
                    let p = self.layout.tools.saved.placement(&id);
                    p.sidebar && p.slot.edge() == edge
                })
            })
            .collect()
    }
    fn stripe_id(tool: StripeTool) -> Option<String> {
        match tool {
            StripeTool::Left(tool) => Some(layout::tool_ref_id(layout::ToolRef::Left(tool))),
            StripeTool::Bottom(tool) => Some(layout::tool_ref_id(layout::ToolRef::Bottom(tool))),
            StripeTool::Ai => Some(layout::tool_ref_id(layout::ToolRef::Ai)),
            StripeTool::Learn => Some(layout::tool_ref_id(layout::ToolRef::Learn)),
        }
    }
    fn shows_tool_stripes(&self) -> bool {
        self.project.is_some()
            && self.shows_chrome()
            && self.settings.appearance.show_tool_window_bars
    }

    fn stripe_selected(&self, tool: StripeTool) -> bool {
        match tool {
            StripeTool::Left(t) => self
                .framework_visible()
                .contains(&layout::tool_ref_id(layout::ToolRef::Left(t))),
            StripeTool::Bottom(t) => self
                .framework_visible()
                .contains(&layout::tool_ref_id(layout::ToolRef::Bottom(t))),
            StripeTool::Ai => self
                .framework_visible()
                .contains(&layout::tool_ref_id(layout::ToolRef::Ai)),
            StripeTool::Learn => self
                .framework_visible()
                .contains(&layout::tool_ref_id(layout::ToolRef::Learn)),
        }
    }

    fn activate_stripe(&mut self, tool: StripeTool, action: Action, ctx: &egui::Context) {
        // Keyboard actions for Project and Terminal hide only when focused, and
        // Python Console always opens. A selected stripe button always hides.
        if self.stripe_selected(tool) {
            match tool {
                StripeTool::Left(_) | StripeTool::Bottom(_) => {
                    if let Some(id) = Self::stripe_id(tool) {
                        self.hide_framework_tool(&id);
                    }
                }
                StripeTool::Ai | StripeTool::Learn => {
                    if let Some(id) = Self::stripe_id(tool) {
                        self.hide_framework_tool(&id);
                    }
                }
            }
            self.focus_editor = true;
        } else {
            self.handle_action(action, ctx);
            if let Some(id) = Self::stripe_id(tool) {
                let slot = self.layout.tools.saved.placement(&id).slot;
                self.layout.tools.saved.selected.insert(slot, id.clone());
                self.layout.tools.focused = Some(id);
            }
            self.focus_editor = false;
            match tool {
                StripeTool::Left(t) => self.layout.last_tool = Some(layout::ToolRef::Left(t)),
                StripeTool::Bottom(t) => self.layout.last_tool = Some(layout::ToolRef::Bottom(t)),
                StripeTool::Ai => self.layout.last_tool = Some(layout::ToolRef::Ai),
                StripeTool::Learn => self.layout.last_tool = Some(layout::ToolRef::Learn),
            }
        }
    }

    /// Call before the left project panel; this remains visible while it is open.
    pub(super) fn left_tool_stripe(&mut self, ui: &mut egui::Ui) {
        if !self.shows_tool_stripes() {
            return;
        }
        let theme = self.theme.clone();
        let tools = self.placed_stripe_tools(0);
        egui::Panel::left("persistent_left_tool_stripe")
            .resizable(false)
            .exact_size(if self.settings.chrome.new_ui {
                36.0
            } else {
                24.0
            })
            .frame(egui::Frame::new().fill(theme.panel_bg))
            .show(ui, |ui| self.vertical_tool_stripe(ui, &tools, false));
    }

    /// Call before AI/Learn right panels; both buttons remain independently usable.
    pub(super) fn right_tool_stripe(&mut self, ui: &mut egui::Ui) {
        if !self.shows_tool_stripes() {
            return;
        }
        let theme = self.theme.clone();
        let tools = self.placed_stripe_tools(1);
        egui::Panel::right("persistent_right_tool_stripe")
            .resizable(false)
            .exact_size(if self.settings.chrome.new_ui {
                36.0
            } else {
                24.0
            })
            .frame(egui::Frame::new().fill(theme.panel_bg))
            .show(ui, |ui| self.vertical_tool_stripe(ui, &tools, true));
    }

    fn vertical_tool_stripe(
        &mut self,
        ui: &mut egui::Ui,
        tools: &[(StripeTool, &'static str, Action)],
        clockwise: bool,
    ) {
        let theme = self.theme.clone();
        egui::ScrollArea::vertical()
            .id_salt(if clockwise {
                "right_stripe_scroll"
            } else {
                "left_stripe_scroll"
            })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for &(tool, name, action) in tools {
                    let selected = self.stripe_selected(tool);
                    let galley = ui.painter().layout_no_wrap(
                        self.stripe_caption(name, action),
                        egui::FontId::proportional(self.settings.appearance.ui_font_size),
                        theme.text,
                    );
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(
                            ui.available_width(),
                            if self.settings.chrome.new_ui {
                                32.0
                            } else {
                                galley.size().x + 16.0
                            },
                        ),
                        egui::Sense::click(),
                    );
                    if let Some(id) = Self::stripe_id(tool) {
                        self.layout.tools.stripes.insert(id, rect);
                    }
                    let response = response.on_hover_text(format!(
                        "{} {} ({})",
                        if selected { "Hide" } else { "Show" },
                        name,
                        self.keymap.shortcut_text(action)
                    ));
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            crate::i18n::tr_static(name),
                        )
                    });
                    if selected || response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            2.0,
                            if selected {
                                theme.selection_inactive_bg
                            } else {
                                theme.hover_bg
                            },
                        );
                    }
                    if selected {
                        let x = if clockwise {
                            rect.right() - 2.0
                        } else {
                            rect.left()
                        };
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                egui::vec2(2.0, rect.height()),
                            ),
                            0.0,
                            theme.accent,
                        );
                    }
                    let pos = if clockwise {
                        egui::pos2(rect.center().x + galley.size().y / 2.0, rect.top() + 8.0)
                    } else {
                        egui::pos2(rect.center().x - galley.size().y / 2.0, rect.bottom() - 8.0)
                    };
                    let angle = if clockwise {
                        std::f32::consts::FRAC_PI_2
                    } else {
                        -std::f32::consts::FRAC_PI_2
                    };
                    if self.settings.chrome.new_ui {
                        ui.painter().galley(
                            rect.center() - galley.size() * 0.5,
                            galley,
                            theme.text,
                        );
                    } else {
                        ui.painter().add(
                            egui::epaint::TextShape::new(pos, galley, theme.text).with_angle(angle),
                        );
                    }
                    #[cfg(test)]
                    remember_hit(ui.ctx(), Some(tool), rect);
                    if response.clicked() {
                        self.activate_stripe(tool, action, ui.ctx());
                    }
                }
            });
    }

    /// Call before the bottom dock, after the status panel. Overflow is a menu,
    /// so every registered bottom tool remains accessible at small widths.
    pub(super) fn bottom_tool_stripe(&mut self, ui: &mut egui::Ui) {
        if !self.shows_tool_stripes() {
            return;
        }
        let theme = self.theme.clone();
        let tools = self.placed_stripe_tools(3);
        egui::Panel::bottom("persistent_bottom_tool_stripe")
            .resizable(false)
            .exact_size(self.settings.appearance.ui_font_size + 12.0)
            .frame(egui::Frame::new().fill(theme.panel_bg))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Reserve room for the overflow control before fitting tabs.
                    let mut remaining = (ui.available_width() - 75.0).max(0.0);
                    let mut first_hidden = tools.len();
                    for (index, &(tool, name, action)) in tools.iter().enumerate() {
                        let width = ui
                            .painter()
                            .layout_no_wrap(
                                self.stripe_caption(name, action),
                                egui::FontId::proportional(self.settings.appearance.ui_font_size),
                                theme.text,
                            )
                            .size()
                            .x
                            + 16.0;
                        if width > remaining {
                            first_hidden = index;
                            break;
                        }
                        remaining -= width + ui.spacing().item_spacing.x;
                        self.horizontal_stripe_button(ui, tool, name, action);
                    }
                    if first_hidden < tools.len() {
                        let overflow = ui.menu_button("More…", |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(400.0)
                                .show(ui, |ui| {
                                    for &(tool, name, action) in &tools[first_hidden..] {
                                        if self.horizontal_stripe_button(ui, tool, name, action) {
                                            ui.close();
                                        }
                                    }
                                });
                        });
                        #[cfg(test)]
                        remember_hit(ui.ctx(), None, overflow.response.rect);
                        #[cfg(not(test))]
                        let _ = overflow;
                    }
                });
            });
    }

    fn horizontal_stripe_button(
        &mut self,
        ui: &mut egui::Ui,
        tool: StripeTool,
        name: &'static str,
        action: Action,
    ) -> bool {
        let selected = self.stripe_selected(tool);
        let response = ui
            .selectable_label(selected, self.stripe_caption(name, action))
            .on_hover_text(format!(
                "{} {}",
                if selected { "Hide" } else { "Show" },
                name
            ));
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                selected,
                crate::i18n::tr_static(name),
            )
        });
        #[cfg(test)]
        remember_hit(ui.ctx(), Some(tool), response.rect);
        if let Some(id) = Self::stripe_id(tool) {
            self.layout.tools.stripes.insert(id, response.rect);
        }
        if response.clicked() {
            self.activate_stripe(tool, action, ui.ctx());
            true
        } else {
            false
        }
    }
    pub(super) fn top_tool_stripe(&mut self, ui: &mut egui::Ui) {
        if !self.shows_tool_stripes() {
            return;
        }
        let tools = self.placed_stripe_tools(2);
        if tools.is_empty() {
            return;
        }
        egui::Panel::top("persistent_top_tool_stripe")
            .resizable(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (tool, name, action) in tools {
                        self.horizontal_stripe_button(ui, tool, name, action);
                    }
                });
            });
    }
}

#[cfg(test)]
fn remember_hit(ctx: &egui::Context, tool: Option<StripeTool>, rect: egui::Rect) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Vec<(Option<StripeTool>, egui::Rect)>>(egui::Id::new(
            "stripe_test_hits",
        ))
        .push((tool, rect));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_project() -> IdeApp {
        // No open_project: no watcher, interpreter discovery, settings writes or shell.
        let mut app = IdeApp::new(Settings::default(), None);
        let root = PathBuf::from("/rustpy-stripe-test-project");
        app.project = Some(Project {
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            root,
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app
    }

    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
    ) -> Vec<(Option<StripeTool>, egui::Rect)> {
        ctx.data_mut(|d| {
            d.remove::<Vec<(Option<StripeTool>, egui::Rect)>>(egui::Id::new("stripe_test_hits"))
        });
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.left_tool_stripe(ui);
                app.right_tool_stripe(ui);
                app.bottom_tool_stripe(ui);
                egui::CentralPanel::default().show(ui, |_| {});
            },
        )
        .drop_without_applying_deltas();
        ctx.data(|d| {
            d.get_temp::<Vec<(Option<StripeTool>, egui::Rect)>>(egui::Id::new("stripe_test_hits"))
        })
        .unwrap_or_default()
    }

    fn click(
        app: &mut IdeApp,
        ctx: &egui::Context,
        width: f32,
        pos: egui::Pos2,
    ) -> Vec<(Option<StripeTool>, egui::Rect)> {
        frame(
            app,
            ctx,
            width,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            app,
            ctx,
            width,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }

    fn position(hits: &[(Option<StripeTool>, egui::Rect)], tool: Option<StripeTool>) -> egui::Pos2 {
        hits.iter().find(|(t, _)| *t == tool).unwrap().1.center()
    }

    #[test]
    fn pointer_toggles_visible_left_dock_and_switches_tools() {
        let mut app = app_with_project();
        let ctx = egui::Context::default();
        let hits = frame(&mut app, &ctx, 1500.0, vec![]);
        assert!(app.layout.left_visible);
        click(
            &mut app,
            &ctx,
            1500.0,
            position(&hits, Some(StripeTool::Left(LeftTool::Project))),
        );
        assert!(!app.layout.left_visible);
        let hits = frame(&mut app, &ctx, 1500.0, vec![]);
        click(
            &mut app,
            &ctx,
            1500.0,
            position(&hits, Some(StripeTool::Left(LeftTool::Structure))),
        );
        assert!(app.layout.left_visible);
        assert_eq!(app.left_tool, LeftTool::Structure);
        assert_eq!(
            app.layout.last_tool,
            Some(layout::ToolRef::Left(LeftTool::Structure))
        );
    }

    #[test]
    fn explicit_terminal_pointer_activation_and_selected_hide() {
        let mut app = app_with_project();
        app.safe_mode = true;
        app.terminal_panel.set_auto_shell(false);
        let ctx = egui::Context::default();
        let hits = frame(&mut app, &ctx, 1500.0, vec![]);
        let pos = position(&hits, Some(StripeTool::Bottom(BottomTool::Terminal)));
        click(&mut app, &ctx, 1500.0, pos);
        assert_eq!(app.bottom, Some(BottomTool::Terminal));
        assert!(app.terminal_panel.auto_shell());
        assert_eq!(
            app.terminal_panel.tab_count(),
            0,
            "rendering chrome starts no shell"
        );
        click(&mut app, &ctx, 1500.0, pos);
        assert_eq!(
            app.bottom, None,
            "selected click hides even before terminal gains focus"
        );
        assert!(app.focus_editor);
    }

    #[test]
    fn pointer_toggles_both_right_tools() {
        let mut app = app_with_project();
        let ctx = egui::Context::default();
        for tool in [StripeTool::Ai, StripeTool::Learn] {
            let hits = frame(&mut app, &ctx, 1500.0, vec![]);
            let pos = position(&hits, Some(tool));
            click(&mut app, &ctx, 1500.0, pos);
            assert!(app.stripe_selected(tool));
            click(&mut app, &ctx, 1500.0, pos);
            assert!(!app.stripe_selected(tool));
        }
    }

    #[test]
    fn narrow_overflow_pointer_opens_otherwise_inaccessible_tool() {
        let mut app = app_with_project();
        let ctx = egui::Context::default();
        let hits = frame(&mut app, &ctx, 280.0, vec![]);
        assert!(!hits
            .iter()
            .any(|(t, _)| *t == Some(StripeTool::Bottom(BottomTool::Hierarchy))));
        click(&mut app, &ctx, 280.0, position(&hits, None));
        let hits = frame(&mut app, &ctx, 280.0, vec![]);
        let pos = position(&hits, Some(StripeTool::Bottom(BottomTool::Hierarchy)));
        click(&mut app, &ctx, 280.0, pos);
        assert_eq!(app.bottom, Some(BottomTool::Hierarchy));
        let saved = app.tool_window_layout();
        let mut restored = app_with_project();
        restored.restore_tool_window_layout(&saved);
        assert_eq!(restored.bottom, Some(BottomTool::Hierarchy));
    }

    #[test]
    fn hidden_bars_and_view_modes_reserve_no_space() {
        let mut app = app_with_project();
        let ctx = egui::Context::default();
        app.settings.appearance.show_tool_window_bars = false;
        assert!(frame(&mut app, &ctx, 800.0, vec![]).is_empty());
        app.settings.appearance.show_tool_window_bars = true;
        for mode in [
            layout::ViewMode::DistractionFree,
            layout::ViewMode::Presentation,
        ] {
            app.layout.view_mode = mode;
            assert!(frame(&mut app, &ctx, 800.0, vec![]).is_empty());
        }
        app.layout.view_mode = layout::ViewMode::Normal;
        app.project = None;
        assert!(frame(&mut app, &ctx, 800.0, vec![]).is_empty());
    }

    #[test]
    fn edge_panels_surround_docks_and_suppression_restores_editor_space() {
        let mut app = app_with_project();
        let ctx = egui::Context::default();
        let mut dock_rects = Vec::new();
        let mut center = egui::Rect::NOTHING;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.left_tool_stripe(ui);
                app.right_tool_stripe(ui);
                app.bottom_tool_stripe(ui);
                dock_rects.push(
                    egui::Panel::left("test_project_dock")
                        .exact_size(200.0)
                        .show(ui, |_| {})
                        .response
                        .rect,
                );
                dock_rects.push(
                    egui::Panel::right("test_right_dock")
                        .exact_size(180.0)
                        .show(ui, |_| {})
                        .response
                        .rect,
                );
                dock_rects.push(
                    egui::Panel::bottom("test_bottom_dock")
                        .exact_size(140.0)
                        .show(ui, |_| {})
                        .response
                        .rect,
                );
                center = egui::CentralPanel::default().show(ui, |_| {}).response.rect;
            },
        )
        .drop_without_applying_deltas();
        let hits = ctx
            .data(|d| {
                d.get_temp::<Vec<(Option<StripeTool>, egui::Rect)>>(egui::Id::new(
                    "stripe_test_hits",
                ))
            })
            .unwrap();
        let left = position(&hits, Some(StripeTool::Left(LeftTool::Project)));
        let right = position(&hits, Some(StripeTool::Ai));
        let bottom = position(&hits, Some(StripeTool::Bottom(BottomTool::Terminal)));
        assert!(left.x < dock_rects[0].left());
        assert!(right.x > dock_rects[1].right());
        assert!(bottom.y > dock_rects[2].bottom());
        assert!(center.left() >= dock_rects[0].right());
        assert!(center.right() <= dock_rects[1].left());
        assert!(center.bottom() <= dock_rects[2].top());

        app.settings.appearance.show_tool_window_bars = false;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                ..Default::default()
            },
            |ui| {
                let before = ui.available_rect_before_wrap();
                app.left_tool_stripe(ui);
                app.right_tool_stripe(ui);
                app.bottom_tool_stripe(ui);
                assert_eq!(ui.available_rect_before_wrap(), before);
            },
        )
        .drop_without_applying_deltas();
    }

    #[test]
    fn every_persisted_bottom_tool_has_a_stripe_action() {
        assert_eq!(BOTTOM.len(), 26);
        for &(tool, _) in BOTTOM {
            assert_eq!(layout::bottom_tool_by_title(tool.title()), Some(tool));
        }
        for (i, (tool, _)) in BOTTOM.iter().enumerate() {
            assert!(!BOTTOM[..i].iter().any(|(other, _)| other == tool));
        }
    }
}

//! One placement authority around the existing, process-owning tool contents.
use super::layout::{tool_ref_by_id, tool_ref_id, ToolRef};
use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::tool_windows::{Framework, Mode, Slot};
use std::collections::BTreeMap;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct State {
    pub debug_tabs: BTreeMap<u64, crate::tool_tabs::Appearance>,
    pub saved: Framework,
    rects: BTreeMap<String, (egui::ViewportId, egui::Rect)>,
    pub stripes: BTreeMap<String, egui::Rect>,
    pub focused: Option<String>,
    drawing: Option<String>,
    tool_filter: String,
    resize_undocked: BTreeMap<String, egui::Vec2>,
    seen_primary: Option<(Option<String>, Option<String>)>,
    apply_geometry: bool,
    drawn: std::collections::BTreeSet<String>,
    native_max_request: BTreeMap<String, bool>,
    native_modal_hidden: std::collections::BTreeSet<String>,
}
impl State {
    pub fn restore(mut saved: Framework) -> Self {
        saved.sanitize();
        let native_max_request = saved
            .tools
            .iter()
            .filter(|(_, placement)| matches!(placement.mode, Mode::Float | Mode::Window))
            .map(|(id, _)| (id.clone(), saved.window_maximized.contains(id)))
            .collect();
        Self {
            saved,
            native_max_request,
            apply_geometry: true,
            ..Default::default()
        }
    }
}

fn all_tools() -> Vec<ToolRef> {
    layout::LEFT_TOOLS
        .iter()
        .map(|(tool, _)| ToolRef::Left(*tool))
        .chain(layout::BOTTOM_TOOLS.iter().copied().map(ToolRef::Bottom))
        .chain([ToolRef::Ai, ToolRef::Learn])
        .collect()
}
fn title(tool: ToolRef) -> &'static str {
    match tool {
        ToolRef::Ai => "AI Assistant",
        ToolRef::Learn => "Learn",
        ToolRef::Left(tool) => layout::left_tool_name(tool),
        ToolRef::Bottom(tool) => tool.title(),
    }
}

impl IdeApp {
    pub(super) fn framework_visible(&self) -> Vec<String> {
        let mut tools = self.layout.tools.saved.open.clone();
        if self.ai_panel.visible {
            tools.push(tool_ref_id(ToolRef::Ai));
        }
        if self.learn.visible {
            tools.push(tool_ref_id(ToolRef::Learn));
        }
        if self.layout.left_visible {
            tools.push(tool_ref_id(ToolRef::Left(self.left_tool)));
        }
        if let Some(tool) = self.bottom {
            tools.push(tool_ref_id(ToolRef::Bottom(tool)));
        }
        tools.retain(|id| tool_ref_by_id(id).is_some());
        tools.sort();
        tools.dedup();
        tools
    }
    fn native_tool_modal_suppressed(&self) -> bool {
        !matches!(self.dialog, Dialog::None)
            || self.choice.is_some()
            || self.layout.exit_prompt.is_some()
            || self.coordinated_quit_active()
            || self.coordinated_quit_committed
            || self.exit_requested
    }
    fn native_tool_builder(&self, id: &str, suppressed: bool) -> egui::ViewportBuilder {
        let placement = self.layout.tools.saved.placement(id);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("{} – RustPy", self.tool_display_title(id)))
            .with_inner_size([900.0, 440.0])
            .with_visible(!suppressed)
            .with_window_level(if placement.mode == Mode::Float && !suppressed {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            });
        if let Some([x, y, w, h]) = self.layout.tools.saved.window_bounds.get(id).copied() {
            builder = builder.with_position([x, y]).with_inner_size([w, h]);
        }
        builder.with_maximized(self.layout.tools.saved.window_maximized.contains(id))
    }
    /// Global egui modals belong to the root native window. An AlwaysOnTop
    /// sibling otherwise covers their buttons even while its WebView is hidden.
    /// Hide only the presentation; process owners and saved placement survive.
    pub(super) fn sync_native_tool_modality(&mut self, ctx: &egui::Context) {
        let suppressed = self.native_tool_modal_suppressed();
        let native = self
            .framework_visible()
            .into_iter()
            .filter(|id| {
                matches!(
                    self.layout.tools.saved.placement(id).mode,
                    Mode::Float | Mode::Window
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.layout
            .tools
            .native_modal_hidden
            .retain(|id| native.contains(id));
        for id in native {
            let viewport = egui::ViewportId::from_hash_of(("native-tool", &id));
            if suppressed {
                // Commands alone do not mark a viewport used. The quit guard
                // skips ordinary tool rendering, so keep its actual native host
                // declared (with no interactive content) instead of destroying it.
                ctx.show_viewport_deferred(
                    viewport,
                    self.native_tool_builder(&id, true),
                    |_, _| {},
                );
                if self.layout.tools.native_modal_hidden.insert(id) {
                    ctx.send_viewport_cmd_to(
                        viewport,
                        egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal),
                    );
                    ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(false));
                }
            } else if self.layout.tools.native_modal_hidden.remove(&id) {
                ctx.show_viewport_deferred(
                    viewport,
                    self.native_tool_builder(&id, false),
                    |_, _| {},
                );
                let level = if self.layout.tools.saved.placement(&id).mode == Mode::Float {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                };
                ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::WindowLevel(level));
                ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(true));
            }
        }
    }
    pub(super) fn tool_sidebar_visible(&self, id: &str) -> bool {
        self.layout.tools.saved.placement(id).sidebar
    }
    pub(super) fn tool_window_has_frame_host(&self) -> bool {
        self.layout.tools.drawing.is_some()
    }
    pub(super) fn legacy_bottom_tool_tabs(&self) -> Vec<BottomTool> {
        if self.tool_window_has_frame_host() {
            Vec::new()
        } else {
            layout::BOTTOM_TOOLS.to_vec()
        }
    }
    /// Programmatic activation uses the same selected slot as stripes and tabs.
    /// This does not launch, reveal a hidden process, or request keyboard focus.
    pub(super) fn reveal_tool_window_content(&mut self, tool: ToolRef) {
        let id = tool_ref_id(tool);
        let slot = self.layout.tools.saved.placement(&id).slot;
        self.layout.tools.saved.selected.insert(slot, id.clone());
        if self
            .layout
            .tools
            .saved
            .maximized
            .as_ref()
            .is_some_and(|maximized| maximized != &id)
        {
            self.layout.tools.saved.maximized = None;
        }
    }
    pub(super) fn hide_framework_tool(&mut self, id: &str) {
        if matches!(
            tool_ref_by_id(id),
            Some(ToolRef::Bottom(BottomTool::WebPreview))
        ) {
            self.release_web_preview_native_parent();
        }
        self.layout.tools.saved.open.retain(|open| open != id);
        match tool_ref_by_id(id) {
            Some(ToolRef::Ai) => self.ai_panel.visible = false,
            Some(ToolRef::Learn) => self.learn.visible = false,
            Some(ToolRef::Left(tool)) if self.left_tool == tool => self.layout.left_visible = false,
            Some(ToolRef::Bottom(tool)) if self.bottom == Some(tool) => self.bottom = None,
            _ => {}
        }
        if self.layout.tools.saved.maximized.as_deref() == Some(id) {
            self.layout.tools.saved.maximized = None;
        }
    }
    pub(super) fn open_framework_tool(&mut self, id: &str) {
        if let Some(tool) = tool_ref_by_id(id) {
            match tool {
                ToolRef::Ai => self.ai_panel.visible = true,
                ToolRef::Learn => self.learn.visible = true,
                ToolRef::Bottom(BottomTool::Terminal) => self
                    .terminal_panel
                    .set_auto_shell(!self.project_safe_mode()),
                _ => {}
            }
            if !self.layout.tools.saved.open.iter().any(|open| open == id) {
                self.layout.tools.saved.open.push(id.into());
            }
            let slot = self.layout.tools.saved.placement(id).slot;
            self.layout.tools.saved.selected.insert(slot, id.into());
            self.layout.last_tool = Some(tool);
            self.layout.tools.focused = Some(id.into());
            self.focus_editor = false;
        }
    }
    pub(super) fn tool_mode(&mut self, tool: ToolRef, mode: Mode) {
        let id = tool_ref_id(tool);
        self.layout.tools.focused = Some(id.clone());
        self.layout.tools.saved.placement_mut(&id).mode = mode;
        self.bottom_maximized = false;
        self.layout.tools.saved.maximized = None;
        if let ToolRef::Bottom(bottom) = tool {
            if self.bottom == Some(bottom) {
                self.floating_bottom = !mode.docked();
            }
        }
    }
    pub(super) fn tool_windows_docks(&mut self, ui: &mut egui::Ui) {
        if self.project.is_none() || !self.shows_chrome() {
            return;
        }
        self.layout.tools.drawn.clear();
        self.auto_hide_editor_return(ui.ctx());
        let primary = (
            self.layout
                .left_visible
                .then(|| tool_ref_id(ToolRef::Left(self.left_tool))),
            self.bottom.map(|tool| tool_ref_id(ToolRef::Bottom(tool))),
        );
        for (id, previous) in [
            (
                &primary.0,
                self.layout
                    .tools
                    .seen_primary
                    .as_ref()
                    .and_then(|old| old.0.as_ref()),
            ),
            (
                &primary.1,
                self.layout
                    .tools
                    .seen_primary
                    .as_ref()
                    .and_then(|old| old.1.as_ref()),
            ),
        ] {
            if let Some(id) = id {
                let slot = self.layout.tools.saved.placement(id).slot;
                if self.layout.tools.seen_primary.is_some() && previous != Some(id) {
                    self.layout.tools.saved.selected.insert(slot, id.clone());
                } else {
                    self.layout
                        .tools
                        .saved
                        .selected
                        .entry(slot)
                        .or_insert_with(|| id.clone());
                }
            }
        }
        self.layout.tools.seen_primary = Some(primary);
        let ids = self.framework_visible();
        let maximized = self
            .layout
            .tools
            .saved
            .maximized
            .clone()
            .or_else(|| {
                self.bottom
                    .filter(|_| self.bottom_maximized && !self.floating_bottom)
                    .map(|tool| tool_ref_id(ToolRef::Bottom(tool)))
            })
            .filter(|id| ids.contains(id));
        if let Some(id) = maximized {
            super::bottom_dock::show_bottom_dock(ui, true, self.layout.bottom_height, |ui| {
                self.framework_content(ui, &id)
            });
            return;
        }
        for edge in if self.layout.tools.saved.wide {
            [3, 0, 1, 2]
        } else {
            [0, 1, 2, 3]
        } {
            let members: Vec<String> = ids
                .iter()
                .filter(|id| {
                    let p = self.layout.tools.saved.placement(id);
                    p.mode.docked() && p.slot.edge() == edge
                })
                .cloned()
                .collect();
            if members.is_empty() {
                continue;
            }
            let default = match edge {
                0 => self.layout.left_width,
                3 => self.layout.bottom_height,
                _ => self.layout.tools.saved.edge_sizes[edge],
            };
            let response = if edge == 3 {
                super::bottom_dock::show_bottom_dock(ui, false, default, |ui| {
                    self.framework_edge(ui, edge, &members)
                })
            } else {
                let panel = match edge {
                    0 => egui::Panel::left("project_panel"),
                    1 => egui::Panel::right("right_tools"),
                    _ => egui::Panel::top("top_tools"),
                };
                panel
                    .resizable(true)
                    .min_size(if edge < 2 { 140.0 } else { 80.0 })
                    .default_size(default)
                    .show(ui, |ui| self.framework_edge(ui, edge, &members))
            };
            let size = if edge < 2 {
                response.response.rect.width()
            } else {
                response.response.rect.height()
            };
            self.layout.tools.saved.edge_sizes[edge] = size;
            if edge == 0 {
                self.layout.left_width = size;
            }
            if edge == 3 {
                self.layout.bottom_height = size;
            }
        }
        for id in ids {
            let placement = self.layout.tools.saved.placement(&id);
            match placement.mode {
                Mode::Undock => {
                    let mut open = true;
                    let bounds = self.layout.tools.saved.window_bounds.get(&id).copied();
                    let mut window = egui::Window::new(self.tool_display_title(&id))
                        .id(egui::Id::new(("undocked-tool", &id)))
                        .open(&mut open)
                        .default_size([640.0, 420.0]);
                    if self.layout.tools.apply_geometry {
                        if let Some([x, y, w, h]) = bounds {
                            window = window.current_pos(egui::pos2(x, y)).fixed_size([w, h]);
                        }
                    }
                    if let Some(size) = self.layout.tools.resize_undocked.remove(&id) {
                        window = window.fixed_size(size);
                    }
                    if let Some(response) =
                        window.show_modeless(ui.ctx(), |ui| self.framework_content(ui, &id))
                    {
                        let size = self
                            .layout
                            .tools
                            .rects
                            .get(&id)
                            .map(|(_, rect)| rect.size())
                            .unwrap_or(response.response.rect.size());
                        self.layout.tools.saved.window_bounds.insert(
                            id.clone(),
                            [
                                response.response.rect.left(),
                                response.response.rect.top(),
                                size.x,
                                size.y,
                            ],
                        );
                    }
                    if !open {
                        self.hide_framework_tool(&id);
                    }
                }
                Mode::Float | Mode::Window => {
                    let mut close = false;
                    let suppressed = self.native_tool_modal_suppressed();
                    let viewport = egui::ViewportId::from_hash_of(("native-tool", &id));
                    let builder = self.native_tool_builder(&id, suppressed);
                    ui.ctx()
                        .show_viewport_immediate(viewport, builder, |ctx, _| {
                            close = ctx.input(|input| input.viewport().close_requested());
                            if let Some(maximized) = ctx.input(|input| input.viewport().maximized) {
                                let requested =
                                    self.layout.tools.native_max_request.get(&id).copied();
                                if requested.is_none_or(|requested| requested == maximized) {
                                    self.layout.tools.native_max_request.remove(&id);
                                    if maximized {
                                        self.layout.tools.saved.window_maximized.insert(id.clone());
                                    } else {
                                        self.layout.tools.saved.window_maximized.remove(&id);
                                    }
                                }
                            }
                            if let Some((outer, inner)) = ctx.input(|input| {
                                input.viewport().outer_rect.zip(input.viewport().inner_rect)
                            }) {
                                self.layout.tools.saved.window_bounds.insert(
                                    id.clone(),
                                    [outer.left(), outer.top(), inner.width(), inner.height()],
                                );
                            }
                            egui::CentralPanel::default().show(ctx, |ui| {
                                if ui.input(|input| input.viewport().focused != Some(false))
                                    && self.component_menu_owner_available(false)
                                    && !egui::Popup::is_any_open(ui.ctx())
                                    && !self.keymap_sequence(ui, false)
                                {
                                    self.tool_window_native_keys(ui.ctx());
                                }
                                if !suppressed {
                                    self.framework_content(ui, &id);
                                }
                            });
                        });
                    if close {
                        self.hide_framework_tool(&id);
                    }
                }
                _ => {}
            }
        }
        self.layout.tools.apply_geometry = false;
        self.auto_hide_editor_return(ui.ctx());
    }
    fn framework_edge(&mut self, ui: &mut egui::Ui, edge: usize, members: &[String]) {
        let (first, second): (Vec<_>, Vec<_>) = members
            .iter()
            .cloned()
            .partition(|id| !self.layout.tools.saved.placement(id).slot.second());
        if !self.layout.tools.saved.side_by_side || first.is_empty() || second.is_empty() {
            self.framework_group(ui, members);
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let ratio = self.layout.tools.saved.split_ratios[edge];
        let (a, b, handle) = if edge < 2 {
            let y = rect.top() + rect.height() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), y - 3.0)),
                egui::Rect::from_min_max(egui::pos2(rect.left(), y + 3.0), rect.max),
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), y - 3.0),
                    egui::pos2(rect.right(), y + 3.0),
                ),
            )
        } else {
            let x = rect.left() + rect.width() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(x - 3.0, rect.bottom())),
                egui::Rect::from_min_max(egui::pos2(x + 3.0, rect.top()), rect.max),
                egui::Rect::from_min_max(
                    egui::pos2(x - 3.0, rect.top()),
                    egui::pos2(x + 3.0, rect.bottom()),
                ),
            )
        };
        let split = ui
            .interact(
                handle,
                egui::Id::new(("tool-slot-split", edge)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(if edge < 2 {
                egui::CursorIcon::ResizeVertical
            } else {
                egui::CursorIcon::ResizeHorizontal
            });
        if split.dragged() {
            let delta = if edge < 2 {
                split.drag_delta().y / rect.height()
            } else {
                split.drag_delta().x / rect.width()
            };
            self.layout.tools.saved.split_ratios[edge] = (ratio + delta).clamp(0.15, 0.85);
        }
        for (index, (rect, group)) in [(a, first), (b, second)].into_iter().enumerate() {
            ui.scope_builder(
                egui::UiBuilder::new()
                    .id_salt(("tool-slot", edge, index))
                    .max_rect(rect),
                |ui| {
                    ui.set_clip_rect(rect);
                    self.framework_group(ui, &group);
                },
            );
        }
        ui.allocate_rect(rect, egui::Sense::hover());
    }
    fn framework_group(&mut self, ui: &mut egui::Ui, members: &[String]) {
        let Some(first) = members.first() else {
            return;
        };
        let slot = self.layout.tools.saved.placement(first).slot;
        if !self.layout.tools.saved.group_tabs && members.len() > 1 {
            let height = ui.available_height() / members.len() as f32;
            for id in members {
                ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                    self.framework_content(ui, id)
                });
            }
            return;
        }
        let preferred = self
            .layout
            .tools
            .saved
            .selected
            .get(&slot)
            .filter(|id| members.contains(id))
            .cloned();
        let active = preferred
            .or_else(|| {
                self.layout
                    .tools
                    .focused
                    .clone()
                    .filter(|id| members.contains(id))
            })
            .unwrap_or_else(|| first.clone());
        if members.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for id in members {
                    let placement = self.layout.tools.saved.placement(id);
                    let mut name = self.tool_display_title(id);
                    if placement.pinned {
                        name = format!("📌 {name}");
                    }
                    let mut text = egui::RichText::new(name);
                    if let Some(color) = placement.color {
                        text = text.color(egui::Color32::from_rgb(color[0], color[1], color[2]));
                    }
                    if ui.selectable_label(*id == active, text).clicked() {
                        self.layout.tools.saved.selected.insert(slot, id.clone());
                        self.layout.tools.focused = Some(id.clone());
                    }
                }
            });
        }
        self.framework_content(ui, &active);
    }
    fn tool_display_title(&self, id: &str) -> String {
        let placement = self.layout.tools.saved.placement(id);
        if !placement.title.trim().is_empty() {
            placement.title
        } else {
            tool_ref_by_id(id)
                .map(|tool| crate::i18n::tr_static(title(tool)).to_string())
                .unwrap_or_else(|| id.into())
        }
    }
    fn framework_content(&mut self, ui: &mut egui::Ui, id: &str) {
        if !self.layout.tools.drawn.insert(id.into()) {
            return;
        }
        let Some(tool) = tool_ref_by_id(id) else {
            return;
        };
        let rect = ui.available_rect_before_wrap();
        self.layout
            .tools
            .rects
            .insert(id.into(), (ui.ctx().viewport_id(), rect));
        if ui.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        }) {
            self.layout.tools.focused = Some(id.into());
            self.layout.last_tool = Some(tool);
            self.focus_editor = false;
        }
        self.layout.tools.drawing = Some(id.into());
        ui.push_id(("tool-content", id), |ui| match tool {
            ToolRef::Ai => self.ai_tool_contents(ui),
            ToolRef::Learn => self.learn_tool_contents(ui),
            ToolRef::Left(tool) => {
                let previous = self.left_tool;
                self.left_tool = tool;
                self.left_tool_contents(ui);
                if self.left_tool == tool {
                    self.left_tool = previous;
                }
            }
            ToolRef::Bottom(tool) => {
                let previous = self.bottom;
                self.bottom = Some(tool);
                self.bottom_panel(ui);
                if self.bottom == Some(tool) {
                    self.bottom = previous;
                } else if self.bottom.is_none() {
                    self.layout.tools.saved.open.retain(|open| open != id);
                    if previous != Some(tool) {
                        self.bottom = previous;
                    }
                }
            }
        });
        self.layout.tools.drawing = None;
        if (tool == ToolRef::Ai && !self.ai_panel.visible)
            || (tool == ToolRef::Learn && !self.learn.visible)
        {
            self.layout.tools.saved.open.retain(|open| open != id);
        }
        if let Some(primary) = self.bottom {
            let id = tool_ref_id(ToolRef::Bottom(primary));
            self.floating_bottom = !self.layout.tools.saved.placement(&id).mode.docked();
            if matches!(
                self.layout.tools.saved.placement(&id).mode,
                Mode::Float | Mode::Window
            ) {
                self.bottom_maximized = self.layout.tools.saved.window_maximized.contains(&id);
            }
        }
    }
    pub(super) fn tool_window_gear(&mut self, ui: &mut egui::Ui, tool: ToolRef) {
        let id = tool_ref_id(tool);
        ui.horizontal(|ui| {
            ui.strong(self.tool_display_title(&id));
            ui.menu_button("⚙", |ui| {
                if ui
                    .button("Widen Tool Window")
                    .on_hover_text(self.keymap.shortcut_text(Action::WidenToolWindow))
                    .clicked()
                {
                    self.tool_window_action(Action::WidenToolWindow, ui.ctx());
                    ui.close();
                }
                match tool {
                    ToolRef::Left(LeftTool::Project) => {
                        let changed = ui
                            .checkbox(
                                &mut self.settings.editor.autoscroll_from_source,
                                "Autoscroll from source",
                            )
                            .changed()
                            | ui.checkbox(
                                &mut self.settings.editor.autoscroll_to_source,
                                "Autoscroll to source",
                            )
                            .changed();
                        if changed {
                            let _ = self.settings.save();
                        }
                    }
                    ToolRef::Left(LeftTool::Structure) => {
                        let changed = ui
                            .checkbox(
                                &mut self.settings.editor.structure.autoscroll_from_source,
                                "Autoscroll from source",
                            )
                            .changed()
                            | ui.checkbox(
                                &mut self.settings.editor.structure.autoscroll_to_source,
                                "Autoscroll to source",
                            )
                            .changed();
                        if changed {
                            let _ = self.settings.save();
                        }
                    }
                    _ => {}
                }
                let placement = self.layout.tools.saved.placement(&id);
                ui.menu_button("View Mode", |ui| {
                    for mode in Mode::ALL {
                        if ui
                            .selectable_label(placement.mode == mode, mode.label())
                            .clicked()
                        {
                            self.tool_mode(tool, mode);
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Move to", |ui| {
                    for slot in Slot::ALL {
                        if ui
                            .selectable_label(placement.slot == slot, slot.label())
                            .clicked()
                        {
                            self.layout.tools.saved.placement_mut(&id).slot = slot;
                            self.layout.tools.saved.selected.insert(slot, id.clone());
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Split and move", |ui| {
                    for slot in Slot::ALL {
                        if ui.button(slot.label()).clicked() {
                            for visible in self.framework_visible() {
                                if visible != id && !self.layout.tools.saved.open.contains(&visible)
                                {
                                    self.layout.tools.saved.open.push(visible);
                                }
                            }
                            self.layout.tools.saved.placement_mut(&id).slot = slot;
                            self.open_framework_tool(&id);
                            ui.close();
                        }
                    }
                });
                ui.checkbox(&mut self.layout.tools.saved.group_tabs, "Group tabs");
                ui.checkbox(
                    &mut self.layout.tools.saved.side_by_side,
                    "Side-by-side layout",
                );
                ui.checkbox(
                    &mut self.layout.tools.saved.wide,
                    "Widescreen tool window layout",
                );
                ui.checkbox(
                    &mut self.layout.tools.saved.placement_mut(&id).pinned,
                    "Pin tab",
                );
                ui.label("Rename tab");
                ui.text_edit_singleline(&mut self.layout.tools.saved.placement_mut(&id).title);
                let mut color = placement.color.unwrap_or([120, 160, 220]);
                if ui.color_edit_button_srgb(&mut color).changed() {
                    self.layout.tools.saved.placement_mut(&id).color = Some(color);
                }
                if ui.button("Reset tab color").clicked() {
                    self.layout.tools.saved.placement_mut(&id).color = None;
                }
                if ui.button("Close other tool tabs").clicked() {
                    for other in self.framework_visible() {
                        if other != id && !self.layout.tools.saved.placement(&other).pinned {
                            self.hide_framework_tool(&other);
                        }
                    }
                    ui.close();
                }
                if ui.button("Remove from Sidebar").clicked() {
                    self.layout.tools.saved.placement_mut(&id).sidebar = false;
                    self.hide_framework_tool(&id);
                    ui.close();
                }
                if ui.button("Maximize / Restore").clicked() {
                    self.focus_editor = false;
                    self.layout.tools.saved.maximized =
                        if self.layout.tools.saved.maximized.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id.clone())
                        };
                    ui.close();
                }
            });
            if ui
                .small_button("—")
                .on_hover_text("Hide tool window")
                .clicked()
            {
                self.hide_framework_tool(&id);
                self.focus_editor = true;
            }
        });
    }
    /// Window › Layouts: the layout actions (also in Find Action and the
    /// keymap) plus one Restore / Delete row per named layout.
    pub(super) fn tool_window_layout_menu(&mut self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        ui.menu_button("Layouts", |ui| {
            let keymap = self.keymap.clone();
            let mut item = |ui: &mut egui::Ui, action: Action| {
                let button =
                    egui::Button::new(action.text()).shortcut_text(keymap.shortcut_text(action));
                if ui.add(button).clicked() {
                    pending.push(action);
                    ui.close();
                }
            };
            item(ui, Action::StoreNewLayout);
            item(ui, Action::StoreDefaultLayout);
            item(ui, Action::RestoreDefaultLayout);
            item(ui, Action::RestoreFactoryDefaultLayout);
            let names: Vec<String> = self
                .settings
                .tool_window_layouts
                .named
                .keys()
                .cloned()
                .collect();
            if !names.is_empty() {
                ui.separator();
                item(ui, Action::RestoreNamedLayout);
                item(ui, Action::DeleteNamedLayout);
            }
            for name in names {
                ui.horizontal(|ui| {
                    if ui
                        .button(crate::i18n::tr_fmt(
                            "Apply / Restore {name}",
                            &[("name", &name)],
                        ))
                        .clicked()
                    {
                        self.apply_layout_choice(ChoiceKind::RestoreLayout, &name);
                        ui.close();
                    }
                    let label = crate::i18n::tr_fmt("Delete layout {name}", &[("name", &name)]);
                    let delete = ui.small_button("×").on_hover_text(&label);
                    delete
                        .ctx
                        .accesskit_node_builder(delete.id, |node| node.set_label(label));
                    if delete.clicked() {
                        self.apply_layout_choice(ChoiceKind::DeleteLayout, &name);
                    }
                });
            }
            ui.separator();
            if ui
                .checkbox(
                    &mut self.settings.tool_window_layouts.restore_on_startup,
                    "Open tool windows on startup state restore",
                )
                .changed()
            {
                let _ = self.settings.save();
            }
        });
        ui.menu_button("Tool Window Bars", |ui| {
            if ui
                .checkbox(
                    &mut self.settings.appearance.show_tool_window_bars,
                    "Show tool window stripes",
                )
                .changed()
            {
                let _ = self.settings.save();
            }
            ui.checkbox(
                &mut self.layout.tools.saved.side_by_side,
                "Side-by-side layout",
            );
            ui.checkbox(&mut self.layout.tools.saved.group_tabs, "Group tabs");
            ui.menu_button("Add to Sidebar / Show", |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.layout.tools.tool_filter)
                        .hint_text("Speed search tools…"),
                );
                for tool in all_tools() {
                    if !title(tool)
                        .to_lowercase()
                        .contains(&self.layout.tools.tool_filter.to_lowercase())
                    {
                        continue;
                    }
                    let id = tool_ref_id(tool);
                    if ui.button(title(tool)).clicked() {
                        self.layout.tools.saved.placement_mut(&id).sidebar = true;
                        self.open_framework_tool(&id);
                        ui.close();
                    }
                }
            });
        });
    }
    pub(super) fn tool_window_bars_settings(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Tool Window Bars", |ui| {
            ui.checkbox(
                &mut self.settings.appearance.show_tool_window_bars,
                "Show tool window stripes",
            );
            ui.checkbox(
                &mut self.layout.tools.saved.side_by_side,
                "Side-by-side layout",
            );
            ui.checkbox(&mut self.layout.tools.saved.group_tabs, "Group tabs");
            ui.checkbox(
                &mut self.settings.tool_window_layouts.restore_on_startup,
                "Restore tool windows on project startup",
            );
            ui.label("Move, split, pin and change view mode from a tool header's gear menu.");
        });
    }
    fn auto_hide_editor_return(&mut self, ctx: &egui::Context) {
        if !self.focus_editor
            || self.layout.tools.focused.is_none()
            || egui::Popup::is_any_open(ctx)
            || !self.component_menu_owner_available(false)
        {
            return;
        }
        for id in self.framework_visible() {
            if self.layout.tools.saved.placement(&id).mode == Mode::DockUnpinned {
                self.hide_framework_tool(&id);
            }
        }
    }
    pub(super) fn tool_window_frame_start(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.pointer.any_pressed())
            || egui::Popup::is_any_open(ctx)
            || !self.component_menu_owner_available(false)
        {
            return;
        }
        let Some(pos) = ctx.input(|input| input.pointer.interact_pos()) else {
            return;
        };
        let viewport = ctx.viewport_id();
        let active = self.framework_visible();
        for id in active {
            if self.layout.tools.saved.placement(&id).mode != Mode::DockUnpinned {
                continue;
            }
            if self
                .layout
                .tools
                .rects
                .get(&id)
                .is_some_and(|(owner, rect)| *owner == viewport && !rect.contains(pos))
                && !self
                    .layout
                    .tools
                    .stripes
                    .get(&id)
                    .is_some_and(|rect| rect.contains(pos))
            {
                self.hide_framework_tool(&id);
            }
        }
        if !self
            .layout
            .tools
            .rects
            .values()
            .any(|(owner, rect)| *owner == viewport && rect.contains(pos))
        {
            self.layout.tools.focused = None;
        }
    }
    pub(super) fn tool_notification_anchor(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        if !self.settings.appearance.show_tool_window_bars {
            return None;
        }
        // Only "Tool window balloon" groups are anchored to a stripe button.
        let notification = self
            .notifications
            .tool_window_balloon_ids()
            .last()
            .and_then(|id| self.notifications.get(*id))?;
        let name = notification
            .actions
            .iter()
            .find_map(|(_, action)| match action {
                crate::notifications::NotificationAction::ShowToolWindow(name) => Some(*name),
                _ => None,
            })
            .unwrap_or(match notification.group.as_str() {
                "Git" => "Git",
                "GitHub" => "Pull Requests",
                "Python Packages" | "Python Interpreter" => "Python Packages",
                _ => "Notifications",
            });
        let id =
            layout::bottom_tool_by_title(name).map(|tool| tool_ref_id(ToolRef::Bottom(tool)))?;
        if !self.tool_sidebar_visible(&id) {
            return None;
        }
        let rect = self.layout.tools.stripes.get(&id)?;
        let screen = ctx.content_rect();
        Some(egui::pos2(
            rect.left().min(screen.right() - 390.0).max(screen.left()),
            (rect.top() - 220.0).max(screen.top()),
        ))
    }
    fn focused_framework_tool(&self, ctx: &egui::Context) -> Option<String> {
        let candidate = self
            .layout
            .tools
            .drawing
            .clone()
            .or_else(|| self.layout.tools.focused.clone())
            .or_else(|| {
                ProjectTree::has_focus(ctx).then(|| tool_ref_id(ToolRef::Left(self.left_tool)))
            })?;
        if !self.framework_visible().contains(&candidate) {
            return None;
        }
        let (viewport, rect) = self.layout.tools.rects.get(&candidate)?;
        if *viewport != ctx.viewport_id() {
            return None;
        }
        if let Some(focus) = ctx.memory(|memory| memory.focused()) {
            if let Some(response) = ctx.read_response(focus) {
                if !rect.intersects(response.rect) {
                    return None;
                }
            }
        }
        Some(candidate)
    }
    pub(super) fn tool_window_action(&mut self, action: Action, ctx: &egui::Context) -> bool {
        use Action::*;
        let toggled = Self::framework_tool_for_action(action);
        if let Some(tool) = toggled {
            self.reveal_tool_window_content(tool);
            let id = tool_ref_id(tool);
            let slot = self.layout.tools.saved.placement(&id).slot;
            self.layout.tools.saved.selected.insert(slot, id.clone());
            if tool == ToolRef::Ai
                && self.ai_panel.visible
                && self.layout.tools.saved.open.contains(&id)
            {
                self.hide_framework_tool(&id);
                return true;
            }
            if self.layout.tools.saved.open.contains(&id)
                && self.focused_framework_tool(ctx).as_deref() == Some(id.as_str())
            {
                self.hide_framework_tool(&id);
                self.focus_editor = true;
                return true;
            }
        }
        let contextual = matches!(
            action,
            MoveElementLeft | MoveElementRight | VcsShowNextChangeMarker | VcsShowPrevChangeMarker
        );
        let (dx, dy) = match action {
            StretchToolWindowLeft | MoveElementLeft => (-1, 0),
            StretchToolWindowRight | MoveElementRight => (1, 0),
            StretchToolWindowUp | VcsShowPrevChangeMarker => (0, -1),
            StretchToolWindowDown | VcsShowNextChangeMarker => (0, 1),
            WidenToolWindow => (4, 0),
            _ => (0, 0),
        };
        let dedicated = matches!(
            action,
            StretchToolWindowLeft
                | StretchToolWindowRight
                | StretchToolWindowUp
                | StretchToolWindowDown
                | WidenToolWindow
                | HideActiveToolWindow
        );
        if !contextual
            && !dedicated
            && !matches!(action, ToggleToolWindowFloating | ToggleToolWindowMaximized)
        {
            return false;
        }
        let focused = self.focused_framework_tool(ctx);
        if contextual && focused.is_none() {
            return false;
        }
        let id = focused.or_else(|| {
            dedicated
                .then(|| self.layout.last_tool.map(tool_ref_id))
                .flatten()
        });
        let Some(id) = id else {
            if dedicated {
                self.set_status("Focus a tool window first");
                return true;
            }
            return false;
        };
        let Some(tool) = tool_ref_by_id(&id) else {
            return dedicated;
        };
        let placement = self.layout.tools.saved.placement(&id);
        match action {
            HideActiveToolWindow => {
                self.hide_framework_tool(&id);
                self.focus_editor = true;
            }
            ToggleToolWindowFloating => self.tool_mode(
                tool,
                if placement.mode.docked() {
                    Mode::Float
                } else {
                    Mode::DockPinned
                },
            ),
            ToggleToolWindowMaximized => {
                if matches!(placement.mode, Mode::Float | Mode::Window) {
                    if let Some((viewport, _)) = self.layout.tools.rects.get(&id) {
                        let maximized = !self.layout.tools.saved.window_maximized.contains(&id);
                        self.layout
                            .tools
                            .native_max_request
                            .insert(id.clone(), maximized);
                        if maximized {
                            self.layout.tools.saved.window_maximized.insert(id.clone());
                        } else {
                            self.layout.tools.saved.window_maximized.remove(&id);
                        }
                        ctx.send_viewport_cmd_to(
                            *viewport,
                            egui::ViewportCommand::Maximized(maximized),
                        );
                    }
                } else {
                    self.focus_editor = false;
                    self.layout.tools.saved.maximized =
                        if self.layout.tools.saved.maximized.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id.clone())
                        };
                }
            }
            _ => {
                if matches!(placement.mode, Mode::Float | Mode::Window | Mode::Undock) {
                    if let Some((viewport, rect)) = self.layout.tools.rects.get(&id).copied() {
                        let size = egui::vec2(
                            (rect.width() + dx as f32 * 40.0).clamp(240.0, 2000.0),
                            (rect.height() - dy as f32 * 40.0).clamp(160.0, 1600.0),
                        );
                        if placement.mode == Mode::Undock {
                            self.layout.tools.resize_undocked.insert(id, size);
                        } else {
                            ctx.send_viewport_cmd_to(
                                viewport,
                                egui::ViewportCommand::InnerSize(size),
                            );
                        }
                    }
                } else if action == WidenToolWindow && placement.slot.edge() >= 2 {
                    self.layout.tools.saved.wide = !self.layout.tools.saved.wide;
                } else {
                    self.stretch_framework_tool(placement.slot, dx, dy, ctx);
                }
            }
        }
        ctx.request_repaint();
        true
    }
    fn stretch_framework_tool(&mut self, slot: Slot, dx: i32, dy: i32, ctx: &egui::Context) {
        let edge = slot.edge();
        let delta = if edge < 2 {
            dx as f32 * if edge == 0 { 40.0 } else { -40.0 }
        } else {
            dy as f32 * if edge == 2 { 40.0 } else { -40.0 }
        };
        if delta == 0.0 {
            let direction = if edge < 2 { -(dy as f32) } else { dx as f32 };
            let ratio = &mut self.layout.tools.saved.split_ratios[edge];
            *ratio = (*ratio + direction * 0.05 * if slot.second() { -1.0 } else { 1.0 })
                .clamp(0.15, 0.85);
            return;
        }
        let current = match edge {
            0 => self.layout.left_width,
            3 => self.layout.bottom_height,
            _ => self.layout.tools.saved.edge_sizes[edge],
        };
        let available = if edge < 2 {
            ctx.content_rect().width() - 140.0
        } else {
            ctx.content_rect().height() - 80.0
        };
        let size = (current + delta).clamp(100.0, available.max(100.0));
        self.layout.tools.saved.edge_sizes[edge] = size;
        if edge == 0 {
            self.layout.left_width = size;
        }
        if edge == 3 {
            self.layout.bottom_height = size;
        }
        let id = egui::Id::new(match edge {
            0 => "project_panel",
            1 => "right_tools",
            2 => "top_tools",
            _ => "bottom_tools",
        });
        let mut rect = egui::PanelState::load(ctx, id)
            .map(|state| state.outer_rect)
            .unwrap_or_else(|| ctx.content_rect());
        if edge < 2 {
            rect.max.x = rect.min.x + size;
        } else {
            rect.max.y = rect.min.y + size;
        }
        ctx.data_mut(|data| data.insert_persisted(id, egui::PanelState { outer_rect: rect }));
    }
    fn tool_window_native_keys(&mut self, ctx: &egui::Context) {
        if !self.component_menu_owner_available(false) || egui::Popup::is_any_open(ctx) {
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(action) = self.keymap.lookup_key(key, physical_key, modifiers) else {
                continue;
            };
            if (Self::framework_tool_for_action(action).is_some()
                || matches!(
                    action,
                    Action::HideAllToolWindows
                        | Action::JumpToLastToolWindow
                        | Action::HideActiveToolWindow
                        | Action::ToggleToolWindowMaximized
                        | Action::ToggleToolWindowFloating
                        | Action::WidenToolWindow
                        | Action::StretchToolWindowLeft
                        | Action::StretchToolWindowRight
                        | Action::StretchToolWindowUp
                        | Action::StretchToolWindowDown
                        | Action::MoveElementLeft
                        | Action::MoveElementRight
                        | Action::VcsShowNextChangeMarker
                        | Action::VcsShowPrevChangeMarker
                ))
                && ctx.input_mut(|input| input.consume_key(modifiers, key))
            {
                self.handle_action(action, ctx);
            }
        }
    }
}

/// Window › Layouts actions (Find Action, keymap and the Layouts menu).
impl IdeApp {
    pub(super) fn tool_window_layout_action(&mut self, action: Action) {
        match action {
            Action::StoreNewLayout => self.dialog = Dialog::SaveLayout(String::new()),
            Action::StoreDefaultLayout => {
                self.settings.tool_window_layouts.default_layout = Some(self.tool_window_layout());
                let _ = self.settings.save();
                self.set_status(crate::i18n::tr_static(
                    "Current layout stored as the default layout",
                ));
            }
            Action::RestoreDefaultLayout => {
                let saved =
                    (self.settings.tool_window_layouts.default_layout.clone()).unwrap_or_default();
                self.restore_tool_window_layout(&saved);
            }
            Action::RestoreFactoryDefaultLayout => {
                self.restore_tool_window_layout(&Default::default())
            }
            Action::RestoreNamedLayout => self.open_layout_choice(ChoiceKind::RestoreLayout),
            Action::DeleteNamedLayout => self.open_layout_choice(ChoiceKind::DeleteLayout),
            _ => {}
        }
    }

    /// Save Current Layout As…: stores (or replaces) a named layout.
    pub(super) fn save_named_tool_window_layout(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let saved = self.tool_window_layout();
        self.settings
            .tool_window_layouts
            .named
            .insert(name.to_string(), saved);
        let _ = self.settings.save();
        self.set_status(crate::i18n::tr_fmt(
            "Layout '{name}' saved",
            &[("name", &name)],
        ));
    }

    /// The chooser of Restore Layout… / Delete Layout…, one row per named layout.
    fn open_layout_choice(&mut self, kind: ChoiceKind) {
        let rows: Vec<(String, String)> = self
            .settings
            .tool_window_layouts
            .named
            .keys()
            .map(|name| (name.clone(), String::new()))
            .collect();
        if rows.is_empty() {
            self.set_status(crate::i18n::tr_static(
                "No saved layouts (Window › Layouts › Save Current Layout As…)",
            ));
            return;
        }
        let title = if kind == ChoiceKind::DeleteLayout {
            crate::i18n::tr_static("Delete Layout")
        } else {
            crate::i18n::tr_static("Restore Layout")
        };
        self.choice = Some(ChoicePopup::new(kind, title.into(), rows));
    }

    /// Restores or deletes the named layout picked in a layout chooser or the menu.
    pub(super) fn apply_layout_choice(&mut self, kind: ChoiceKind, name: &str) {
        match kind {
            ChoiceKind::RestoreLayout => {
                if let Some(saved) = self.settings.tool_window_layouts.named.get(name).cloned() {
                    self.restore_tool_window_layout(&saved);
                }
            }
            ChoiceKind::DeleteLayout
                if self
                    .settings
                    .tool_window_layouts
                    .named
                    .remove(name)
                    .is_some() =>
            {
                let _ = self.settings.save();
                self.set_status(crate::i18n::tr_fmt(
                    "Layout '{name}' deleted",
                    &[("name", &name)],
                ));
            }
            _ => {}
        }
    }
}

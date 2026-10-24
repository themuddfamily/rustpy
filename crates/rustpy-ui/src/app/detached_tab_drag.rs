//! Stable-view drag-back authority. Screen geometry is supplied only by actual
//! rendered root targets/native viewport information; no path reopening occurs.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RootTabDropTarget {
    pub rect: egui::Rect,
    pub group: u8,
    pub before: Option<egui::Id>,
    pub after: bool,
}
#[derive(Clone, Debug, PartialEq)]
struct Scene(Vec<(egui::Id, rustpy_text::DocumentId, u8)>);
impl Scene {
    fn capture(app: &IdeApp) -> Self {
        Self(
            app.tabs
                .iter()
                .map(|tab| (tab.view.id, tab.state.doc.identity(), tab.group))
                .collect(),
        )
    }
}
/// All coordinate authority from one actual native viewport input frame.
#[derive(Clone, Copy)]
pub(super) struct SourceGeometry {
    viewport: egui::ViewportId,
    source_inner: egui::Rect,
    content_origin: egui::Pos2,
    current_root_inner: Option<egui::Rect>,
}

#[derive(Clone, Copy)]
struct Drag {
    view: egui::Id,
    document: rustpy_text::DocumentId,
    viewport: egui::ViewportId,
    origin: egui::Pos2,
    moved: bool,
}
struct RootTargets {
    inner: egui::Rect,
    pixels_per_point: f32,
    scene: Scene,
    targets: Vec<RootTabDropTarget>,
}
#[derive(Default)]
pub(super) struct DetachedTabDrag {
    drag: Option<Drag>,
    root: Option<RootTargets>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReturnDrop {
    pub view: egui::Id,
    document: rustpy_text::DocumentId,
    pub group: u8,
    pub before: Option<egui::Id>,
    pub after: bool,
}
fn valid_rect(rect: egui::Rect) -> bool {
    rect.min.x.is_finite()
        && rect.min.y.is_finite()
        && rect.max.x.is_finite()
        && rect.max.y.is_finite()
        && rect.width() > 0.0
        && rect.height() > 0.0
}
fn screen_position(
    inner: egui::Rect,
    content_origin: egui::Pos2,
    local: egui::Pos2,
) -> Option<egui::Pos2> {
    let point = inner.min + (local - content_origin);
    (valid_rect(inner) && point.x.is_finite() && point.y.is_finite()).then_some(point)
}
impl DetachedTabDrag {
    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub(super) fn cancel(&mut self) {
        self.drag = None;
    }

    /// Call at the start of a root strip paint pass; all visible group targets
    /// from that same pass are appended below. Clearing prevents old geometry use.
    pub(super) fn begin_root_pass(
        &mut self,
        app: &IdeApp,
        inner: egui::Rect,
        pixels_per_point: f32,
    ) {
        self.root = valid_rect(inner).then(|| RootTargets {
            inner,
            pixels_per_point,
            scene: Scene::capture(app),
            targets: Vec::new(),
        });
    }
    pub(super) fn add_root_target(&mut self, target: RootTabDropTarget) {
        if let Some(root) = &mut self.root {
            if target.group < 2 && valid_rect(target.rect) && root.inner.intersects(target.rect) {
                root.targets.push(target);
            }
        }
    }
    /// Begin only from the real detached-header primary drag-start response.
    pub(super) fn begin(
        &mut self,
        app: &IdeApp,
        viewport: egui::ViewportId,
        source_inner: egui::Rect,
        content_origin: egui::Pos2,
        local: egui::Pos2,
        owner: bool,
    ) -> bool {
        self.cancel();
        let Some(index) = app.detached else {
            return false;
        };
        let Some(tab) = app.tabs.get(index) else {
            return false;
        };
        if !owner || viewport != app.detached_viewport_id(index) {
            return false;
        }
        let Some(origin) = screen_position(source_inner, content_origin, local) else {
            return false;
        };
        self.drag = Some(Drag {
            view: tab.view.id,
            document: tab.state.doc.identity(),
            viewport,
            origin,
            moved: false,
        });
        true
    }
    /// Observe physical events in the source viewport (which can retain native
    /// pointer capture across OS windows). Release resolves one measured target.
    /// `current_root_inner` comes from this input's ROOT ViewportInfo, not cached bounds.
    pub(super) fn event(
        &mut self,
        app: &IdeApp,
        geometry: SourceGeometry,
        owner: bool,
        event: &egui::Event,
    ) -> Option<ReturnDrop> {
        let SourceGeometry {
            viewport,
            source_inner,
            content_origin,
            current_root_inner,
        } = geometry;
        let drag = self.drag?;
        let current = app.detached.and_then(|index| app.tabs.get(index));
        if !owner
            || current.is_none_or(|tab| {
                tab.view.id != drag.view || tab.state.doc.identity() != drag.document
            })
        {
            self.cancel();
            return None;
        }
        if viewport != drag.viewport {
            return None;
        }
        match event {
            egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                ..
            }
            | egui::Event::WindowFocused(false) => {
                self.cancel();
                None
            }
            egui::Event::PointerMoved(local) => {
                if let Some(point) = screen_position(source_inner, content_origin, *local) {
                    self.drag.as_mut().unwrap().moved |= point.distance(drag.origin) >= 6.0;
                } else {
                    self.cancel();
                }
                None
            }
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                ..
            } => {
                // Take before validation: duplicate releases never apply twice.
                let drag = self.drag.take().unwrap();
                let point = screen_position(source_inner, content_origin, *pos)?;
                if !drag.moved {
                    return None;
                }
                let root = self.root.as_ref()?;
                if current_root_inner != Some(root.inner)
                    || !root.inner.contains(point)
                    || root.scene != Scene::capture(app)
                {
                    return None;
                }
                let target = root
                    .targets
                    .iter()
                    .rev()
                    .find(|target| target.rect.contains(point))?;
                if target.before == Some(drag.view) {
                    return None;
                }
                Some(ReturnDrop {
                    view: drag.view,
                    document: drag.document,
                    group: target.group,
                    before: target.before,
                    after: target.after,
                })
            }
            _ => None,
        }
    }
}
impl IdeApp {
    pub(super) fn detached_drag_owner(&self, ctx: &egui::Context) -> bool {
        self.editor_keyboard()
            && !self.coordinated_quit_active()
            && !self.find_in_files.visible
            && !self.completion.visible
            && self.quick_doc.is_none()
            && self.intentions.is_none()
            && self.inplace_rename.is_none()
            && !egui::Popup::is_any_open(ctx)
    }
    /// Resolve every destination against current IDs before delegating existing
    /// move/reattach lifecycle. No replacement Document or fresh EditorView.
    pub(super) fn apply_detached_return(&mut self, drop: ReturnDrop, ctx: &egui::Context) -> bool {
        if !self.detached_drag_owner(ctx) || drop.group > 1 {
            return false;
        }
        let Some(index) = self.detached else {
            return false;
        };
        if self
            .tabs
            .get(index)
            .is_none_or(|tab| tab.view.id != drop.view || tab.state.doc.identity() != drop.document)
        {
            return false;
        }
        let target = match drop.before {
            Some(id) => {
                let Some(index) = self.tabs.iter().position(|tab| {
                    tab.view.id == id && tab.group == drop.group && id != drop.view
                }) else {
                    return false;
                };
                Some(index)
            }
            None => None,
        };
        self.move_tab(drop.view, target, drop.after, drop.group);
        self.reattach_editor();
        true
    }
}

impl DetachedTabDrag {
    pub(super) fn register_root_rect(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        id: egui::Id,
        group: u8,
        vertical: bool,
        detached: bool,
    ) {
        if ui.ctx().viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let Some(root) = &self.root else { return };
        let rect = rect.intersect(ui.clip_rect());
        if !valid_rect(rect) {
            return;
        }
        let delta = root.inner.min - ui.ctx().viewport_rect().min;
        let screen = rect.translate(delta);
        let mut first = screen;
        let mut second = screen;
        if vertical {
            first.max.y = screen.center().y;
            second.min.y = screen.center().y;
        } else {
            first.max.x = screen.center().x;
            second.min.x = screen.center().x;
        }
        let before = (!detached).then_some(id);
        self.add_root_target(RootTabDropTarget {
            rect: first,
            group,
            before,
            after: false,
        });
        self.add_root_target(RootTabDropTarget {
            rect: second,
            group,
            before,
            after: true,
        });
    }
}
impl IdeApp {
    pub(super) fn begin_detached_root_targets(&mut self, ctx: &egui::Context) {
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let inner = ctx.input(|input| input.viewport().inner_rect);
        let mut drag = std::mem::take(&mut self.detached_tab_drag);
        if let Some(inner) = inner {
            drag.begin_root_pass(self, inner, ctx.pixels_per_point());
        } else {
            drag = DetachedTabDrag::default();
        }
        self.detached_tab_drag = drag;
    }
    pub(super) fn detached_drag_header(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
    ) -> (egui::Response, Option<ReturnDrop>) {
        let title = self.tabs[index].state.title();
        let full = self.tabs[index]
            .state
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| title.clone());
        let response = ui
            .push_id(("detached-tab-header", self.tabs[index].view.id), |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&title).color(self.theme.text_dim))
                        .sense(egui::Sense::click_and_drag()),
                )
                .on_hover_text(&full)
            })
            .inner;
        crate::a11y::button(&response, &format!("Drag {title} back to editor tabs"));
        let ctx = ui.ctx();
        let viewport = ctx.viewport_id();
        let (source_inner, root_inner, origin, events) = ctx.input(|input| {
            (
                input.viewport().inner_rect,
                input
                    .raw
                    .viewports
                    .get(&egui::ViewportId::ROOT)
                    .and_then(|info| info.inner_rect),
                input.pointer.press_origin(),
                input.events.clone(),
            )
        });
        let owner = self.detached_drag_owner(ctx);
        let mut gesture = std::mem::take(&mut self.detached_tab_drag);
        let mut returned = None;
        if let Some(source_inner) = source_inner.filter(|_| {
            gesture
                .root
                .as_ref()
                .is_some_and(|root| root.pixels_per_point == ctx.pixels_per_point())
        }) {
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(origin) = origin {
                    gesture.begin(
                        self,
                        viewport,
                        source_inner,
                        ctx.viewport_rect().min,
                        origin,
                        owner,
                    );
                }
            }
            for event in &events {
                if let Some(drop) = gesture.event(
                    self,
                    SourceGeometry {
                        viewport,
                        source_inner,
                        content_origin: ctx.viewport_rect().min,
                        current_root_inner: root_inner,
                    },
                    owner,
                    event,
                ) {
                    returned = Some(drop);
                }
            }
        } else {
            gesture.cancel();
        }
        self.detached_tab_drag = gesture;
        if self.detached_tab_drag.is_dragging() {
            ctx.request_repaint();
        }
        (response, returned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("π🙂 = 1\n", Language::Python));
        app.handle_action(Action::SplitVertically, &egui::Context::default());
        app.handle_action(Action::DetachTab, &egui::Context::default());
        app
    }
    fn rect(x: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(300.0, 300.0))
    }
    fn start(app: &IdeApp, gesture: &mut DetachedTabDrag) -> egui::ViewportId {
        gesture.begin_root_pass(app, rect(0.0), 1.0);
        gesture.add_root_target(RootTabDropTarget {
            rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 30.0)),
            group: 0,
            before: Some(app.tabs[0].view.id),
            after: true,
        });
        let viewport = app.detached_viewport_id(app.detached.unwrap());
        assert!(gesture.begin(
            app,
            viewport,
            rect(400.0),
            egui::Pos2::ZERO,
            egui::pos2(10.0, 10.0),
            true
        ));
        viewport
    }
    fn release(
        app: &IdeApp,
        gesture: &mut DetachedTabDrag,
        viewport: egui::ViewportId,
        owner: bool,
    ) -> Option<ReturnDrop> {
        gesture.event(
            app,
            SourceGeometry {
                viewport,
                source_inner: rect(400.0),
                content_origin: egui::Pos2::ZERO,
                current_root_inner: Some(rect(0.0)),
            },
            owner,
            &egui::Event::PointerMoved(egui::pos2(-350.0, 10.0)),
        );
        gesture.event(
            app,
            SourceGeometry {
                viewport,
                source_inner: rect(400.0),
                content_origin: egui::Pos2::ZERO,
                current_root_inner: Some(rect(0.0)),
            },
            owner,
            &egui::Event::PointerButton {
                pos: egui::pos2(-350.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        )
    }
    #[test]
    fn source_captured_release_returns_same_shared_editor_history_and_selection_once() {
        let mut app = app();
        let ctx = egui::Context::default();
        let index = app.detached.unwrap();
        let id = app.tabs[index].view.id;
        let doc = app.tabs[index].state.doc.identity();
        app.tabs[index]
            .state
            .insert_text("changed_β", &crate::editor::EditOptions::default());
        let carets = app.tabs[index].state.carets.clone();
        let mut gesture = DetachedTabDrag::default();
        let viewport = start(&app, &mut gesture);
        let drop = release(&app, &mut gesture, viewport, true).unwrap();
        assert!(app.apply_detached_return(drop, &ctx));
        assert!(app.detached.is_none());
        let tab = &app.tabs[app.active.unwrap()];
        assert_eq!(tab.view.id, id);
        assert_eq!(tab.state.doc.identity(), doc);
        assert_eq!(tab.state.carets.as_slice(), carets.as_slice());
        assert!(tab.state.doc.is_modified());
        assert!(tab.state.doc.can_undo());
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        assert!(release(&app, &mut gesture, viewport, true).is_none());
    }
    #[test]
    fn wrong_viewport_cancel_modal_and_stale_scene_never_reattach() {
        let mut app = app();
        let mut gesture = DetachedTabDrag::default();
        let viewport = start(&app, &mut gesture);
        assert!(release(&app, &mut gesture, egui::ViewportId::ROOT, true).is_none());
        assert!(app.detached.is_some());
        gesture.cancel();
        assert!(release(&app, &mut gesture, viewport, true).is_none());
        start(&app, &mut gesture);
        assert!(release(&app, &mut gesture, viewport, false).is_none());
        start(&app, &mut gesture);
        app.tabs.swap(0, 1);
        app.detached = Some(0);
        assert!(release(&app, &mut gesture, viewport, true).is_none());
        assert!(app.detached.is_some());
    }
    #[test]
    fn stale_editor_and_moved_root_geometry_are_refused() {
        let mut app = app();
        let mut gesture = DetachedTabDrag::default();
        let viewport = start(&app, &mut gesture);
        app.detached = None;
        assert!(release(&app, &mut gesture, viewport, true).is_none());
        app.detached = Some(1);
        start(&app, &mut gesture);
        gesture.event(
            &app,
            SourceGeometry {
                viewport,
                source_inner: rect(400.0),
                content_origin: egui::Pos2::ZERO,
                current_root_inner: Some(rect(20.0)),
            },
            true,
            &egui::Event::PointerMoved(egui::pos2(-350.0, 10.0)),
        );
        assert!(gesture
            .event(
                &app,
                SourceGeometry {
                    viewport,
                    source_inner: rect(400.0),
                    content_origin: egui::Pos2::ZERO,
                    current_root_inner: Some(rect(20.0))
                },
                true,
                &egui::Event::PointerButton {
                    pos: egui::pos2(-350.0, 10.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE
                }
            )
            .is_none());
        assert!(app.detached.is_some());
    }
    #[test]
    fn actual_rendered_header_drag_uses_root_strip_geometry_and_returns_same_view() {
        let mut app = app();
        let index = app.detached.unwrap();
        let id = app.tabs[index].view.id;
        let doc = app.tabs[index].state.doc.identity();
        app.tabs[index].state.doc.set_read_only(true);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let source = app.detached_viewport_id(index);
        fn frame(
            app: &mut IdeApp,
            ctx: &egui::Context,
            viewport: egui::ViewportId,
            source: egui::ViewportId,
            events: Vec<egui::Event>,
        ) -> Option<egui::Rect> {
            let mut input = egui::RawInput {
                viewport_id: viewport,
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 300.0),
                )),
                events,
                ..Default::default()
            };
            input.viewports.insert(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    inner_rect: Some(rect(0.0)),
                    native_pixels_per_point: Some(1.0),
                    ..Default::default()
                },
            );
            input.viewports.insert(
                source,
                egui::ViewportInfo {
                    parent: Some(egui::ViewportId::ROOT),
                    inner_rect: Some(rect(400.0)),
                    native_pixels_per_point: Some(1.0),
                    ..Default::default()
                },
            );
            let mut header = None;
            let mut applied = None;
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    if viewport == egui::ViewportId::ROOT {
                        app.editor_area(ui);
                    } else {
                        let index = app.detached.unwrap();
                        let (response, drop) = app.detached_drag_header(ui, index);
                        header = Some(response.rect);
                        if let Some(drop) = drop {
                            applied = Some(app.apply_detached_return(drop, ctx));
                        }
                    }
                });
            });
            output.drop_without_applying_deltas();
            if let Some(applied) = applied {
                assert!(applied);
            }
            header
        }
        frame(&mut app, &ctx, egui::ViewportId::ROOT, source, vec![]);
        let root_target = app
            .detached_tab_drag
            .root
            .as_ref()
            .unwrap()
            .targets
            .iter()
            .find(|target| target.before == Some(app.tabs[0].view.id))
            .unwrap()
            .rect
            .center();
        let header = frame(&mut app, &ctx, source, source, vec![])
            .unwrap()
            .center();
        frame(
            &mut app,
            &ctx,
            source,
            source,
            vec![
                egui::Event::PointerMoved(header),
                egui::Event::PointerButton {
                    pos: header,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let captured = root_target - egui::vec2(400.0, 0.0);
        frame(
            &mut app,
            &ctx,
            source,
            source,
            vec![egui::Event::PointerMoved(captured)],
        );
        assert!(
            app.detached_tab_drag.is_dragging(),
            "real header response arms the drag"
        );
        frame(
            &mut app,
            &ctx,
            source,
            source,
            vec![egui::Event::PointerButton {
                pos: captured,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(app.detached.is_none());
        assert_eq!(app.tabs[app.active.unwrap()].view.id, id);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.identity(), doc);
        assert!(app.tabs[app.active.unwrap()].state.doc.is_read_only());
        assert!(!app.tabs[app.active.unwrap()].state.doc.can_undo());
    }
    #[test]
    fn lone_detached_editor_returns_to_its_actual_empty_root_destination() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("only_π = 1", Language::Python));
        let id = app.tabs[0].view.id;
        app.handle_action(Action::DetachTab, &egui::Context::default());
        let mut gesture = DetachedTabDrag::default();
        gesture.begin_root_pass(&app, rect(0.0), 1.0);
        gesture.add_root_target(RootTabDropTarget {
            rect: rect(0.0),
            group: 0,
            before: None,
            after: false,
        });
        let viewport = app.detached_viewport_id(0);
        assert!(gesture.begin(
            &app,
            viewport,
            rect(400.0),
            egui::Pos2::ZERO,
            egui::pos2(10.0, 10.0),
            true
        ));
        let drop = release(&app, &mut gesture, viewport, true).unwrap();
        assert!(app.apply_detached_return(drop, &egui::Context::default()));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].view.id, id);
        assert!(app.detached.is_none());
    }
}

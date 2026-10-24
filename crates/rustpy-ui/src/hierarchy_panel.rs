//! Hierarchy tool window: type (supertypes / subtypes), method and call hierarchies.

use crate::a11y::AccessibleCombo;
use std::collections::HashSet;
use std::path::PathBuf;

use egui::{Color32, Sense, Vec2};
use rustpy_core::TextRange;
use rustpy_semantic::hierarchy::HierarchyNode;

use crate::theme::UiTheme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HierarchyKind {
    Both,
    Supertypes,
    Subtypes,
    Method,
    Callers,
    Callees,
}

impl HierarchyKind {
    pub fn title(self) -> &'static str {
        match self {
            HierarchyKind::Both => "Type Hierarchy",
            HierarchyKind::Supertypes => "Supertypes",
            HierarchyKind::Subtypes => "Subtypes",
            HierarchyKind::Method => "Method Hierarchy",
            HierarchyKind::Callers => "Callers",
            HierarchyKind::Callees => "Callees",
        }
    }
}

pub enum HierarchyEvent {
    Navigate(PathBuf, TextRange),
    /// Expand a call-hierarchy node lazily: `(path, range, kind)`.
    Expand(PathBuf, TextRange, HierarchyKind),
    SwitchKind(HierarchyKind),
    Refresh,
    Rebase(PathBuf, TextRange),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HierarchyScope {
    #[default]
    Project,
    Directory,
    File,
}
#[derive(Clone)]
struct HistoryEntry {
    kind: HierarchyKind,
    title: String,
    roots: Vec<HierarchyNode>,
    origin: Option<(PathBuf, TextRange)>,
    scope: HierarchyScope,
    pinned: bool,
}

#[derive(Default)]
pub struct HierarchyPanel {
    pub origin: Option<(PathBuf, TextRange)>,
    pub scope: HierarchyScope,
    pub pinned: bool,
    query: String,
    selected: Option<(PathBuf, TextRange)>,
    follow_source: bool,
    select_only: bool,
    preview: bool,
    group_files: bool,
    scroll_target: Option<(PathBuf, TextRange)>,
    history: Vec<HistoryEntry>,
    rendered_scope: HierarchyScope,
    accessibility_blocked: bool,
    tree_epoch: u64,
    pub kind: Option<HierarchyKind>,
    pub root_title: String,
    pub roots: Vec<HierarchyNode>,
    pub collapsed: HashSet<(PathBuf, TextRange)>,
    pub expanded_calls: HashSet<(PathBuf, TextRange)>,
    pub message: Option<String>,
}

impl HierarchyPanel {
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
    }

    pub fn set(&mut self, kind: HierarchyKind, title: String, roots: Vec<HierarchyNode>) {
        if let Some(previous) = self.kind {
            self.history.push(HistoryEntry {
                kind: previous,
                title: self.root_title.clone(),
                roots: self.roots.clone(),
                origin: self.origin.clone(),
                scope: self.rendered_scope,
                pinned: self.pinned,
            });
            if self.history.len() > 32 {
                if let Some(index) = self.history.iter().position(|entry| !entry.pinned) {
                    self.history.remove(index);
                }
            }
        }
        self.rendered_scope = self.scope;
        self.pinned = false;
        self.tree_epoch = crate::app::window_lifecycle::next_process_owner();
        self.kind = Some(kind);
        self.root_title = title;
        self.roots = roots;
        self.collapsed.clear();
        self.expanded_calls.clear();
        self.message = None;
    }

    /// Replaces the children of the node identified by `(path, range)`.
    pub fn set_children(&mut self, path: &PathBuf, range: TextRange, children: Vec<HierarchyNode>) {
        fn walk(
            nodes: &mut [HierarchyNode],
            path: &PathBuf,
            range: TextRange,
            children: &mut Option<Vec<HierarchyNode>>,
        ) {
            for n in nodes.iter_mut() {
                if &n.path == path && n.range == range {
                    if let Some(c) = children.take() {
                        n.children = c;
                    }
                    return;
                }
                walk(&mut n.children, path, range, children);
            }
        }
        let mut c = Some(children);
        walk(&mut self.roots, path, range, &mut c);
        self.expanded_calls.insert((path.clone(), range));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) -> Vec<HierarchyEvent> {
        let mut events = Vec::new();
        let Some(kind) = self.kind else {
            ui.label(egui::RichText::new("Hierarchy: use Navigate → Type Hierarchy (Ctrl+H), Method Hierarchy (Ctrl+Shift+H) or Call Hierarchy (Ctrl+Alt+H) on a class or function.").color(theme.text_dim));
            return events;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&self.root_title).strong());
            ui.separator();
            let group: &[HierarchyKind] = match kind {
                HierarchyKind::Both | HierarchyKind::Supertypes | HierarchyKind::Subtypes => &[
                    HierarchyKind::Both,
                    HierarchyKind::Subtypes,
                    HierarchyKind::Supertypes,
                ],
                HierarchyKind::Callers | HierarchyKind::Callees => {
                    &[HierarchyKind::Callers, HierarchyKind::Callees]
                }
                HierarchyKind::Method => &[HierarchyKind::Method],
            };
            for k in group {
                if ui.selectable_label(*k == kind, k.title()).clicked() && *k != kind {
                    events.push(HierarchyEvent::SwitchKind(*k));
                }
            }
            if let Some(m) = &self.message {
                ui.label(egui::RichText::new(m).color(theme.text_dim));
            }
        });
        let mut restore = None;
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.pinned, "Pin this hierarchy");
            egui::ComboBox::from_id_salt("hierarchy_history")
                .selected_text("Hierarchy history")
                .show_ui(ui, |ui| {
                    for (index, entry) in self.history.iter().enumerate().rev() {
                        if ui
                            .selectable_label(
                                false,
                                format!("{}{}", if entry.pinned { "📌 " } else { "" }, entry.title),
                            )
                            .clicked()
                        {
                            restore = Some(index);
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Hierarchy history"));
            let before = self.scope;
            egui::ComboBox::from_id_salt("hierarchy_scope")
                .selected_text(format!("Scope: {:?}", self.scope))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.scope, HierarchyScope::Project, "Project");
                    ui.selectable_value(
                        &mut self.scope,
                        HierarchyScope::Directory,
                        "Base directory",
                    );
                    ui.selectable_value(&mut self.scope, HierarchyScope::File, "Base file");
                })
                .combo_named(crate::i18n::tr_static("Scope"));
            if before != self.scope || ui.button("Refresh").clicked() {
                events.push(HierarchyEvent::Refresh);
            }
        });
        if let Some(index) = restore {
            let entry = self.history.remove(index);
            self.set(entry.kind, entry.title, entry.roots);
            self.origin = entry.origin;
            self.scope = entry.scope;
            self.rendered_scope = entry.scope;
            self.pinned = entry.pinned;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Speed search hierarchy"));
            ui.checkbox(&mut self.follow_source, "Autoscroll from source");
            let mut follow = !self.select_only;
            ui.checkbox(&mut follow, "Autoscroll to source");
            self.select_only = !follow;
            ui.checkbox(&mut self.preview, "Preview");
            ui.checkbox(&mut self.group_files, "Group roots by file");
            if ui.button("Expand all").clicked() {
                self.collapsed.clear();
            }
            if ui.button("Collapse all").clicked() {
                fn keys(nodes: &[HierarchyNode], out: &mut HashSet<(PathBuf, TextRange)>) {
                    for node in nodes {
                        out.insert((node.path.clone(), node.range));
                        keys(&node.children, out);
                    }
                }
                keys(&self.roots, &mut self.collapsed);
            }
        });
        ui.separator();
        let mut roots = filtered(&self.roots, &self.query);
        if self.group_files {
            roots.sort_by(|a, b| a.path.cmp(&b.path).then(a.name.cmp(&b.name)));
        }

        egui::ScrollArea::both()
            .id_salt("hierarchy_tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if roots.is_empty() {
                    ui.label(egui::RichText::new("Nothing found").color(theme.text_dim));
                }
                let mut file = None;
                for (index, r) in roots.iter().enumerate() {
                    if self.group_files && file != Some(&r.path) {
                        file = Some(&r.path);
                        ui.strong(r.path.display().to_string());
                    }
                    ui.push_id(index, |ui| self.node_ui(ui, theme, r, 0, kind, &mut events));
                }
            });
        self.scroll_target = None;
        events
    }
    pub(crate) fn preview_location(&self) -> Option<(PathBuf, TextRange)> {
        self.preview.then(|| self.selected.clone()).flatten()
    }
    pub(crate) fn follow_editor(&mut self, path: &std::path::Path, offset: rustpy_core::TextSize) {
        if !self.follow_source || self.pinned {
            return;
        }
        type LocatedNode = ((PathBuf, TextRange), Vec<(PathBuf, TextRange)>);
        fn locate(
            nodes: &[HierarchyNode],
            path: &std::path::Path,
            offset: rustpy_core::TextSize,
            parents: &mut Vec<(PathBuf, TextRange)>,
            best: &mut Option<LocatedNode>,
        ) {
            for node in nodes {
                let key = (node.path.clone(), node.range);
                parents.push(key.clone());
                if node.path == path
                    && node.range.start() <= offset
                    && best
                        .as_ref()
                        .is_none_or(|(previous, _)| previous.1.start() < node.range.start())
                {
                    *best = Some((key, parents.clone()));
                }
                locate(&node.children, path, offset, parents, best);
                parents.pop();
            }
        }
        let mut best = None;
        locate(&self.roots, path, offset, &mut Vec::new(), &mut best);
        if let Some((key, parents)) = best {
            if self.selected.as_ref() != Some(&key) {
                for parent in parents {
                    self.collapsed.remove(&parent);
                }
                self.selected = Some(key.clone());
                self.scroll_target = Some(key);
            }
        }
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        node: &HierarchyNode,
        depth: usize,
        kind: HierarchyKind,
        events: &mut Vec<HierarchyEvent>,
    ) {
        let key = (node.path.clone(), node.range);
        let lazy = matches!(kind, HierarchyKind::Callers | HierarchyKind::Callees);
        let can_expand = !node.children.is_empty() || (lazy && !self.expanded_calls.contains(&key));
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().max(300.0), 22.0),
            Sense::hover(),
        );
        let id = ui
            .id()
            .with(("hierarchy-node", self.tree_epoch, kind.title(), &key));
        let resp = ui.interact(rect, id, Sense::click());
        if self.scroll_target.as_ref() == Some(&key) {
            resp.scroll_to_me(Some(egui::Align::Center));
        }
        if self.selected.as_ref() == Some(&key) {
            ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
        }
        resp.context_menu(|ui| {
            if ui.button("Base hierarchy on this symbol").clicked() {
                events.push(HierarchyEvent::Rebase(node.path.clone(), node.range));
                ui.close();
            }
        });
        let available = !self.accessibility_blocked;
        let label = format!(
            "{}. {}. {}",
            node.name.chars().take(160).collect::<String>(),
            node.detail.chars().take(160).collect::<String>(),
            node.path.display()
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, available, &label));
        let (left, right) = if available && resp.has_focus() && !egui::Popup::is_any_open(ui.ctx())
        {
            crate::a11y::announce(
                ui.ctx(),
                id,
                &format!("{label}. Enter opens source, Left collapses, Right expands."),
            );
            ui.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                )
            })
        } else {
            (false, false)
        };
        if can_expand && left {
            self.collapsed.insert(key.clone());
        }
        if can_expand && right {
            if lazy && node.children.is_empty() && !self.expanded_calls.contains(&key) {
                events.push(HierarchyEvent::Expand(node.path.clone(), node.range, kind));
            } else {
                self.collapsed.remove(&key);
            }
        }

        let collapsed = self.query.trim().is_empty() && self.collapsed.contains(&key);
        if resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
        }
        let x = rect.left() + 6.0 + depth as f32 * 16.0;
        let y = rect.center().y;
        if can_expand {
            ui.painter().text(
                egui::Pos2::new(x, y),
                egui::Align2::LEFT_CENTER,
                if collapsed || (lazy && node.children.is_empty()) {
                    "▶"
                } else {
                    "▼"
                },
                egui::FontId::proportional(9.0),
                theme.text_dim,
            );
        }
        let icon_color = if theme.high_contrast {
            theme.text
        } else {
            match kind {
                HierarchyKind::Both | HierarchyKind::Supertypes | HierarchyKind::Subtypes => {
                    Color32::from_rgb(0x4A, 0x88, 0xC7)
                }
                _ => Color32::from_rgb(0x59, 0xA8, 0x69),
            }
        };
        let icon_rect =
            egui::Rect::from_center_size(egui::Pos2::new(x + 20.0, y), Vec2::splat(12.0));
        ui.painter()
            .circle_filled(icon_rect.center(), 5.0, icon_color);
        ui.painter().text(
            icon_rect.center(),
            egui::Align2::CENTER_CENTER,
            if matches!(kind, HierarchyKind::Supertypes | HierarchyKind::Subtypes) {
                "C"
            } else {
                "f"
            },
            egui::FontId::proportional(9.0),
            if theme.high_contrast {
                theme.panel_bg
            } else {
                Color32::WHITE
            },
        );
        let name_galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                node.name.clone(),
                egui::FontId::proportional(13.0),
                theme.text,
            )
        });
        ui.painter().galley(
            egui::Pos2::new(x + 32.0, y - name_galley.size().y / 2.0),
            name_galley.clone(),
            theme.text,
        );
        if !node.detail.is_empty() {
            ui.painter().text(
                egui::Pos2::new(x + 32.0 + name_galley.size().x + 10.0, y),
                egui::Align2::LEFT_CENTER,
                &node.detail,
                egui::FontId::proportional(12.0),
                theme.text_dim,
            );
        }
        if available && resp.clicked() {
            let on_arrow = resp.clicked_by(egui::PointerButton::Primary)
                && ui.input(|i| i.pointer.interact_pos().is_some_and(|p| p.x < x + 14.0));
            if can_expand && on_arrow {
                if lazy && node.children.is_empty() && !self.expanded_calls.contains(&key) {
                    events.push(HierarchyEvent::Expand(node.path.clone(), node.range, kind));
                } else if collapsed {
                    self.collapsed.remove(&key);
                } else {
                    self.collapsed.insert(key.clone());
                }
            } else {
                self.selected = Some(key.clone());
                if !self.select_only || resp.double_clicked() {
                    events.push(HierarchyEvent::Navigate(node.path.clone(), node.range));
                }
            }
        }
        if available && resp.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            events.push(HierarchyEvent::Navigate(node.path.clone(), node.range));
        }
        if available
            && resp.double_clicked()
            && lazy
            && node.children.is_empty()
            && !self.expanded_calls.contains(&key)
        {
            events.push(HierarchyEvent::Expand(node.path.clone(), node.range, kind));
        }
        // Capture model state after this frame's activation, not AccessKit's
        // child links (which are populated independently during tree building).
        let collapsed = self.query.trim().is_empty() && self.collapsed.contains(&key);
        let expanded = !collapsed && !(lazy && node.children.is_empty());
        // AccessKit's builder holds the Context write lock. Read focus first;
        // Response::has_focus accesses that same Context and cannot run inside.
        let focused = resp.has_focus();
        ui.ctx().accesskit_node_builder(id, |accessible_node| {
            accessible_node.set_role(egui::accesskit::Role::TreeItem);
            accessible_node.set_selected(focused);
            if can_expand {
                accessible_node.set_expanded(expanded);
            }
        });
        if !collapsed {
            for (index, c) in node.children.iter().enumerate() {
                ui.push_id((id, index), |ui| {
                    self.node_ui(ui, theme, c, depth + 1, kind, events)
                });
            }
        }
    }
}

fn filtered(nodes: &[HierarchyNode], query: &str) -> Vec<HierarchyNode> {
    nodes
        .iter()
        .filter_map(|node| {
            let mut found = node.clone();
            if crate::project_tree::speed_search_matches(
                &format!("{} {} {}", node.name, node.detail, node.path.display()),
                query,
            ) {
                return Some(found);
            }
            found.children = filtered(&node.children, query);
            (!found.children.is_empty()).then_some(found)
        })
        .collect()
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    use rustpy_core::TextSize;

    #[test]
    fn populated_lazy_model_announces_current_expansion_after_each_key() {
        let range = TextRange::new(TextSize::new(0), TextSize::new(2));
        let child = HierarchyNode {
            name: "childπ".into(),
            path: PathBuf::from("π.py"),
            range: TextRange::new(TextSize::new(3), TextSize::new(5)),
            detail: String::new(),
            children: vec![],
        };
        let root = HierarchyNode {
            name: "rootπ".into(),
            path: PathBuf::from("π.py"),
            range,
            detail: String::new(),
            children: vec![child],
        };
        let mut panel = HierarchyPanel::default();
        panel.set(HierarchyKind::Callers, "Callers".into(), vec![root]);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let theme = UiTheme::high_contrast();
        let mut paint = |events| {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(600.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        panel.ui(ui, &theme);
                    });
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        };
        let first = paint(vec![]);
        let (id, node) = first
            .nodes
            .iter()
            .find(|(_, node)| {
                node.label()
                    .is_some_and(|label| label.starts_with("rootπ."))
            })
            .unwrap();
        assert_eq!(
            node.is_expanded(),
            Some(true),
            "populated lazy model is expanded before AccessKit child links exist"
        );
        let id = *id;
        // Find the actual registered response through its node ID on real Tab.
        let mut reached = false;
        let mut cycle_start = None;
        // Traverse a real focus cycle: toolbar controls precede the lazy tree.
        // The cap guards against a broken cycle, without assuming its length.
        for _ in 0..128 {
            paint(key(egui::Key::Tab));
            let focused = ctx.memory(|memory| memory.focused());
            if focused.is_some_and(|focus| focus.accesskit_id() == id) {
                reached = true;
                break;
            }
            if let Some(focused) = focused {
                if cycle_start == Some(focused) {
                    break;
                }
                cycle_start.get_or_insert(focused);
            }
        }
        assert!(reached, "a complete Tab cycle must reach the lazy root row");
        let collapsed = paint(key(egui::Key::ArrowLeft));
        assert_eq!(
            collapsed
                .nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .unwrap()
                .1
                .is_expanded(),
            Some(false)
        );
        let expanded = paint(key(egui::Key::ArrowRight));
        assert_eq!(
            expanded
                .nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .unwrap()
                .1
                .is_expanded(),
            Some(true)
        );
    }
    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn node(name: &str) -> HierarchyNode {
        HierarchyNode {
            name: name.into(),
            path: PathBuf::from(format!("{name}.py")),
            range: TextRange::empty(0.into()),
            detail: String::new(),
            children: Vec::new(),
        }
    }

    #[test]
    fn pinned_history_preserves_origin_and_scope_when_a_new_hierarchy_opens() {
        let mut panel = HierarchyPanel {
            scope: HierarchyScope::Directory,
            ..Default::default()
        };
        panel.set(HierarchyKind::Both, "Base".into(), vec![node("Base")]);
        panel.origin = Some((PathBuf::from("base.py"), TextRange::empty(7.into())));
        panel.scope = HierarchyScope::Directory;
        panel.pinned = true;
        panel.set(HierarchyKind::Callees, "Other".into(), vec![node("Other")]);
        let entry = &panel.history[0];
        assert!(entry.pinned);
        assert_eq!(entry.scope, HierarchyScope::Directory);
        assert_eq!(entry.origin.as_ref().unwrap().0, PathBuf::from("base.py"));
        assert_eq!(entry.kind, HierarchyKind::Both);
        assert_eq!(entry.roots[0].name, "Base");
        assert!(!panel.pinned);
    }
}

#[cfg(test)]
mod workspace_fixtures {
    use super::*;
    fn node(name: &str, start: u32, children: Vec<HierarchyNode>) -> HierarchyNode {
        HierarchyNode {
            name: name.into(),
            path: "/p/code.py".into(),
            range: TextRange::new(start.into(), (start + name.len() as u32).into()),
            detail: "method".into(),
            children,
        }
    }
    #[test]
    fn speed_search_keeps_ancestors_and_source_follow_expands_current_symbol() {
        let mut panel = HierarchyPanel {
            roots: vec![node("Parent", 0, vec![node("selected_child", 20, vec![])])],
            ..Default::default()
        };
        let results = filtered(&panel.roots, "selected_child");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].children[0].name, "selected_child");
        panel.follow_source = true;
        panel
            .collapsed
            .insert((panel.roots[0].path.clone(), panel.roots[0].range));
        panel.follow_editor(std::path::Path::new("/p/code.py"), 35.into());
        assert_eq!(panel.selected.as_ref().unwrap().1.start(), 20.into());
        assert!(panel.collapsed.is_empty());
        panel.pinned = true;
        panel.follow_editor(std::path::Path::new("/p/code.py"), 0.into());
        assert_eq!(panel.selected.as_ref().unwrap().1.start(), 20.into());
    }
}

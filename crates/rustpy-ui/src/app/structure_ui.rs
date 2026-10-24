//! Shared physical outline and inherited-member model for all three consumers.
use super::*;
use crate::a11y::DialogWindow;
use crate::editor::syntax_support::{StructureItem, StructureKind as K};
use rustpy_semantic::{classes::MemberKind, infer::Inferer};
use std::collections::HashSet;

type Key = (PathBuf, TextRange, Option<usize>);
#[derive(Clone)]
struct Row {
    item: StructureItem,
    path: PathBuf,
    inherited: bool,
    parent: Option<usize>,
}
impl Row {
    fn key(&self) -> Key {
        (self.path.clone(), self.item.name_range, self.parent)
    }
}
#[derive(Default)]
pub(super) struct State {
    query: String,
    pinned: Option<(PathBuf, egui::Id)>,
    preview: bool,
    pub(super) focus_popup: bool,
    pub(super) focus_tool: bool,
    selected: Option<Key>,
    selected_focus: Option<(egui::Context, egui::Id)>,
    caret: Option<(rustpy_text::DocumentId, u64, TextSize)>,
    collapsed: HashSet<Key>,
    cache: Option<(rustpy_text::DocumentId, u64, u64, bool, Vec<Row>)>,
    cache_owner: Option<std::sync::Weak<Analysis>>,
}
/// The accessible identity and hover description always retain the complete
/// cached signature, including when optional visual details are hidden.
fn structure_row_label(
    ui: &mut egui::Ui,
    row: &Row,
    selected: bool,
    indent: bool,
    details: bool,
) -> egui::Response {
    let prefix = format!(
        "{}{} {}",
        if indent {
            "  ".repeat(row.item.depth)
        } else {
            String::new()
        },
        structure_badge(row.item.kind).0,
        row.item.name
    );
    let full = format!("{prefix}{}", row.item.detail);
    let response = ui.selectable_label(selected, if details { full.clone() } else { prefix });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::TreeItem);
        node.set_selected(selected);
        node.set_label(full.clone());
    });
    response.on_hover_text(full)
}

fn kind_rank(kind: K) -> u8 {
    match kind {
        K::Class => 0,
        K::Function => 1,
        K::Method => 2,
        K::Variable => 3,
        K::Import => 4,
    }
}
fn visibility(name: &str) -> u8 {
    if name.starts_with("__") && !name.ends_with("__") {
        2
    } else if name.starts_with('_') {
        1
    } else {
        0
    }
}
fn allowed(row: &Row, options: &rustpy_settings::StructureSettings) -> bool {
    (options.show_fields || row.item.kind != K::Variable)
        && (options.show_imports || row.item.kind != K::Import)
        && (options.show_inherited || !row.inherited)
        && (options.show_dunder
            || !(row.item.name.starts_with("__") && row.item.name.ends_with("__")))
}
fn visible(
    rows: &[Row],
    options: &rustpy_settings::StructureSettings,
    query: &str,
    collapsed: &HashSet<Key>,
) -> Vec<usize> {
    let mut keep = vec![false; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        if allowed(row, options) && crate::project_tree::speed_search_matches(&row.item.name, query)
        {
            keep[index] = true;
            let mut parent = row.parent;
            while let Some(index) = parent {
                keep[index] = true;
                parent = rows[index].parent;
            }
        }
    }
    struct OutlineTraversal<'a> {
        tree: &'a [Vec<usize>],
        rows: &'a [Row],
        keep: &'a [bool],
        options: &'a rustpy_settings::StructureSettings,
        query: &'a str,
        collapsed: &'a HashSet<Key>,
    }
    impl OutlineTraversal<'_> {
        fn append(&self, parent: Option<usize>, out: &mut Vec<usize>) {
            let Self {
                tree,
                rows,
                keep,
                options,
                query,
                collapsed,
            } = self;
            let mut children: Vec<_> = tree[parent.unwrap_or(rows.len())]
                .iter()
                .copied()
                .filter(|index| keep[*index])
                .collect();
            children.sort_by(|a, b| {
                let (a, b) = (&rows[*a], &rows[*b]);
                let group = if options.group_by_kind {
                    kind_rank(a.item.kind).cmp(&kind_rank(b.item.kind))
                } else {
                    std::cmp::Ordering::Equal
                };
                let access = if options.sort_visibility {
                    visibility(&a.item.name).cmp(&visibility(&b.item.name))
                } else {
                    std::cmp::Ordering::Equal
                };
                group.then(access).then_with(|| {
                    if options.sort_alphabetically {
                        a.item.name.to_lowercase().cmp(&b.item.name.to_lowercase())
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
            });
            for index in children {
                out.push(index);
                if !query.is_empty() || !collapsed.contains(&rows[index].key()) {
                    self.append(Some(index), out);
                }
            }
        }
    }
    let mut tree = vec![Vec::new(); rows.len() + 1];
    for (index, row) in rows.iter().enumerate() {
        tree[row.parent.unwrap_or(rows.len())].push(index);
    }
    let mut out = Vec::new();
    OutlineTraversal {
        tree: &tree,
        rows,
        keep: &keep,
        options,
        query,
        collapsed,
    }
    .append(None, &mut out);
    out
}
fn controls(ui: &mut egui::Ui, options: &mut rustpy_settings::StructureSettings) {
    ui.menu_button("View options", |ui| {
        ui.checkbox(&mut options.show_fields, "Show fields");
        ui.checkbox(&mut options.show_inherited, "Show inherited members");
        ui.checkbox(&mut options.show_imports, "Show imports");
        ui.checkbox(&mut options.show_dunder, "Show dunder members");
        ui.separator();
        ui.checkbox(&mut options.sort_alphabetically, "Sort alphabetically");
        ui.checkbox(&mut options.sort_visibility, "Sort by visibility");
        ui.checkbox(&mut options.group_by_kind, "Group by kind");
        ui.separator();
        ui.checkbox(
            &mut options.autoscroll_from_source,
            "Autoscroll from source",
        );
        ui.checkbox(&mut options.autoscroll_to_source, "Autoscroll to source");
        ui.checkbox(
            &mut options.show_members_in_project,
            "Show Members in Project View",
        );
    });
}
impl IdeApp {
    fn structure_rows(&mut self) -> Vec<Row> {
        let pinned = self.structure_ui.pinned.as_ref().and_then(|(root, id)| {
            self.project
                .as_ref()
                .filter(|project| &project.root == root)?;
            self.tabs.iter().position(|tab| tab.view.id == *id)
        });
        let Some(index) = pinned.or(self.active) else {
            return Vec::new();
        };
        let state = &self.tabs[index].state;
        let Some(path) = state.path.clone() else {
            return Vec::new();
        };
        let identity = state.doc.identity();
        let version = state.doc.version();
        let revision = self
            .project
            .as_ref()
            .map_or(0, |p| p.analysis.revision_excluding(&path));
        let inherited = self.settings.editor.structure.show_inherited;
        let same_owner = match (&self.structure_ui.cache_owner, self.project.as_ref()) {
            (Some(owner), Some(project)) => owner
                .upgrade()
                .is_some_and(|owner| Arc::ptr_eq(&owner, &project.analysis)),
            (None, None) => true,
            _ => false,
        };
        if let Some((id, v, r, base, rows)) = &self.structure_ui.cache {
            if same_owner
                && *id == identity
                && *v == version
                && *r == revision
                && *base == inherited
            {
                return rows.clone();
            }
        }
        let raw = self.tabs[index].state.structure();
        let mut rows: Vec<Row> = Vec::new();
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for item in raw {
            while stack.last().is_some_and(|(depth, _)| *depth >= item.depth) {
                stack.pop();
            }
            let parent = stack.last().map(|(_, index)| *index);
            let depth = item.depth;
            let index = rows.len();
            rows.push(Row {
                item,
                path: path.clone(),
                inherited: false,
                parent,
            });
            stack.push((depth, index));
        }
        if self.tabs[index].state.language == Language::Python && rows.len() < 16384 {
            self.sync_analysis_now();
            if let Some((db, file)) = self.analysis_file(index) {
                let inf = Inferer::new(&db);
                let own = rows.len();
                for parent in 0..own {
                    if rows[parent].item.kind != K::Class {
                        continue;
                    }
                    let Some(class) = file
                        .class_declarations()
                        .iter()
                        .find(|class| class.range == rows[parent].item.range)
                    else {
                        continue;
                    };
                    let mut seen: HashSet<String> = rows
                        .iter()
                        .filter(|row| row.parent == Some(parent))
                        .map(|row| row.item.name.clone())
                        .collect();
                    let bases = inf.mro(class);
                    for base in bases.iter().take(if inherited { 64 } else { 1 }) {
                        let Some(info) = inf.class_info(base) else {
                            continue;
                        };
                        for (name, member) in &info.members {
                            if rows.len() >= 16384 {
                                break;
                            }
                            if !seen.insert(name.to_string()) {
                                continue;
                            }
                            let Some(owner) = db.file(member.file) else {
                                continue;
                            };
                            let kind = match member.kind {
                                MemberKind::Method
                                | MemberKind::StaticMethod
                                | MemberKind::ClassMethod
                                | MemberKind::Property => K::Method,
                                MemberKind::NestedClass => K::Class,
                                _ => K::Variable,
                            };
                            rows.push(Row {
                                item: StructureItem {
                                    name: name.to_string(),
                                    detail: if base == class {
                                        String::new()
                                    } else {
                                        format!(" (inherited from {})", base.name)
                                    },
                                    kind,
                                    range: member.range,
                                    name_range: member.range,
                                    depth: rows[parent].item.depth + 1,
                                    line: owner.text
                                        [..usize::from(member.range.start()).min(owner.text.len())]
                                        .bytes()
                                        .filter(|b| *b == b'\n')
                                        .count(),
                                },
                                path: owner.path.clone(),
                                inherited: base != class,
                                parent: Some(parent),
                            });
                        }
                    }
                }
            }
        }
        self.structure_ui.cache_owner = self
            .project
            .as_ref()
            .map(|project| Arc::downgrade(&project.analysis));
        self.structure_ui.cache = Some((identity, version, revision, inherited, rows.clone()));
        rows
    }
    pub(super) fn jump_to_selected_structure(&mut self) -> bool {
        if self.left_tool != LeftTool::Structure || !self.layout.left_visible {
            return false;
        }
        if !self
            .structure_ui
            .selected_focus
            .as_ref()
            .is_some_and(|(context, id)| context.memory(|memory| memory.has_focus(*id)))
        {
            return false;
        }
        let selected = self.structure_ui.selected.clone();
        let rows = self.structure_rows();
        if let Some(row) = rows.iter().find(|row| Some(row.key()) == selected) {
            self.structure_jump(row, true);
        } else {
            self.set_status("Structure selection is no longer current");
        }
        true
    }
    fn structure_jump(&mut self, row: &Row, focus: bool) {
        self.navigate_to(&row.path, row.item.name_range);
        self.focus_editor = focus;
    }
    /// [`IdeApp::structure_rows`] for the panel, which draws every frame:
    /// while typing in a large file it keeps the rows of the previous version
    /// of the document (rebuilding them pushes the text into the analysis and
    /// infers each class's members on the UI thread) until the typing pauses.
    fn structure_rows_for_view(&mut self, ctx: &egui::Context) -> Vec<Row> {
        let now = ctx.input(|i| i.time);
        let quiet = now - self.last_edit_time;
        if (0.0..=super::analysis_jobs::DEBOUNCE).contains(&quiet) {
            let state = self.active.and_then(|i| self.tabs.get(i)).map(|t| &t.state);
            if let (Some(state), Some((id, _, _, _, rows))) = (state, &self.structure_ui.cache) {
                if self.structure_ui.pinned.is_none()
                    && *id == state.doc.identity()
                    && usize::from(state.doc.len()) >= super::analysis_jobs::BACKGROUND_SYNC_BYTES
                {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        super::analysis_jobs::DEBOUNCE - quiet + 0.01,
                    ));
                    return rows.clone();
                }
            }
        }
        self.structure_rows()
    }
    pub(super) fn structure_view(&mut self, ui: &mut egui::Ui) {
        let rows = self.structure_rows_for_view(ui.ctx());
        if self.active.is_none() {
            ui.label("Nothing to show");
            return;
        }
        ui.horizontal(|ui| {
            controls(ui, &mut self.settings.editor.structure);
            let mut pin = self.structure_ui.pinned.is_some();
            if ui.checkbox(&mut pin, "Pin file").changed() {
                self.structure_ui.pinned = if pin {
                    self.active.and_then(|index| {
                        Some((
                            self.project.as_ref()?.root.clone(),
                            self.tabs[index].view.id,
                        ))
                    })
                } else {
                    None
                };
            }
            ui.checkbox(&mut self.structure_ui.preview, "Preview");
            if ui.small_button("Expand all").clicked() {
                self.structure_ui.collapsed.clear();
            }
            if ui.small_button("Collapse all").clicked() {
                self.structure_ui.collapsed = rows
                    .iter()
                    .filter_map(|row| row.parent)
                    .map(|index| rows[index].key())
                    .collect();
            }
        });
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.structure_ui.query).hint_text("Speed search"),
        );
        if self.structure_ui.focus_tool {
            search.request_focus();
            self.structure_ui.focus_tool = false;
        }
        let query = self.structure_ui.query.clone();
        let current = self.active.and_then(|i| {
            let caret = self.tabs[i].state.primary().head();
            let path = self.tabs[i].state.path.as_ref();
            rows.iter()
                .filter(|row| {
                    !row.inherited && Some(&row.path) == path && row.item.range.contains(caret)
                })
                .max_by_key(|row| row.item.depth)
                .map(Row::key)
        });
        let stamp = self.active.map(|i| {
            let state = &self.tabs[i].state;
            (
                state.doc.identity(),
                state.doc.version(),
                state.primary().head(),
            )
        });
        let follow = self.structure_ui.pinned.is_none()
            && self.settings.editor.structure.autoscroll_from_source
            && self.structure_ui.caret != stamp;
        self.structure_ui.caret = stamp;
        if follow && !search.has_focus() {
            self.structure_ui.selected = current;
        }
        if follow && !search.has_focus() {
            if let Some(index) = rows
                .iter()
                .position(|row| Some(row.key()) == self.structure_ui.selected)
            {
                let mut parent = rows[index].parent;
                while let Some(index) = parent {
                    self.structure_ui.collapsed.remove(&rows[index].key());
                    parent = rows[index].parent;
                }
            }
        }
        let indices = visible(
            &rows,
            &self.settings.editor.structure,
            &query,
            &self.structure_ui.collapsed,
        );
        let mut jump = None;
        if search.has_focus() {
            let mut selected = indices
                .iter()
                .position(|index| Some(rows[*index].key()) == self.structure_ui.selected)
                .unwrap_or(0);
            let (down, up, enter) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                )
            });
            if down {
                selected = (selected + 1).min(indices.len().saturating_sub(1));
            }
            if up {
                selected = selected.saturating_sub(1);
            }
            if let Some(index) = indices.get(selected) {
                if down || up || enter {
                    self.structure_ui.selected = Some(rows[*index].key());
                    self.structure_ui.selected_focus = Some((ui.ctx().clone(), search.id));
                    if enter || self.settings.editor.structure.autoscroll_to_source {
                        jump = Some((rows[*index].clone(), enter));
                    }
                }
            }
        }
        let parents: HashSet<_> = rows.iter().filter_map(|row| row.parent).collect();
        egui::ScrollArea::vertical()
            .id_salt("structure_view")
            .show(ui, |ui| {
                let mut previous = None;
                for index in indices {
                    let row = &rows[index];
                    if self.settings.editor.structure.group_by_kind
                        && previous != Some((row.item.depth, row.item.kind))
                    {
                        ui.label(egui::RichText::new(format!("{:?}", row.item.kind)).strong());
                        previous = Some((row.item.depth, row.item.kind));
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(12.0 * row.item.depth as f32);
                        let has_children = parents.contains(&index);
                        if has_children {
                            if ui
                                .small_button(if self.structure_ui.collapsed.contains(&row.key()) {
                                    "▸"
                                } else {
                                    "▾"
                                })
                                .clicked()
                                && !self.structure_ui.collapsed.remove(&row.key())
                            {
                                self.structure_ui.collapsed.insert(row.key());
                            }
                        } else {
                            ui.add_space(18.0);
                        }
                        let selected = self.structure_ui.selected.as_ref() == Some(&row.key());
                        let response = structure_row_label(
                            ui,
                            row,
                            selected,
                            false,
                            self.settings.appearance.show_tree_details,
                        );
                        if response.clicked() {
                            response.request_focus();
                            self.structure_ui.selected_focus =
                                Some((ui.ctx().clone(), response.id));
                            self.focus_editor = false;
                            self.structure_ui.selected = Some(row.key());
                            if self.settings.editor.structure.autoscroll_to_source {
                                jump = Some((row.clone(), false));
                            }
                        }
                        if response.double_clicked() {
                            jump = Some((row.clone(), true));
                        }
                        if selected && follow && !search.has_focus() {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                    });
                }
            });
        if self.structure_ui.preview {
            if let Some(row) = rows
                .iter()
                .find(|row| self.structure_ui.selected.as_ref() == Some(&row.key()))
            {
                let line = self
                    .tabs
                    .iter()
                    .find(|tab| tab.state.path.as_ref() == Some(&row.path))
                    .map(|tab| tab.state.doc.line_of(row.item.name_range.start()))
                    .unwrap_or(0);
                self.tool_source_preview(ui, &row.path, line);
            }
        }
        if let Some((row, focus)) = jump {
            self.structure_jump(&row, focus);
        }
    }
    pub(super) fn file_structure_popup(
        &mut self,
        ctx: &egui::Context,
        mut query: String,
        mut selected: usize,
    ) {
        let rows = self.structure_rows();
        let mut open = true;
        let mut close = false;
        let mut jump = None;
        egui::Window::new("File Structure")
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 480.0])
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                controls(ui, &mut self.settings.editor.structure);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .hint_text("Type to filter / camel humps")
                        .desired_width(f32::INFINITY),
                );
                if self.structure_ui.focus_popup {
                    response.request_focus();
                    self.structure_ui.focus_popup = false;
                }
                if response.changed() {
                    selected = 0;
                }
                let indices = visible(
                    &rows,
                    &self.settings.editor.structure,
                    &query,
                    &HashSet::new(),
                );
                selected = selected.min(indices.len().saturating_sub(1));
                let (down, up, enter, escape) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    )
                });
                if down {
                    selected = (selected + 1).min(indices.len().saturating_sub(1));
                }
                if up {
                    selected = selected.saturating_sub(1);
                }
                if escape {
                    close = true;
                }
                if enter {
                    jump = indices.get(selected).map(|index| rows[*index].clone());
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (position, index) in indices.iter().enumerate() {
                        let row = &rows[*index];
                        let response = structure_row_label(
                            ui,
                            row,
                            position == selected,
                            true,
                            self.settings.appearance.show_tree_details,
                        );
                        if position == selected {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            jump = Some(row.clone());
                        }
                    }
                });
            });
        if let Some(row) = jump {
            self.structure_jump(&row, true);
        } else if open && !close {
            self.dialog = Dialog::FileStructure { query, selected };
        } else {
            self.focus_editor = true;
        }
    }
    pub(super) fn project_members_view(&mut self, ui: &mut egui::Ui) {
        if !self.settings.editor.structure.show_members_in_project {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let path = self.tabs[index].state.path.clone();
        if path.as_ref() != self.project.as_ref().and_then(|p| p.tree.selected.as_ref()) {
            return;
        }
        let rows = self.structure_rows();
        ui.collapsing("File members", |ui| {
            let mut jump = None;
            egui::ScrollArea::vertical()
                .id_salt("project_file_members")
                .max_height(180.0)
                .show(ui, |ui| {
                    for index in
                        visible(&rows, &self.settings.editor.structure, "", &HashSet::new())
                    {
                        let row = &rows[index];
                        if structure_row_label(
                            ui,
                            row,
                            false,
                            true,
                            self.settings.appearance.show_tree_details,
                        )
                        .clicked()
                        {
                            jump = Some(row.clone());
                        }
                    }
                });
            if let Some(row) = jump {
                self.structure_jump(&row, true);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        app.open_file(&path);
        (directory, app, path)
    }
    #[test]
    fn tree_details_action_is_canonical_persisted_and_legacy_defaults_preserve_signatures() {
        let legacy: rustpy_settings::AppearanceSettings = serde_json::from_str("{}").unwrap();
        assert!(legacy.show_tree_details);
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        let ctx = egui::Context::default();
        app.handle_action(Action::ToggleTreeDetails, &ctx);
        assert!(!app.settings.appearance.show_tree_details);
        let stored = serde_json::to_string(&app.settings.appearance).unwrap();
        let restored: rustpy_settings::AppearanceSettings = serde_json::from_str(&stored).unwrap();
        assert!(!restored.show_tree_details);
        app.handle_action(Action::ToggleTreeDetails, &ctx);
        assert!(app.settings.appearance.show_tree_details);
    }
    #[test]
    fn structure_details_hide_visual_signature_but_preserve_selected_accessible_identity() {
        let row = Row {
            item: StructureItem {
                name: "convert".into(),
                detail: "(value: int) -> str".into(),
                kind: K::Function,
                range: TextRange::new(0.into(), 50.into()),
                name_range: TextRange::new(4.into(), 11.into()),
                depth: 0,
                line: 0,
            },
            path: PathBuf::from("/cached/main.py"),
            inherited: false,
            parent: None,
        };
        let full = format!(
            "{} convert(value: int) -> str",
            structure_badge(K::Function).0
        );
        let plain = format!("{} convert", structure_badge(K::Function).0);
        for details in [true, false] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                structure_row_label(ui, &row, true, false, details);
            });
            let shapes = std::mem::take(&mut output.shapes);
            let accessibility = output.platform_output.accesskit_update.take();
            output.drop_without_applying_deltas();
            let accessibility = accessibility.expect("structure accessibility update");
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()==if details {&full}else{&plain})));
            assert!(accessibility
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some(full.as_str())
                    && node.is_selected() == Some(true)));
        }
        assert_eq!(row.key(), (row.path.clone(), row.item.name_range, None));
    }

    #[test]
    fn shared_model_has_import_aliases_instance_fields_and_physical_inherited_targets() {
        let source="import os as operating\nclass Base:\n    def inherited(self): pass\n    def replace(self): pass\nclass Child(Base):\n    class Nested: pass\n    class_value:int=1\n    def __init__(this):\n        this.instance_value=1\n    def replace(self): pass\n";
        let (_directory, mut app, path) = app(source);
        app.settings.editor.structure.show_inherited = true;
        let rows = app.structure_rows();
        let import = rows
            .iter()
            .find(|row| row.item.name == "operating")
            .unwrap();
        assert_eq!(import.item.kind, K::Import);
        assert_eq!(&source[import.item.name_range], "operating");
        assert!(rows.iter().any(|row| row.item.name == "instance_value"
            && row.item.kind == K::Variable
            && !row.inherited));
        let child = rows
            .iter()
            .position(|row| row.item.name == "Child")
            .unwrap();
        let inherited = rows
            .iter()
            .find(|row| row.item.name == "inherited" && row.parent == Some(child))
            .unwrap();
        assert!(inherited.inherited);
        assert_eq!(inherited.path, path);
        assert_eq!(&source[inherited.item.name_range], "inherited");
        assert_eq!(
            rows.iter()
                .filter(|row| row.item.name == "replace" && row.parent == Some(child))
                .count(),
            1
        );
        app.structure_jump(inherited, true);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.primary().range(),
            inherited.item.name_range
        );
    }
    #[test]
    fn filters_sorting_speed_search_and_expansion_preserve_tree_ancestry() {
        let source="import os\nclass Example:\n    z=1\n    __private=2\n    _protected=3\n    a=4\n    def readValue(self): pass\n    def __str__(self): return ''\n";
        let (_directory, mut app, _) = app(source);
        let rows = app.structure_rows();
        let class = rows
            .iter()
            .position(|row| row.item.name == "Example")
            .unwrap();
        let mut options = rustpy_settings::StructureSettings {
            show_fields: false,
            show_dunder: false,
            ..Default::default()
        };
        let names: Vec<_> = visible(&rows, &options, "", &HashSet::new())
            .iter()
            .map(|index| rows[*index].item.name.as_str())
            .collect();
        assert_eq!(names, ["Example", "readValue"]);
        options.show_fields = true;
        options.sort_visibility = true;
        options.sort_alphabetically = true;
        let names: Vec<_> = visible(&rows, &options, "", &HashSet::new())
            .iter()
            .map(|index| rows[*index].item.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Example", "a", "readValue", "z", "_protected", "__private"]
        );
        let collapsed = HashSet::from([rows[class].key()]);
        assert_eq!(visible(&rows, &options, "", &collapsed), [class]);
        let found = visible(&rows, &options, "rV", &collapsed);
        assert_eq!(found.len(), 2);
        assert_eq!(rows[found[1]].item.name, "readValue");
        options.show_imports = true;
        assert!(visible(&rows, &options, "os", &HashSet::new())
            .iter()
            .any(|index| rows[*index].item.kind == K::Import));
    }
    #[test]
    fn edited_outline_invalidates_cache_and_inherited_navigation_opens_owner_file() {
        let (_directory, mut app, path) = app("from base import Base\nclass Child(Base): pass\n");
        let base = path.with_file_name("base.py");
        std::fs::write(&base, "class Base:\n    def old(self): pass\n").unwrap();
        app.settings.editor.structure.show_inherited = true;
        assert!(app
            .structure_rows()
            .iter()
            .any(|row| row.item.name == "old" && row.inherited));
        app.open_file(&base);
        let base_index = app.active.unwrap();
        app.tabs[base_index]
            .state
            .doc
            .set_text("class Base:\n    def fresh(self): pass\n", &[], &[])
            .unwrap();
        app.sync_analysis_now();
        app.open_file(&path);
        let rows = app.structure_rows();
        assert!(!rows
            .iter()
            .any(|row| row.item.name == "old" && row.inherited));
        let row = rows
            .iter()
            .find(|row| row.item.name == "fresh" && row.inherited)
            .unwrap();
        app.structure_jump(row, true);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_deref(),
            Some(base.as_path())
        );
        assert_eq!(
            app.tabs[app.active.unwrap()].state.primary().range(),
            row.item.name_range
        );
    }
    #[test]
    fn popup_and_tool_controls_are_persistent_without_mutating_source() {
        let source = "class Example:\n    field=1\n    def method(self): pass\n";
        let (_directory, mut app, _) = app(source);
        let context = egui::Context::default();
        app.handle_action(Action::FileStructure, &context);
        context
            .run_ui(egui::RawInput::default(), |ui| {
                app.file_structure_popup(ui.ctx(), "method".into(), 0);
            })
            .drop_without_applying_deltas();
        assert!(matches!(app.dialog, Dialog::FileStructure { .. }));
        assert_eq!(app.tabs[0].state.doc.text(), source);
        app.settings.editor.structure.show_members_in_project = true;
        app.settings.editor.structure.group_by_kind = true;
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert!(
            restored.editor.structure.show_members_in_project
                && restored.editor.structure.group_by_kind
        );
    }
}

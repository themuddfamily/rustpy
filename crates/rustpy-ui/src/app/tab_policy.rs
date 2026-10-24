//! Tab opening, stable-view LRU, titles and scope colors. Close decisions remain
//! with the existing tab lifecycle (including shared-document aliases).
use super::*;
use rustpy_settings::tab_policy::{TabOpeningOrder, TabPolicySettings};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct TabPolicy {
    clock: u64,
    used: HashMap<egui::Id, u64>,
    last_active: Option<egui::Id>,
    /// Explicit preview opening uses its own existing replacement policy.
    pub explicit_preview: bool,
}
impl TabPolicy {
    fn activate(&mut self, id: egui::Id) {
        if self.last_active == Some(id) {
            return;
        }
        if self.clock == u64::MAX {
            let mut ordered: Vec<_> = self.used.iter().map(|(id, age)| (*id, *age)).collect();
            ordered.sort_by_key(|(_, age)| *age);
            for (index, (id, _)) in ordered.into_iter().enumerate() {
                self.used.insert(id, index as u64 + 1);
            }
            self.clock = self.used.len() as u64;
        }
        self.clock += 1;
        self.used.insert(id, self.clock);
        self.last_active = Some(id);
    }
}
impl IdeApp {
    /// Call at the start and end of real editor-group rendering, and immediately
    /// before opening a tab: activation authority follows EditorView IDs, not docs.
    pub(super) fn observe_tab_policy(&mut self) {
        self.tab_policy
            .used
            .retain(|id, _| self.tabs.iter().any(|t| t.view.id == *id));
        if let Some(id) = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id)
        {
            self.tab_policy.activate(id);
        } else {
            self.tab_policy.last_active = None;
        }
    }
    pub(super) fn tab_lru_candidate(&self) -> Option<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(index, t)| {
                !t.pinned && !t.has_unsaved_changes() && self.detached != Some(*index)
            })
            .min_by_key(|(index, t)| {
                (
                    self.tab_policy.used.get(&t.view.id).copied().unwrap_or(0),
                    *index,
                )
            })
            .map(|(index, _)| index)
    }
    /// General reuse is opt-in and never participates in explicit preview opens.
    /// Shared-document peers, notebook/viewer tabs and detached editors cannot
    /// be silently replaced. The actual caller closes via existing close_tab.
    pub(super) fn reusable_normal_tab(&self) -> Option<usize> {
        if !self.settings.editor.tabs.reuse_unmodified || self.tab_policy.explicit_preview {
            return None;
        }
        let index = self.active?;
        let tab = self.tabs.get(index)?;
        let identity = tab.state.doc.identity();
        (!tab.pinned
            && !tab.preview
            && !tab.has_unsaved_changes()
            && tab.state.path.is_some()
            && tab.image.is_none()
            && tab.notebook.is_none()
            && self.detached != Some(index)
            && self
                .tabs
                .iter()
                .filter(|t| t.state.doc.identity() == identity)
                .count()
                == 1)
            .then_some(index)
    }
    /// Apply ordering with stable editor authority; no close/prompt paths touched.
    pub(super) fn order_opened_tab(&mut self, opened: egui::Id, previous: Option<egui::Id>) {
        use TabOpeningOrder::*;
        let policy = &self.settings.editor.tabs;
        let active_id = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let detached_id = self
            .detached
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        if policy.sort_by_name || policy.opening_order == Alphabetical {
            self.tabs.sort_by_key(|t| {
                (
                    !t.pinned,
                    t.state.title().to_lowercase(),
                    t.state.path.clone(),
                )
            });
        } else if policy.opening_order == AfterCurrent {
            if let Some(from) = self.tabs.iter().position(|t| t.view.id == opened) {
                if let Some(target) =
                    previous.and_then(|id| self.tabs.iter().position(|t| t.view.id == id))
                {
                    let tab = self.tabs.remove(from);
                    let target = target - usize::from(from < target);
                    self.tabs.insert((target + 1).min(self.tabs.len()), tab);
                }
            }
            self.sort_pinned_first();
        } else {
            self.sort_pinned_first();
        }
        self.active = active_id.and_then(|id| self.tabs.iter().position(|t| t.view.id == id));
        self.detached = detached_id.and_then(|id| self.tabs.iter().position(|t| t.view.id == id));
        self.observe_tab_policy();
    }
    pub(super) fn sort_tabs_by_filename(&mut self) {
        let current = self.settings.editor.tabs.sort_by_name;
        self.settings.editor.tabs.sort_by_name = true;
        if let Some(id) = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id)
        {
            self.order_opened_tab(id, None);
        }
        self.settings.editor.tabs.sort_by_name = current;
    }
    pub(super) fn tab_policy_title(&self, index: usize, distinguishing: &str) -> String {
        let Some(tab) = self.tabs.get(index) else {
            return String::new();
        };
        let title = if self.settings.editor.tabs.show_full_path {
            tab.state
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| tab.state.title())
        } else if tab.state.path.is_none() {
            tab.state.title()
        } else {
            distinguishing.into()
        };
        let icon = if self.settings.editor.tabs.show_file_icons {
            tab_icon(
                tab.state.path.as_deref(),
                tab.image.is_some(),
                tab.notebook.is_some(),
            )
        } else {
            ""
        };
        format!("{icon}{title}")
    }
    pub(super) fn tab_scope_color(&self, index: usize) -> Option<Color32> {
        let path = self.tabs.get(index)?.state.path.as_ref()?;
        if self.settings.scopes.file_colors_enabled {
            if let Some(rgb) = self
                .settings
                .scopes
                .color(path, &self.capture_scope_context())
            {
                return Some(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            }
        }
        scope_color(
            &self.settings.editor.tabs,
            self.project.as_ref().map(|p| p.root.as_path()),
            path,
        )
        .map(|rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
    }
    /// Drag-out is an explicit release outside this viewport, never hover. The
    /// payload is a view ID; reuse the accepted detached editor lifecycle.
    pub(super) fn detach_dragged_tab(&mut self, id: egui::Id, ctx: &egui::Context) {
        let Some(index) = self.tabs.iter().position(|tab| tab.view.id == id) else {
            return;
        };
        if self.detached.is_some() && self.detached != Some(index) {
            self.set_status("Reattach the existing detached editor before detaching another");
            return;
        }
        self.active = Some(index);
        self.handle_action(Action::DetachTab, ctx);
    }
}
fn tab_icon(path: Option<&Path>, image: bool, notebook: bool) -> &'static str {
    if image {
        return "▧ ";
    }
    if notebook {
        return "▤ ";
    }
    match path
        .and_then(Path::extension)
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "py" | "pyi" | "pyw" => "Py ",
        "json" | "yaml" | "yml" | "toml" => "{} ",
        "html" | "xml" => "<> ",
        "css" => "# ",
        "md" | "rst" => "¶ ",
        _ => "□ ",
    }
}
fn scope_color(policy: &TabPolicySettings, root: Option<&Path>, path: &Path) -> Option<[u8; 3]> {
    policy
        .file_colors
        .iter()
        .filter_map(|scope| {
            let directory = if scope.directory.is_absolute() {
                scope.directory.clone()
            } else {
                root?.join(&scope.directory)
            };
            path.starts_with(&directory)
                .then_some((directory.components().count(), scope.rgb))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, rgb)| rgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp) {
        let root = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
        for name in ["α.py", "z.py"] {
            let p = root.path().join(name);
            std::fs::write(&p, "π=1\n").unwrap();
            app.open_file(&p);
        }
        (root, app)
    }
    #[test]
    fn view_lru_survives_reorder_and_dirty_or_explicit_preview_is_not_reusable() {
        let (_root, mut app) = fixture();
        app.active = Some(1);
        app.observe_tab_policy();
        app.active = Some(0);
        app.observe_tab_policy();
        let oldest = app.tabs[1].view.id;
        app.tabs.swap(0, 1);
        app.active = Some(1);
        app.observe_tab_policy();
        assert_eq!(app.tabs[app.tab_lru_candidate().unwrap()].view.id, oldest);
        app.tabs[0].pinned = true;
        assert_ne!(app.tabs[app.tab_lru_candidate().unwrap()].view.id, oldest);
        app.settings.editor.tabs.reuse_unmodified = true;
        assert_eq!(app.reusable_normal_tab(), Some(1));
        app.tab_policy.explicit_preview = true;
        assert_eq!(app.reusable_normal_tab(), None);
        app.tab_policy.explicit_preview = false;
        assert!(app.tabs[1].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "changed".into())],
            "Dirty fixture"
        ));
        assert_eq!(app.reusable_normal_tab(), None);
    }
    #[test]
    fn alphabetical_reorder_keeps_active_detached_and_group_editor_ids() {
        let (_root, mut app) = fixture();
        let a = app.tabs[0].view.id;
        let z = app.tabs[1].view.id;
        app.active = Some(1);
        app.detached = Some(0);
        app.group_active = [Some(a), Some(z)];
        app.sort_tabs_by_filename();
        assert_eq!(app.tabs[app.active.unwrap()].view.id, z);
        assert_eq!(app.tabs[app.detached.unwrap()].view.id, a);
        assert_eq!(app.group_active, [Some(a), Some(z)]);
        assert_eq!(app.tabs[0].state.title(), "z.py");
    }
    #[test]
    fn actual_open_evicts_recently_unused_view_and_reuse_preserves_dirty_tab() {
        let (root, mut app) = fixture();
        app.settings.editor.tab_limit = 2;
        let first = app.tabs[0].state.path.clone().unwrap();
        let second = app.tabs[1].state.path.clone().unwrap();
        app.open_file(&first);
        app.observe_tab_policy();
        let third = root.path().join("third.py");
        std::fs::write(&third, "δ=3\n").unwrap();
        app.open_file(&third);
        assert!(app
            .tabs
            .iter()
            .any(|t| t.state.path.as_ref() == Some(&first)));
        assert!(!app
            .tabs
            .iter()
            .any(|t| t.state.path.as_ref() == Some(&second)));
        assert_eq!(app.closed_tabs.last().unwrap().path, second);
        app.settings.editor.tabs.reuse_unmodified = true;
        let dirty = app.active.unwrap();
        app.tabs[dirty].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# retained\n".into())],
            "Retain dirty",
        );
        let dirty_id = app.tabs[dirty].view.id;
        let fourth = root.path().join("fourth.py");
        std::fs::write(&fourth, "ε=4\n").unwrap();
        app.open_file(&fourth);
        assert!(app
            .tabs
            .iter()
            .any(|t| t.view.id == dirty_id && t.has_unsaved_changes()));
        assert_eq!(
            app.tabs[app.active.unwrap()].state.path.as_ref(),
            Some(&fourth)
        );
    }
    #[test]
    fn after_current_opening_and_drag_out_use_actual_view_identity() {
        let (root, mut app) = fixture();
        app.settings.editor.tabs.opening_order = TabOpeningOrder::AfterCurrent;
        let first = app.tabs[0].view.id;
        app.active = Some(0);
        let path = root.path().join("middle.py");
        std::fs::write(&path, "μ=2\n").unwrap();
        app.open_file(&path);
        let middle = app.tabs[app.active.unwrap()].view.id;
        assert_eq!(app.tabs[0].view.id, first);
        assert_eq!(app.tabs[1].view.id, middle);
        let ctx = egui::Context::default();
        app.tabs.swap(0, 1);
        app.detach_dragged_tab(middle, &ctx);
        assert_eq!(app.tabs[app.detached.unwrap()].view.id, middle);
        let old = app.detached;
        app.detach_dragged_tab(egui::Id::new("closed editor"), &ctx);
        assert_eq!(app.detached, old);
        assert!(!app.tabs.iter().any(|t| t.state.doc.can_undo()));
    }
    #[test]
    fn scoped_colors_use_component_boundaries_and_most_specific_scope() {
        use rustpy_settings::tab_policy::TabFileColor;
        let policy = TabPolicySettings {
            file_colors: vec![
                TabFileColor {
                    name: "Source".into(),
                    directory: "src".into(),
                    rgb: [1, 2, 3],
                },
                TabFileColor {
                    name: "Tests".into(),
                    directory: "src/tests".into(),
                    rgb: [4, 5, 6],
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            scope_color(&policy, Some(Path::new("/p")), Path::new("/p/src2/a.py")),
            None
        );
        assert_eq!(
            scope_color(
                &policy,
                Some(Path::new("/p")),
                Path::new("/p/src/tests/π.py")
            ),
            Some([4, 5, 6])
        );
    }
}

#[cfg(test)]
#[path = "tab_policy_connected_tests.rs"]
mod connected_tests;

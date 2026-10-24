//! The editor tab strip (roadmap §1.5 Tabs): context menu (close others / all /
//! all but pinned / to the left / to the right, pin, split, copy path, select
//! in project), pinned tabs first, distinguishing folder names for identical
//! file names, horizontal scrolling with a list of hidden tabs, drag to
//! reorder (also into the other split group), closing tabs and Reopen Closed Tab.

use super::*;
use crate::a11y::Accessible as _;

/// Measure with the actual text format; preserve identifying folder context.
fn fitted_tab_title(
    full: &str,
    identifying: &str,
    prefix: &str,
    budget: f32,
    mut width: impl FnMut(&str) -> f32,
) -> String {
    if width(full) <= budget {
        return full.into();
    }
    let compact = format!("{prefix}…/{identifying}");
    if width(&compact) <= budget {
        return compact;
    }
    let (folders, basename) = identifying.rsplit_once('/').unwrap_or(("", identifying));
    let folder: Vec<char> = folders.chars().collect();
    let name: Vec<char> = basename.chars().collect();
    let mut folder_keep = folder.len();
    let mut name_keep = name.len();
    let shorten = |chars: &[char], keep: usize| -> String {
        if keep == chars.len() {
            return chars.iter().collect();
        }
        let left = keep.div_ceil(2);
        let right = keep / 2;
        chars[..left]
            .iter()
            .chain(std::iter::once(&'…'))
            .chain(chars[chars.len() - right..].iter())
            .collect()
    };
    loop {
        let context = shorten(&folder, folder_keep);
        let base = shorten(&name, name_keep);
        let value = if context.is_empty() {
            format!("{prefix}{base}")
        } else {
            format!("{prefix}{base} ‹{context}›")
        };
        if width(&value) <= budget {
            return value;
        }
        // Keep both ends of the distinguishing context instead of dropping it.
        if folder_keep > 2 {
            folder_keep -= 1;
            continue;
        }
        if name_keep > 2 {
            name_keep -= 1;
            continue;
        }
        let minimum = format!("{prefix}…");
        return if width(&minimum) <= budget {
            minimum
        } else {
            String::new()
        };
    }
}

/// Drag-and-drop payload: the dragged tab's editor id.
#[derive(Clone, Copy)]
struct TabDrag(egui::Id);

/// What the user did in a tab strip.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TabEvent {
    Activate(usize),
    Close(usize),
    CloseOthers(usize),
    CloseAll(u8),
    CloseAllButPinned(u8),
    CloseLeft(usize),
    CloseRight(usize),
    TogglePin(usize),
    SplitRight(usize),
    SplitDown(usize),
    MoveToOppositeGroup(usize),
    CopyPath(usize),
    CopyReference(usize),
    SelectInProject(usize),
    /// Drop the tab `id` next to tab `target` (after it when `after`), or at the
    /// end of `group` when there is no target.
    Move {
        id: egui::Id,
        target: Option<usize>,
        after: bool,
        group: u8,
    },
}

/// A closed editor tab that Reopen Closed Tab can bring back: the file and
/// where its tab was (split group, position among that group's tabs).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClosedTab {
    pub path: PathBuf,
    pub group: u8,
    pub index: usize,
}

/// How many closed tabs Reopen Closed Tab remembers.
const CLOSED_TABS_KEPT: usize = 50;

/// A tab index after the tab at `removed` is gone (`None` when it was that tab).
fn index_after_removal(index: Option<usize>, removed: usize) -> Option<usize> {
    match index {
        Some(j) if j == removed => None,
        Some(j) if j > removed => Some(j - 1),
        j => j,
    }
}

/// The tab to show in `closed`'s group once it is closed, as an index into the
/// remaining tabs (`groups`: every tab's split group before the close). The
/// default closing policy: the left neighbour, else the next tab.
fn neighbour_after_close(groups: &[u8], closed: usize) -> Option<usize> {
    let group = *groups.get(closed)?;
    (0..closed).rev().find(|j| groups[*j] == group).or_else(|| {
        (closed + 1..groups.len())
            .find(|j| groups[*j] == group)
            .map(|j| j - 1)
    })
}

/// Tab titles: the file names, and for a name that is open more than once the
/// shortest parent-folder suffix that tells the files apart (`app1/models.py`).
pub(super) fn unique_titles(paths: &[Option<&Path>]) -> Vec<String> {
    let name = |p: Option<&Path>| {
        p.and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    };
    // Parent folder names, nearest first.
    let parents = |p: &Path| -> Vec<String> {
        p.parent()
            .map(|d| {
                d.components()
                    .rev()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .filter(|c| !c.is_empty() && c != "/" && c != "\\")
                    .collect()
            })
            .unwrap_or_default()
    };
    let names: Vec<String> = paths.iter().map(|p| name(*p)).collect();
    let mut out = names.clone();
    for (i, path) in paths.iter().enumerate() {
        let Some(path) = path else { continue };
        let others: Vec<Vec<String>> = paths
            .iter()
            .enumerate()
            .filter(|(j, p)| *j != i && names[*j] == names[i] && p.is_some_and(|p| p != *path))
            .map(|(_, p)| parents(p.unwrap_or(Path::new(""))))
            .collect();
        if others.is_empty() {
            continue;
        }
        let mine = parents(path);
        let k = (1..=mine.len())
            .find(|k| others.iter().all(|o| o.get(..*k) != mine.get(..*k)))
            .unwrap_or(mine.len());
        let dirs: Vec<&str> = mine[..k].iter().rev().map(String::as_str).collect();
        if !dirs.is_empty() {
            out[i] = format!("{}/{}", dirs.join("/"), names[i]);
        }
    }
    out
}

/// Indices of the tabs a bulk close affects: `others` (same group, not `i`),
/// `left` / `right` of `i`, or the whole group; pinned tabs only when `pinned_too`.
fn tabs_to_close(
    tabs: &[(u8, bool)],
    group: u8,
    anchor: Option<usize>,
    side: Option<bool>,
    pinned_too: bool,
) -> Vec<usize> {
    tabs.iter()
        .enumerate()
        .filter(|(j, (g, pinned))| {
            *g == group
                && (pinned_too || !*pinned)
                && match (anchor, side) {
                    (Some(i), None) => *j != i,
                    (Some(i), Some(false)) => *j < i,
                    (Some(i), Some(true)) => *j > i,
                    (None, _) => true,
                }
        })
        .map(|(j, _)| j)
        .collect()
}

impl IdeApp {
    /// Closes tab `i` without asking about unsaved changes and remembers it for
    /// Reopen Closed Tab. Closing a background tab keeps the active editor
    /// (the active file never switches then); closing the tab a group shows
    /// selects its left neighbour, else the next tab, and an emptied group hands
    /// over to the other group's editor.
    pub(super) fn close_tab(&mut self, i: usize) {
        self.reconcile_document_markers();
        if i >= self.tabs.len() {
            return;
        }
        let groups: Vec<u8> = self.tabs.iter().map(|t| t.group).collect();
        let group = groups[i];
        let shown = self.group_active_tab(group) == Some(i);
        let index = groups[..i].iter().filter(|g| **g == group).count();
        let tab = self.tabs.remove(i);
        self.sync_external_file_watches();
        if let Some(path) = tab.state.path.filter(|path| {
            !self
                .tabs
                .iter()
                .any(|other| other.state.path.as_deref() == Some(path.as_path()))
        }) {
            self.disk_revisions.remove(&path);
            self.observed_revisions.remove(&path);
            if matches!(&self.dialog, Dialog::ExternalChange(p) if *p == path) {
                self.dialog = Dialog::None;
            }
            self.external_changes.retain(|p| *p != path);
            self.closed_tabs.push(ClosedTab { path, group, index });
            if self.closed_tabs.len() > CLOSED_TABS_KEPT {
                self.closed_tabs.remove(0);
            }
        }
        self.detached = index_after_removal(self.detached, i);
        let neighbour = if shown {
            neighbour_after_close(&groups, i)
        } else {
            None
        };
        if let Some(j) = neighbour {
            self.group_active[usize::from(group)] = Some(self.tabs[j].view.id);
        }
        self.active = match index_after_removal(self.active, i) {
            Some(a) => Some(a),
            None if self.tabs.is_empty() => None,
            None => neighbour
                .or_else(|| {
                    self.group_active[usize::from(group ^ 1)]
                        .and_then(|id| self.tabs.iter().position(|t| t.view.id == id))
                })
                .or(Some(0)),
        };
        self.focus_editor = true;
    }

    /// Reopen Closed Tab: the most recently closed file comes back where its
    /// tab was (files deleted since are skipped).
    pub(super) fn reopen_closed_tab(&mut self) {
        while let Some(closed) = self.closed_tabs.pop() {
            if !closed.path.starts_with("<typeshed>") && !closed.path.exists() {
                continue;
            }
            let before = self.tabs.len();
            self.open_file(&closed.path);
            if self.tabs.len() > before {
                if let Some(id) = self
                    .active
                    .and_then(|a| self.tabs.get(a))
                    .map(|t| t.view.id)
                {
                    let group = if self.split.is_some() {
                        closed.group
                    } else {
                        0
                    };
                    let target = self
                        .tabs
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.group == group && t.view.id != id)
                        .nth(closed.index)
                        .map(|(j, _)| j);
                    self.move_tab(id, target, false, group);
                }
            }
            return;
        }
    }

    /// Drops every editor tab and everything that refers to tabs by index
    /// (opening or closing a project): nothing of the old project can be
    /// reopened or stay in a detached window.
    pub(super) fn forget_all_tabs(&mut self) {
        self.file_session = self.file_session.wrapping_add(1);
        self.disk_revisions.clear();
        self.observed_revisions.clear();
        self.external_changes.clear();
        if matches!(self.dialog, Dialog::ExternalChange(_)) {
            self.dialog = Dialog::None;
        }
        self.tabs.clear();
        self.sync_external_file_watches();
        self.active = None;
        self.detached = None;
        self.detached_bounds = None;
        self.detached_bounds_pending = false;
        self.split = None;
        self.split_fraction = 0.5;
        self.closed_tabs.clear();
        self.group_active = [None, None];
    }

    /// Keeps pinned tabs first (stable within each group) and the active /
    /// detached tab indices pointing at the same editors.
    pub(super) fn sort_pinned_first(&mut self) {
        let active = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let detached = self
            .detached
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        self.tabs.sort_by_key(|t| !t.pinned);
        let find = |tabs: &[Tab], id: Option<egui::Id>| {
            id.and_then(|id| tabs.iter().position(|t| t.view.id == id))
        };
        self.active = find(&self.tabs, active).or(self.active.filter(|i| *i < self.tabs.len()));
        self.detached = find(&self.tabs, detached);
    }

    /// Closes the given tabs except modified ones (reported in the status bar);
    /// `keep_active` stays the active editor.
    fn close_tab_set(&mut self, picked: Vec<usize>, keep_active: Option<usize>) {
        let ids: Vec<egui::Id> = picked
            .iter()
            .filter_map(|i| self.tabs.get(*i))
            .map(|t| t.view.id)
            .collect();
        let keep = keep_active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let mut kept = 0;
        for id in ids {
            let Some(i) = self.tabs.iter().position(|t| t.view.id == id) else {
                continue;
            };
            if self.tabs[i].state.doc.is_modified() {
                kept += 1;
                continue;
            }
            self.close_tab(i);
        }
        if let Some(i) = keep.and_then(|id| self.tabs.iter().position(|t| t.view.id == id)) {
            self.active = Some(i);
        }
        if kept > 0 {
            self.set_status(format!("{kept} tab(s) with unsaved changes were kept open"));
        }
    }

    fn tab_flags(&self) -> Vec<(u8, bool)> {
        self.tabs.iter().map(|t| (t.group, t.pinned)).collect()
    }

    /// Close Other Tabs: the other unpinned tabs of `i`'s group.
    pub(super) fn close_other_tabs(&mut self, i: usize) {
        let Some(group) = self.tabs.get(i).map(|t| t.group) else {
            return;
        };
        let picked = tabs_to_close(&self.tab_flags(), group, Some(i), None, false);
        self.close_tab_set(picked, Some(i));
    }

    /// Close Tabs to the Left / Right of `i` (unpinned, same group).
    pub(super) fn close_tabs_beside(&mut self, i: usize, right: bool) {
        let Some(group) = self.tabs.get(i).map(|t| t.group) else {
            return;
        };
        let picked = tabs_to_close(&self.tab_flags(), group, Some(i), Some(right), false);
        self.close_tab_set(picked, Some(i));
    }

    /// Close All (`pinned_too`) or Close All but Pinned in one group.
    pub(super) fn close_group_tabs(&mut self, group: u8, pinned_too: bool) {
        let picked = tabs_to_close(&self.tab_flags(), group, None, None, pinned_too);
        let keep = self.active.filter(|a| !picked.contains(a));
        self.close_tab_set(picked, keep);
    }

    pub(super) fn toggle_pin(&mut self, i: usize) {
        let Some(tab) = self.tabs.get_mut(i) else {
            return;
        };
        tab.pinned = !tab.pinned;
        let msg = format!(
            "{} {}",
            if tab.pinned { "Pinned" } else { "Unpinned" },
            tab.state.title()
        );
        self.sort_pinned_first();
        self.set_status(msg);
    }

    /// Moves the tab `id` next to tab `target` (or to the end of `group`).
    pub(super) fn move_tab(&mut self, id: egui::Id, target: Option<usize>, after: bool, group: u8) {
        let Some(from) = self.tabs.iter().position(|t| t.view.id == id) else {
            return;
        };
        let target_id = target.and_then(|t| self.tabs.get(t)).map(|t| t.view.id);
        if target_id == Some(id) {
            return;
        }
        let active = self
            .active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let detached = self
            .detached
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let mut tab = self.tabs.remove(from);
        tab.group = group;
        let at = match target_id.and_then(|tid| self.tabs.iter().position(|t| t.view.id == tid)) {
            Some(t) => t + usize::from(after),
            None => self
                .tabs
                .iter()
                .rposition(|t| t.group == group)
                .map(|t| t + 1)
                .unwrap_or(self.tabs.len()),
        };
        self.tabs.insert(at.min(self.tabs.len()), tab);
        self.active = active.and_then(|a| self.tabs.iter().position(|t| t.view.id == a));
        self.detached = detached.and_then(|d| self.tabs.iter().position(|t| t.view.id == d));
        self.sort_pinned_first();
        if let Some(i) = self.tabs.iter().position(|t| t.view.id == id) {
            self.active = Some(i);
            self.group_active[group as usize] = Some(id);
        }
        self.focus_editor = true;
    }

    fn handle_tab_event(&mut self, ev: TabEvent, ctx: &egui::Context) {
        match ev {
            TabEvent::Activate(i) => {
                self.active = Some(i);
                self.focus_editor = true;
            }
            TabEvent::Close(i) => self.request_close_tab(i),
            TabEvent::CloseOthers(i) => self.close_other_tabs(i),
            TabEvent::CloseAll(group) => self.close_group_tabs(group, true),
            TabEvent::CloseAllButPinned(group) => self.close_group_tabs(group, false),
            TabEvent::CloseLeft(i) => self.close_tabs_beside(i, false),
            TabEvent::CloseRight(i) => self.close_tabs_beside(i, true),
            TabEvent::TogglePin(i) => self.toggle_pin(i),
            TabEvent::SplitRight(i)
            | TabEvent::SplitDown(i)
            | TabEvent::MoveToOppositeGroup(i)
            | TabEvent::SelectInProject(i) => {
                self.active = Some(i);
                let action = match ev {
                    TabEvent::SplitRight(_) => Action::SplitVertically,
                    TabEvent::SplitDown(_) => Action::SplitHorizontally,
                    TabEvent::MoveToOppositeGroup(_) => Action::MoveTabToOppositeGroup,
                    _ => Action::SelectInProjectView,
                };
                self.handle_action(action, ctx);
            }
            TabEvent::CopyPath(i) => {
                if let Some(target) = self.copy_target_for_tab(i) {
                    self.open_copy_path_target(target);
                }
            }
            TabEvent::CopyReference(i) => {
                if let Some(target) = self.copy_target_for_tab(i) {
                    self.copy_reference_target(target);
                }
            }
            TabEvent::Move {
                id,
                target,
                after,
                group,
            } => self.move_tab(id, target, after, group),
        }
    }

    /// One group's tab strip. Tabs scroll horizontally (the mouse wheel works
    /// too); the active tab is scrolled into view when it changes.
    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui, group: u8) {
        let group_active = self.group_active_tab(group);
        let menu_available = self.line_actions_owner_available(ui.ctx());
        let theme = self.theme.clone();
        let bar_h = 30.0;
        let font = egui::FontId::proportional(13.0);
        let titles = unique_titles(
            &self
                .tabs
                .iter()
                .map(|t| t.state.path.as_deref())
                .collect::<Vec<_>>(),
        );
        let split = self.split.is_some();
        // (index, label, width) of this group's tabs, in order.
        let members: Vec<(usize, String, f32)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group == group)
            .map(|(i, t)| {
                let title = self.tab_policy_title(i, &titles[i]);
                let modified = t.state.doc.is_modified();
                let label = if modified && self.settings.editor.mark_modified_tabs_with_asterisk {
                    format!("*{title}")
                } else {
                    title
                };
                let text_w = ui
                    .fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), theme.text))
                    .size()
                    .x;
                (i, label, text_w + 40.0 + if t.pinned { 14.0 } else { 0.0 })
            })
            .collect();
        let side = tab_presentation::vertical(self.settings.editor.tabs.placement);
        let wrapped = self.settings.editor.tabs.multiple_rows && !side;
        let widths: Vec<_> = members.iter().map(|m| m.2).collect();
        let rows = tab_presentation::wrapped_rows(ui.available_width(), &widths);
        let height = if side {
            ui.available_height()
        } else if wrapped {
            (rows as f32 * bar_h).min(
                if self.settings.editor.tabs.placement
                    == rustpy_settings::tab_policy::TabPlacement::Bottom
                {
                    ui.available_height().max(bar_h)
                } else {
                    (ui.available_height() - 80.0).max(bar_h)
                },
            )
        } else {
            bar_h
        };
        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        ui.painter().rect_filled(bar_rect, 0.0, theme.tab_bar_bg);
        ui.painter().line_segment(
            [bar_rect.left_bottom(), bar_rect.right_bottom()],
            Stroke::new(1.0, theme.border),
        );
        let total: f32 = members.iter().map(|m| m.2).sum();
        let overflow = !side && !wrapped && total > bar_rect.width();
        let list_w = if overflow { 30.0 } else { 0.0 };
        let strip_rect = Rect::from_min_max(
            bar_rect.min,
            Pos2::new(bar_rect.right() - list_w, bar_rect.bottom()),
        );
        // Scroll the active tab into view when it changes.
        let memory_id = ui.id().with(("tab_strip_active", group));
        let active_id = group_active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.view.id);
        let reveal_active = ui.data(|d| d.get_temp::<egui::Id>(memory_id)) != active_id;
        if let Some(id) = active_id {
            ui.data_mut(|d| d.insert_temp(memory_id, id));
        }
        let mut events: Vec<TabEvent> = Vec::new();
        let mut rects: Vec<(usize, Rect)> = Vec::new();
        let dragging = egui::DragAndDrop::has_payload_of_type::<TabDrag>(ui.ctx());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(strip_rect).layout(if side {
            egui::Layout::top_down(egui::Align::Min)
        } else {
            egui::Layout::left_to_right(if wrapped {
                egui::Align::Min
            } else {
                egui::Align::Center
            })
            .with_main_wrap(wrapped)
        }));
        child.style_mut().always_scroll_the_only_direction = true;
        let scroll = (if side || wrapped {
            egui::ScrollArea::vertical()
        } else {
            egui::ScrollArea::horizontal()
        })
        .id_salt(("tab_strip", group))
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if wrapped {
                // Match the planned rows * bar_h strip height: egui's wrapping
                // cursor must start with the actual tab height and no row gap.
                ui.set_row_height(bar_h);
                ui.spacing_mut().item_spacing.y = 0.0;
            }
            for (i, label, w) in &members {
                let (i, w) = (
                    *i,
                    if side {
                        strip_rect.width()
                    } else if wrapped {
                        w.min(strip_rect.width())
                    } else {
                        *w
                    },
                );
                let tab = &self.tabs[i];
                let (rect, _) = ui.allocate_exact_size(Vec2::new(w, bar_h), Sense::hover());
                // Ids follow the editor (not the position) so a drag survives reordering.
                let resp = ui.interact(
                    rect,
                    ui.id().with(("tab", tab.view.id)),
                    Sense::click_and_drag(),
                );
                self.detached_tab_drag.register_root_rect(
                    ui,
                    rect,
                    tab.view.id,
                    group,
                    side,
                    self.detached
                        .is_some_and(|index| self.tabs[index].view.id == tab.view.id),
                );
                rects.push((i, rect));
                let active = group_active == Some(i);
                if active && reveal_active {
                    resp.scroll_to_me(None);
                }
                let modified = tab.state.doc.is_modified();
                let scope_bg = self.tab_scope_color(i);
                let bg = if active {
                    theme.tab_active_bg
                } else if resp.hovered() {
                    theme.tab_hover_bg
                } else {
                    theme.tab_bar_bg
                };
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, bg);
                if let Some(color) = scope_bg {
                    painter.rect_filled(
                        Rect::from_min_max(rect.min, Pos2::new(rect.left() + 3.0, rect.bottom())),
                        0.0,
                        color,
                    );
                }
                if active {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(rect.left(), rect.bottom() - 2.0),
                            rect.right_bottom(),
                        ),
                        0.0,
                        theme.tab_underline,
                    );
                }
                let color = if modified { theme.modified } else { theme.text };
                let mut x = rect.left() + 10.0;
                if tab.pinned {
                    // A small push-pin.
                    let c = Pos2::new(x + 4.0, rect.center().y - 2.0);
                    painter.circle_filled(c, 3.0, theme.text_dim);
                    painter.line_segment(
                        [c, Pos2::new(c.x, c.y + 7.0)],
                        Stroke::new(1.5, theme.text_dim),
                    );
                    x += 14.0;
                }
                let path_title = tab
                    .state
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string());
                let prefix = if self.settings.editor.tabs.show_full_path {
                    path_title
                        .as_deref()
                        .and_then(|path| label.strip_suffix(path))
                        .unwrap_or("")
                } else {
                    label.strip_suffix(&titles[i]).unwrap_or("")
                };
                let text_width = (rect.right() - 26.0 - x).max(0.0);
                let painted_label =
                    fitted_tab_title(label, &titles[i], prefix, text_width, |text| {
                        let mut measured = egui::text::LayoutJob::default();
                        measured.append(
                            text,
                            0.0,
                            egui::text::TextFormat {
                                font_id: font.clone(),
                                color,
                                italics: tab.preview && !modified,
                                ..Default::default()
                            },
                        );
                        ui.fonts_mut(|fonts| fonts.layout_job(measured)).size().x
                    });
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &painted_label,
                    0.0,
                    egui::text::TextFormat {
                        font_id: font.clone(),
                        color,
                        italics: tab.preview && !modified,
                        ..Default::default()
                    },
                );
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                painter.galley(
                    Pos2::new(x, rect.center().y - galley.size().y / 2.0),
                    galley,
                    color,
                );
                // Close button.
                let close_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 14.0, rect.center().y),
                    Vec2::splat(14.0),
                );
                let close_resp = ui.interact(
                    close_rect,
                    ui.id().with(("tab_close", tab.view.id)),
                    Sense::click(),
                );
                if close_resp.hovered() {
                    ui.painter().rect_filled(close_rect, 3.0, theme.hover_bg);
                }
                if active || resp.hovered() || close_resp.hovered() {
                    ui.painter().text(
                        close_rect.center(),
                        Align2::CENTER_CENTER,
                        "×",
                        egui::FontId::proportional(15.0),
                        theme.text_dim,
                    );
                }
                ui.painter().line_segment(
                    [rect.right_top(), rect.right_bottom()],
                    Stroke::new(1.0, theme.border),
                );
                // Drag to reorder: a marker shows where the tab would land.
                resp.dnd_set_drag_payload(TabDrag(tab.view.id));
                if let Some(drag) = resp.dnd_hover_payload::<TabDrag>() {
                    if drag.0 != tab.view.id {
                        let after = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|p| {
                            if side {
                                p.y > rect.center().y
                            } else {
                                p.x > rect.center().x
                            }
                        });
                        let x = if after {
                            rect.right() - 1.0
                        } else {
                            rect.left() + 1.0
                        };
                        let marker = if side {
                            let y = if after {
                                rect.bottom() - 1.0
                            } else {
                                rect.top() + 1.0
                            };
                            [
                                Pos2::new(rect.left() + 3.0, y),
                                Pos2::new(rect.right() - 3.0, y),
                            ]
                        } else {
                            [
                                Pos2::new(x, rect.top() + 3.0),
                                Pos2::new(x, rect.bottom() - 3.0),
                            ]
                        };
                        ui.painter()
                            .line_segment(marker, Stroke::new(2.0, theme.tab_underline));
                    }
                }
                if let Some(drag) = resp.dnd_release_payload::<TabDrag>() {
                    let after = ui.input(|inp| inp.pointer.interact_pos()).is_some_and(|p| {
                        if side {
                            p.y > rect.center().y
                        } else {
                            p.x > rect.center().x
                        }
                    });
                    events.push(TabEvent::Move {
                        id: drag.0,
                        target: Some(i),
                        after,
                        group,
                    });
                }
                // Screen readers: a tab (selected when active) and its close button.
                crate::a11y::tab(&resp, &titles[i], active, modified);
                crate::a11y::button(&close_resp, &format!("Close {}", titles[i]));
                if close_resp.clicked() || resp.middle_clicked() {
                    events.push(TabEvent::Close(i));
                } else if resp.clicked() {
                    events.push(TabEvent::Activate(i));
                }
                let pinned = tab.pinned;
                let has_path = tab.state.path.is_some();
                let resp = match &tab.state.path {
                    Some(p) => resp.on_hover_text(p.display().to_string()),
                    None => resp,
                };
                let menu_authority = egui::Id::new((
                    tab.view.id,
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    &tab.state.path,
                ));
                crate::a11y::bound_keyboard_context_menu(&resp, menu_available, menu_authority)
                    .show(|ui| {
                        let mut item = |ui: &mut egui::Ui, text: &str, ev: TabEvent| {
                            if ui.button(text).clicked() {
                                events.push(ev);
                                ui.close();
                            }
                        };
                        item(ui, "Close", TabEvent::Close(i));
                        item(ui, "Close Other Tabs", TabEvent::CloseOthers(i));
                        item(ui, "Close All Tabs", TabEvent::CloseAll(group));
                        item(
                            ui,
                            "Close All but Pinned",
                            TabEvent::CloseAllButPinned(group),
                        );
                        item(ui, "Close Tabs to the Left", TabEvent::CloseLeft(i));
                        item(ui, "Close Tabs to the Right", TabEvent::CloseRight(i));
                        ui.separator();
                        item(
                            ui,
                            if pinned { "Unpin Tab" } else { "Pin Tab" },
                            TabEvent::TogglePin(i),
                        );
                        ui.separator();
                        item(ui, "Split Right", TabEvent::SplitRight(i));
                        item(ui, "Split Down", TabEvent::SplitDown(i));
                        if split {
                            item(
                                ui,
                                "Move to Opposite Group",
                                TabEvent::MoveToOppositeGroup(i),
                            );
                        }
                        if has_path {
                            ui.separator();
                            item(ui, "Copy Path…", TabEvent::CopyPath(i));
                            item(ui, "Copy Reference", TabEvent::CopyReference(i));
                            item(ui, "Select in Project View", TabEvent::SelectInProject(i));
                        }
                    });
            }
        });
        // Dropping past the last tab (or into an empty strip) appends the tab.
        let end_x = rects
            .iter()
            .map(|(_, r)| r.right())
            .fold(strip_rect.left(), f32::max);
        if !side && !wrapped && dragging && end_x < strip_rect.right() - 1.0 {
            let rest = Rect::from_min_max(Pos2::new(end_x, strip_rect.top()), strip_rect.max);
            let resp = ui.interact(rest, ui.id().with(("tab_strip_end", group)), Sense::hover());
            if let Some(drag) = resp.dnd_release_payload::<TabDrag>() {
                events.push(TabEvent::Move {
                    id: drag.0,
                    target: None,
                    after: true,
                    group,
                });
            }
        }
        // Tabs that are scrolled (partly) out of view are listed in a drop-down.
        let visible = scroll.inner_rect;
        let hidden: Vec<usize> = rects
            .iter()
            .filter(|(_, r)| {
                r.left() < visible.left() - 0.5
                    || r.right() > visible.right() + 0.5
                    || r.top() < visible.top() - 0.5
                    || r.bottom() > visible.bottom() + 0.5
            })
            .map(|(i, _)| *i)
            .collect();
        if overflow {
            let list_rect =
                Rect::from_min_max(Pos2::new(strip_rect.right(), bar_rect.top()), bar_rect.max);
            let mut list_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(list_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            list_ui
                .menu_button(
                    egui::RichText::new(format!("»{}", hidden.len())).color(theme.text_dim),
                    |ui| {
                        ui.label(
                            egui::RichText::new("Hidden tabs")
                                .color(theme.text_dim)
                                .small(),
                        );
                        for &i in &hidden {
                            if ui.button(&titles[i]).clicked() {
                                events.push(TabEvent::Activate(i));
                                ui.close();
                            }
                        }
                    },
                )
                .response
                .icon_hint("Show hidden tabs");
        }
        let ctx = ui.ctx().clone();
        let content_rect = ctx.content_rect();
        if ctx.input(|input| {
            input.pointer.any_released()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|position| !content_rect.contains(position))
        }) {
            if let Some(payload) = egui::DragAndDrop::take_payload::<TabDrag>(&ctx) {
                self.detach_dragged_tab(payload.0, &ctx);
            }
        }
        // Indices refer to the tabs as drawn: apply index-changing events last.
        events.sort_by_key(|e| {
            matches!(
                e,
                TabEvent::Move { .. }
                    | TabEvent::TogglePin(_)
                    | TabEvent::Close(_)
                    | TabEvent::CloseOthers(_)
                    | TabEvent::CloseAll(_)
                    | TabEvent::CloseAllButPinned(_)
                    | TabEvent::CloseLeft(_)
                    | TabEvent::CloseRight(_)
            )
        });
        if let Some(ev) = events.into_iter().next() {
            self.handle_tab_event(ev, &ctx);
        }
    }
}

#[cfg(test)]
#[path = "project_tab_interleaved_tests.rs"]
mod interleaved_project_tab_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_narrow_tab_titles_keep_distinct_unicode_basenames_inside_clip() {
        let directory = tempfile::tempdir().unwrap();
        let long = directory
            .path()
            .join("very_long_shared_directory_prefix_for_all_three_editors");
        std::fs::create_dir(&long).unwrap();
        let mut app = app_with(&[]);
        let names = ["alpha_π.py", "beta_β.py", "gamma_γ.py"];
        for name in names {
            let path = long.join(name);
            std::fs::write(&path, "value = 1\n").unwrap();
            app.open_file(&path);
        }
        app.settings.editor.tabs.show_full_path = true;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        for placement in [
            rustpy_settings::tab_policy::TabPlacement::Left,
            rustpy_settings::tab_policy::TabPlacement::Right,
            rustpy_settings::tab_policy::TabPlacement::Top,
        ] {
            app.settings.editor.tabs.placement = placement;
            app.settings.editor.tabs.multiple_rows = true;
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(220.0, 300.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| app.tab_bar(ui, 0));
                },
            );
            let shapes = output.shapes.clone();
            output.drop_without_applying_deltas();
            for name in names {
                let painted = shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text().contains(name) => {
                            Some((shape.clip_rect, text))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    painted.len(),
                    1,
                    "distinct filename must be painted: {name}"
                );
                let (clip, text) = painted[0];
                assert!(
                    clip.contains_rect(Rect::from_min_size(text.pos, text.galley.size())),
                    "filename galley must fit its visible clip: {name}; placement={placement:?}; clip={clip:?}; pos={:?}; galley_size={:?}; painted={:?}",
                    text.pos,
                    text.galley.size(),
                    text.galley.text()
                );
            }
        }
    }

    #[test]
    fn actual_narrow_duplicate_titles_keep_folder_context_and_literal_ellipsis() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = app_with(&[]);
        for folder in ["alpha_π", "beta_β"] {
            let parent = directory
                .path()
                .join("very_long_shared_prefix_for_duplicate_filenames")
                .join(folder);
            std::fs::create_dir_all(&parent).unwrap();
            let path = parent.join("same.py");
            std::fs::write(&path, "value = 1\n").unwrap();
            app.open_file(&path);
        }
        let special = directory.path().join("………");
        std::fs::write(&special, "x").unwrap();
        app.open_file(&special);
        app.tabs[0].preview = true;
        app.settings.editor.tabs.show_full_path = true;
        app.settings.editor.tabs.placement = rustpy_settings::tab_policy::TabPlacement::Left;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(220.0, 300.0))),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.tab_bar(ui, 0));
            },
        );
        let shapes = output.shapes.clone();
        output.drop_without_applying_deltas();
        for folder in ["alpha_π", "beta_β"] {
            assert!(shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains(folder) && text.galley.text().contains("same.py") && shape.clip_rect.contains_rect(Rect::from_min_size(text.pos,text.galley.size())))), "duplicate folder identity must remain visible: {folder}");
        }
    }

    #[test]
    fn keyboard_tab_menu_follows_editor_identity_through_reorder_and_retires_reset() {
        fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
        fn frame(
            app: &mut IdeApp,
            ctx: &egui::Context,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    app.tab_bar(ui, 0);
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        }
        let (directory, paths) = temp_files("keyboard-menu-π", &["first.py", "other.py"]);
        let mut app = app_with(&paths);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        let mut found = false;
        for _ in 0..20 {
            let update = frame(&mut app, &ctx, key(egui::Key::Tab, egui::Modifiers::NONE));
            found = update.nodes.iter().any(|(id, node)| {
                node.role() == egui::accesskit::Role::Tab
                    && node.label().is_some_and(|label| label.contains("first.py"))
                    && ctx
                        .memory(|memory| memory.focused())
                        .is_some_and(|focus| focus.accesskit_id() == *id)
            });
            if found {
                break;
            }
        }
        assert!(found, "real Tab reaches the named first tab");
        let focused = ctx.memory(|memory| memory.focused()).unwrap();
        let editor = app
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_ref() == Some(&paths[0]))
            .unwrap()
            .view
            .id;
        app.tabs.swap(0, 1);
        frame(&mut app, &ctx, vec![]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(focused),
            "focus follows editor, not ordinal"
        );
        frame(
            &mut app,
            &ctx,
            key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        );
        let update = frame(&mut app, &ctx, vec![]);
        for label in ["Close Other Tabs", "Split Right", "Copy Path…"] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(label)),
                "existing menu exposes {label}"
            );
        }
        assert!(egui::Popup::is_any_open(&ctx));
        let index = app
            .tabs
            .iter()
            .position(|tab| tab.view.id == editor)
            .unwrap();
        app.tabs[index].state.doc.reset("replacement = '😀'\n");
        frame(&mut app, &ctx, vec![]);
        assert!(
            !egui::Popup::is_any_open(&ctx),
            "same tab ID cannot preserve a stale document menu"
        );
        assert_eq!(app.tabs[index].state.doc.text(), "replacement = '😀'\n");
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "# first.py\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn copy_menus_use_clicked_background_tab_without_activation() {
        let (directory, paths) = temp_files("copy-path-target", &["first.py", "other.py"]);
        std::fs::write(
            &paths[0],
            "class Sample:\n    def method(self):\n        pass\n",
        )
        .unwrap();
        let mut app = app_with(&paths);
        let first = app
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_ref() == Some(&paths[0]))
            .unwrap();
        assert_ne!(app.active, Some(first));
        assert_eq!(
            app.tabs[first].state.doc.text(),
            std::fs::read_to_string(&paths[0]).unwrap()
        );
        app.tabs[first].state.go_to_line(3, Some(9));
        let active = app.active;
        let ctx = egui::Context::default();
        app.handle_tab_event(TabEvent::CopyPath(first), &ctx);
        let items = &app.choice.as_ref().unwrap().items;
        assert!(items
            .iter()
            .any(|(name, value)| name == "Path With Line Number"
                && value == &format!("{}:3", paths[0].display())));
        assert_eq!(app.active, active);
        // No project in this fixture: copying a reference cannot invent a module.
        app.pending_copy = Some("old clipboard".into());
        app.handle_tab_event(TabEvent::CopyReference(first), &ctx);
        assert_eq!(app.pending_copy.as_deref(), Some("old clipboard"));
        assert_eq!(app.active, active);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn identical_names_get_the_shortest_distinguishing_folders() {
        let a = PathBuf::from("/p/app1/models.py");
        let b = PathBuf::from("/p/app2/models.py");
        let c = PathBuf::from("/p/main.py");
        let d = PathBuf::from("/x/src/pkg/__init__.py");
        let e = PathBuf::from("/y/src/pkg/__init__.py");
        let titles = unique_titles(&[Some(&a), Some(&b), Some(&c), Some(&d), Some(&e), None]);
        assert_eq!(
            titles,
            [
                "app1/models.py",
                "app2/models.py",
                "main.py",
                "x/src/pkg/__init__.py",
                "y/src/pkg/__init__.py",
                "Untitled"
            ]
        );
    }

    #[test]
    fn bulk_close_selection_respects_group_and_pins() {
        // (group, pinned) per tab.
        let tabs = [(0, true), (0, false), (1, false), (0, false), (0, false)];
        assert_eq!(
            tabs_to_close(&tabs, 0, Some(3), None, false),
            vec![1, 4],
            "others: same group, unpinned"
        );
        assert_eq!(
            tabs_to_close(&tabs, 0, Some(3), Some(false), false),
            vec![1],
            "to the left"
        );
        assert_eq!(
            tabs_to_close(&tabs, 0, Some(1), Some(true), false),
            vec![3, 4],
            "to the right"
        );
        assert_eq!(
            tabs_to_close(&tabs, 0, None, None, false),
            vec![1, 3, 4],
            "all but pinned"
        );
        assert_eq!(
            tabs_to_close(&tabs, 0, None, None, true),
            vec![0, 1, 3, 4],
            "all"
        );
        assert_eq!(tabs_to_close(&tabs, 1, None, None, true), vec![2]);
    }

    #[test]
    fn indices_after_a_close() {
        assert_eq!(
            index_after_removal(Some(2), 0),
            Some(1),
            "a tab right of the closed one shifts left"
        );
        assert_eq!(index_after_removal(Some(0), 1), Some(0));
        assert_eq!(index_after_removal(Some(1), 1), None);
        assert_eq!(index_after_removal(None, 0), None);
        // Groups 0 0 1 0 1: the left neighbour in the same group, else the next one.
        let groups = [0, 0, 1, 0, 1];
        assert_eq!(neighbour_after_close(&groups, 3), Some(1));
        assert_eq!(
            neighbour_after_close(&groups, 0),
            Some(0),
            "the next tab of group 0 (index 1) becomes index 0"
        );
        assert_eq!(neighbour_after_close(&groups, 4), Some(2));
        assert_eq!(
            neighbour_after_close(&groups, 2),
            Some(3),
            "the next tab of group 1 (index 4) becomes index 3"
        );
        assert_eq!(
            neighbour_after_close(&[1, 0], 0),
            None,
            "the group is empty now"
        );
        assert_eq!(neighbour_after_close(&groups, 9), None);
    }

    /// Files `names` in a fresh directory (canonical paths, as `open_file` keeps them).
    fn temp_files(tag: &str, names: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!("rustpy-tabs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, format!("# {n}\n")).unwrap();
                dunce::canonicalize(&p).unwrap()
            })
            .collect();
        (dir, paths)
    }

    /// An IDE without a project that has `paths` open (the last one active).
    fn app_with(paths: &[PathBuf]) -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.forget_all_tabs();
        for p in paths {
            app.open_file(p);
        }
        app
    }

    fn titles(app: &IdeApp) -> Vec<String> {
        app.tabs.iter().map(|t| t.state.title()).collect()
    }

    fn active_title(app: &IdeApp) -> Option<String> {
        app.active
            .and_then(|i| app.tabs.get(i))
            .map(|t| t.state.title())
    }

    #[test]
    fn closing_a_background_tab_keeps_the_active_editor() {
        let (dir, paths) = temp_files("background", &["main.py", "hints.py", "dnd.py", "extra.py"]);
        let mut app = app_with(&paths[..3]);
        assert_eq!(active_title(&app).as_deref(), Some("dnd.py"));
        // The × of the first tab (a tab left of the active one).
        app.request_close_tab(0);
        assert_eq!(titles(&app), ["hints.py", "dnd.py"]);
        assert_eq!(
            active_title(&app).as_deref(),
            Some("dnd.py"),
            "the active file never switches when another tab closes"
        );
        // A tab right of the active one.
        app.open_file(&paths[3]);
        app.open_file(&paths[1]);
        app.request_close_tab(2);
        assert_eq!(titles(&app), ["hints.py", "dnd.py"]);
        assert_eq!(active_title(&app).as_deref(), Some("hints.py"));
        // Closing the active tab selects its left neighbour, the first one its right neighbour.
        app.open_file(&paths[3]);
        app.request_close_tab(2);
        assert_eq!(active_title(&app).as_deref(), Some("dnd.py"));
        app.active = Some(0);
        app.request_close_tab(0);
        assert_eq!(active_title(&app).as_deref(), Some("dnd.py"));
        app.request_close_tab(0);
        assert_eq!(app.active, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_tabs_in_split_groups_and_detached_windows() {
        let (dir, paths) = temp_files("groups", &["a.py", "b.py", "c.py", "d.py"]);
        let mut app = app_with(&paths);
        app.split = Some(SplitDir::Horizontal);
        app.tabs[2].group = 1;
        app.tabs[3].group = 1;
        app.active = Some(0);
        app.group_active = [Some(app.tabs[0].view.id), Some(app.tabs[3].view.id)];
        app.detached = Some(2);
        // d.py is what group 1 shows: c.py takes its place, a.py stays active.
        app.close_tab(3);
        assert_eq!(active_title(&app).as_deref(), Some("a.py"));
        assert_eq!(
            app.group_active_tab(1)
                .map(|i| app.tabs[i].state.title())
                .as_deref(),
            Some("c.py")
        );
        // The detached editor (c.py) keeps its window while other tabs close.
        app.close_tab(0);
        assert_eq!(active_title(&app).as_deref(), Some("b.py"));
        assert_eq!(
            app.detached.map(|i| app.tabs[i].state.title()).as_deref(),
            Some("c.py")
        );
        // Group 0 runs empty: group 1's editor becomes the active one.
        app.close_tab(0);
        assert_eq!(active_title(&app).as_deref(), Some("c.py"));
        app.close_tab(0);
        assert_eq!(
            app.detached, None,
            "closing the detached editor closes its window"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopen_closed_tab_after_close_other_tabs_restores_the_order() {
        let (dir, paths) = temp_files("reopen", &["main.py", "hints.py", "dnd.py"]);
        let mut app = app_with(&paths);
        app.close_other_tabs(2);
        assert_eq!(titles(&app), ["dnd.py"]);
        app.reopen_closed_tab();
        assert_eq!(titles(&app), ["hints.py", "dnd.py"]);
        assert_eq!(active_title(&app).as_deref(), Some("hints.py"));
        app.handle_action(Action::ReopenClosedTab, &egui::Context::default());
        assert_eq!(
            titles(&app),
            ["main.py", "hints.py", "dnd.py"],
            "each tab comes back where it was"
        );
        assert_eq!(active_title(&app).as_deref(), Some("main.py"));
        // Files deleted since are skipped; nothing left is a no-op.
        app.close_tab(0);
        app.close_tab(0);
        std::fs::remove_file(&paths[1]).unwrap();
        app.reopen_closed_tab();
        assert_eq!(titles(&app), ["main.py", "dnd.py"]);
        app.reopen_closed_tab();
        assert_eq!(titles(&app), ["main.py", "dnd.py"]);
        assert!(app.closed_tabs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tabs_over_the_limit_can_be_reopened() {
        let (dir, paths) = temp_files("limit", &["a.py", "b.py", "c.py"]);
        let mut app = app_with(&[]);
        app.settings.editor.tab_limit = 2;
        app.open_file(&paths[0]);
        app.open_file(&paths[1]);
        // Detach an actual B editor before C triggers the limit/LRU close path.
        app.handle_action(Action::DetachTab, &egui::Context::default());
        app.open_file(&paths[2]);
        assert_eq!(titles(&app), ["b.py", "c.py"]);
        assert_eq!(
            app.closed_tabs.last().map(|c| c.path.clone()),
            Some(paths[0].clone())
        );
        assert_eq!(
            app.detached.map(|i| app.tabs[i].state.title()).as_deref(),
            Some("b.py"),
            "the detached index follows its editor"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Alt+1 project traversal does not activate the tree on the Windows runner; see known issues"
    )]
    fn actual_project_activation_tab_traverses_to_editor_tab_and_its_menu() {
        fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
        fn frame(
            app: &mut IdeApp,
            ctx: &egui::Context,
            time: f64,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut native_frame = eframe::Frame::_new_kittest();
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
                    ..Default::default()
                },
                |ui| eframe::App::ui(app, ui, &mut native_frame),
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("π.py");
        std::fs::write(&path, "π = 1\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        let identity = app.tabs[app.active.unwrap()].state.doc.identity();
        let version = app.tabs[app.active.unwrap()].state.doc.version();
        frame(
            &mut app,
            &ctx,
            0.2,
            key(
                egui::Key::Num1,
                egui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
        );
        frame(&mut app, &ctx, 0.3, vec![]);
        assert!(
            ProjectTree::has_focus(&ctx),
            "real Alt+1 activates the actual Project tree"
        );
        let tree_selection = app.project.as_ref().unwrap().tree.selected.clone();
        let mut time = 0.4;
        let mut update = frame(
            &mut app,
            &ctx,
            time,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        assert!(
            !ProjectTree::has_focus(&ctx),
            "ordinary Tab must leave the tree focus owner"
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(egui::Id::new("project_panel").with("__resize")),
            "the first Tab reaches the actual Project resize response"
        );
        let named_tab_focused = |update: &egui::accesskit::TreeUpdate| {
            update.nodes.iter().any(|(id, node)| {
                node.role() == egui::accesskit::Role::Tab
                    && node.label().is_some_and(|label| label.contains("π.py"))
                    && ctx
                        .memory(|memory| memory.focused())
                        .is_some_and(|focus| focus.accesskit_id() == *id)
            })
        };
        // Data only: no per-frame formatting, String allocation, focus changes
        // or extra frames. The fixed array covers resize plus exactly 20 Tabs.
        let snapshot = |update: &egui::accesskit::TreeUpdate, time: f64, app: &IdeApp| {
            let named = || {
                update.nodes.iter().filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Tab
                        && node.label().is_some_and(|label| label.contains("π.py"))
                })
            };
            (
                time,
                ctx.memory(|memory| memory.focused()),
                update.focus,
                named().count(),
                named().next().map(|(id, _)| *id),
                app.active
                    .and_then(|index| app.tabs.get(index))
                    .map(|tab| tab.view.id),
                app.focus_editor,
                app.editor_keyboard(),
                ProjectTree::has_focus(&ctx),
                matches!(app.dialog, Dialog::None),
            )
        };
        let mut focus_snapshots = [None; 21];
        focus_snapshots[0] = Some(snapshot(&update, time, &app));
        let mut snapshot_count = 1;
        for _ in 0..20 {
            if named_tab_focused(&update) {
                break;
            }
            time += 0.1;
            update = frame(
                &mut app,
                &ctx,
                time,
                key(egui::Key::Tab, egui::Modifiers::NONE),
            );
            focus_snapshots[snapshot_count] = Some(snapshot(&update, time, &app));
            snapshot_count += 1;
        }
        if !named_tab_focused(&update) {
            eprintln!(
                "PROJECT_TAB_FRAME_FIELDS time,memory_focus,accesskit_focus,named_tab_count,first_named_tab,active_view,queued_editor,keyboard,tree_focus,dialog_none"
            );
            for (ordinal, snapshot) in focus_snapshots[..snapshot_count].iter().enumerate() {
                eprintln!("PROJECT_TAB_FRAME ordinal={ordinal} data={snapshot:?}");
            }
            eprintln!(
                "PROJECT_TAB_FAILURE time={time} focus={:?} accesskit_focus={:?} queued_editor={} editor_keyboard={} active={:?} tree_focus={} dialog_none={}",
                ctx.memory(|memory| memory.focused()),
                update.focus,
                app.focus_editor,
                app.editor_keyboard(),
                app.active,
                ProjectTree::has_focus(&ctx),
                matches!(app.dialog, Dialog::None),
            );
            for (id, node) in &update.nodes {
                eprintln!(
                    "PROJECT_TAB_NODE id={id:?} role={:?} label={:?} value={:?}",
                    node.role(),
                    node.label(),
                    node.value()
                );
            }
        }
        assert!(
            named_tab_focused(&update),
            "bounded real Tab traversal reaches the actual named editor tab"
        );
        frame(
            &mut app,
            &ctx,
            time + 0.1,
            key(
                egui::Key::F10,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    alt: true,
                    ..Default::default()
                },
            ),
        );
        let menu = frame(&mut app, &ctx, time + 0.2, vec![]);
        let labels: Vec<_> = menu
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label())
            .collect();
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Close Other Tabs")),
            "actual Tab menu opens: {labels:?}"
        );
        assert!(
            !labels.iter().any(|label| label.contains("New Directory")),
            "the Project menu must not retain the chord: {labels:?}"
        );
        assert_eq!(app.project.as_ref().unwrap().tree.selected, tree_selection);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.identity(), identity);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.version(), version);
        assert!(!app.tabs[app.active.unwrap()].state.doc.can_undo());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "π = 1\n");
    }

    #[test]
    fn actual_app_theme_styles_each_fresh_context_independently() {
        fn paint(app: &mut IdeApp, ctx: &egui::Context) {
            let mut native_frame = eframe::Frame::_new_kittest();
            let output = ctx.run_ui(
                egui::RawInput {
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
                    ..Default::default()
                },
                |ui| eframe::App::ui(app, ui, &mut native_frame),
            );
            output.drop_without_applying_deltas();
        }
        let mut first_app = IdeApp::new(Settings::default(), None);
        let first = egui::Context::default();
        crate::theme::install_fonts(&first);
        paint(&mut first_app, &first);
        let mut second_app = IdeApp::new(Settings::default(), None);
        let second = egui::Context::default();
        crate::theme::install_fonts(&second);
        second.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(31.0, 37.0);
            style.spacing.button_padding = egui::vec2(41.0, 43.0);
            style.spacing.menu_margin = egui::Margin::same(17);
        });
        paint(&mut second_app, &second);
        let style = second.style_of(second.theme());
        assert_eq!(
            style.spacing.item_spacing,
            egui::vec2(6.0, 4.0),
            "actual second App frame applies its theme spacing"
        );
        assert_eq!(style.spacing.button_padding, egui::vec2(8.0, 3.0));
        assert_eq!(style.spacing.menu_margin, egui::Margin::same(6));
    }
}

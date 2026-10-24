//! Accessibility (roadmap §1.6 Accessibility): names and roles for screen
//! readers (AccessKit, enabled through eframe) on widgets that egui cannot
//! describe by itself: rows and tabs drawn with the painter, and buttons that
//! show only an icon.

pub use crate::accessibility_policy::{reduced_motion, spinner, spinner_sized};
use egui::accesskit::Role;

/// Accessible names for icon-only widgets.
pub trait Accessible {
    /// Shows `hint` as the tooltip and makes it the widget's accessible name;
    /// a trailing `(shortcut)` becomes the keyboard shortcut instead
    /// ("Run (Shift+F10)": name "Run", shortcut "Shift+F10").
    fn icon_hint(self, hint: &str) -> Self;
}

impl Accessible for egui::Response {
    fn icon_hint(self, hint: &str) -> Self {
        let (label, shortcut) = split_hint(hint);
        let hover = if crate::custom_menus::tooltip_shortcuts(&self.ctx) {
            hint.to_string()
        } else {
            label.clone()
        };
        self.ctx.accesskit_node_builder(self.id, |node| {
            node.set_label(label);
            if let Some(s) = shortcut {
                node.set_keyboard_shortcut(s);
            }
        });
        self.on_hover_text(hover)
    }
}

/// `"Run (Shift+F10)"` → `("Run", Some("Shift+F10"))`; a hint without a
/// trailing parenthesis is all name.
pub fn split_hint(hint: &str) -> (String, Option<String>) {
    let hint = hint.trim();
    if let Some(open) = hint.rfind(" (") {
        if let Some(inner) = hint[open + 2..].strip_suffix(')') {
            if !inner.is_empty() && !hint[..open].trim().is_empty() {
                return (hint[..open].trim().to_string(), Some(inner.to_string()));
            }
        }
    }
    (hint.to_string(), None)
}

/// Names a widget that egui leaves unnamed; call after it was shown.
pub fn name(ctx: &egui::Context, id: egui::Id, label: &str) {
    ctx.accesskit_node_builder(id, |node| node.set_label(label));
}

/// Accessible names for combo boxes. A combo box built from an id salt has no
/// name of its own, so a screen reader announces only "combo box"; chain one of
/// these onto its `show_ui` / `show_index` result. The source lint in the tests
/// below rejects an id-salted combo box without one.
pub trait AccessibleCombo {
    /// Labelled by the visible `label` beside it (`let label = ui.label(…)`), so
    /// the name is the text shown, in the UI language.
    fn combo_labelled(self, label: &egui::Response) -> Self;
    /// Named `name` (translated by the caller) when no visible label describes it.
    fn combo_named(self, name: &str) -> Self;
}

impl AccessibleCombo for egui::Response {
    fn combo_labelled(self, label: &egui::Response) -> Self {
        self.labelled_by(label.id)
    }
    fn combo_named(self, name: &str) -> Self {
        crate::a11y::name(&self.ctx, self.id, name);
        self
    }
}

impl<R> AccessibleCombo for egui::InnerResponse<R> {
    fn combo_labelled(mut self, label: &egui::Response) -> Self {
        self.response = self.response.combo_labelled(label);
        self
    }
    fn combo_named(mut self, name: &str) -> Self {
        self.response = self.response.combo_named(name);
        self
    }
}

/// Keeps Tab / Shift+Tab (and pointer input) inside the dialog window on
/// `layer` while it is shown: widgets behind it no longer take part in the
/// focus cycle, so the keyboard cannot leave a modal dialog for the main
/// window. Call every frame the dialog is shown; popups and windows opened on
/// top of it stay usable.
pub fn trap_focus(ctx: &egui::Context, layer: egui::LayerId) {
    ctx.memory_mut(|memory| memory.set_modal_layer(layer));
}

/// [`trap_focus`] for the result of `egui::Window::show`.
pub fn trap_window_focus<R>(ctx: &egui::Context, window: Option<&egui::InnerResponse<R>>) {
    if let Some(window) = window {
        trap_focus(ctx, window.response.layer_id);
    }
}

/// How a window takes part in keyboard navigation. Every `egui::Window` in the
/// UI is shown through one of these instead of a plain `show` (the source lint
/// in the tests below rejects one that is not), so each new dialog decides
/// whether it blocks the main window.
pub trait DialogWindow {
    /// Shows a modal dialog: while it is open, Tab / Shift+Tab and the pointer
    /// stay inside it (and in popups or dialogs opened on top of it), as in a
    /// native modal dialog. See [`trap_focus`].
    fn show_modal<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>>;
    /// Shows a non-modal tool window, preview or popup that is used alongside
    /// the main window: the keyboard can move between it and the IDE.
    fn show_modeless<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>>;
}

impl DialogWindow for egui::Window<'_> {
    fn show_modal<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>> {
        let response = self.show(ctx, add_contents);
        trap_window_focus(ctx, response.as_ref());
        response
    }

    fn show_modeless<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>> {
        self.show(ctx, add_contents)
    }
}

/// A button drawn with the painter (the × that closes a tab).
pub fn button(resp: &egui::Response, label: &str) {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
}

/// A tab drawn with the painter: role Tab, selected when it is the active one.
pub fn tab(resp: &egui::Response, title: &str, selected: bool, modified: bool) {
    let label = if modified {
        format!("{title}, modified")
    } else {
        title.to_string()
    };
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Tab);
        node.clear_toggled();
        node.set_selected(selected);
    });
}

/// A row of a tree drawn with the painter: role TreeItem, its level (1 for
/// the root), expanded or collapsed for folders, and selected.
pub fn tree_item(
    resp: &egui::Response,
    label: &str,
    level: usize,
    expanded: Option<bool>,
    selected: bool,
) {
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::TreeItem);
        node.clear_toggled();
        node.set_level(level);
        node.set_selected(selected);
        match expanded {
            Some(e) => node.set_expanded(e),
            None => node.clear_expanded(),
        }
    });
}

/// The `Ui` (or widget) `id` holding tree items: role Tree, named `label`, with
/// the selected row as its active descendant, so a screen reader announces
/// the selection as it moves while the tree itself keeps the keyboard focus.
pub fn tree(ctx: &egui::Context, id: egui::Id, label: &str, active: Option<egui::Id>) {
    ctx.accesskit_node_builder(id, |node| {
        node.set_role(Role::Tree);
        node.set_label(label);
        match active {
            Some(a) => node.set_active_descendant(a.accesskit_id()),
            None => node.clear_active_descendant(),
        }
    });
}

/// A stable non-focusable polite live region, linked to its actual owner.
///
/// egui attaches a node it has not seen this pass to its nearest registered
/// ancestor (the root for this synthetic id), so the region is detached from
/// the root before it is linked to `owner`: a node listed under two parents
/// (or twice under one) makes the platform adapter (accesskit_consumer) panic
/// with "TreeUpdate includes duplicate child" as soon as a screen reader
/// activates the tree.
pub fn announce(ctx: &egui::Context, owner: egui::Id, text: &str) {
    let id = owner.with("announcement");
    let child = id.accesskit_id();
    ctx.accesskit_node_builder(owner, |_| ());
    ctx.accesskit_node_builder(id, |node| {
        node.set_role(Role::Status);
        node.set_label(text);
        node.set_live(egui::accesskit::Live::Polite);
        node.set_live_atomic();
    });
    ctx.accesskit_node_builder(egui::accesskit_root_id(), |root| {
        if root.children().contains(&child) {
            let kept: Vec<_> = root
                .children()
                .iter()
                .copied()
                .filter(|c| *c != child)
                .collect();
            root.set_children(kept);
        }
    });
    ctx.accesskit_node_builder(owner, |node| {
        if !node.children().contains(&child) {
            node.push_child(child);
        }
    });
}

/// The same context menu for pointer users and the focused keyboard owner.
/// Capture the response rectangle so moving focus into its controls does not
/// move the keyboard-opened menu. Callers supply their current modal authority.
pub(crate) fn keyboard_context_menu(response: &egui::Response, available: bool) -> egui::Popup<'_> {
    context_menu_with_key(response, available, egui::Modifiers::SHIFT, "Shift+F10")
}

/// Focused component fallback: default keymaps leave Ctrl Alt F10 unbound,
/// retaining the IDE Run meaning of Shift F10 outside reviewed tool owners.
pub(crate) fn keyboard_context_menu_fallback(
    response: &egui::Response,
    available: bool,
) -> egui::Popup<'_> {
    context_menu_with_key(
        response,
        available,
        egui::Modifiers::CTRL | egui::Modifiers::ALT,
        "Ctrl+Alt+F10",
    )
}

/// A menu for a retained response whose underlying document/item can change.
/// Retire the previous popup before painting a different authority, even if
/// the response keeps its focus ID. Pointer and keyboard share the same menu.
pub(crate) fn bound_keyboard_context_menu<'a>(
    response: &'a egui::Response,
    available: bool,
    authority: egui::Id,
) -> egui::Popup<'a> {
    bound_context_menu_for_owner(response, available, response.has_focus(), authority)
}

/// Trees retain focus on their parent rather than on individual painted rows.
/// The caller must bind `keyboard_owner` to its actual focused, selected row.
pub(crate) fn bound_context_menu_for_owner<'a>(
    response: &'a egui::Response,
    available: bool,
    keyboard_owner: bool,
    authority: egui::Id,
) -> egui::Popup<'a> {
    let base = egui::Popup::default_response_id(response);
    let id = base.with(authority);
    let retained = base.with("bound-authority");
    if let Some(previous) = response
        .ctx
        .data(|data| data.get_temp::<egui::Id>(retained))
    {
        if previous != id {
            egui::Popup::close_id(&response.ctx, previous);
        }
    }
    response.ctx.data_mut(|data| data.insert_temp(retained, id));
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_keyboard_shortcut("Ctrl+Alt+F10");
    });
    let anchor_id = id.with("keyboard-anchor");
    if !egui::Popup::is_id_open(&response.ctx, id) {
        response
            .ctx
            .data_mut(|data| data.remove::<egui::Rect>(anchor_id));
    }
    if available
        && keyboard_owner
        && !egui::Popup::is_any_open(&response.ctx)
        && response.ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, egui::Key::F10)
        })
    {
        response
            .ctx
            .data_mut(|data| data.insert_temp(anchor_id, response.rect));
        egui::Popup::open_id(&response.ctx, id);
    }
    let popup = egui::Popup::context_menu(response).id(id);
    match response
        .ctx
        .data(|data| data.get_temp::<egui::Rect>(anchor_id))
    {
        Some(anchor) => popup.anchor(anchor),
        None => popup,
    }
}

fn context_menu_with_key<'a>(
    response: &'a egui::Response,
    available: bool,
    modifiers: egui::Modifiers,
    shortcut: &str,
) -> egui::Popup<'a> {
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_keyboard_shortcut(shortcut));
    let id = egui::Popup::default_response_id(response);
    let anchor_id = id.with("keyboard-anchor");
    if !egui::Popup::is_id_open(&response.ctx, id) {
        response
            .ctx
            .data_mut(|data| data.remove::<egui::Rect>(anchor_id));
    }
    let open = available
        && response.has_focus()
        && !egui::Popup::is_any_open(&response.ctx)
        && response
            .ctx
            .input_mut(|input| input.consume_key(modifiers, egui::Key::F10));
    let popup = egui::Popup::context_menu(response);
    if open {
        response
            .ctx
            .data_mut(|data| data.insert_temp(anchor_id, response.rect));
        egui::Popup::open_id(&response.ctx, id);
    }
    match response
        .ctx
        .data(|data| data.get_temp::<egui::Rect>(anchor_id))
    {
        Some(anchor) => popup.anchor(anchor),
        None => popup,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every node has one parent, as the platform adapter requires: a live
    /// region listed under its owner and also under the root made
    /// accesskit_consumer panic ("TreeUpdate includes duplicate child") the
    /// moment Orca enabled the tree.
    #[test]
    fn announcement_region_has_exactly_one_parent() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let owner = egui::Id::new("announcing-owner");
        for _ in 0..2 {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let resp = ui.interact(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
                    owner,
                    egui::Sense::click(),
                );
                announce(ui.ctx(), resp.id, "Line 1, column 1");
                // A second announcement in the same pass replaces the text only.
                announce(ui.ctx(), resp.id, "Line 1, column 2");
            });
            let update = output.platform_output.accesskit_update.as_ref().unwrap();
            let region = owner.with("announcement").accesskit_id();
            let mut seen = std::collections::HashSet::new();
            for (_, node) in &update.nodes {
                for child in node.children() {
                    assert!(seen.insert(*child), "duplicate child {child:?}");
                }
            }
            let parents: Vec<_> = update
                .nodes
                .iter()
                .filter(|(_, n)| n.children().contains(&region))
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(parents, vec![owner.accesskit_id()]);
            let label = update
                .nodes
                .iter()
                .find(|(id, _)| *id == region)
                .and_then(|(_, n)| n.label().map(str::to_owned));
            assert_eq!(label.as_deref(), Some("Line 1, column 2"));
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn context_fallback_is_unbound_while_run_chord_is_preserved() {
        let keys = crate::actions::Keymap::default_keymap();
        assert_eq!(
            keys.lookup(crate::actions::KeyChord::parse("Ctrl+Alt+F10").unwrap()),
            None
        );
        assert_eq!(
            keys.lookup(crate::actions::KeyChord::parse("Shift+F10").unwrap()),
            Some(crate::actions::Action::Run)
        );
    }

    #[test]
    fn bound_menu_retires_changed_target_and_preserves_foreign_owner_key() {
        let ctx = egui::Context::default();
        let response_id = egui::Id::new("retained-editor-response");
        let first = egui::Id::new(("document", 1_u64, "π.py"));
        let second = egui::Id::new(("document", 2_u64, "π.py"));
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::ALT;
        let key = || egui::Event::Key {
            key: egui::Key::F10,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let frame = |authority, available, events: Vec<egui::Event>| {
            let mut popup_id = None;
            let mut remaining = false;
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = ui.interact(
                        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 80.0)),
                        response_id,
                        egui::Sense::click(),
                    );
                    popup_id = Some(egui::Popup::default_response_id(&response).with(authority));
                    bound_keyboard_context_menu(&response, available, authority).show(|ui| {
                        let _ = ui.button("Copy captured source");
                    });
                    remaining = ui.input(|input| {
                        input.events.iter().any(|event| {
                            matches!(
                                event,
                                egui::Event::Key {
                                    key: egui::Key::F10,
                                    ..
                                }
                            )
                        })
                    });
                },
            );
            output.drop_without_applying_deltas();
            (popup_id.unwrap(), remaining)
        };
        frame(first, true, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(response_id));
        let (old_popup, remaining) = frame(
            first,
            true,
            vec![egui::Event::ModifiersChanged(modifiers), key()],
        );
        assert!(
            !remaining,
            "the focused editor owns its explicit context chord"
        );
        assert!(egui::Popup::is_id_open(&ctx, old_popup));
        let (new_popup, _) = frame(second, true, vec![]);
        assert!(!egui::Popup::is_id_open(&ctx, old_popup));
        assert!(
            !egui::Popup::is_id_open(&ctx, new_popup),
            "same response focus must not reopen a replaced document's menu"
        );
        ctx.memory_mut(|memory| memory.request_focus(response_id));
        let (_, remaining) = frame(
            second,
            false,
            vec![egui::Event::ModifiersChanged(modifiers), key()],
        );
        assert!(remaining, "a competing owner retains its input");
        assert!(!egui::Popup::is_id_open(&ctx, new_popup));
    }

    #[test]
    fn hints_split_into_name_and_shortcut() {
        assert_eq!(
            split_hint("Run (Shift+F10)"),
            ("Run".to_string(), Some("Shift+F10".to_string()))
        );
        assert_eq!(
            split_hint("Hide (Shift+Escape)"),
            ("Hide".to_string(), Some("Shift+Escape".to_string()))
        );
        assert_eq!(
            split_hint("Collapse All"),
            ("Collapse All".to_string(), None)
        );
        assert_eq!(
            split_hint("Next Occurrence (F3)"),
            ("Next Occurrence".to_string(), Some("F3".to_string()))
        );
        // Parentheses that are not a trailing shortcut stay in the name.
        assert_eq!(
            split_hint("Open (in a new window) now"),
            ("Open (in a new window) now".to_string(), None)
        );
        assert_eq!(split_hint("(F3)"), ("(F3)".to_string(), None));
        assert_eq!(split_hint("Stop ()"), ("Stop ()".to_string(), None));
    }

    #[test]
    fn tree_rows_and_tabs_get_roles_and_states() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut row_id = None;
        let mut tab_id = None;
        let tree_id = egui::Id::new("test-tree");
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let resp = ui.scope_builder(
                egui::UiBuilder::new()
                    .id(tree_id)
                    .sense(egui::Sense::FOCUSABLE),
                |ui| {
                    let (_, row) =
                        ui.allocate_exact_size(egui::vec2(100.0, 20.0), egui::Sense::CLICK);
                    tree_item(&row, "pkg", 2, Some(false), true);
                    row_id = Some(row.id);
                    let (_, t) =
                        ui.allocate_exact_size(egui::vec2(100.0, 20.0), egui::Sense::click());
                    tab(&t, "main.py", true, true);
                    tab_id = Some(t.id);
                },
            );
            tree(ui.ctx(), resp.response.id, "Project", row_id);
        });
        output.textures_delta.clear();
        let update = output
            .platform_output
            .accesskit_update
            .expect("accesskit is enabled");
        let node = |id: egui::Id| {
            update
                .nodes
                .iter()
                .find(|(n, _)| *n == id.accesskit_id())
                .map(|(_, node)| node.clone())
                .expect("node")
        };
        let row = node(row_id.unwrap());
        assert_eq!(row.role(), Role::TreeItem);
        assert_eq!(row.label(), Some("pkg"));
        assert_eq!(
            (
                row.level(),
                row.is_expanded(),
                row.is_selected(),
                row.toggled()
            ),
            (Some(2), Some(false), Some(true), None)
        );
        let tree_node = node(tree_id);
        assert_eq!(tree_node.role(), Role::Tree);
        assert_eq!(tree_node.label(), Some("Project"));
        assert_eq!(
            tree_node.active_descendant(),
            Some(row_id.unwrap().accesskit_id())
        );
        let t = node(tab_id.unwrap());
        assert_eq!(
            (t.role(), t.label(), t.is_selected()),
            (Role::Tab, Some("main.py, modified"), Some(true))
        );
    }

    /// The end of the expression statement starting at `start`: a `;` or `,`
    /// at its own nesting depth, or the bracket closing the enclosing one.
    /// String and char literals are skipped so their brackets do not count.
    fn statement_end(src: &str, start: usize) -> usize {
        let bytes = src.as_bytes();
        let mut depth = 0_i32;
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    // A raw string (`r"…"`, `r#"…"#`) ends at the quote and its hashes.
                    let mut hashes = 0;
                    let mut j = i;
                    while j > 0 && bytes[j - 1] == b'#' {
                        hashes += 1;
                        j -= 1;
                    }
                    let raw = j > 0 && bytes[j - 1] == b'r';
                    i += 1;
                    while i < bytes.len() {
                        if !raw && bytes[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if bytes[i] == b'"'
                            && (!raw || bytes[i + 1..].iter().take(hashes).all(|b| *b == b'#'))
                        {
                            i += if raw { hashes } else { 0 };
                            break;
                        }
                        i += 1;
                    }
                }
                b'\'' => {
                    // A char literal ('x', '\n', '{'); otherwise a lifetime.
                    if bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        while i < bytes.len() && bytes[i] != b'\'' {
                            i += 1;
                        }
                    } else if let Some(c) = src[i + 1..].chars().next() {
                        if src[i + 1 + c.len_utf8()..].starts_with('\'') {
                            i += c.len_utf8() + 1;
                        }
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth < 0 {
                        return i;
                    }
                }
                b';' | b',' if depth == 0 => return i,
                _ => {}
            }
            i += 1;
        }
        bytes.len()
    }

    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Unlabelled combo boxes in `src` (line numbers): an id-salted combo box
    /// whose statement does not name it with [`AccessibleCombo`].
    fn unnamed_combo_lines(src: &str) -> Vec<usize> {
        let pattern = concat!("ComboBox::", "from_id_salt(");
        src.match_indices(pattern)
            .map(|(at, _)| at)
            .filter(|&at| {
                let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
                !src[line_start..at].trim_start().starts_with("//")
            })
            .filter(|&at| {
                let statement = &src[at..statement_end(src, at)];
                !statement.contains(".combo_labelled(") && !statement.contains(".combo_named(")
            })
            .map(|at| src[..at].matches('\n').count() + 1)
            .collect()
    }

    #[test]
    fn combo_lint_finds_unnamed_combo_boxes() {
        let unnamed = concat!(
            "fn f(ui: &mut Ui) {\n    egui::ComboBox::",
            "from_id_salt(\"a\").show_ui(ui, |ui| { ui.label(\"}\"); let c = '{'; });\n}\n"
        );
        assert_eq!(unnamed_combo_lines(unnamed), vec![2]);
        let named = concat!(
            "fn f(ui: &mut Ui) {\n    let l = ui.label(\"Theme\");\n    egui::ComboBox::",
            "from_id_salt(\"a\")\n        .show_ui(ui, |ui| { ui.label(\"x\"); })\n",
            "        .combo_labelled(&l);\n    egui::ComboBox::",
            "from_id_salt(\"b\").show_index(ui, &mut i, 2, |i| r#\"(\"#.into()).combo_named(\"Mode\");\n}\n"
        );
        assert!(unnamed_combo_lines(named).is_empty());
        // The name must belong to the same statement, not a later one.
        let later = concat!(
            "fn f(ui: &mut Ui) {\n    egui::ComboBox::",
            "from_id_salt(\"a\").show_ui(ui, |ui| {});\n    x.combo_named(\"b\");\n}\n"
        );
        assert_eq!(unnamed_combo_lines(later), vec![2]);
    }

    /// The text of `src[start..end]` with every bracketed group emptied, so
    /// only the top level of a method chain remains:
    /// `Window::new(…).open(…).show_modal(…)` → `Window::new().open().show_modal()`.
    fn top_level(src: &str, start: usize, end: usize) -> String {
        let mut out = String::new();
        let mut i = start;
        while i < end {
            let c = src[i..].chars().next().unwrap();
            out.push(c);
            if matches!(c, '(' | '[' | '{') {
                let mut j = i + 1;
                loop {
                    let e = statement_end(src, j);
                    if e < src.len() && matches!(src.as_bytes()[e], b',' | b';') {
                        j = e + 1;
                    } else {
                        i = e;
                        break;
                    }
                }
                continue;
            }
            i += c.len_utf8();
        }
        out
    }

    /// Windows in `src` (line numbers) not shown through [`DialogWindow`]:
    /// the `Window::new` chain, or the variable it is bound to, must end in
    /// `show_modal` or `show_modeless`, never a plain `show`.
    fn unclassified_window_lines(src: &str) -> Vec<usize> {
        let pattern = concat!("Window::", "new(");
        let shown = |text: &str| {
            (text.contains(".show_modal(") || text.contains(".show_modeless("))
                && !text.contains(".show(")
        };
        src.match_indices(pattern)
            .map(|(at, _)| at)
            .filter(|&at| {
                let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
                let prefix = src[line_start..at].trim_start();
                if prefix.starts_with("//") {
                    return false;
                }
                let chain = top_level(src, at, statement_end(src, at));
                if shown(&chain) {
                    return false;
                }
                // `let mut window = egui::Window::new(…)…;` shown later as `window.show_…`.
                let binding = prefix
                    .strip_prefix("let ")
                    .map(|rest| rest.trim_start_matches("mut "))
                    .and_then(|rest| rest.split('=').next())
                    .map(str::trim)
                    .filter(|name| {
                        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    });
                match binding {
                    Some(name) => {
                        let rest = &src[at..];
                        let modal = format!("{name}.show_modal(");
                        let modeless = format!("{name}.show_modeless(");
                        let plain = format!("{name}.show(");
                        !((rest.contains(&modal) || rest.contains(&modeless))
                            && !rest.contains(&plain))
                    }
                    None => true,
                }
            })
            .map(|at| src[..at].matches('\n').count() + 1)
            .collect()
    }

    #[test]
    fn window_lint_finds_windows_without_a_modality() {
        let w = concat!("egui::Window::", "new");
        let plain = format!(
            "fn f(ctx: &Context) {{\n    {w}(\"a\").open(&mut o).show(ctx, |ui| {{ \
             ScrollArea::new().show_modal(ui, |ui| {{}}); }});\n}}\n"
        );
        assert_eq!(unclassified_window_lines(&plain), vec![2]);
        let classified = format!(
            "fn f(ctx: &Context) {{\n    {w}(if x {{ \"a\" }} else {{ \"b\" }})\n        \
             .show_modal(ctx, |ui| {{ ui.label(\")\"); }});\n    \
             let r = {w}(\"b\").show_modeless(ctx, |ui| {{ x.show(ui) }});\n}}\n"
        );
        assert!(unclassified_window_lines(&classified).is_empty());
        let bound = format!(
            "fn f(ctx: &Context) {{\n    let mut win = {w}(\"a\");\n    win = win.open(&mut o);\n    \
             win.show_modal(ctx, |ui| {{}});\n    let mut other = {w}(\"b\");\n    \
             other.show(ctx, |ui| {{}});\n}}\n"
        );
        assert_eq!(unclassified_window_lines(&bound), vec![5]);
    }

    /// Every `egui::Window` in the UI sources says whether it is a modal dialog
    /// (focus trapped inside it) or a non-modal tool window, so a new dialog
    /// cannot silently let Tab wander into the main window behind it.
    #[test]
    fn every_window_declares_its_modality() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        files.sort();
        let mut problems = Vec::new();
        let mut modal = 0;
        for file in files {
            let src = std::fs::read_to_string(&file).unwrap();
            modal += src.matches(".show_modal(").count();
            for line in unclassified_window_lines(&src) {
                problems.push(format!(
                    "{}:{line}",
                    file.strip_prefix(&root).unwrap().display()
                ));
            }
        }
        assert!(modal > 100, "the modal dialogs trap focus ({modal})");
        assert!(
            problems.is_empty(),
            "windows shown without a modality (use .show_modal(ctx, …) for a dialog that \
             blocks the main window, .show_modeless(ctx, …) for a tool window, from \
             crate::a11y::DialogWindow):\n{}",
            problems.join("\n")
        );
    }

    /// A modal window keeps Tab inside it; a non-modal one leaves the widgets
    /// behind it in the focus cycle.
    #[test]
    fn modal_windows_trap_tab_and_modeless_windows_do_not() {
        for modal in [true, false] {
            let ctx = egui::Context::default();
            let mut layers = (None, None);
            let tab = egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let mut focused_layers = std::collections::HashSet::new();
            for pass in 0..12 {
                let events = if pass > 1 { vec![tab.clone()] } else { vec![] };
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let main = ui.add(egui::Button::new("Main window button"));
                        let _ = ui.button("Another main button");
                        layers.0 = Some(main.layer_id);
                        let window = egui::Window::new("Dialog").default_pos([300.0, 200.0]);
                        let add = |ui: &mut egui::Ui| {
                            let _ = ui.button("OK");
                            let _ = ui.button("Cancel");
                        };
                        let shown = if modal {
                            window.show_modal(ui.ctx(), add)
                        } else {
                            window.show_modeless(ui.ctx(), add)
                        };
                        layers.1 = shown.map(|r| r.response.layer_id);
                    },
                );
                output.drop_without_applying_deltas();
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    focused_layers.insert(ctx.read_response(id).map(|r| r.layer_id));
                }
            }
            let (main, dialog) = (layers.0.unwrap(), layers.1.unwrap());
            if modal {
                assert_eq!(ctx.memory(|m| m.top_modal_layer()), Some(dialog));
                assert_eq!(focused_layers, [Some(dialog)].into_iter().collect());
            } else {
                assert_eq!(ctx.memory(|m| m.top_modal_layer()), None);
                assert!(
                    focused_layers.contains(&Some(main)),
                    "Tab reaches the main window past a non-modal window: {focused_layers:?}"
                );
            }
        }
    }

    /// Every combo box built from an id salt in the UI sources has an
    /// accessible name: Orca read the unnamed ones as just "combo box".
    #[test]
    fn every_id_salted_combo_box_has_an_accessible_name() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        files.sort();
        let mut problems = Vec::new();
        for file in files {
            let src = std::fs::read_to_string(&file).unwrap();
            for line in unnamed_combo_lines(&src) {
                problems.push(format!(
                    "{}:{line}",
                    file.strip_prefix(&root).unwrap().display()
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "combo boxes without an accessible name (chain .combo_labelled(&label) or \
             .combo_named(tr(…)) from crate::a11y::AccessibleCombo):\n{}",
            problems.join("\n")
        );
    }
}

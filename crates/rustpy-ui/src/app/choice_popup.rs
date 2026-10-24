//! List popups (Surround With, Insert Live Template, Bookmarks, Copy Path,
//! Paste from History, Generate, Refactor This, the declaration / usage /
//! test choosers, Quick List, tasks and contexts). They are drawn at the top
//! level, at the caret of a text editor or centred, so they show over any
//! editor (or none), and they own the keyboard while open: typing filters the
//! rows (speed search), and with nothing typed the digits 1-9 pick a row.

use super::*;

impl ChoicePopup {
    pub(super) fn new(kind: ChoiceKind, title: String, items: Vec<(String, String)>) -> Self {
        Self {
            kind,
            title,
            items,
            selected: 0,
            filter: String::new(),
        }
    }

    /// Indices of the rows the speed search shows, in order.
    pub(super) fn shown(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| choice_matches(&self.items[i].0, &self.items[i].1, &self.filter))
            .collect()
    }

    /// The selected row, if the speed search shows it.
    pub(super) fn current(&self) -> Option<usize> {
        self.shown()
            .contains(&self.selected)
            .then_some(self.selected)
    }

    /// Typed text grows the speed search; the first matching row is selected.
    pub(super) fn type_text(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        // A space alone does not start a search.
        if text.is_empty() || (self.filter.is_empty() && text.trim().is_empty()) {
            return;
        }
        self.filter.push_str(&text);
        self.refilter();
    }

    pub(super) fn backspace(&mut self) {
        if self.filter.pop().is_some() {
            self.refilter();
        }
    }

    /// After the search changed: the row whose key starts
    /// with it, else the selection if still shown, else the first shown row.
    fn refilter(&mut self) {
        let shown = self.shown();
        let filter = self.filter.trim().to_lowercase();
        let closest = shown
            .iter()
            .copied()
            .find(|&i| !filter.is_empty() && self.items[i].0.to_lowercase().starts_with(&filter));
        match closest {
            Some(i) => self.selected = i,
            None if shown.contains(&self.selected) => {}
            None => {
                if let Some(&first) = shown.first() {
                    self.selected = first;
                }
            }
        }
    }

    /// Moves the selection by `delta` among the shown rows (clamped).
    pub(super) fn move_selection(&mut self, delta: i32) {
        let shown = self.shown();
        if shown.is_empty() {
            return;
        }
        let pos = shown.iter().position(|&i| i == self.selected).unwrap_or(0) as i32;
        self.selected = shown[(pos + delta).clamp(0, shown.len() as i32 - 1) as usize];
    }
}

/// Speed search over a popup row: its key (`try / except`, an action, a
/// file name) or its description, by substring or word starts, ignoring case.
pub(super) fn choice_matches(key: &str, desc: &str, filter: &str) -> bool {
    let filter = filter.trim();
    // Keep the query ordered and intact: splitting it into independent words
    // lets "with w" reuse the same "with" in every row's shared description.
    crate::project_tree::speed_search_matches(key, filter)
        || crate::project_tree::speed_search_matches(desc, filter)
}

const DIGITS: [egui::Key; 9] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

impl IdeApp {
    /// Keys for the open list popup (before global shortcuts see them): arrows
    /// and Page Up/Down select, Enter applies, Esc closes, Backspace and typed
    /// text edit the speed search, and with nothing typed 1-9 pick a row.
    pub(super) fn choice_popup_keys(&mut self, ui: &mut egui::Ui) {
        let consume = |ui: &mut egui::Ui, key: egui::Key| {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
        };
        for (key, delta) in [
            (egui::Key::ArrowDown, 1),
            (egui::Key::ArrowUp, -1),
            (egui::Key::PageDown, 8),
            (egui::Key::PageUp, -8),
        ] {
            if consume(ui, key) {
                if let Some(c) = &mut self.choice {
                    c.move_selection(delta);
                }
            }
        }
        if consume(ui, egui::Key::Enter) {
            // Nothing shown (the speed search matches no row): Enter does nothing.
            if let Some(i) = self.choice.as_ref().and_then(|c| c.current()) {
                self.apply_choice(i);
            }
            return;
        }
        if consume(ui, egui::Key::Escape) {
            if matches!(
                self.choice.as_ref().map(|c| c.kind),
                Some(ChoiceKind::ExtractOccurrences(..))
            ) {
                self.clear_sticky_highlights();
            }
            self.choice = None;
            self.focus_editor = true;
            return;
        }
        let Some(choice) = &mut self.choice else {
            return;
        };
        if choice.filter.is_empty() {
            let shown = choice.shown();
            for (n, key) in DIGITS.into_iter().enumerate() {
                if consume(ui, key) {
                    // The digit's text must not start a speed search.
                    ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
                    if let Some(&i) = shown.get(n) {
                        self.apply_choice(i);
                    }
                    return;
                }
            }
        }
        if consume(ui, egui::Key::Backspace) {
            choice.backspace();
        }
        let typed: String = ui.input_mut(|i| {
            let mut typed = String::new();
            i.events.retain(|e| match e {
                egui::Event::Text(t) => {
                    typed.push_str(t);
                    false
                }
                _ => true,
            });
            typed
        });
        choice.type_text(&typed);
    }

    /// Draws the open list popup below the caret of the active text editor,
    /// or centred (the centred kinds, images, notebooks, no editor).
    pub(super) fn draw_choice_popup(&mut self, ctx: &egui::Context) {
        let Some(choice) = self.choice.clone() else {
            return;
        };
        let theme = self.theme.clone();
        let caret = self
            .active
            .and_then(|i| self.tabs.get(i))
            .filter(|t| t.image.is_none() && t.notebook.is_none())
            .and_then(|t| t.view.caret_screen_rect);
        let mut area =
            egui::Area::new(egui::Id::new("choice_popup")).order(egui::Order::Foreground);
        area = match caret.filter(|_| !choice.kind.centered()) {
            Some(rect) => area.fixed_pos(Pos2::new(rect.left(), rect.bottom() + 2.0)),
            None => area.anchor(Align2::CENTER_TOP, Vec2::new(0.0, 120.0)),
        };
        let shown = choice.shown();
        let numbered = choice.filter.is_empty();
        let mut clicked: Option<usize> = None;
        let area_response = area.show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(theme.panel_bg)
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.set_min_width(if choice.kind.centered() { 560.0 } else { 300.0 });
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&choice.title)
                                .color(theme.text_dim)
                                .small(),
                        );
                        if !choice.filter.is_empty() {
                            let color = if shown.is_empty() {
                                theme.error
                            } else {
                                theme.text
                            };
                            ui.label(
                                egui::RichText::new(format!("🔍 {}", choice.filter))
                                    .color(color)
                                    .small(),
                            );
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(
                            22.0 * f32::from(self.dialog_workspace.prefs.popup_rows.clamp(6, 30)),
                        )
                        .show(ui, |ui| {
                            for (row, &i) in shown.iter().enumerate() {
                                let (key, desc) = &choice.items[i];
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width().max(300.0), 22.0),
                                    Sense::click(),
                                );
                                let selected = i == choice.selected;
                                resp.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        true,
                                        selected,
                                        format!("{key}, {desc}"),
                                    )
                                });
                                resp.ctx.accesskit_node_builder(resp.id, |node| {
                                    node.clear_toggled();
                                    node.set_selected(selected);
                                });
                                if selected {
                                    ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
                                }
                                let fg = if selected { Color32::WHITE } else { theme.text };
                                if numbered && row < 9 {
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 6.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        format!("{}.", row + 1),
                                        egui::FontId::proportional(12.0),
                                        theme.text_dim,
                                    );
                                }
                                let key_w = if choice.kind.centered() { 180.0 } else { 92.0 };
                                let key_galley = ui.fonts_mut(|f| {
                                    f.layout_no_wrap(key.clone(), egui::FontId::monospace(13.0), fg)
                                });
                                let key_x = rect.left() + 28.0;
                                ui.painter().galley(
                                    Pos2::new(key_x, rect.center().y - key_galley.size().y / 2.0),
                                    key_galley.clone(),
                                    fg,
                                );
                                let desc_x = key_x + key_galley.size().x.max(key_w - 10.0) + 10.0;
                                ui.painter().text(
                                    Pos2::new(desc_x, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    desc,
                                    egui::FontId::proportional(12.0),
                                    if selected {
                                        Color32::WHITE
                                    } else {
                                        theme.text_dim
                                    },
                                );
                                if resp.clicked() {
                                    clicked = Some(i);
                                }
                            }
                            if shown.is_empty() {
                                ui.label(
                                    egui::RichText::new("Nothing matches").color(theme.text_dim),
                                );
                            }
                        });
                });
        });
        if matches!(self.dialog, Dialog::None)
            && !self.everywhere.visible
            && self.layout.exit_prompt.is_none()
            && !self.coordinated_quit_active()
        {
            let selected = shown
                .iter()
                .position(|&i| i == choice.selected)
                .map(|row| {
                    let (key, description) = &choice.items[choice.selected];
                    format!(
                        "Choice {} of {}: {key}, {description}",
                        row + 1,
                        shown.len()
                    )
                })
                .unwrap_or_else(|| "No matching choices".into());
            area_response.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &choice.title)
            });
            crate::a11y::announce(ctx, area_response.response.id, &format!(
                "{}. {selected}. Up and Down to select; type to filter; Enter to apply; Escape to close", choice.title
            ));
        }
        if let Some(i) = clicked {
            self.apply_choice(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surround() -> ChoicePopup {
        let items = [
            "if",
            "if / else",
            "while",
            "for",
            "try / except",
            "with",
            "<editor-fold...> Comments",
        ]
        .iter()
        .map(|k| (k.to_string(), format!("Surround with {k}")))
        .collect();
        ChoicePopup::new(ChoiceKind::Surround, "Surround With".into(), items)
    }

    fn keys(c: &ChoicePopup) -> Vec<&str> {
        c.shown()
            .into_iter()
            .map(|i| c.items[i].0.as_str())
            .collect()
    }

    #[test]
    fn typing_filters_the_rows_and_selects_the_first_match() {
        let mut c = surround();
        assert_eq!(c.shown().len(), 7);
        c.type_text("t");
        c.type_text("r");
        assert_eq!(
            keys(&c),
            ["try / except"],
            "a word start or a substring, not any subsequence"
        );
        assert_eq!(c.current(), Some(4));
        c.backspace();
        c.backspace();
        assert_eq!(c.filter, "");
        assert_eq!(c.shown().len(), 7);
        assert_eq!(
            c.current(),
            Some(4),
            "clearing the search keeps the selection"
        );
        // Matching ignores case and also looks at the description.
        c.type_text("WITH");
        assert_eq!(c.shown().len(), 7, "every description says 'Surround with'");
        assert_eq!(
            c.current(),
            Some(5),
            "the row whose key starts with the search"
        );
        c.type_text(" w");
        assert_eq!(keys(&c), ["while", "with"]);
        assert_eq!(
            c.current(),
            Some(5),
            "a selection that is still shown stays"
        );
        // The selection filtered out and no key starting with the search: the first shown row.
        let mut c = surround();
        c.move_selection(3);
        c.type_text("ex");
        assert_eq!(keys(&c), ["try / except"]);
        assert_eq!(c.current(), Some(4));
        c.type_text("zz");
        assert!(c.shown().is_empty());
        assert_eq!(c.current(), None, "Enter has nothing to apply");
    }

    #[test]
    fn up_down_move_among_the_shown_rows() {
        let mut c = surround();
        c.move_selection(-1);
        assert_eq!(c.selected, 0, "clamped at the top");
        c.move_selection(3);
        assert_eq!(c.selected, 3);
        c.move_selection(100);
        assert_eq!(c.selected, 6, "clamped at the bottom");
        c.type_text("if");
        assert_eq!(keys(&c), ["if", "if / else"]);
        assert_eq!(c.selected, 0);
        c.move_selection(1);
        assert_eq!(c.selected, 1);
        c.move_selection(1);
        assert_eq!(c.selected, 1, "only shown rows can be selected");
    }

    #[test]
    fn a_space_alone_does_not_start_a_search_and_control_characters_are_dropped() {
        let mut c = surround();
        c.type_text(" ");
        c.type_text("\u{8}\n");
        assert_eq!(c.filter, "");
        c.type_text("for\t");
        assert_eq!(c.filter, "for");
        assert_eq!(keys(&c), ["for"]);
    }

    #[test]
    fn bookmark_rows_match_their_file_and_line() {
        let items = vec![
            ("1".to_string(), "main.py:3  ".to_string()),
            ("•".to_string(), "pkg/shapes.py:12  area".to_string()),
        ];
        let mut c = ChoicePopup::new(ChoiceKind::Bookmarks, "Bookmarks".into(), items);
        c.type_text("shap");
        assert_eq!(c.shown(), [1]);
        assert_eq!(c.current(), Some(1));
    }
}

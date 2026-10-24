//! Recent Files (Ctrl+E), Switcher (Ctrl+Tab) and Recent Locations
//! (Ctrl+Shift+E) popups (roadmap §1.5): most-recently-used order, the
//! previous file preselected, speed search, *Show edited/changed only*, and
//! tool windows next to the files. They are drawn at the top level, so they
//! also work with no editor open, and they own the keyboard while visible.

use std::path::PathBuf;

use egui::{Align2, Color32, Key, Modifiers, Pos2, Rect, Sense, Vec2};

use crate::actions::KeyChord;
use crate::nav_history::NavPoint;
use crate::theme::UiTheme;

/// A file row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    /// Directory relative to the project root.
    pub dir: String,
    /// Edited in this session (or unsaved).
    pub edited: bool,
    /// Unsaved changes.
    pub modified: bool,
    /// The active editor's file.
    pub current: bool,
}

/// A tool window row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolEntry {
    pub label: String,
    pub shortcut: String,
}

/// A place row of Recent Locations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceEntry {
    pub point: NavPoint,
    /// `main.py:12`
    pub title: String,
    pub dir: String,
    /// Code around the place: `(0-based line, text)`, the place's line included.
    pub lines: Vec<(usize, String)>,
}

/// What a popup was closed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopupOutcome {
    Cancel,
    File(PathBuf),
    /// An existing editor captured by stable identity, including unsaved buffers.
    Editor(egui::Id),
    Tool(usize),
    Place(NavPoint),
}

/// Indices of `files` shown for `query` (fuzzy on the name, or on
/// `dir/name` when the query has a `/`), in MRU order.
pub fn filter_files(files: &[FileEntry], query: &str, edited_only: bool) -> Vec<usize> {
    let q = query.trim();
    files
        .iter()
        .enumerate()
        .filter(|(_, f)| !edited_only || f.edited)
        .filter(|(_, f)| {
            if q.is_empty() {
                return true;
            }
            if q.contains('/') {
                let full = if f.dir.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}/{}", f.dir.replace('\\', "/"), f.name)
                };
                rustpy_search::fuzzy_score(&full, q).is_some()
            } else {
                rustpy_search::fuzzy_score(&f.name, q).is_some()
            }
        })
        .map(|(i, _)| i)
        .collect()
}

/// The row selected when the list (re)appears: the previous file (row 1)
/// when row 0 is the current one, so Ctrl+E, Enter switches back.
pub fn initial_selection(files: &[FileEntry], shown: &[usize]) -> usize {
    match shown.first() {
        Some(&first) if files[first].current && shown.len() > 1 => 1,
        _ => 0,
    }
}

/// Indices of `places` matching `query` (file name or any context line,
/// case-insensitive).
pub fn filter_places(places: &[PlaceEntry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    places
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            q.is_empty()
                || p.title.to_lowercase().contains(&q)
                || p.lines.iter().any(|(_, t)| t.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Removes the first press of `key` from this frame's events; returns its modifiers.
fn take_key(ctx: &egui::Context, key: Key) -> Option<Modifiers> {
    ctx.input_mut(|i| {
        let pos = i.events.iter().position(
            |e| matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key),
        )?;
        match i.events.remove(pos) {
            egui::Event::Key { modifiers, .. } => Some(modifiers),
            _ => None,
        }
    })
}

/// Removes the first press of any of `chords`; true if there was one.
fn take_chord(ctx: &egui::Context, chords: &[KeyChord]) -> bool {
    ctx.input_mut(|i| {
        let pos = i.events.iter().position(|e| matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if chords.contains(&KeyChord::new(*key, *modifiers))));
        pos.map(|p| i.events.remove(p)).is_some()
    })
}

/// True when the pointer was pressed outside `rect` this frame.
fn pressed_outside(ctx: &egui::Context, rect: Rect) -> bool {
    ctx.input(|i| {
        i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p))
    })
}

fn row_bg(ui: &egui::Ui, rect: Rect, selected: bool, hovered: bool, theme: &UiTheme) {
    if selected {
        ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
    } else if hovered {
        ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
    }
}

fn file_label(file: &FileEntry) -> String {
    let identity = if file.path.as_os_str().is_empty() {
        file.name.clone()
    } else {
        file.path.display().to_string()
    };
    format!(
        "File {identity}{}{}",
        if file.modified {
            ", unsaved changes"
        } else {
            ""
        },
        if file.edited {
            ", edited in this session"
        } else {
            ""
        }
    )
}
fn tool_label(tool: &ToolEntry) -> String {
    format!(
        "Tool window {}{}",
        tool.label,
        if tool.shortcut.is_empty() {
            String::new()
        } else {
            format!(", {}", tool.shortcut)
        }
    )
}
fn place_label(place: &PlaceEntry) -> String {
    let context: String = place
        .lines
        .iter()
        .find(|(line, _)| *line == place.point.line)
        .map(|(_, text)| text.chars().take(160).collect())
        .unwrap_or_default();
    format!(
        "{}: line {}, column {}: {context}",
        place.point.path.display(),
        place.point.line.saturating_add(1),
        place.point.col.saturating_add(1)
    )
}
fn named_row(response: &egui::Response, label: String, selected: bool) {
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            label.clone(),
        )
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.clear_toggled();
        node.set_selected(selected);
    });
}

/// Draws a file row; true when clicked.
fn file_row(ui: &mut egui::Ui, f: &FileEntry, selected: bool, theme: &UiTheme) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    named_row(&resp, file_label(f), selected);
    row_bg(ui, rect, selected, resp.hovered(), theme);
    let fg = if selected {
        Color32::WHITE
    } else if f.modified {
        theme.modified
    } else {
        theme.text
    };
    let dim = if selected {
        Color32::from_gray(225)
    } else {
        theme.text_dim
    };
    let y = rect.center().y;
    let name = if f.modified {
        format!("*{}", f.name)
    } else {
        f.name.clone()
    };
    let g = ui.fonts_mut(|fo| fo.layout_no_wrap(name, egui::FontId::proportional(13.5), fg));
    ui.painter().galley(
        Pos2::new(rect.left() + 8.0, y - g.size().y / 2.0),
        g.clone(),
        fg,
    );
    if f.edited {
        ui.painter().circle_filled(
            Pos2::new(rect.left() + 14.0 + g.size().x, y),
            2.5,
            if selected {
                Color32::WHITE
            } else {
                theme.accent
            },
        );
    }
    ui.painter().text(
        Pos2::new(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        &f.dir,
        egui::FontId::proportional(11.5),
        dim,
    );
    resp
}

/// Draws a tool window row; true when clicked.
fn tool_row(ui: &mut egui::Ui, t: &ToolEntry, selected: bool, theme: &UiTheme) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
    named_row(&resp, tool_label(t), selected);
    row_bg(ui, rect, selected, resp.hovered(), theme);
    let fg = if selected { Color32::WHITE } else { theme.text };
    let dim = if selected {
        Color32::from_gray(225)
    } else {
        theme.text_dim
    };
    ui.painter().text(
        Pos2::new(rect.left() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        &t.label,
        egui::FontId::proportional(13.0),
        fg,
    );
    ui.painter().text(
        Pos2::new(rect.right() - 6.0, rect.center().y),
        Align2::RIGHT_CENTER,
        &t.shortcut,
        egui::FontId::proportional(11.0),
        dim,
    );
    resp
}

// ---------------------------------------------------------------------------
// Recent Files

/// Recent Files (Ctrl+E): recently opened files, newest first, with speed
/// search and *Show edited only* (also toggled by pressing Ctrl+E again).
#[derive(Default)]
pub struct RecentFilesPopup {
    pub visible: bool,
    title_override: Option<&'static str>,
    pub edited_only: bool,
    files: Vec<FileEntry>,
    tools: Vec<ToolEntry>,
    query: String,
    shown: Vec<usize>,
    selected: usize,
    tool_selected: usize,
    tools_active: bool,
    reveal_selection: bool,
    #[cfg(test)]
    last_scroll: Option<(Rect, Vec2)>,
    #[cfg(test)]
    last_selected_row: Option<Rect>,
    filter_key: Option<(String, bool)>,
    focus: bool,
    fresh: bool,
}

impl RecentFilesPopup {
    pub fn open(&mut self, files: Vec<FileEntry>, tools: Vec<ToolEntry>, edited_only: bool) {
        *self = Self {
            visible: true,
            edited_only,
            files,
            tools,
            focus: true,
            fresh: true,
            reveal_selection: true,
            ..Default::default()
        };
        self.refilter();
    }

    /// The title, reflecting *Show edited only*.
    pub fn set_title(&mut self, title: &'static str) {
        self.title_override = Some(title);
    }

    pub fn title(&self) -> &'static str {
        if let Some(title) = self.title_override {
            return title;
        }
        if self.edited_only {
            "Recently Edited Files"
        } else {
            "Recent Files"
        }
    }

    /// Rows currently listed (paths), for tests and smoke logs.
    pub fn shown_paths(&self) -> Vec<PathBuf> {
        self.shown
            .iter()
            .map(|&i| self.files[i].path.clone())
            .collect()
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.shown
            .get(self.selected)
            .map(|&i| self.files[i].path.clone())
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
        self.tools_active = false;
        self.refilter();
    }

    pub fn toggle_edited(&mut self) {
        self.title_override = None;
        self.edited_only = !self.edited_only;
        self.refilter();
    }

    fn refilter(&mut self) {
        let key = (self.query.clone(), self.edited_only);
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        self.shown = filter_files(&self.files, &self.query, self.edited_only);
        self.selected = if self.query.trim().is_empty() {
            initial_selection(&self.files, &self.shown)
        } else {
            0
        };
        self.reveal_selection = true;
        self.filter_key = Some(key);
    }

    /// Moves the selection by `delta` rows (clamped).
    pub fn move_selection(&mut self, delta: i32) {
        let len = if self.tools_active {
            self.tools.len()
        } else {
            self.shown.len()
        };
        if len == 0 {
            return;
        }
        let selected = if self.tools_active {
            &mut self.tool_selected
        } else {
            &mut self.selected
        };
        let next = (*selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.reveal_selection |= next != *selected;
        *selected = next;
    }

    fn select_tools(&mut self, active: bool) {
        if active && self.tools.is_empty() {
            return;
        }
        self.tools_active = active;
        self.reveal_selection = true;
    }

    /// Draws the popup; `toggle` are the Recent Files shortcuts (pressing one
    /// again toggles *Show edited only*).
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        toggle: &[KeyChord],
    ) -> Option<PopupOutcome> {
        if !self.visible {
            return None;
        }
        let mut outcome = None;
        if take_chord(ctx, toggle) {
            self.toggle_edited();
        }
        if take_key(ctx, Key::Escape).is_some() {
            outcome = Some(PopupOutcome::Cancel);
        }
        if take_key(ctx, Key::ArrowLeft).is_some() {
            self.select_tools(true);
        }
        if take_key(ctx, Key::ArrowRight).is_some() {
            self.select_tools(false);
        }
        if take_key(ctx, Key::ArrowDown).is_some() {
            self.move_selection(1);
        }
        if take_key(ctx, Key::ArrowUp).is_some() {
            self.move_selection(-1);
        }
        if take_key(ctx, Key::PageDown).is_some() {
            self.move_selection(10);
        }
        if take_key(ctx, Key::PageUp).is_some() {
            self.move_selection(-10);
        }
        if take_key(ctx, Key::Enter).is_some() {
            outcome = Some(if self.tools_active {
                PopupOutcome::Tool(self.tool_selected)
            } else {
                match self.selected_path() {
                    Some(p) => PopupOutcome::File(p),
                    None => PopupOutcome::Cancel,
                }
            });
        }
        let area = egui::Area::new(egui::Id::new("recent_files_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 90.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(660.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(self.title()).strong().color(theme.text));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hint = toggle
                                        .first()
                                        .map(|c| format!("({c})"))
                                        .unwrap_or_default();
                                    ui.label(
                                        egui::RichText::new(hint).small().color(theme.text_dim),
                                    );
                                    let mut edited = self.edited_only;
                                    if ui.checkbox(&mut edited, "Show edited only").changed() {
                                        self.toggle_edited();
                                    }
                                },
                            );
                        });
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .desired_width(f32::INFINITY)
                                .hint_text("Type to search (speed search)"),
                        );
                        if self.focus {
                            resp.request_focus();
                            self.focus = false;
                        }
                        if resp.changed() {
                            self.tools_active = false;
                            self.refilter();
                        }
                        if outcome.is_none() && resp.has_focus() {
                            let selected = if self.tools_active { self.tools.get(self.tool_selected).map(|t| format!("Tool {} of {}: {}", self.tool_selected + 1, self.tools.len(), tool_label(t))) } else { self.shown.get(self.selected).map(|&i| format!(
                                "Result {} of {}: {}", self.selected + 1, self.shown.len(), file_label(&self.files[i])
                            )) }.unwrap_or_else(|| "No matching files".into());
                            crate::a11y::announce(ctx, resp.id, &format!("{}. {selected}. Up and Down to select; Left and Right to change column; Enter to open; Escape to close", self.title()));
                        }
                        ui.separator();
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(170.0);
                                ui.label(
                                    egui::RichText::new("Tool Windows")
                                        .small()
                                        .color(theme.text_dim),
                                );
                                for (i, t) in self.tools.iter().enumerate() {
                                    if tool_row(ui, t, self.tools_active && self.tool_selected == i, theme).clicked() {
                                        outcome = Some(PopupOutcome::Tool(i));
                                    }
                                }
                            });
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                let scroll_output = egui::ScrollArea::vertical()
                                    .id_salt("recent_files_rows")
                                    .max_height(380.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for (row, &i) in self.shown.iter().enumerate() {
                                            let resp = file_row(
                                                ui,
                                                &self.files[i],
                                                !self.tools_active && row == self.selected,
                                                theme,
                                            );
                                            #[cfg(test)]
                                            if !self.tools_active && row == self.selected {
                                                self.last_selected_row = Some(resp.rect);
                                            }
                                            if !self.tools_active && row == self.selected && self.reveal_selection {
                                                resp.scroll_to_me(None);
                                            }
                                            if resp.clicked() {
                                                outcome = Some(PopupOutcome::File(
                                                    self.files[i].path.clone(),
                                                ));
                                            }
                                        }
                                        if self.shown.is_empty() {
                                            ui.label(
                                                egui::RichText::new(if self.edited_only {
                                                    "No edited files"
                                                } else {
                                                    "Nothing found"
                                                })
                                                .color(theme.text_dim),
                                            );
                                        }
                                    });
                                #[cfg(test)]
                                { self.last_scroll = Some((scroll_output.inner_rect, scroll_output.state.offset)); }
                                #[cfg(not(test))]
                                let _ = scroll_output;
                            });
                        });
                        ui.label(
                            egui::RichText::new("Up/Down select · Enter open · Esc close")
                                .small()
                                .color(theme.text_dim),
                        );
                    });
            });
        if !self.fresh && outcome.is_none() && pressed_outside(ctx, area.response.rect) {
            outcome = Some(PopupOutcome::Cancel);
        }
        self.fresh = false;
        self.reveal_selection = false;
        if outcome.is_some() {
            self.visible = false;
        }
        outcome
    }
}

// ---------------------------------------------------------------------------
// Switcher

/// Switcher (Ctrl+Tab): open editors in MRU order (the previous one
/// preselected) and tool windows. While Ctrl is held, Tab / Shift+Tab move
/// the selection and releasing Ctrl activates it; Enter and clicks work too.
#[derive(Default)]
pub struct SwitcherPopup {
    pub visible: bool,
    files: Vec<FileEntry>,
    editor_ids: Vec<egui::Id>,
    tools: Vec<ToolEntry>,
    /// Column: 0 = tool windows, 1 = files.
    col: usize,
    selected: usize,
    /// Modifiers held when it opened: releasing them activates the selection.
    hold: Modifiers,
    fresh: bool,
}

impl SwitcherPopup {
    /// Opens with `hold` = the modifiers held by the opening shortcut
    /// (none when opened from a menu: then it stays until Enter or a click).
    pub fn open(&mut self, files: Vec<FileEntry>, tools: Vec<ToolEntry>, hold: Modifiers) {
        let hold = Modifiers {
            ctrl: hold.ctrl,
            alt: hold.alt,
            command: hold.command,
            mac_cmd: hold.mac_cmd,
            shift: false,
        };
        let col = usize::from(!files.is_empty());
        let selected = if col == 1 && files.len() > 1 && files[0].current {
            1
        } else {
            0
        };
        *self = Self {
            visible: true,
            files,
            editor_ids: Vec::new(),
            tools,
            col,
            selected,
            hold,
            fresh: true,
        };
    }

    /// Opens editor rows bound to existing view identities rather than filesystem paths.
    pub fn open_editors(
        &mut self,
        editors: Vec<(egui::Id, FileEntry)>,
        tools: Vec<ToolEntry>,
        hold: Modifiers,
    ) {
        let (ids, files) = editors.into_iter().unzip();
        self.open(files, tools, hold);
        self.editor_ids = ids;
    }

    fn file_outcome(&self, index: usize) -> Option<PopupOutcome> {
        self.files.get(index).map(|file| {
            self.editor_ids.get(index).map_or_else(
                || PopupOutcome::File(file.path.clone()),
                |id| PopupOutcome::Editor(*id),
            )
        })
    }

    fn len(&self, col: usize) -> usize {
        if col == 0 {
            self.tools.len()
        } else {
            self.files.len()
        }
    }

    /// Next/previous row, wrapping around within the column.
    pub fn step(&mut self, delta: i32) {
        let n = self.len(self.col) as i32;
        if n > 0 {
            self.selected = ((self.selected as i32 + delta) % n + n) as usize % n as usize;
        }
    }

    fn switch_column(&mut self, col: usize) {
        if self.len(col) > 0 {
            self.col = col;
            self.selected = self.selected.min(self.len(col) - 1);
        }
    }

    pub fn selection(&self) -> Option<PopupOutcome> {
        match self.col {
            0 => (self.selected < self.tools.len()).then_some(PopupOutcome::Tool(self.selected)),
            _ => self.file_outcome(self.selected),
        }
    }

    /// Draws the popup; `cycle` are the Switcher shortcuts (each press moves on).
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        cycle: &[KeyChord],
    ) -> Option<PopupOutcome> {
        if !self.visible {
            return None;
        }
        let mut outcome = None;
        while let Some(m) = take_key(ctx, Key::Tab) {
            self.step(if m.shift { -1 } else { 1 });
        }
        while take_chord(ctx, cycle) {
            self.step(1);
        }
        if take_key(ctx, Key::ArrowDown).is_some() {
            self.step(1);
        }
        if take_key(ctx, Key::ArrowUp).is_some() {
            self.step(-1);
        }
        if take_key(ctx, Key::ArrowLeft).is_some() {
            self.switch_column(0);
        }
        if take_key(ctx, Key::ArrowRight).is_some() {
            self.switch_column(1);
        }
        if take_key(ctx, Key::Escape).is_some() {
            outcome = Some(PopupOutcome::Cancel);
        }
        if take_key(ctx, Key::Enter).is_some() {
            outcome = Some(self.selection().unwrap_or(PopupOutcome::Cancel));
        }
        // Released the held modifier (Ctrl): activate.
        let held = ctx.input(|i| {
            (self.hold.ctrl && i.modifiers.ctrl)
                || (self.hold.alt && i.modifiers.alt)
                || ((self.hold.command || self.hold.mac_cmd)
                    && (i.modifiers.command || i.modifiers.mac_cmd))
        });
        if self.hold.any() && !held && outcome.is_none() {
            outcome = Some(self.selection().unwrap_or(PopupOutcome::Cancel));
        }
        let focus_id = egui::Id::new("switcher_focus");
        let area = egui::Area::new(egui::Id::new("switcher_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 110.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(620.0);
                        // Holds keyboard focus so the editor below does not react to Tab/arrows/Enter.
                        let focus =
                            ui.interact(ui.max_rect(), focus_id, Sense::focusable_noninteractive());
                        if self.fresh {
                            focus.request_focus();
                        }
                        focus.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Switcher"));
                        if outcome.is_none() && focus.has_focus() {
                            let selected = if self.col == 0 {
                                self.tools.get(self.selected).map(tool_label)
                            } else {
                                self.files.get(self.selected).map(file_label)
                            }.unwrap_or_else(|| "No items".into());
                            let position = if self.len(self.col) == 0 { "No items".into() } else { format!("Item {} of {}: {selected}", self.selected + 1, self.len(self.col)) };
                            crate::a11y::announce(ctx, focus.id, &format!("Switcher. {position}. Up and Down to select; Left and Right to change column; Enter to open; Escape to close"));
                        }
                        ui.memory_mut(|m| {
                            m.set_focus_lock_filter(
                                focus_id,
                                egui::EventFilter {
                                    tab: true,
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    escape: true,
                                },
                            )
                        });
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Switcher").strong().color(theme.text));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hint = if self.hold.any() {
                                        "Tab / Shift+Tab to move · release to open"
                                    } else {
                                        "Arrows select · Enter open · Esc close"
                                    };
                                    ui.label(
                                        egui::RichText::new(hint).small().color(theme.text_dim),
                                    );
                                },
                            );
                        });
                        ui.separator();
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(180.0);
                                ui.label(
                                    egui::RichText::new("Tool Windows")
                                        .small()
                                        .color(theme.text_dim),
                                );
                                for (i, t) in self.tools.iter().enumerate() {
                                    if tool_row(ui, t, self.col == 0 && self.selected == i, theme)
                                        .clicked()
                                    {
                                        outcome = Some(PopupOutcome::Tool(i));
                                    }
                                }
                            });
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Open Files")
                                        .small()
                                        .color(theme.text_dim),
                                );
                                egui::ScrollArea::vertical()
                                    .id_salt("switcher_rows")
                                    .max_height(380.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for (i, f) in self.files.iter().enumerate() {
                                            let sel = self.col == 1 && self.selected == i;
                                            let resp = file_row(ui, f, sel, theme);
                                            if sel {
                                                resp.scroll_to_me(None);
                                            }
                                            if resp.clicked() {
                                                outcome = self.file_outcome(i);
                                            }
                                        }
                                        if self.files.is_empty() {
                                            ui.label(
                                                egui::RichText::new("No open files")
                                                    .color(theme.text_dim),
                                            );
                                        }
                                    });
                            });
                        });
                    });
            });
        if !self.fresh && outcome.is_none() && pressed_outside(ctx, area.response.rect) {
            outcome = Some(PopupOutcome::Cancel);
        }
        self.fresh = false;
        if outcome.is_some() {
            self.visible = false;
            ctx.memory_mut(|m| m.surrender_focus(focus_id));
        }
        outcome
    }
}

// ---------------------------------------------------------------------------
// Recent Locations

/// Recent Locations (Ctrl+Shift+E): visited places with the code around
/// them, speed search, and *Show changed only* (also Ctrl+Shift+E again).
#[derive(Default)]
pub struct RecentLocationsPopup {
    pub visible: bool,
    pub changed_only: bool,
    recent: Vec<PlaceEntry>,
    changed: Vec<PlaceEntry>,
    query: String,
    shown: Vec<usize>,
    selected: usize,
    filter_key: Option<(String, bool)>,
    focus: bool,
    fresh: bool,
    reveal_selection: bool,
    #[cfg(test)]
    last_scroll: Option<(Rect, Vec2)>,
}

impl RecentLocationsPopup {
    /// `recent` and `changed` are most recent first; `skip_first` preselects
    /// row 1 of the recent list when row 0 is where the caret already is.
    pub fn open(&mut self, recent: Vec<PlaceEntry>, changed: Vec<PlaceEntry>, changed_only: bool) {
        *self = Self {
            visible: true,
            changed_only,
            recent,
            changed,
            focus: true,
            fresh: true,
            reveal_selection: true,
            ..Default::default()
        };
        self.refilter();
    }

    fn places(&self) -> &[PlaceEntry] {
        if self.changed_only {
            &self.changed
        } else {
            &self.recent
        }
    }

    pub fn toggle_changed(&mut self) {
        self.changed_only = !self.changed_only;
        self.refilter();
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
        self.refilter();
    }

    pub fn shown_titles(&self) -> Vec<String> {
        self.shown
            .iter()
            .map(|&i| self.places()[i].title.clone())
            .collect()
    }

    fn refilter(&mut self) {
        let key = (self.query.clone(), self.changed_only);
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        self.shown = filter_places(self.places(), &self.query);
        self.selected = 0;
        self.reveal_selection = true;
        self.filter_key = Some(key);
    }

    fn move_selection(&mut self, delta: i32) {
        if !self.shown.is_empty() {
            let next =
                (self.selected as i32 + delta).clamp(0, self.shown.len() as i32 - 1) as usize;
            self.reveal_selection |= next != self.selected;
            self.selected = next;
        }
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        toggle: &[KeyChord],
        mono: egui::FontId,
    ) -> Option<PopupOutcome> {
        if !self.visible {
            return None;
        }
        let mut outcome = None;
        if take_chord(ctx, toggle) {
            self.toggle_changed();
        }
        if take_key(ctx, Key::Escape).is_some() {
            outcome = Some(PopupOutcome::Cancel);
        }
        if take_key(ctx, Key::ArrowDown).is_some() {
            self.move_selection(1);
        }
        if take_key(ctx, Key::ArrowUp).is_some() {
            self.move_selection(-1);
        }
        if take_key(ctx, Key::PageDown).is_some() {
            self.move_selection(5);
        }
        if take_key(ctx, Key::PageUp).is_some() {
            self.move_selection(-5);
        }
        if take_key(ctx, Key::Enter).is_some() {
            outcome = Some(match self.shown.get(self.selected) {
                Some(&i) => PopupOutcome::Place(self.places()[i].point.clone()),
                None => PopupOutcome::Cancel,
            });
        }
        let line_h = mono.size + 5.0;
        let area = egui::Area::new(egui::Id::new("recent_locations_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 80.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(720.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(if self.changed_only {
                                    "Recently Changed Locations"
                                } else {
                                    "Recent Locations"
                                })
                                .strong()
                                .color(theme.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hint = toggle
                                        .first()
                                        .map(|c| format!("({c})"))
                                        .unwrap_or_default();
                                    ui.label(
                                        egui::RichText::new(hint).small().color(theme.text_dim),
                                    );
                                    let mut changed = self.changed_only;
                                    if ui.checkbox(&mut changed, "Show changed only").changed() {
                                        self.toggle_changed();
                                    }
                                },
                            );
                        });
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .desired_width(f32::INFINITY)
                                .hint_text("Type to search file names and code"),
                        );
                        if self.focus {
                            resp.request_focus();
                            self.focus = false;
                        }
                        if resp.changed() {
                            self.refilter();
                        }
                        if outcome.is_none() && resp.has_focus() {
                            let places = if self.changed_only { &self.changed } else { &self.recent };
                            let selected = self.shown.get(self.selected).map(|&i| format!(
                                "Result {} of {}: {}", self.selected + 1, self.shown.len(), place_label(&places[i])
                            )).unwrap_or_else(|| "No matching locations".into());
                            let title = if self.changed_only { "Recently Changed Locations" } else { "Recent Locations" };
                            crate::a11y::announce(ctx, resp.id, &format!("{title}. {selected}. Up and Down to select; Enter to open; Escape to close"));
                        }
                        ui.separator();
                        let places = if self.changed_only {
                            &self.changed
                        } else {
                            &self.recent
                        };
                        let scroll_output = egui::ScrollArea::vertical()
                            .id_salt("recent_locations_rows")
                            .max_height(460.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (row, &i) in self.shown.iter().enumerate() {
                                    let p = &places[i];
                                    let h = 24.0 + line_h * p.lines.len() as f32 + 4.0;
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), h),
                                        Sense::click(),
                                    );
                                    let sel = row == self.selected;
                                    named_row(&resp, place_label(p), sel);
                                    row_bg(ui, rect, sel, resp.hovered(), theme);
                                    if sel && self.reveal_selection {
                                        resp.scroll_to_me(None);
                                    }
                                    let fg = if sel { Color32::WHITE } else { theme.text };
                                    let dim = if sel {
                                        Color32::from_gray(225)
                                    } else {
                                        theme.text_dim
                                    };
                                    ui.painter().text(
                                        Pos2::new(rect.left() + 8.0, rect.top() + 12.0),
                                        Align2::LEFT_CENTER,
                                        &p.title,
                                        egui::FontId::proportional(13.5),
                                        fg,
                                    );
                                    ui.painter().text(
                                        Pos2::new(rect.right() - 8.0, rect.top() + 12.0),
                                        Align2::RIGHT_CENTER,
                                        &p.dir,
                                        egui::FontId::proportional(11.5),
                                        dim,
                                    );
                                    for (k, (line, text)) in p.lines.iter().enumerate() {
                                        let y =
                                            rect.top() + 24.0 + line_h * k as f32 + line_h / 2.0;
                                        let here = *line == p.point.line;
                                        ui.painter().text(
                                            Pos2::new(rect.left() + 46.0, y),
                                            Align2::RIGHT_CENTER,
                                            format!("{}", line + 1),
                                            mono.clone(),
                                            dim,
                                        );
                                        let shown: String = text.chars().take(110).collect();
                                        ui.painter().text(
                                            Pos2::new(rect.left() + 56.0, y),
                                            Align2::LEFT_CENTER,
                                            shown,
                                            mono.clone(),
                                            if here { fg } else { dim },
                                        );
                                    }
                                    if resp.clicked() {
                                        outcome = Some(PopupOutcome::Place(p.point.clone()));
                                    }
                                }
                                if self.shown.is_empty() {
                                    ui.label(
                                        egui::RichText::new(if self.changed_only {
                                            "No changed locations"
                                        } else {
                                            "No recent locations"
                                        })
                                        .color(theme.text_dim),
                                    );
                                }
                            });
                                #[cfg(test)]
                                { self.last_scroll = Some((scroll_output.inner_rect, scroll_output.state.offset)); }
                                #[cfg(not(test))]
                                let _ = scroll_output;
                    });
            });
        if !self.fresh && outcome.is_none() && pressed_outside(ctx, area.response.rect) {
            outcome = Some(PopupOutcome::Cancel);
        }
        self.fresh = false;
        self.reveal_selection = false;
        if outcome.is_some() {
            self.visible = false;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, dir: &str, current: bool, edited: bool) -> FileEntry {
        FileEntry {
            path: PathBuf::from(format!("/p/{dir}/{name}")),
            name: name.into(),
            dir: dir.into(),
            edited,
            modified: false,
            current,
        }
    }

    #[test]
    fn recent_files_preselect_the_previous_file_and_filter_by_speed_search() {
        let files = vec![
            file("main.py", "", true, false),
            file("models.py", "app", false, true),
            file("views.py", "app", false, false),
            file("test_models.py", "tests", false, true),
        ];
        let mut p = RecentFilesPopup::default();
        p.open(files, Vec::new(), false);
        assert!(p.visible);
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/p/app/models.py")),
            "row 1: Ctrl+E, Enter goes back"
        );
        p.set_query("mod");
        assert_eq!(
            p.shown_paths(),
            vec![
                PathBuf::from("/p/app/models.py"),
                PathBuf::from("/p/tests/test_models.py")
            ],
            "MRU order kept"
        );
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/p/app/models.py")),
            "first match selected"
        );
        p.set_query("tests/");
        assert_eq!(
            p.shown_paths(),
            vec![PathBuf::from("/p/tests/test_models.py")],
            "a slash matches directories"
        );
        p.set_query("");
        p.toggle_edited();
        assert_eq!(p.title(), "Recently Edited Files");
        assert_eq!(
            p.shown_paths(),
            vec![
                PathBuf::from("/p/app/models.py"),
                PathBuf::from("/p/tests/test_models.py")
            ]
        );
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/p/app/models.py")),
            "no current row here: the first one"
        );
        p.move_selection(5);
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/p/tests/test_models.py")),
            "clamped to the last row"
        );
    }

    #[test]
    fn single_file_and_empty_lists_select_row_zero() {
        let files = vec![file("main.py", "", true, false)];
        assert_eq!(initial_selection(&files, &[0]), 0);
        assert_eq!(initial_selection(&files, &[]), 0);
        assert!(filter_files(&files, "zzz", false).is_empty());
    }

    #[test]
    fn switcher_starts_on_the_previous_editor_and_wraps() {
        let files = vec![
            file("a.py", "", true, false),
            file("b.py", "", false, false),
            file("c.py", "", false, false),
        ];
        let tools = vec![ToolEntry {
            label: "Project".into(),
            shortcut: "Alt+1".into(),
        }];
        let mut s = SwitcherPopup::default();
        s.open(files.clone(), tools.clone(), Modifiers::CTRL);
        assert_eq!(
            s.selection(),
            Some(PopupOutcome::File(PathBuf::from("/p//b.py")))
        );
        s.step(1);
        assert_eq!(
            s.selection(),
            Some(PopupOutcome::File(PathBuf::from("/p//c.py")))
        );
        s.step(1);
        assert_eq!(
            s.selection(),
            Some(PopupOutcome::File(PathBuf::from("/p//a.py"))),
            "wraps around"
        );
        s.step(-1);
        assert_eq!(
            s.selection(),
            Some(PopupOutcome::File(PathBuf::from("/p//c.py")))
        );
        s.switch_column(0);
        assert_eq!(s.selection(), Some(PopupOutcome::Tool(0)));
        // No open editors: the tool windows column.
        s.open(Vec::new(), tools, Modifiers::NONE);
        assert_eq!(s.selection(), Some(PopupOutcome::Tool(0)));
    }

    #[test]
    fn recent_locations_filter_on_code_and_toggle_changed_only() {
        let place = |f: &str, line: usize, code: &str| PlaceEntry {
            point: NavPoint::new(format!("/p/{f}"), line, 0),
            title: format!("{f}:{}", line + 1),
            dir: String::new(),
            lines: vec![(line, code.to_string())],
        };
        let mut p = RecentLocationsPopup::default();
        p.open(
            vec![
                place("a.py", 3, "def load():"),
                place("b.py", 9, "x = compute()"),
            ],
            vec![place("c.py", 1, "edited = True")],
            false,
        );
        assert_eq!(p.shown_titles(), vec!["a.py:4", "b.py:10"]);
        p.set_query("COMPUTE");
        assert_eq!(p.shown_titles(), vec!["b.py:10"]);
        p.set_query("");
        p.toggle_changed();
        assert_eq!(p.shown_titles(), vec!["c.py:2"]);
    }
    fn accessible_context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        ctx
    }
    fn accessible_frame(
        ctx: &egui::Context,
        time: f64,
        key: Option<Key>,
        mut paint: impl FnMut() -> Option<PopupOutcome>,
    ) -> (egui::accesskit::TreeUpdate, Option<PopupOutcome>) {
        let events = key
            .map(|key| {
                vec![
                    egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                    egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: false,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                ]
            })
            .unwrap_or_default();
        let mut outcome = None;
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
                ..Default::default()
            },
            |_| outcome = paint(),
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        (update, outcome)
    }
    fn status(update: &egui::accesskit::TreeUpdate, prefix: &str) -> String {
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Status
                    && node.label().is_some_and(|s| s.starts_with(prefix))
            })
            .expect("real focused popup announcement");
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, owner)| owner.children().contains(id)),
            "actual owner links its announcement"
        );
        node.label().unwrap().into()
    }

    #[test]
    fn recent_files_tree_announces_mru_selection_and_exact_enter_target() {
        let ctx = accessible_context();
        let theme = UiTheme::dark();
        let mut popup = RecentFilesPopup::default();
        let files = vec![
            file("α.py", "src", true, false),
            file("β.py", "src", false, true),
            file("γ space.py", "tests", false, false),
        ];
        let expected = files[2].path.clone();
        popup.open(files, vec![], false);
        for time in [0.0, 0.1] {
            accessible_frame(&ctx, time, None, || popup.ui(&ctx, &theme, &[]));
        }
        let (update, _) = accessible_frame(&ctx, 0.2, Some(Key::ArrowDown), || {
            popup.ui(&ctx, &theme, &[])
        });
        assert!(
            status(&update, "Recent Files.").contains("Result 3 of 3: File /p/tests/γ space.py")
        );
        assert!(update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("File /p/tests/γ space.py")
                && node.is_selected() == Some(true)));
        let (_, outcome) =
            accessible_frame(&ctx, 0.3, Some(Key::Enter), || popup.ui(&ctx, &theme, &[]));
        assert_eq!(outcome, Some(PopupOutcome::File(expected)));
        assert!(!popup.visible);
        let (update, _) = accessible_frame(&ctx, 0.4, None, || popup.ui(&ctx, &theme, &[]));
        assert!(!update
            .nodes
            .iter()
            .any(|(_, node)| node.role() == egui::accesskit::Role::Status
                && node.label().is_some_and(|s| s.starts_with("Recent Files."))));
    }

    #[test]
    fn switcher_tree_announces_tool_column_and_preserves_stable_editor_identity() {
        let ctx = accessible_context();
        let theme = UiTheme::dark();
        let mut popup = SwitcherPopup::default();
        let editors = vec![
            (egui::Id::new("α editor"), file("α.py", "src", true, false)),
            (egui::Id::new("β editor"), file("β.py", "src", false, true)),
        ];
        let tools = vec![
            ToolEntry {
                label: "Project".into(),
                shortcut: "Alt+1".into(),
            },
            ToolEntry {
                label: "Problems".into(),
                shortcut: "Alt+6".into(),
            },
        ];
        popup.open_editors(editors.clone(), tools.clone(), Modifiers::NONE);
        for time in [0.0, 0.1] {
            accessible_frame(&ctx, time, None, || popup.ui(&ctx, &theme, &[]));
        }
        let (update, _) = accessible_frame(&ctx, 0.2, Some(Key::ArrowLeft), || {
            popup.ui(&ctx, &theme, &[])
        });
        assert!(status(&update, "Switcher.").contains("Item 2 of 2: Tool window Problems, Alt+6"));
        assert!(update.nodes.iter().any(|(_, node)| node.label()
            == Some("Tool window Problems, Alt+6")
            && node.is_selected() == Some(true)));
        let (_, outcome) =
            accessible_frame(&ctx, 0.3, Some(Key::Enter), || popup.ui(&ctx, &theme, &[]));
        assert_eq!(outcome, Some(PopupOutcome::Tool(1)));
        popup.open_editors(editors, tools, Modifiers::NONE);
        for time in [0.4, 0.5] {
            accessible_frame(&ctx, time, None, || popup.ui(&ctx, &theme, &[]));
        }
        let (_, outcome) =
            accessible_frame(&ctx, 0.6, Some(Key::Enter), || popup.ui(&ctx, &theme, &[]));
        assert_eq!(
            outcome,
            Some(PopupOutcome::Editor(egui::Id::new("β editor")))
        );
    }

    #[test]
    fn recent_locations_tree_announces_unicode_context_and_exact_position() {
        let ctx = accessible_context();
        let theme = UiTheme::dark();
        let mono = egui::FontId::monospace(12.0);
        let mut popup = RecentLocationsPopup::default();
        let first = PlaceEntry {
            point: NavPoint::new("/p/α.py", 0, 0),
            title: "α.py:1".into(),
            dir: "".into(),
            lines: vec![(0, "α = 1".into())],
        };
        let second = PlaceEntry {
            point: NavPoint::new("/p/β.py", 5, 2),
            title: "β.py:6".into(),
            dir: "".into(),
            lines: vec![(5, "π = '😀'".into())],
        };
        let expected = second.point.clone();
        popup.open(vec![first, second], vec![], false);
        for time in [0.0, 0.1] {
            accessible_frame(&ctx, time, None, || {
                popup.ui(&ctx, &theme, &[], mono.clone())
            });
        }
        let (update, _) = accessible_frame(&ctx, 0.2, Some(Key::ArrowDown), || {
            popup.ui(&ctx, &theme, &[], mono.clone())
        });
        assert!(status(&update, "Recent Locations.")
            .contains("Result 2 of 2: /p/β.py: line 6, column 3: π = '😀'"));
        assert!(update.nodes.iter().any(|(_, node)| node
            .label()
            .is_some_and(|s| s.starts_with("/p/β.py: line 6, column 3"))
            && node.is_selected() == Some(true)));
        let (_, outcome) = accessible_frame(&ctx, 0.3, Some(Key::Enter), || {
            popup.ui(&ctx, &theme, &[], mono.clone())
        });
        assert_eq!(outcome, Some(PopupOutcome::Place(expected)));
        assert!(!popup.visible);
    }
}

#[cfg(test)]
mod recent_scroll_tests {
    use super::*;
    fn frame(
        ctx: &egui::Context,
        _theme: &UiTheme,
        time: f64,
        events: Vec<egui::Event>,
        mut show: impl FnMut(),
    ) {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                ..Default::default()
            },
            |_| show(),
        );
        output.drop_without_applying_deltas();
    }
    fn key(key: Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            })
            .collect()
    }
    #[test]
    fn actual_recent_files_keyboard_reveals_distant_row_and_idle_preserves_manual_wheel() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let theme = UiTheme::dark();
        let mut popup = RecentFilesPopup::default();
        popup.open(
            (0..80)
                .map(|i| FileEntry {
                    path: PathBuf::from(format!("/p/δ_{i}.py")),
                    name: format!("δ_{i}.py"),
                    dir: "p".into(),
                    current: i == 0,
                    edited: false,
                    modified: false,
                })
                .collect(),
            vec![],
            false,
        );
        for i in 0..3 {
            frame(&ctx, &theme, i as f64 * 0.1, vec![], || {
                popup.ui(&ctx, &theme, &[]);
            });
        }
        for i in 0..4 {
            frame(
                &ctx,
                &theme,
                0.3 + i as f64 * 0.1,
                key(Key::PageDown),
                || {
                    popup.ui(&ctx, &theme, &[]);
                },
            );
        }
        frame(&ctx, &theme, 2.0, vec![], || {
            popup.ui(&ctx, &theme, &[]);
        });
        let (clip, selected) = popup.last_scroll.unwrap();
        assert!(
            selected.y > 300.0,
            "keyboard target must be visible, not merely selected offscreen"
        );
        let events = vec![
            egui::Event::PointerMoved(clip.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, clip.height() + 300.0),
                modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            },
        ];
        frame(&ctx, &theme, 2.1, events, || {
            popup.ui(&ctx, &theme, &[]);
        });
        // egui drains wheel smoothing per frame using stable_dt capped at 0.1;
        // a large timestamp jump is not a completed manual scroll gesture.
        for i in 1..=60 {
            frame(&ctx, &theme, 2.1 + i as f64 / 60.0, vec![], || {
                popup.ui(&ctx, &theme, &[]);
            });
            if !ctx.input(|input| input.is_scrolling()) {
                break;
            }
        }
        assert!(
            !ctx.input(|input| input.is_scrolling()),
            "actual manual wheel smoothing must finish within sixty frames"
        );
        assert!(
            !popup.reveal_selection,
            "idle must not reissue keyboard reveal"
        );
        let moved = popup.last_scroll.unwrap().1;
        frame(&ctx, &theme, 4.0, vec![], || {
            popup.ui(&ctx, &theme, &[]);
        });
        let idle = popup.last_scroll.unwrap().1;
        assert!(
            moved.y < selected.y - 50.0,
            "actual wheel moves viewport away from selection"
        );
        assert!(
            (moved.y - idle.y).abs() < 1.0,
            "idle must preserve manual browsing"
        );
        let row = popup.last_selected_row.unwrap();
        let actual_clip = popup.last_scroll.unwrap().0;
        assert!(
            row.min.y >= actual_clip.max.y || row.max.y <= actual_clip.min.y,
            "manual browsing must place the selected row offscreen: row={row:?}, clip={actual_clip:?}"
        );
        let previous_selection = popup.selected;
        frame(&ctx, &theme, 4.1, key(Key::ArrowDown), || {
            popup.ui(&ctx, &theme, &[]);
        });
        assert_eq!(popup.selected, previous_selection + 1);
        // scroll_to_me installs its animation target after painting the rows;
        // advance real frames through the default maximum 0.3-second animation.
        for i in 1..=60 {
            frame(&ctx, &theme, 4.1 + i as f64 / 60.0, vec![], || {
                popup.ui(&ctx, &theme, &[]);
            });
        }
        assert!(
            popup.last_scroll.unwrap().1.y > moved.y + 50.0,
            "next keyboard selection reveals again: moved={moved:?}, revealed={:?}, selection={}",
            popup.last_scroll.unwrap().1,
            popup.selected,
        );
        let row = popup.last_selected_row.unwrap();
        let actual_clip = popup.last_scroll.unwrap().0;
        assert!(
            row.min.y >= actual_clip.min.y && row.max.y <= actual_clip.max.y,
            "keyboard selection must actually be visible: row={row:?}, clip={actual_clip:?}"
        );
    }
    #[test]
    fn actual_recent_locations_page_navigation_reveals_snippet_selection() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let theme = UiTheme::dark();
        let mono = egui::FontId::monospace(12.0);
        let entries = (0..35)
            .map(|i| PlaceEntry {
                point: NavPoint::new(PathBuf::from("/p/π.py"), i, 0),
                title: format!("π.py:{}", i + 1),
                dir: "p".into(),
                lines: vec![(i, format!("π = {i}"))],
            })
            .collect();
        let mut popup = RecentLocationsPopup::default();
        popup.open(entries, vec![], false);
        for i in 0..3 {
            frame(&ctx, &theme, i as f64 * 0.1, vec![], || {
                popup.ui(&ctx, &theme, &[], mono.clone());
            });
        }
        for i in 0..4 {
            frame(
                &ctx,
                &theme,
                0.3 + i as f64 * 0.1,
                key(Key::PageDown),
                || {
                    popup.ui(&ctx, &theme, &[], mono.clone());
                },
            );
        }
        frame(&ctx, &theme, 2.0, vec![], || {
            popup.ui(&ctx, &theme, &[], mono.clone());
        });
        assert_eq!(popup.selected, 20);
        assert!(popup.last_scroll.unwrap().1.y > 300.0);
    }
}

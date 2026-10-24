//! The in-file Find / Replace bar (roadmap §1.4): match case, whole words and
//! regex toggles, find in selection, regex replacement with groups and case
//! conversions, preserve case, a persisted search history, Exclude, Alt+Enter
//! to select every match, and an occurrence counter.

use super::*;
use crate::a11y::Accessible as _;
use crate::a11y::AccessibleCombo;
use crate::editor::find::{self, FindOptions};

const HISTORY_LIMIT: usize = 20;

/// Recent searches and replacements, kept in `<config>/find_history.json`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FindHistory {
    #[serde(default)]
    find: Vec<String>,
    #[serde(default)]
    replace: Vec<String>,
}

impl FindHistory {
    fn path() -> PathBuf {
        Settings::config_dir().join("find_history.json")
    }

    fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(Settings::config_dir());
            let _ = std::fs::write(Self::path(), text);
        }
    }
}

/// Takes this frame's Enter press and returns its modifiers. The event is
/// removed so that the editor, which gets the focus back in the same frame
/// when the bar closes (Alt+Enter), does not act on it too.
fn take_enter(ui: &egui::Ui) -> Option<egui::Modifiers> {
    ui.input_mut(|i| take_key_event(&mut i.events, egui::Key::Enter))
}

/// Removes the first press of `key` from `events`; returns its modifiers.
fn take_key_event(events: &mut Vec<egui::Event>, key: egui::Key) -> Option<egui::Modifiers> {
    let at = events
        .iter()
        .position(|e| matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key))?;
    match events.remove(at) {
        egui::Event::Key { modifiers, .. } => Some(modifiers),
        _ => None,
    }
}

/// Takes this frame's Escape press (closing the bar must not also clear the
/// editor's selection of the current match).
fn take_escape(ui: &egui::Ui) -> bool {
    ui.input_mut(|i| take_key_event(&mut i.events, egui::Key::Escape))
        .is_some()
}

/// A small button with an up or down chevron (the UI font has no arrow glyphs).
fn arrow_button(ui: &mut egui::Ui, up: bool, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(22.0, 20.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let c = rect.center();
    let d = if up { -1.0 } else { 1.0 };
    let points = vec![
        Pos2::new(c.x - 4.5, c.y - 2.5 * d),
        Pos2::new(c.x, c.y + 2.5 * d),
        Pos2::new(c.x + 4.5, c.y - 2.5 * d),
    ];
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));
    resp
}

/// Moves `value` to the front of `list`; returns whether the list changed.
fn remember(list: &mut Vec<String>, value: &str) -> bool {
    if value.is_empty() || list.first().map(String::as_str) == Some(value) {
        return false;
    }
    list.retain(|v| v != value);
    list.insert(0, value.to_string());
    list.truncate(HISTORY_LIMIT);
    true
}

/// "Find in selection": the searched range in one editor, valid for a document version.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FindScope {
    pub tab: egui::Id,
    pub range: TextRange,
    pub version: u64,
}

#[derive(Default)]
pub(super) struct FindBar {
    pub visible: bool,
    pub replace_mode: bool,
    pub query: String,
    pub replacement: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub regex: bool,
    pub preserve_case: bool,
    pub multiline: bool,
    pub filter: find::SearchFilter,
    preview: Option<find::ReplacementPreview>,
    preview_error: Option<String>,
    preview_key: Option<PreviewKey>,
    pub scope: Option<FindScope>,
    /// Matches in the active editor that are highlighted (excluded ones left out).
    pub matches: Vec<TextRange>,
    /// Matches left out of Replace All ("Exclude").
    pub excluded: Vec<TextRange>,
    pub focus_query: bool,
    pub message: String,
    pub error: Option<String>,
    history: Option<FindHistory>,
    /// What `matches` were computed for: (editor, document version, query, options).
    computed: Option<SearchKey>,
    /// The search `excluded` belongs to (edits and other queries reset it).
    excluded_for: Option<SearchKey>,
    /// The query / replacement field had keyboard focus at the end of the last
    /// frame (egui drops it on Enter and Escape before the bar sees the key).
    query_focused: bool,
    replacement_focused: bool,
    /// Who had the keyboard before the pointer went down (see [`keyboard_owner`]).
    owner: KeyboardOwner,
}

type SearchKey = (
    egui::Id,
    rustpy_text::DocumentId,
    u64,
    crate::editor::Language,
    String,
    FindOptions,
);
type PreviewKey = (SearchKey, String, Vec<TextRange>);

/// Multiline fields keep ordinary Enter for text input. Modified Enter belongs
/// to the bar before TextEdit can insert a newline or surrender focus.
fn take_navigation_enter(ui: &egui::Ui, multiline: bool) -> Option<egui::Modifiers> {
    if !multiline {
        return None;
    }
    ui.input_mut(|input| {
        let at = input.events.iter().position(|event| {
            matches!(event,
            egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. }
            if modifiers.ctrl || modifiers.command || modifiers.alt)
        })?;
        match input.events.remove(at) {
            egui::Event::Key { modifiers, .. } => Some(modifiers),
            _ => None,
        }
    })
}
fn preview_text(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars();
    for character in chars.by_ref().take(120) {
        match character {
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    if chars.next().is_some() {
        output.push('…');
    }
    if output.is_empty() {
        output.push_str("(empty)");
    }
    output
}

/// Where the keyboard is while the find bar is shown. The bar's buttons are not
/// focusable and never keep it: a click on one gives it back to its owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum KeyboardOwner {
    #[default]
    Elsewhere,
    Query,
    Replacement,
    Editor,
}

/// The keyboard owner to remember for this frame. While a pointer button is
/// down or being released it stays as it was: clicking one of the bar's
/// buttons takes the focus from the field or the editor (egui drops it on a
/// click elsewhere) that the click then gives back.
fn keyboard_owner(
    previous: KeyboardOwner,
    pointer_busy: bool,
    query: bool,
    replacement: bool,
    editor: bool,
) -> KeyboardOwner {
    if pointer_busy {
        previous
    } else if query {
        KeyboardOwner::Query
    } else if replacement {
        KeyboardOwner::Replacement
    } else if editor {
        KeyboardOwner::Editor
    } else {
        KeyboardOwner::Elsewhere
    }
}

impl FindBar {
    fn options(&self, scope: Option<TextRange>) -> FindOptions {
        FindOptions {
            case_sensitive: self.case_sensitive,
            whole_words: self.whole_words,
            regex: self.regex,
            preserve_case: self.preserve_case && !self.regex,
            multiline: self.multiline,
            filter: self.filter,
            scope,
        }
    }

    fn history(&mut self) -> &mut FindHistory {
        self.history.get_or_insert_with(FindHistory::load)
    }

    fn remember_query(&mut self) {
        let (q, r, replace) = (
            self.query.clone(),
            self.replacement.clone(),
            self.replace_mode,
        );
        let h = self.history();
        let mut changed = remember(&mut h.find, &q);
        if replace {
            changed |= remember(&mut h.replace, &r);
        }
        if changed {
            h.save();
        }
    }
}

impl IdeApp {
    /// Ctrl+F / Ctrl+R: a selection over several lines becomes the search scope
    /// ("in selection"); a one-line selection or the word at the caret becomes the query.
    pub(super) fn open_find_bar(&mut self, replace: bool) {
        self.find.visible = true;
        self.find.replace_mode = replace;
        self.find.focus_query = true;
        self.find.computed = None;
        self.find.excluded.clear();
        let multi_line = self.active.and_then(|i| self.tabs.get(i)).and_then(|t| {
            let k = t.state.primary();
            (k.has_selection() && t.state.doc.slice(k.range()).contains('\n')).then(|| FindScope {
                tab: t.view.id,
                range: k.range(),
                version: t.state.doc.version(),
            })
        });
        match multi_line {
            Some(scope) => self.find.scope = Some(scope),
            None => {
                self.find.scope = None;
                self.seed_find_query();
            }
        }
    }

    pub(super) fn seed_find_query(&mut self) {
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            let k = tab.state.primary();
            let text = if k.has_selection() {
                tab.state.doc.slice(k.range())
            } else {
                tab.state
                    .doc
                    .word_range_at(k.head())
                    .map(|r| tab.state.doc.slice(r))
                    .unwrap_or_default()
            };
            if !text.is_empty() && !text.contains('\n') {
                self.find.query = if self.find.regex {
                    regex::escape(&text)
                } else {
                    text
                };
            }
        }
    }

    /// Find Word at Caret (Ctrl+F3): the word (or one-line selection) in the whole file.
    pub(super) fn find_word_at_caret(&mut self) {
        self.seed_find_query();
        self.find.scope = None;
        self.find.visible = true;
        self.find_step(false);
    }

    pub(super) fn find_step(&mut self, backwards: bool) {
        self.find_step_impl(backwards, false);
        self.find.remember_query();
    }

    /// Hides the bar (Escape, ×, Alt+Enter) and gives the keyboard back to the editor.
    pub(super) fn close_find_bar(&mut self) {
        self.find.visible = false;
        self.find.matches.clear();
        self.find.computed = None;
        self.find.preview = None;
        self.find.preview_error = None;
        self.find.preview_key = None;
        self.find.query_focused = false;
        self.find.replacement_focused = false;
        // F3 / Shift+F3 after closing search the whole file again.
        self.find.scope = None;
        self.focus_editor = true;
    }

    /// The search scope for the active editor, following edits made since it was set.
    fn current_find_scope(&mut self) -> Option<TextRange> {
        let scope = self.find.scope?;
        let tab = self.active.and_then(|i| self.tabs.get(i))?;
        if tab.view.id != scope.tab {
            return None;
        }
        let version = tab.state.doc.version();
        if version == scope.version {
            return Some(scope.range);
        }
        let mut range = scope.range;
        match tab.state.doc.changes_since(scope.version) {
            Some(changes) => {
                for edits in changes {
                    range = TextRange::new(
                        crate::editor::state::map_offset(&edits, range.start(), false),
                        crate::editor::state::map_offset(&edits, range.end(), true),
                    );
                }
                self.find.scope = Some(FindScope {
                    range,
                    version,
                    ..scope
                });
                Some(range)
            }
            None => {
                self.find.scope = None;
                None
            }
        }
    }

    /// Recomputes the highlighted matches when the query, the options, the
    /// editor or its text changed.
    fn refresh_find_matches(&mut self) {
        let scope = self.current_find_scope();
        let opts = self.find.options(scope);
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            self.find.matches.clear();
            return;
        };
        let key = (
            tab.view.id,
            tab.state.doc.identity(),
            tab.state.doc.version(),
            tab.state.language,
            self.find.query.clone(),
            opts.clone(),
        );
        if self.find.computed.as_ref() == Some(&key) {
            return;
        }
        if self.find.excluded_for.as_ref() != Some(&key) {
            // Excluded matches belong to one search over one text.
            self.find.excluded.clear();
            self.find.excluded_for = None;
        }
        match tab.state.find_matches(&self.find.query, &opts) {
            Ok(all) => {
                self.find.error = None;
                self.find.matches = all
                    .into_iter()
                    .filter(|m| !self.find.excluded.contains(m))
                    .collect();
            }
            Err(e) => {
                self.find.error = Some(e);
                self.find.matches.clear();
            }
        }
        self.find.computed = Some(key);
    }

    /// An immutable, bounded plan; typing in either field never edits the document.
    fn refresh_replacement_preview(&mut self) {
        if !self.find.visible || !self.find.replace_mode {
            self.find.preview = None;
            self.find.preview_error = None;
            self.find.preview_key = None;
            return;
        }
        let scope = self.current_find_scope();
        let options = self.find.options(scope);
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            self.find.preview = None;
            self.find.preview_error = None;
            self.find.preview_key = None;
            return;
        };
        let key = (
            (
                tab.view.id,
                tab.state.doc.identity(),
                tab.state.doc.version(),
                tab.state.language,
                self.find.query.clone(),
                options.clone(),
            ),
            self.find.replacement.clone(),
            self.find.excluded.clone(),
        );
        if self.find.preview_key.as_ref() == Some(&key) {
            return;
        }
        match tab.state.replacement_preview(
            &self.find.query,
            &self.find.replacement,
            &options,
            &self.find.excluded,
            3,
        ) {
            Ok(preview) => {
                self.find.preview = Some(preview);
                self.find.preview_error = None;
            }
            Err(error) => {
                self.find.preview = None;
                self.find.preview_error = Some(error);
            }
        }
        self.find.preview_key = Some(key);
    }

    /// Keys handled by the focused find bar before global shortcut dispatch.
    pub(super) fn find_bar_owns_key(&self, action: Action) -> bool {
        // Multiline Ctrl+Shift+Enter belongs to navigation/replacement, not Complete Statement.
        self.find.visible
            && (action == Action::ShowIntentionActions
                || (self.find.multiline && action == Action::CompleteStatement))
            && (self.find.query_focused || self.find.replacement_focused)
    }

    fn update_find_counter(&mut self) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        if self.find.query.is_empty() {
            self.find.message.clear();
            return;
        }
        let excluded = if self.find.excluded.is_empty() {
            String::new()
        } else {
            format!(" ({} excluded)", self.find.excluded.len())
        };
        let n = self.find.matches.len();
        let more = if n >= find::MAX_MATCHES { "+" } else { "" };
        self.find.message = match self
            .find
            .matches
            .iter()
            .position(|m| *m == tab.state.primary().range())
        {
            _ if n == 0 => "No matches".into(),
            Some(i) => format!("{}/{n}{more}{excluded}", i + 1),
            None => format!("{n}{more} results{excluded}"),
        };
    }

    fn find_step_impl(&mut self, backwards: bool, incremental: bool) {
        let scope = self.current_find_scope();
        let opts = self.find.options(scope);
        let q = self.find.query.clone();
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            match tab.state.find_with(&q, &opts, backwards, incremental) {
                Ok(_) => self.find.error = None,
                Err(e) => self.find.error = Some(e),
            }
        }
        self.find.computed = None;
        self.refresh_find_matches();
    }

    pub(super) fn find_bar(&mut self, ui: &mut egui::Ui) {
        let mut do_find: Option<bool> = None;
        let mut incremental = false;
        let mut close = false;
        let mut select_all = false;
        let mut replace_one = false;
        let mut replace_all = false;
        let mut exclude = false;
        // A click on one of the bar's buttons gives the keyboard back to its owner.
        let mut bar_click = false;
        let (mut query_id, mut replacement_id) = (None, None);
        let editor_focused = self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|t| t.view.focused);
        let pointer_busy = ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
        self.find.owner = keyboard_owner(
            self.find.owner,
            pointer_busy,
            self.find.query_focused,
            self.find.replacement_focused,
            editor_focused,
        );
        let theme = self.theme.clone();
        let options_before = self.find.options(None);
        let scope_before = self.find.scope.is_some();
        egui::Frame::new().fill(theme.panel_bg).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                bar_click |= ui.checkbox(&mut self.find.multiline, "Multiline").on_hover_text("Enter inserts a newline; Ctrl+Enter finds next, Ctrl+Shift+Enter finds previous").changed();
                let supported = self.active.and_then(|index| self.tabs.get(index)).is_some_and(|tab| find::supports_filter(tab.state.language));
                egui::ComboBox::from_id_salt("find-context-filter").selected_text(self.find.filter.label()).show_ui(ui, |ui| {
                    for filter in [find::SearchFilter::Anywhere, find::SearchFilter::Comments,
                        find::SearchFilter::Strings, find::SearchFilter::ExceptComments,
                        find::SearchFilter::ExceptStrings, find::SearchFilter::ExceptCommentsAndStrings] {
                        let response = ui.add_enabled(supported || filter == find::SearchFilter::Anywhere,
                            egui::Button::new(filter.label()).selected(self.find.filter == filter));
                        if response.clicked() { self.find.filter = filter; bar_click = true; ui.close(); }
                    }
                }).combo_named(crate::i18n::tr_static("Context"));
                if !supported && self.find.filter != find::SearchFilter::Anywhere {
                    ui.label(egui::RichText::new("Context filtering is unavailable for this language").color(theme.error));
                }
            });
            ui.horizontal(|ui| {
                ui.menu_button("Recent", |ui| {
                    ui.label(egui::RichText::new("Recent searches").color(theme.text_dim).small());
                    let items = self.find.history().find.clone();
                    if items.is_empty() {
                        ui.label(egui::RichText::new("(none)").color(theme.text_dim));
                    }
                    for h in items {
                        if ui.button(&h).clicked() {
                            self.find.query = h;
                            do_find = Some(false);
                            incremental = true;
                            bar_click = true;
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("Search history");
                let navigation_enter = if self.find.query_focused { take_navigation_enter(ui, self.find.multiline) } else { None };
                let field = if self.find.multiline { egui::TextEdit::multiline(&mut self.find.query).desired_rows(3) }
                    else { egui::TextEdit::singleline(&mut self.find.query) };
                let resp = ui.add(field.id_salt("find-query-field").desired_width(260.0).hint_text(if self.find.regex { "Regex" } else { "Search" }));
                query_id = Some(resp.id);
                if self.find.focus_query {
                    resp.request_focus();
                    crate::theme::select_all_text(ui, &resp, &self.find.query);
                    self.find.focus_query = false;
                    // A control clicked immediately after opening must retain
                    // this field, even before an idle frame observes its focus.
                    self.find.owner = KeyboardOwner::Query;
                    // The field takes the keyboard: a pending request for the
                    // editor (drawn after the bar) must not take it back.
                    self.focus_editor = false;
                }
                if resp.changed() {
                    incremental = true;
                    do_find = Some(false);
                }
                if self.find.query_focused || resp.has_focus() {
                    if let Some(mods) = navigation_enter.or_else(|| (!self.find.multiline).then(|| take_enter(ui)).flatten()) {
                        // Enter / Shift+Enter: next / previous (the field keeps focus); Alt+Enter: select all.
                        if mods.alt {
                            select_all = true;
                        } else {
                            do_find = Some(mods.shift);
                            resp.request_focus();
                        }
                    }
                    if take_escape(ui) {
                        close = true;
                    }
                }
                self.find.query_focused = resp.has_focus();
                bar_click |= ui.toggle_value(&mut self.find.case_sensitive, "Cc").on_hover_text("Match Case").clicked();
                bar_click |= ui.toggle_value(&mut self.find.whole_words, "W").on_hover_text("Words").clicked();
                bar_click |= ui.toggle_value(&mut self.find.regex, ".*").on_hover_text("Regex: $1 / ${name} groups and \\u \\l \\U \\L \\E case conversions in the replacement").clicked();
                let mut in_selection = self.find.scope.is_some();
                let can_scope = in_selection || self.active.and_then(|i| self.tabs.get(i)).is_some_and(|t| t.state.primary().has_selection());
                if ui.add_enabled(can_scope, egui::Button::new("In Selection").selected(in_selection)).on_hover_text("Search only in the selected text").on_disabled_hover_text("Select text to search in it").clicked() {
                    bar_click = true;
                    in_selection = !in_selection;
                    self.find.scope = if in_selection { self.active.and_then(|i| self.tabs.get(i)).map(|t| FindScope { tab: t.view.id, range: t.state.primary().range(), version: t.state.doc.version() }) } else { None };
                }
                if arrow_button(ui, true, theme.text).icon_hint("Previous Occurrence (Shift+F3)").clicked() {
                    bar_click = true;
                    do_find = Some(true);
                }
                if arrow_button(ui, false, theme.text).icon_hint("Next Occurrence (F3)").clicked() {
                    bar_click = true;
                    do_find = Some(false);
                }
                if ui.button("Select All").on_hover_text("Select all occurrences (Alt+Enter)").clicked() {
                    select_all = true;
                }
                match &self.find.error {
                    Some(e) => {
                        ui.label(egui::RichText::new(e).color(theme.error)).on_hover_text("Invalid regular expression");
                    }
                    None => {
                        let color = if self.find.message == "No matches" { theme.error } else { theme.text_dim };
                        ui.label(egui::RichText::new(&self.find.message).color(color));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("×").icon_hint("Close (Escape)").clicked() {
                        close = true;
                    }
                    if ui.selectable_label(self.find.replace_mode, "Replace").clicked() {
                        bar_click = true;
                        self.find.replace_mode = !self.find.replace_mode;
                    }
                });
            });
            if self.find.replace_mode {
                ui.horizontal(|ui| {
                    ui.menu_button("Recent", |ui| {
                        ui.label(egui::RichText::new("Recent replacements").color(theme.text_dim).small());
                        let items = self.find.history().replace.clone();
                        if items.is_empty() {
                            ui.label(egui::RichText::new("(none)").color(theme.text_dim));
                        }
                        for h in items {
                            if ui.button(&h).clicked() {
                                self.find.replacement = h;
                                bar_click = true;
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Replacement history");
                    let navigation_enter = if self.find.replacement_focused { take_navigation_enter(ui, self.find.multiline) } else { None };
                    let field = if self.find.multiline { egui::TextEdit::multiline(&mut self.find.replacement).desired_rows(3) }
                        else { egui::TextEdit::singleline(&mut self.find.replacement) };
                    let resp = ui.add(field.id_salt("find-replacement-field").desired_width(260.0).hint_text("Replace with"));
                    replacement_id = Some(resp.id);
                    if self.find.replacement_focused || resp.has_focus() {
                        if let Some(modifiers) = navigation_enter.or_else(|| (!self.find.multiline).then(|| take_enter(ui)).flatten()) {
                            if modifiers.alt { select_all = true; }
                            else if modifiers.shift && self.find.multiline { replace_all = true; }
                            else { replace_one = true; }
                            resp.request_focus();
                        }
                        if take_escape(ui) {
                            close = true;
                        }
                    }
                    self.find.replacement_focused = resp.has_focus();
                    if ui.button("Replace").clicked() {
                        bar_click = true;
                        replace_one = true;
                    }
                    if ui.button("Replace All").clicked() {
                        bar_click = true;
                        replace_all = true;
                    }
                    if ui.button("Exclude").on_hover_text("Leave the current occurrence out of Replace All").clicked() {
                        bar_click = true;
                        exclude = true;
                    }
                    bar_click |= ui.add_enabled(!self.find.regex, egui::Checkbox::new(&mut self.find.preserve_case, "Preserve case")).on_hover_text("foo → bar, Foo → Bar, FOO → BAR").on_disabled_hover_text("Not available with regular expressions").clicked();
                });
            }
            self.refresh_find_matches();
            self.refresh_replacement_preview();
            if self.find.replace_mode {
                if let Some(error) = &self.find.preview_error {
                    ui.label(egui::RichText::new(format!("Replacement preview: {error}")).color(theme.error));
                } else if let Some(preview) = &self.find.preview {
                    ui.label(egui::RichText::new("Live replacement preview (document unchanged)").small().color(theme.text_dim));
                    if preview.rows.is_empty() {
                        ui.label(if preview.has_more { "Preview omitted due to size limit" } else { "No eligible occurrences" });
                    }
                    for row in &preview.rows {
                        ui.label(egui::RichText::new(format!("{} → {}", preview_text(&row.original), preview_text(&row.replacement))).monospace());
                    }
                    if preview.has_more { ui.label("Preview is bounded; additional changes are not shown"); }
                }
            }
        });
        if self.find.options(None) != options_before || self.find.scope.is_some() != scope_before {
            do_find.get_or_insert(false);
            incremental = true;
        }
        if replace_one || replace_all {
            self.find_replace(replace_all);
        }
        if exclude {
            if let Some(cur) = self
                .active
                .and_then(|i| self.tabs.get(i))
                .map(|t| t.state.primary().range())
            {
                if self.find.matches.contains(&cur) {
                    self.find.excluded.push(cur);
                    self.find.excluded_for = self.find.computed.take();
                }
            }
            do_find = Some(false);
            incremental = false;
        }
        if let Some(backwards) = do_find {
            self.find_step_impl(backwards, incremental);
            if !incremental {
                self.find.remember_query();
            }
        }
        if select_all {
            let scope = self.current_find_scope();
            let opts = self.find.options(scope);
            let q = self.find.query.clone();
            if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                match tab.state.select_all_matches(&q, &opts) {
                    Ok(0) => self.find.message = "No matches".into(),
                    Ok(n) => {
                        self.set_status(format!("{n} occurrences selected"));
                        self.find.remember_query();
                        close = true;
                    }
                    Err(e) => self.find.error = Some(e),
                }
            }
        }
        self.refresh_find_matches();
        self.update_find_counter();
        if close {
            self.close_find_bar();
        } else if bar_click {
            // The bar's buttons are not focusable: a click on one leaves
            // the keyboard where it was: typing goes on in the field, Escape
            // and the arrows keep working in the editor.
            let field = match self.find.owner {
                KeyboardOwner::Query => query_id,
                KeyboardOwner::Replacement => replacement_id.or(query_id),
                KeyboardOwner::Editor => {
                    self.focus_editor = true;
                    None
                }
                KeyboardOwner::Elsewhere => None,
            };
            if let Some(id) = field {
                ui.memory_mut(|m| m.request_focus(id));
                self.find.query_focused = query_id == Some(id);
                self.find.replacement_focused = replacement_id == Some(id);
            }
        }
    }

    /// Replace (the selected occurrence, then move on) or Replace All.
    fn find_replace(&mut self, all: bool) {
        let scope = self.current_find_scope();
        let opts = self.find.options(scope);
        let (q, r) = (self.find.query.clone(), self.find.replacement.clone());
        let excluded = self.find.excluded.clone();
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let version = tab.state.doc.version();
        let result = if all {
            tab.state
                .replace_all_with(&q, &r, &opts, &excluded)
                .map(Some)
        } else {
            tab.state.replace_current_with(&q, &r, &opts).map(|_| None)
        };
        // A single Replace keeps the other occurrences excluded (moved by its edit).
        let still_excluded = if all {
            None
        } else {
            moved_ranges(&tab.state.doc, version, &excluded)
        };
        let refused = tab.state.doc.is_read_only() && tab.state.read_only_refused;
        match result {
            Ok(_) if refused => {
                self.set_status("Cannot replace: file is read-only");
            }
            Ok(Some(n)) => {
                self.find.message = format!("{n} replaced");
                self.set_status(format!("Replaced {n} occurrence(s)"));
            }
            Ok(None) => {}
            Err(e) => self.find.error = Some(e),
        }
        self.find.remember_query();
        self.find.excluded = still_excluded.unwrap_or_default();
        self.find.excluded_for = None;
        if !self.find.excluded.is_empty() {
            let scope = self.current_find_scope();
            let opts = self.find.options(scope);
            if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
                self.find.excluded_for = Some((
                    tab.view.id,
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    tab.state.language,
                    self.find.query.clone(),
                    opts,
                ));
            }
        }
        self.find.computed = None;
    }
}

/// `ranges` (matches) moved through the edits made to `doc` since `version`;
/// `None` when the change log no longer reaches back that far.
fn moved_ranges(
    doc: &rustpy_text::Document,
    version: u64,
    ranges: &[TextRange],
) -> Option<Vec<TextRange>> {
    use crate::editor::state::map_offset;
    let changes = doc.changes_since(version)?;
    Some(
        ranges
            .iter()
            .map(|r| {
                changes.iter().fold(*r, |r, edits| {
                    TextRange::new(
                        map_offset(edits, r.start(), true),
                        map_offset(edits, r.end(), false),
                    )
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_recent_unique_entries() {
        let mut list = vec!["b".to_string(), "a".to_string()];
        assert!(remember(&mut list, "a"));
        assert_eq!(list, ["a", "b"]);
        assert!(!remember(&mut list, "a"), "already first");
        assert!(!remember(&mut list, ""));
        for i in 0..30 {
            remember(&mut list, &i.to_string());
        }
        assert_eq!(list.len(), HISTORY_LIMIT);
        assert_eq!(list[0], "29");
    }

    #[test]
    fn exclusions_follow_a_single_replacement() {
        use crate::editor::find::FindOptions;
        let mut state = crate::editor::EditorState::from_text(
            "foo foo foo foo",
            crate::editor::Language::PlainText,
        );
        let opts = FindOptions::default();
        let all = state.find_matches("foo", &opts).unwrap();
        // Exclude the 2nd and 4th, then Replace the 1st with a longer text.
        let excluded = vec![all[1], all[3]];
        state.set_carets(vec![crate::editor::Caret::new(
            all[0].start(),
            all[0].end(),
        )]);
        let version = state.doc.version();
        assert!(state.replace_current_with("foo", "barbaz", &opts).unwrap());
        let moved = moved_ranges(&state.doc, version, &excluded).unwrap();
        let text = state.doc.text();
        assert!(
            moved
                .iter()
                .all(|r| &text[usize::from(r.start())..usize::from(r.end())] == "foo"),
            "{moved:?} in {text:?}"
        );
        // Replace All leaves the excluded occurrences alone.
        assert_eq!(
            state.replace_all_with("foo", "x", &opts, &moved).unwrap(),
            1
        );
        assert_eq!(state.doc.text(), "barbaz foo x foo");
    }

    #[test]
    fn a_click_on_a_bar_button_gives_the_keyboard_back_to_its_owner() {
        use KeyboardOwner::*;
        // Typing in the query; clicking a toggle takes the focus from the field
        // (egui drops it on a click elsewhere) but the owner stays the query ...
        let owner = keyboard_owner(Elsewhere, false, true, false, false);
        assert_eq!(owner, Query);
        assert_eq!(
            keyboard_owner(owner, true, false, false, false),
            Query,
            "while the button is held and released"
        );
        // ... same for the replacement field and the editor (Next Occurrence clicked from the editor).
        assert_eq!(
            keyboard_owner(Replacement, true, false, false, false),
            Replacement
        );
        assert_eq!(keyboard_owner(Query, false, false, false, true), Editor);
        assert_eq!(keyboard_owner(Editor, true, false, false, false), Editor);
        // Once the pointer is up, the owner follows the focus again.
        assert_eq!(
            keyboard_owner(Editor, false, false, true, false),
            Replacement
        );
        assert_eq!(keyboard_owner(Query, false, false, false, false), Elsewhere);
    }

    #[test]
    fn handled_keys_are_taken_out_of_the_frame() {
        let press = |key, alt| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt,
                ..Default::default()
            },
        };
        let mut events = vec![
            egui::Event::Text("x".into()),
            press(egui::Key::Enter, true),
            press(egui::Key::Escape, false),
        ];
        assert_eq!(
            take_key_event(&mut events, egui::Key::Enter).map(|m| m.alt),
            Some(true)
        );
        assert_eq!(
            events.len(),
            2,
            "the editor, focused again in the same frame, must not see Alt+Enter"
        );
        assert_eq!(take_key_event(&mut events, egui::Key::Enter), None);
        assert!(take_key_event(&mut events, egui::Key::Escape).is_some());
        assert_eq!(events, vec![egui::Event::Text("x".into())]);
    }
}

#[cfg(test)]
#[path = "find_bar_connected_tests.rs"]
mod connected_tests;

//! egui rendering and input handling for the editor (roadmap §1.1, §1.2).

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Align2, Color32, EventFilter, FontId, Galley, Pos2, Rect, Sense, Stroke, Vec2};
use rustpy_core::{Position, TextRange, TextSize};
use rustpy_highlight::{ColorScheme, HighlightTag};
use rustpy_settings::EditorSettings;
use rustpy_text::util::char_width;

use super::session::SessionPosition;
use super::state::{Caret, EditOptions, EditorState};
use crate::actions::{Action, Keymap};
#[cfg(test)]
use crate::theme::mono_family;
use crate::theme::{rgb, UiTheme};

#[path = "inspection_widget.rs"]
mod inspection_widget;
#[path = "scrollbar_preview.rs"]
mod scrollbar_preview;

type ColorCache = (
    rustpy_text::DocumentId,
    u64,
    super::Language,
    bool,
    std::sync::Arc<Vec<super::color_preview::ColorPreview>>,
);

/// Per-tab view state that is not part of the document model.
pub struct EditorView {
    ime: super::ime::Composition,
    pub review_comments: Vec<super::review_comments::Marker>,
    pub scientific_cell_separators: bool,
    pub clipboard_payload: Option<super::commands::clipboard_payload::ClipboardPayload>,
    pub id: egui::Id,
    smooth_scrolling: Option<bool>,
    wheel_gesture: scrollbar_preview::WheelGesture,
    blink: super::caret::BlinkState,
    method_cache: Option<(
        rustpy_text::DocumentId,
        u64,
        super::Language,
        bool,
        Vec<super::method_separators::MethodSeparator>,
    )>,
    color_cache: Option<ColorCache>,
    /// Widest line estimate: (document, version, tab size, columns, versions since a full scan).
    max_columns_cache: Option<(rustpy_text::DocumentId, u64, usize, usize, u32)>,
    drag_anchor: Option<TextSize>,
    drag_virtual_anchor: Option<Position>,
    pub zoom_delta: f32,
    last_click_time: f64,
    /// Where the last click was (a double-click needs both clicks close together).
    last_click_pos: egui::Pos2,
    click_count: u32,
    /// Screen rectangle of the primary caret after the last frame.
    pub caret_screen_rect: Option<Rect>,
    /// Document offset under the mouse pointer (text area only).
    pub hovered_offset: Option<TextSize>,
    /// Set for one frame when the user Ctrl+clicked an offset (go to declaration).
    pub ctrl_click: Option<TextSize>,
    /// Whether the editor had keyboard focus in the last frame.
    pub focused: bool,
    /// Characters typed in the last frame (for completion auto-popup).
    pub typed: String,
    /// A click landed on a sticky header line this frame (ignore it for caret placement).
    pub sticky_consumed_click: bool,
    /// A gutter intention owns its primary press through drag and release.
    gutter_press_owned: bool,
    /// Soft-wrap cache: (doc version, wrap columns, visual rows per line).
    pub wrap_cache: Option<(u64, usize, Vec<u32>)>,
    wrap_hints: Vec<rustpy_semantic::features::InlayHint>,
    /// Drag-and-drop of the selection in progress: the dragged source range.
    dnd: Option<TextRange>,
    /// Where the dragged text would be dropped.
    dnd_target: Option<TextSize>,
    /// VCS change popup opened from a gutter marker (managed by the app):
    /// the marked line and the screen position to show it at (`None`: below the caret).
    pub change_popup: Option<(usize, Option<Pos2>)>,
    /// "Ctrl, Ctrl+Up/Down" (Clone Caret Above/Below): when Ctrl was last pressed
    /// and whether it was pressed twice and is still held.
    last_ctrl_press: f64,
    ctrl_twice: bool,
    /// First and last line shown last frame: rendered docstrings near them are laid out first.
    visible_lines: Option<(usize, usize)>,
    /// See [`DocAnchor`].
    doc_anchor: Option<DocAnchor>,
    pub center_request: super::navigation_scroll::CenterRequest,
    center_target: Option<f32>,
    viewport_height: f32,
    pub page_lines: usize,
    /// Top logical source line after the last completed viewport frame.
    top_line: u32,
    /// A saved viewport also remains available for tabs not rendered yet.
    pending_scroll_line: Option<u32>,
    /// A large-file placeholder cannot clamp the saved caret until its document arrives.
    pending_session_caret: Option<Position>,
    /// Discard a restore if editing/navigation supersedes it before layout.
    session_restore_guard: Option<(u64, Position)>,
}

/// A line shown at the end of a frame (the caret's, else the first), to keep
/// it in place when rendered docstrings above it change size without an edit
/// (laid out late, shown as source while a caret is in them, toggled).
#[derive(Clone, Copy, Debug)]
struct DocAnchor {
    version: u64,
    /// Hash of the rendered docstrings' lines and rows.
    signature: u64,
    line_h: f32,
    line: usize,
    /// The line's first row.
    row: usize,
    /// Vertical scroll offset.
    offset: f32,
}

impl EditorView {
    pub fn ime_composing(&self) -> bool {
        self.ime.composing()
    }

    /// Keyboard equivalent of the painted color tooltip, using only the current
    /// View cache. Eight colors per page keeps long declaration lines bounded.
    pub(crate) fn cached_line_color_information(
        &self,
        state: &EditorState,
        line: usize,
        page: usize,
    ) -> Option<(String, usize)> {
        let (id, version, language, large, colors) = self.color_cache.as_ref()?;
        if *id != state.doc.identity()
            || *version != state.doc.version()
            || *language != state.language
            || *large != state.is_large_file()
            || line >= state.doc.line_count()
        {
            return None;
        }
        let range = state.doc.line_range(line);
        let start = colors.partition_point(|color| color.range.start() < range.start());
        let end = colors.partition_point(|color| color.range.start() < range.end());
        let count = end - start;
        if count == 0 {
            return Some(("No source color previews on this line.".into(), 1));
        }
        let pages = count.div_ceil(8);
        let page = page.min(pages - 1);
        let mut text = format!(
            "Source colors: page {} of {pages}; {count} colors.\n",
            page + 1
        );
        for color in colors[start..end].iter().skip(page * 8).take(8) {
            let source: String = state
                .doc
                .rope()
                .byte_slice(usize::from(color.range.start())..usize::from(color.range.end()))
                .chars()
                .take(160)
                .collect();
            text.push_str(&format!(
                "{source}: RGBA({}, {}, {}, {})\n",
                color.rgba[0], color.rgba[1], color.rgba[2], color.rgba[3]
            ));
        }
        if pages > 1 {
            text.push_str("Page Up and Page Down show previous and next source colors.");
        }
        Some((text, pages))
    }
    pub fn new(id: egui::Id) -> Self {
        Self {
            scientific_cell_separators: true,
            id,
            smooth_scrolling: None,
            wheel_gesture: Default::default(),
            clipboard_payload: None,
            ime: Default::default(),
            blink: super::caret::BlinkState::default(),
            method_cache: None,
            color_cache: None,
            max_columns_cache: None,
            drag_anchor: None,
            drag_virtual_anchor: None,
            zoom_delta: 0.0,
            last_click_time: -10.0,
            last_click_pos: egui::Pos2::ZERO,
            click_count: 0,
            caret_screen_rect: None,
            hovered_offset: None,
            ctrl_click: None,
            focused: false,
            typed: String::new(),
            sticky_consumed_click: false,
            gutter_press_owned: false,
            review_comments: Vec::new(),
            wrap_cache: None,
            wrap_hints: Vec::new(),
            dnd: None,
            dnd_target: None,
            change_popup: None,
            last_ctrl_press: -10.0,
            ctrl_twice: false,
            visible_lines: None,
            doc_anchor: None,
            center_request: super::navigation_scroll::CenterRequest::default(),
            center_target: None,
            viewport_height: 0.0,
            page_lines: 30,
            top_line: 0,
            pending_scroll_line: None,
            pending_session_caret: None,
            session_restore_guard: None,
        }
    }

    /// Explicit centering owns this view's scroll only; selections and history stay intact.
    pub fn request_center(&mut self, state: &mut EditorState) {
        self.cancel_session_restore();
        self.center_target = None;
        self.center_request.request(state);
        state.scroll_to_caret = false;
    }

    /// Capture caret and viewport independently, including a not-yet-loaded tab.
    pub fn session_position(&self, state: &EditorState) -> SessionPosition {
        let current = self.session_restore_is_current(state);
        SessionPosition {
            caret: self
                .pending_session_caret
                .filter(|_| current)
                .unwrap_or_else(|| state.primary_position()),
            scroll_line: self
                .pending_scroll_line
                .filter(|_| current)
                .unwrap_or(self.top_line),
        }
    }

    /// Restore a session without scrolling to its caret. A large-file placeholder
    /// retains both positions until its real document is first rendered.
    pub fn restore_session_position(
        &mut self,
        state: &mut EditorState,
        position: SessionPosition,
        document_loaded: bool,
    ) {
        if document_loaded {
            state.move_primary_to(position.caret);
            self.pending_session_caret = None;
        } else {
            self.pending_session_caret = Some(position.caret);
        }
        self.pending_scroll_line = Some(position.scroll_line);
        self.session_restore_guard = Some((state.doc.version(), state.primary_position()));
        self.doc_anchor = None;
    }

    pub fn request_scroll_line(&mut self, state: &EditorState, line: u32) {
        self.pending_scroll_line = Some(line);
        self.session_restore_guard = Some((state.doc.version(), state.primary_position()));
        self.doc_anchor = None;
    }

    /// Preserve the viewport when the same editor moves to another ScrollArea.
    /// Selection and caret ownership remain with the existing editor state.
    pub fn reanchor_session_scroll(&mut self, state: &EditorState) {
        let current = self.session_restore_is_current(state);
        let position = self.session_position(state);
        if !current {
            self.pending_session_caret = None;
        }
        self.pending_scroll_line = Some(position.scroll_line);
        self.session_restore_guard = Some((state.doc.version(), state.primary_position()));
        self.doc_anchor = None;
    }

    /// Called only after a guarded asynchronous load replaces the placeholder's
    /// state. An empty loaded file is still a completed load: clamp its caret.
    pub fn complete_session_load(&mut self, state: &mut EditorState) {
        if let Some(caret) = self.pending_session_caret.take() {
            state.move_primary_to(caret);
            self.session_restore_guard = Some((state.doc.version(), state.primary_position()));
        }
    }

    /// Explicit navigation (including a jump into an unloaded tab) supersedes
    /// session restoration even when the placeholder clamps every caret to zero.
    pub fn cancel_session_restore(&mut self) {
        self.pending_session_caret = None;
        self.pending_scroll_line = None;
        self.session_restore_guard = None;
        self.doc_anchor = None;
    }

    fn session_restore_is_current(&self, state: &EditorState) -> bool {
        self.session_restore_guard.is_none_or(|(version, caret)| {
            version == state.doc.version() && caret == state.primary_position()
        })
    }

    /// Whether `key` continues the "press Ctrl twice and hold, then Up/Down"
    /// gesture (Clone Caret Above/Below), which the editor handles.
    pub fn clones_caret(&self, key: egui::Key, mods: egui::Modifiers) -> bool {
        self.ctrl_twice
            && mods.ctrl
            && !mods.alt
            && !mods.shift
            && matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown)
    }

    /// Tracks Ctrl presses for [`EditorView::clones_caret`].
    fn track_ctrl(&mut self, pressed: bool, mods: egui::Modifiers, time: f64) {
        if !pressed {
            self.ctrl_twice = false;
        } else if !mods.alt && !mods.shift {
            self.ctrl_twice = time - self.last_ctrl_press < 0.4;
            self.last_ctrl_press = time;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GutterMarker {
    pub offset: TextSize,
    pub has_super: bool,
    pub has_implementations: bool,
}

#[derive(Clone, Copy)]
pub struct ViewConfig<'a> {
    pub gutter_markers: &'a [GutterMarker],
    /// Ascending, at most one per line (see `gutter_actions::quick_fix_offsets`).
    pub gutter_quick_fixes: &'a [TextSize],
    pub code_vision: Option<&'a super::code_vision_view::Display>,
    pub scheme: &'a ColorScheme,
    pub theme: &'a UiTheme,
    pub settings: &'a EditorSettings,
    pub opts: &'a EditOptions,
    pub keymap: &'a Keymap,
    pub font_size: f32,
    pub search_highlights: &'a [TextRange],
    pub request_focus: bool,
    /// Lines that get a ▶ run marker in the gutter.
    pub run_lines: &'a [usize],
    /// VCS change markers (gutter bars).
    pub line_changes: &'a [rustpy_vcs::LineChange],
    /// Lines (0-based) with breakpoints and how each is drawn.
    pub breakpoints: &'a [(usize, BpGlyph)],
    /// Current execution line (0-based) while debugging.
    pub exec_line: Option<usize>,
    /// Debugger values shown at the end of lines while suspended.
    pub inline_values: &'a [InlineValue],
    /// Lines (0-based) with bookmarks and their mnemonic.
    pub bookmarks: &'a [(usize, Option<char>)],
    /// Git blame annotations per line (0-based): `(text, color band, tooltip)`.
    pub annotations: Option<&'a [(String, u8, String)]>,
    /// Coverage per line (0-based, sorted): 0 = covered, 1 = missed, 2 = excluded,
    /// 3 = partially covered (a branch never ran).
    pub coverage: &'a [(usize, u8)],
    /// False while a dialog or popup owns the keyboard: the editor gives up
    /// focus so typing cannot change the document behind it.
    pub keyboard: bool,
}

/// Things the editor cannot handle itself and hands to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlameIntent {
    Log,
    Diff,
    Previous,
    Options,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ViewEvent {
    BlameCommit {
        revision: String,
        intent: BlameIntent,
    },
    GutterIntent(super::gutter_actions::Intent),
    CodeVisionFindUsages(super::code_vision_view::FindUsagesTarget),
    Action(Action),
    ShortcutChoices(Vec<Action>),
    /// Idempotently open current-file Problems for this rendered editor.
    OpenProblems,
    Zoom(f32),
    /// The ▶ gutter icon of a line was clicked (files whose ▶ runs directly).
    GutterRun(usize),
    ScientificCell {
        line: usize,
        action: Action,
    },
    /// An entry of a ▶ icon's Run / Debug / Coverage / Profile menu was chosen.
    GutterRunWith(usize, crate::run_context::RunMode),
    ContextRun(Box<crate::run_context::EditorRequest>),
    /// The gutter of a line was clicked (toggle breakpoint).
    GutterClick(usize),
    /// The VCS change marker of a line was clicked (show the change popup).
    ChangeMarkerClick(usize),
    /// The breakpoint of a line was right-clicked (edit its properties).
    GutterContext(usize),
    /// Text was copied or cut to the clipboard.
    Copied(String),
    ClipboardPayload(super::commands::clipboard_payload::ClipboardPayload),
    /// A paste larger than [`LARGE_PASTE_BYTES`] waits for confirmation.
    LargePaste(String),
    StringPaste(String),
    /// An editor action ran inside the view (for the Features Trainer).
    Performed(Action),
    /// A link in rendered documentation was clicked (http, https or mailto).
    OpenUrl(String),
    DocumentationLink {
        target: String,
        offset: TextSize,
        document: rustpy_text::DocumentId,
        version: u64,
    },
}

/// Whether a documentation link may be opened: web and mail links only (a
/// library's docstring must not open `file:` or other schemes).
pub fn is_openable_doc_link(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// Pastes above this size ask for confirmation first.
pub const LARGE_PASTE_BYTES: usize = 2 * 1024 * 1024;

/// Distance of the VCS change bar's centre from the gutter's right edge.
const CHANGE_BAR_INSET: f32 = 4.0;

/// Whether a gutter click at `x` hits the VCS change bar of its line. The bar
/// is 3 px wide, so the target reaches a few pixels either side. A fold marker's
/// box covers the bar, so on a fold-start line only the box's right part opens
/// the change popup and its left part (with the −/+ sign) still folds.
fn hits_change_bar(x: f32, gutter_right: f32, under_fold_marker: bool) -> bool {
    let d = x - (gutter_right - CHANGE_BAR_INSET);
    if under_fold_marker {
        (-3.0..=6.0).contains(&d)
    } else {
        d.abs() <= 6.0
    }
}

#[cfg(test)]
mod change_bar_tests {
    use super::*;

    #[test]
    fn change_bar_click_target() {
        let right = 303.0;
        let bar = right - CHANGE_BAR_INSET;
        // The painted bar (bar ± 1.5) opens the popup, with or without a fold marker over it.
        for x in [bar - 1.5, bar, bar + 1.5] {
            assert!(
                hits_change_bar(x, right, false) && hits_change_bar(x, right, true),
                "{x}"
            );
        }
        // The fold box (centred 8 px from the edge, 11 px wide) keeps its sign and left half.
        let fold_centre = right - 8.0;
        assert!(!hits_change_bar(fold_centre, right, true));
        assert!(!hits_change_bar(fold_centre - 5.0, right, true));
        assert!(
            hits_change_bar(fold_centre, right, false),
            "without a fold marker the target is wider"
        );
        // Line numbers (right-aligned 18 px from the edge) still toggle breakpoints.
        assert!(!hits_change_bar(right - 18.0, right, false));
    }
}

/// How a line breakpoint is drawn in the gutter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BpGlyph {
    Enabled,
    Disabled,
    /// Has a condition or hit count.
    Conditional,
    /// Logs without suspending.
    Log,
    /// Removed once hit.
    Temporary,
    /// The debugger did not accept it (e.g. no code on that line).
    Unverified,
    /// Mute Breakpoints is on.
    Muted,
}

/// A debugger value painted after the end of a line (`x = 41`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineValue {
    /// 0-based line.
    pub line: usize,
    pub text: String,
    /// The value changed since the previous suspension.
    pub changed: bool,
}

/// Paints a breakpoint glyph centred at `c`.
fn paint_breakpoint(painter: &egui::Painter, c: Pos2, glyph: BpGlyph, gutter_bg: Color32) {
    let red = Color32::from_rgb(0xDB, 0x5C, 0x5C);
    let r = 5.5;
    match glyph {
        BpGlyph::Enabled => {
            painter.circle_filled(c, r, red);
        }
        BpGlyph::Disabled => {
            painter.circle_stroke(c, r - 0.5, Stroke::new(1.5, red));
        }
        BpGlyph::Muted => {
            painter.circle_filled(c, r, Color32::from_rgb(0x9A, 0x9A, 0x9A));
        }
        BpGlyph::Log => {
            painter.circle_filled(c, r, Color32::from_rgb(0xE8, 0xA8, 0x3C));
        }
        BpGlyph::Conditional => {
            painter.circle_filled(c, r, red);
            painter.text(
                c + Vec2::new(0.0, 0.5),
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(9.0),
                Color32::WHITE,
            );
        }
        BpGlyph::Temporary => {
            painter.circle_filled(c, r, red);
            painter.text(
                c + Vec2::new(0.0, 0.5),
                Align2::CENTER_CENTER,
                "1",
                FontId::proportional(8.5),
                Color32::WHITE,
            );
        }
        BpGlyph::Unverified => {
            painter.circle_filled(c, r, red.gamma_multiply(0.45));
            let d = 2.5;
            let s = Stroke::new(1.3, gutter_bg);
            painter.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], s);
            painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], s);
        }
    }
}

struct LineLayout {
    visual: Option<super::unicode_view::VisualMap>,
    line: usize,
    galley: Arc<Galley>,
    origin: Pos2,
    /// Display char index → source char index (only when the line has tabs or inlay hints).
    map: Option<Vec<usize>>,
    /// Inlay hints on this line: `(source char index, display start, label chars)`.
    hints: Vec<(usize, usize, usize)>,
}

/// Lays out and paints the editor; processes input. Returns app-level events.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    view: &mut EditorView,
    cfg: &ViewConfig,
) -> Vec<ViewEvent> {
    let mut reader_settings = std::borrow::Cow::Borrowed(cfg.settings);
    if super::doc_render::reader_mode(state) {
        let settings = reader_settings.to_mut();
        settings.show_line_numbers &= settings.reader_mode.show_line_numbers;
        settings.show_inlay_hints &= settings.reader_mode.show_inlay_hints;
    }
    let mut configured = *cfg;
    configured.settings = &reader_settings;
    if !super::preferences::gutter_visible(cfg.settings, "run") {
        configured.run_lines = &[];
    }
    if !super::preferences::gutter_visible(cfg.settings, "breakpoints") {
        configured.breakpoints = &[];
    }
    if !super::preferences::gutter_visible(cfg.settings, "vcs") {
        configured.line_changes = &[];
    }
    if !super::preferences::gutter_visible(cfg.settings, "bookmarks") {
        configured.bookmarks = &[];
    }
    let cfg = &configured;
    let virtual_enabled = cfg.settings.allow_caret_after_end_of_line
        || (cfg
            .settings
            .allow_caret_after_end_of_line_in_block_selection
            && (state.column_mode
                || state.block.is_some()
                || ui.input(|input| input.modifiers.alt)));
    state.set_virtual_space_policy(
        virtual_enabled,
        cfg.settings
            .allow_caret_after_end_of_line_in_block_selection,
    );
    if !view.session_restore_is_current(state) {
        view.cancel_session_restore();
    }
    let mut events = Vec::new();
    view.ctrl_click = None;
    view.hovered_offset = None;
    view.typed.clear();
    view.sticky_consumed_click = false;
    if ui.input(|i| {
        i.pointer.button_pressed(egui::PointerButton::Primary)
            || !(i.pointer.primary_down()
                || i.pointer.button_released(egui::PointerButton::Primary))
    }) {
        view.gutter_press_owned = false;
    }
    let font_size = cfg.font_size;
    let reader_ligatures =
        super::doc_render::reader_mode(state) && cfg.settings.reader_mode.ligatures;
    let mono_family = |bold, italic| {
        if reader_ligatures {
            crate::fonts::reader_family(bold, italic)
        } else {
            crate::theme::mono_family(bold, italic)
        }
    };
    let font_id = FontId::new(font_size, mono_family(false, false));
    let (char_w, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));
    // Rendered documentation comments and reader mode follow the settings.
    super::doc_render::sync(state, cfg.settings);
    let reader_mode = super::doc_render::reader_mode(state);
    let methods = if cfg.settings.show_method_separators {
        let identity = state.doc.identity();
        let version = state.doc.version();
        let language = state.language;
        let large = state.is_large_file();
        if !view
            .method_cache
            .as_ref()
            .is_some_and(|(id, v, lang, big, _)| {
                *id == identity && *v == version && *lang == language && *big == large
            })
        {
            let boundaries = super::method_separators::boundaries(state);
            view.method_cache = Some((identity, version, language, large, boundaries));
        }
        view.method_cache.as_ref().unwrap().4.clone()
    } else {
        Vec::new()
    };
    let colors = if cfg.settings.show_css_color_preview
        && super::preferences::gutter_visible(cfg.settings, "colors")
    {
        let identity = state.doc.identity();
        let version = state.doc.version();
        let language = state.language;
        let large = state.is_large_file();
        if !view
            .color_cache
            .as_ref()
            .is_some_and(|(id, v, lang, big, _)| {
                *id == identity && *v == version && *lang == language && *big == large
            })
        {
            let mut colors =
                if large || !matches!(language, super::Language::Css | super::Language::Html) {
                    Vec::new()
                } else {
                    super::color_preview::previews(&state.doc.text(), language)
                };
            colors.sort_by_key(|color| color.range.start());
            view.color_cache = Some((
                identity,
                version,
                language,
                large,
                std::sync::Arc::new(colors),
            ));
        }
        view.color_cache.as_ref().unwrap().4.clone()
    } else {
        std::sync::Arc::new(Vec::new())
    };
    let mut colors_per_line =
        std::collections::HashMap::<usize, Vec<&super::color_preview::ColorPreview>>::new();
    for color in colors.iter() {
        colors_per_line
            .entry(state.doc.line_of(color.range.start()))
            .or_default()
            .push(color);
    }
    let color_preview_w = (colors_per_line
        .values()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min(8)
        * 12) as f32;
    let line_spacing = cfg.settings.line_spacing
        * if reader_mode && cfg.settings.reader_mode.increase_line_height {
            1.25
        } else {
            1.0
        };
    let line_h = (row_h * line_spacing).max(font_size + 2.0);
    let line_count = state.doc.line_count();
    let digits = line_count.max(1).to_string().len().max(2);
    let annotation_w = if cfg.annotations.is_some() {
        190.0
    } else {
        0.0
    };
    let actions_w = if !view.review_comments.is_empty() {
        72.0
    } else if cfg.gutter_markers.is_empty() && cfg.gutter_quick_fixes.is_empty() {
        0.0
    } else {
        54.0
    };
    let gutter_w = (if cfg.settings.show_line_numbers {
        digits as f32 * char_w + 34.0
    } else {
        24.0
    }) + annotation_w
        + color_preview_w
        + actions_w;
    let tab_size = cfg.opts.tab_size;
    // Folding: "collapse by default" settings once per file; no folds while
    // folding is off; the primary caret is never inside a hidden region.
    if cfg.settings.code_folding {
        state.apply_default_folds(
            cfg.settings.fold_imports_by_default,
            cfg.settings.fold_docstrings_by_default && !super::doc_render::renders_any(state),
        );
    } else {
        state.expand_all_folds();
    }
    let caret_head = state.primary().head();
    state.reveal_offset(caret_head);
    let hidden = if cfg.settings.code_folding {
        state.hidden_line_ranges()
    } else {
        Vec::new()
    };
    let hidden = super::doc_render::merge_hidden(
        &hidden,
        super::doc_render::hidden_comment_lines(state).into_iter(),
    );
    let avail_pre = ui.available_rect_before_wrap();
    let soft_wrap = cfg.settings.soft_wrap
        && rustpy_settings::editor_pages::matches(
            &cfg.settings.preferences.soft_wrap_masks,
            state
                .path
                .as_deref()
                .unwrap_or(std::path::Path::new("untitled")),
        );
    let wrap_cols = if soft_wrap {
        (((avail_pre.width() - gutter_w - 12.0) / char_w).floor() as usize).max(20)
    } else {
        usize::MAX
    };
    // Caret movement (Up/Down by visual rows) follows the same wrapping.
    state.soft_wrap = soft_wrap.then_some(super::soft_wrap::SoftWrap {
        cols: wrap_cols,
        tab_size,
        original_indent: cfg.settings.preferences.soft_wrap_indent,
    });
    let mut all_hints = cfg
        .code_vision
        .map(|display| display.merged_hints(state))
        .unwrap_or_else(|| state.inlay_hints.clone());
    if !cfg.settings.show_inlay_hints {
        all_hints.clear();
    }
    super::preferences::filter_hints(state, &mut all_hints);
    if view.wrap_hints != all_hints {
        view.wrap_cache = None;
        view.wrap_hints.clone_from(&all_hints);
    }
    let rows_per_line: Option<Vec<u32>> = if soft_wrap {
        let version = state.doc.version();
        let cached = view
            .wrap_cache
            .as_ref()
            .filter(|(v, c, _)| *v == version && *c == wrap_cols)
            .map(|(_, _, r)| r.clone());
        Some(cached.unwrap_or_else(|| {
            let r = compute_rows_per_line(state, tab_size, wrap_cols, &all_hints);
            view.wrap_cache = Some((version, wrap_cols, r.clone()));
            r
        }))
    } else {
        None
    };
    // Rendered docstrings: their lines give way to a block of formatted text
    // that takes the rows it needs (all on the docstring's first line).
    let doc_style = super::doc_render::DocStyle::new(cfg.scheme, font_size);
    let doc_placements = place_docs(
        ui,
        state,
        view,
        &hidden,
        &doc_style,
        (avail_pre.width() - gutter_w - 28.0).max(120.0),
        cfg,
        char_w,
        line_h,
    );
    let (hidden, rows_per_line) = if doc_placements.is_empty() {
        (hidden, rows_per_line)
    } else {
        let hidden = super::doc_render::merge_hidden(
            &hidden,
            doc_placements
                .iter()
                .filter(|p| p.region.last_line > p.region.first_line)
                .map(|p| (p.region.first_line + 1, p.region.last_line)),
        );
        let mut per_line = rows_per_line.unwrap_or_else(|| vec![1; line_count]);
        for p in &doc_placements {
            if let Some(n) = per_line.get_mut(p.region.first_line) {
                *n = p.rows as u32;
            }
        }
        (hidden, Some(per_line))
    };
    let doc_at_line = |line: usize| {
        doc_placements
            .binary_search_by_key(&line, |p| p.region.first_line)
            .ok()
            .map(|i| &doc_placements[i])
    };
    let (mut rows, mut row_sub, mut line_to_row) =
        build_rows(line_count, &hidden, rows_per_line.as_deref());
    let inheritor_lines: Vec<_> = if cfg.settings.preferences.code_vision_inheritors {
        cfg.gutter_markers
            .iter()
            .filter(|marker| marker.has_implementations && marker.offset <= state.doc.len())
            .map(|marker| state.doc.line_of(marker.offset))
            .collect()
    } else {
        Vec::new()
    };
    let vision_rows = super::code_vision_view::reserve_rows_with_extra(
        state,
        cfg.code_vision,
        &mut rows,
        &mut row_sub,
        &mut line_to_row,
        &inheritor_lines,
    );
    let row_count = rows.len().max(1);
    let wrap_px = if soft_wrap {
        wrap_cols as f32 * char_w + 0.5
    } else {
        f32::INFINITY
    };
    let visual_rows_of = |line: usize| -> usize {
        rows_per_line
            .as_ref()
            .and_then(|r| r.get(line))
            .map(|n| (*n).max(1) as usize)
            .unwrap_or(1)
    };
    // Gutter fold markers: per header line, whether a region starting there is
    // folded and the placeholder to draw (regions are sorted outermost first).
    let fold_starts: HashMap<usize, (bool, String)> = if cfg.settings.code_folding {
        let mut starts: HashMap<usize, (bool, String)> = HashMap::new();
        for f in state.frame_fold_regions() {
            let folded = state.is_folded(&f);
            match starts.get_mut(&f.first_line) {
                Some(entry) => {
                    if folded && !entry.0 {
                        *entry = (true, f.placeholder);
                    }
                }
                None => {
                    starts.insert(f.first_line, (folded, f.placeholder));
                }
            }
        }
        starts
    } else {
        HashMap::new()
    };
    let mut diagnostics = match state.language {
        super::Language::Python => state.diagnostics(),
        // Prose files only carry grammar findings once analysed.
        super::Language::Markdown | super::Language::PlainText | super::Language::Gherkin
            if state.semantic_version == Some(state.doc.version()) =>
        {
            state.extra_diagnostics.clone()
        }
        _ if state.has_data_syntax() => state.diagnostics(),
        _ => Vec::new(),
    };
    match state.inspection_level {
        rustpy_settings::inspection_profiles::HighlightingLevel::None => diagnostics.clear(),
        rustpy_settings::inspection_profiles::HighlightingLevel::Syntax => {
            diagnostics.retain(|diagnostic| diagnostic.code == "syntax")
        }
        rustpy_settings::inspection_profiles::HighlightingLevel::All => {}
    }
    if reader_mode && !cfg.settings.reader_mode.show_warnings {
        // Reader mode shows errors only.
        diagnostics.retain(|d| d.severity == rustpy_core::Severity::Error);
    }

    // Keep the inspection summary in a separate toolbar row, outside source
    // hit targets and Code Vision headers, including in narrow editor splits.
    let (summary_header, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    let summary_rect = Rect::from_min_max(
        Pos2::new(
            (summary_header.right() - 354.0).max(summary_header.left()),
            summary_header.top(),
        ),
        Pos2::new(
            (summary_header.right() - 28.0).max(summary_header.left()),
            summary_header.bottom(),
        ),
    );
    let inspected_language = matches!(
        state.language,
        super::Language::Python
            | super::Language::Markdown
            | super::Language::PlainText
            | super::Language::Gherkin
    ) || state.has_data_syntax();
    let readiness = if !inspected_language || state.is_large_file() {
        inspection_widget::Readiness::Unavailable
    } else if state.has_data_syntax() {
        if state.data_syntax_current() {
            inspection_widget::Readiness::Complete
        } else {
            inspection_widget::Readiness::CurrentFindings
        }
    } else if state.semantic_version == Some(state.doc.version()) {
        inspection_widget::Readiness::Complete
    } else if state.semantic_version == Some(u64::MAX) {
        // App analysis_jobs::PENDING is never a document version.
        inspection_widget::Readiness::Pending
    } else {
        inspection_widget::Readiness::CurrentFindings
    };
    let widget = inspection_widget::show(
        ui,
        summary_rect,
        view.id.with("inspection-summary"),
        inspection_widget::Summary::from_diagnostics(&diagnostics),
        readiness,
        cfg.theme.text,
        cfg.theme.panel_bg,
    );
    if widget.open_problems {
        events.push(ViewEvent::OpenProblems);
    }

    let bg = rgb(cfg.scheme.editor.background);
    let avail = ui.available_rect_before_wrap();
    ui.painter().rect_filled(avail, 0.0, bg);
    crate::appearance_assets::paint(ui.ctx(), ui.painter(), avail, true);

    // Approximate content width from the longest *visible-ish* line later; use
    // a generous estimate so horizontal scrolling works.
    let max_cols = if soft_wrap {
        0
    } else {
        cached_max_columns(&mut view.max_columns_cache, state, tab_size)
    };
    let content_w = if soft_wrap {
        (avail.width() - 4.0).max(gutter_w + 100.0)
    } else {
        gutter_w + max_cols as f32 * char_w + 200.0
    };
    let virtual_excess = (0..state.carets.len())
        .map(|i| {
            let p = state.virtual_caret(i).head;
            p.column
                .saturating_sub(state.doc.line_text(p.line as usize).chars().count() as u32)
        })
        .max()
        .unwrap_or(0);
    let content_w = content_w + virtual_excess as f32 * char_w;
    let extra_bottom = if cfg.settings.scroll_beyond_end {
        (avail.height() - line_h).max(0.0)
    } else {
        0.0
    };
    let content_h = row_count as f32 * line_h + extra_bottom + 4.0;

    // Rendered docstrings above the first line shown changed size without an
    // edit: scroll so that line stays in place.
    let doc_signature = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in &doc_placements {
            (p.region.first_line, p.rows).hash(&mut h);
        }
        h.finish()
    };
    let version_at_start = state.doc.version();
    let anchored_offset = view
        .doc_anchor
        .filter(|a| {
            a.version == version_at_start && a.signature != doc_signature && a.line_h == line_h
        })
        .and_then(|a| {
            // A line now inside a rendered docstring: the docstring's first line takes its place.
            let row = (0..=a.line.min(line_count.saturating_sub(1)))
                .rev()
                .find_map(|l| line_to_row.get(l).copied().flatten())?;
            (row != a.row).then(|| (a.offset + (row as f32 - a.row as f32) * line_h).max(0.0))
        });
    // A changed preference cancels any previous scroll-to animation while
    // retaining this view's actual offset. Other views have independent state.
    let smooth_scrolling = cfg.settings.smooth_scrolling && !crate::a11y::reduced_motion();
    if view.smooth_scrolling != Some(smooth_scrolling) {
        let id = ui.make_persistent_id(egui::IdSalt::new(view.id.with("scroll")));
        if let Some(previous) = egui::scroll_area::State::load(ui.ctx(), id) {
            let mut fresh = egui::scroll_area::State::default();
            fresh.offset = previous.offset;
            fresh.store(ui.ctx(), id);
        }
        view.smooth_scrolling = Some(smooth_scrolling);
    }
    // egui smooths wheel input independently of ScrollArea::animated. Disable
    // that easing only under this editor's pointer, keeping zoom and other panes
    // on their normal routes; no wheel event is synthesized or replayed.
    let options = ui.ctx().options(|options| options.input_options);
    // A configured wheel shortcut owns the vertical gesture before ScrollArea
    // consumes it; another editor or a tool under the pointer keeps its input.
    let keymap_wheel_actions =
        if cfg.keyboard && ui.memory(|m| m.has_focus(view.id)) && ui.rect_contains_pointer(avail) {
            ui.input(|input| {
                input.events.iter().find_map(|event| match event {
                    egui::Event::MouseWheel {
                        delta, modifiers, ..
                    } if delta.y != 0.0 => {
                        let actions = cfg.keymap.mouse_candidates(
                            if delta.y > 0.0 {
                                "WheelUp"
                            } else {
                                "WheelDown"
                            },
                            *modifiers,
                        );
                        (!actions.is_empty()).then_some(actions)
                    }
                    _ => None,
                })
            })
            .unwrap_or_default()
        } else {
            Vec::new()
        };
    if !keymap_wheel_actions.is_empty() {
        ui.input_mut(|input| {
            input.smooth_scroll_delta.y = 0.0;
            input.events.retain(
                |event| !matches!(event,egui::Event::MouseWheel {delta,..} if delta.y != 0.0),
            );
        });
    }
    let immediate = ui.input(|input| {
        view.wheel_gesture
            .immediate_wheel(&input.events, &options, input.viewport_rect().height())
    });
    if !smooth_scrolling
        && ui.rect_contains_pointer(avail)
        && ui.input(|input| !input.modifiers.ctrl && !input.modifiers.command)
    {
        ui.input_mut(|input| {
            // A foreground widget may already have consumed either axis. Never
            // reconstruct that consumed input from the original event catalogue.
            input.smooth_scroll_delta = Vec2::new(
                if input.smooth_scroll_delta.x != 0.0 {
                    immediate.x
                } else {
                    0.0
                },
                if input.smooth_scroll_delta.y != 0.0 {
                    immediate.y
                } else {
                    0.0
                },
            );
        });
    }
    // With rendered docstrings, scrolling is not animated: an animation heads for
    // a pixel offset, which rows changing above it (docstrings laid out late) would
    // turn into another line.
    let mut scroll = egui::ScrollArea::both()
        .id_salt(view.id.with("scroll"))
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .animated(smooth_scrolling && !super::doc_render::renders_any(state));
    let restoring = if view.pending_session_caret.is_none() {
        view.pending_scroll_line
    } else {
        None
    };
    let restore_offset =
        restoring.map(|line| session_scroll_row(line, &line_to_row, &rows) as f32 * line_h + 2.0);
    if let Some(offset) = view.center_target.or(restore_offset) {
        // A previous caret-scroll animation otherwise overrides an explicit
        // offset in egui's begin pass (notably after replacing a placeholder).
        let scroll_id = ui.make_persistent_id(egui::IdSalt::new(view.id.with("scroll")));
        let horizontal =
            egui::scroll_area::State::load(ui.ctx(), scroll_id).map_or(0.0, |state| state.offset.x);
        let mut fresh = egui::scroll_area::State::default();
        fresh.offset = Vec2::new(horizontal, offset);
        fresh.store(ui.ctx(), scroll_id);
        scroll = scroll.animated(false).vertical_scroll_offset(offset);
        state.scroll_to_caret = false;
    } else if let Some(y) = anchored_offset {
        scroll = scroll.vertical_scroll_offset(y);
    }
    let output = scroll.show_viewport(ui, |ui, viewport| {
        let view_opts = super::navigation_scroll::options_for_viewport(cfg.opts, viewport.height(), line_h);
        view.page_lines = view_opts.page_lines;
        ui.set_min_size(Vec2::new(content_w, content_h));
        let content_rect = ui.max_rect();
        let text_origin = Pos2::new(content_rect.left() + gutter_w, content_rect.top() + 2.0);
        let clip = ui.clip_rect();
        let painter = ui.painter().with_clip_rect(clip);
        let first_row = ((viewport.min.y - 2.0) / line_h).floor().max(0.0) as usize;
        let last_row = (((viewport.max.y - 2.0) / line_h).ceil() as usize + 1).min(rows.len());
        view.visible_lines = Some((rows.get(first_row).copied().unwrap_or(0), rows.get(last_row.saturating_sub(1)).copied().unwrap_or(0)));
        let row_y = |row: usize| text_origin.y + row as f32 * line_h;
        let line_row = |line: usize| line_to_row.get(line).copied().flatten();

        let vision_pointer = ui.input(|input| input.pointer.press_origin().or(input.pointer.interact_pos()))
            .is_some_and(|pointer| pointer.x >= text_origin.x - 2.0
                && pointer.y >= text_origin.y
                && vision_rows.is_header(((pointer.y - text_origin.y) / line_h).floor() as usize));

        // Interaction surface covering the whole content.
        if cfg.keyboard && ui.memory(|m| m.has_focus(view.id)) {
            // A newly focused editor cannot install egui's focus filter until
            // the following pass. Cancel navigation already planned from this
            // frame's Tab/arrows before registering the editor interaction.
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let resp = ui.interact(content_rect, view.id, Sense::click_and_drag());
        if resp.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
        }
        if cfg.keyboard && (cfg.request_focus || resp.clicked() || resp.drag_started()) {
            resp.request_focus();
        }
        if !cfg.keyboard && resp.has_focus() {
            ui.memory_mut(|m| m.surrender_focus(view.id));
        }
        let focused = resp.has_focus() && cfg.keyboard;
        if focused && !view.focused {
            view.blink.reset(ui.input(|i| i.time));
        }
        view.focused = focused;
        view.ime.validate(state,focused);
        if focused {
            ui.memory_mut(|m| m.set_focus_lock_filter(view.id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        }

        // Register header controls after the editor surface so their smaller
        // pointer targets own a click where the rectangles overlap.


        // ---- current line & selection backgrounds -----------------------
        let primary_line = state.doc.line_of(state.primary().head());
        if cfg.settings.highlight_current_line && !state.has_selection() {
            if let Some(row) = line_row(primary_line) {
                let y = row_y(row);
                let h = line_h * visual_rows_of(primary_line) as f32;
                painter.rect_filled(Rect::from_min_max(Pos2::new(clip.left(), y), Pos2::new(clip.right(), y + h)), 0.0, rgb(cfg.scheme.editor.caret_row));
            }
        }

        if let Some(exec) = cfg.exec_line {
            if let Some(row) = line_row(exec) {
                let y = row_y(row);
                let h = line_h * visual_rows_of(exec) as f32;
                painter.rect_filled(Rect::from_min_max(Pos2::new(clip.left(), y), Pos2::new(clip.right(), y + h)), 0.0, rgb(cfg.scheme.context_color("debugger.execution", if cfg.theme.dark { rustpy_highlight::Rgb::hex(0x2D6099) } else { rustpy_highlight::Rgb::hex(0xBFD6F2) })));
            }
        }

        // `#%%` code cells: a separator above each marker line.
        if view.scientific_cell_separators && state.language == super::Language::Python {
            for (row, &line) in rows.iter().enumerate().take(last_row).skip(first_row) {
                if row_sub[row] == 0 && state.doc.line_slice(line).chars().next() == Some('#') && super::console_code::is_cell_marker(&state.doc.line_text(line)) {
                    let y = row_y(row);
                    painter.line_segment([Pos2::new(clip.left(), y), Pos2::new(clip.right(), y)], Stroke::new(1.0, rgb(cfg.scheme.editor.indent_guide_active)));
                }
            }
        }

        // ---- layout visible lines ---------------------------------------
        let first_line = rows.get(first_row).copied().unwrap_or(0);
        let last_line = rows.get(last_row.saturating_sub(1)).copied().unwrap_or(first_line);
        let (spans, highlighting) = state.view_highlight_spans(first_line, last_line);
        if highlighting {
            ui.ctx().request_repaint();
        }
        let todo_paint = state.todo_paint(&spans);
        // Unused imports and locals read gray instead of underlined.
        let spans = if diagnostics.iter().any(|d| super::diag_style::look(d) == super::diag_style::Look::Unused) {
            let ranges = super::diag_style::unused_ranges(&diagnostics, state.frame_syntax());
            super::diag_style::with_unused(spans, &ranges)
        } else {
            spans
        };
        let mut span_idx = spans.partition_point(|s| s.range.end() <= state.doc.line_range(first_line).start());
        let mut layouts: Vec<LineLayout> = Vec::with_capacity(last_row.saturating_sub(first_row));
        let default_color = rgb(cfg.scheme.editor.foreground);
        let hidden_comments: std::collections::HashMap<_, _> = rows.iter().take(last_row).skip(first_row)
            .map(|&line| (line, super::doc_render::hidden_inline_comment_start(state, line)))
            .collect();
        for (row, &line) in rows.iter().enumerate().take(last_row).skip(first_row) {
            if vision_rows.is_header(row) { continue; }
            if (row_sub[row] != 0 && layouts.last().is_some_and(|l| l.line == line)) || doc_at_line(line).is_some() {
                continue;
            }
            let row = line_row(line).unwrap_or(row);
            let lr = state.doc.line_range(line);
            // Skip spans belonging to hidden lines before this one.
            while span_idx < spans.len() && spans[span_idx].range.end() <= lr.start() {
                span_idx += 1;
            }
            let text = state.doc.line_text(line);
            let (display, map) = expand_tabs(&text, tab_size);
            // Inlay hints of this line as (byte offset in line, label).
            let hs = all_hints.partition_point(|h| h.offset < lr.start());
            let he = all_hints.partition_point(|h| h.offset <= lr.end());
            // Hints come from the last analysis: after an edit their offsets may be
            // stale until it reruns, so keep them on character boundaries.
            let line_hints: Vec<(usize, &str)> = all_hints[hs..he].iter().map(|h| (rustpy_core::floor_char_boundary(&text, usize::from(h.offset - lr.start())), h.label.as_str())).collect();
            let (map, hint_layout) = if line_hints.is_empty() { (map, Vec::new()) } else { display_map_with_hints(&text, tab_size, &line_hints) };
            let mut job = LayoutJob::default();
            let continuation=if soft_wrap&&cfg.settings.preferences.soft_wrap_indent {super::soft_wrap::continuation_indent(&text,wrap_cols,tab_size)}else{0};
            job.wrap.max_width = (wrap_px-continuation as f32*char_w).max(char_w);
            job.wrap.break_anywhere = true;
            // Build sections from spans intersecting this line.
            let mut pos = 0usize; // byte offset within source line
            let line_start = u32::from(lr.start()) as usize;
            let line_len = text.len();
            let mut local_spans = Vec::new();
            while span_idx < spans.len() && (u32::from(spans[span_idx].range.start()) as usize) < line_start + line_len {
                let s = spans[span_idx];
                let s_start = rustpy_core::floor_char_boundary(&text, (u32::from(s.range.start()) as usize).max(line_start) - line_start);
                let s_end = rustpy_core::floor_char_boundary(&text, (u32::from(s.range.end()) as usize).min(line_start + line_len) - line_start);
                if s_start < s_end {
                    local_spans.push((s_start, s_end, s.tag));
                }
                if u32::from(s.range.end()) as usize > line_start + line_len {
                    break; // span continues on the next line
                }
                span_idx += 1;
            }
            let hidden_comment = hidden_comments.get(&line).copied().flatten();
            let hint_color = if cfg.theme.dark { Color32::from_rgb(0xA0, 0xA4, 0xA8) } else { Color32::from_rgb(0x70, 0x74, 0x78) };
            let push_plain = |job: &mut LayoutJob, a: usize, b: usize, tag: Option<HighlightTag>| {
                if a == b {
                    return;
                }
                // Legacy hardcoded TODO tags become ordinary comments; only
                // configured lexical matches receive TODO presentation.
                let tag = tag.map(|tag| if tag == HighlightTag::TodoComment { HighlightTag::Comment } else { tag });
                let style = tag.map(|t| cfg.scheme.style(t)).unwrap_or_default();
                let color = style.fg.map(rgb).unwrap_or(default_color);
                for (range, todo_color) in super::state::todo_highlights::segments(&todo_paint, super::state::todo_highlights::source_range(line_start, a, b)) {
                    let color = todo_color.map(|color| Color32::from_rgb(color[0], color[1], color[2])).unwrap_or(color);
                    let fmt = TextFormat { font_id: FontId::new(font_size, mono_family(style.bold, false)), color, background: style.bg.map(rgb).unwrap_or(Color32::TRANSPARENT), strikethrough: if style.strikeout { Stroke::new(1.0, style.effect_color.map(rgb).unwrap_or(color)) } else { Stroke::NONE }, italics: style.italic, underline: if style.underline || style.bold_underline { Stroke::new(if style.bold_underline {2.0}else{1.0}, style.effect_color.map(rgb).unwrap_or(color)) } else { Stroke::NONE }, line_height: Some(line_h), ..Default::default() };
                    append_reader_segment(job, &text, usize::from(range.start()) - line_start, usize::from(range.end()) - line_start, fmt, hidden_comment, tab_size);
                }
            };
            let push_hint = |job: &mut LayoutJob, label: &str| {
                let fmt = TextFormat { font_id: FontId::new(font_size, mono_family(false, false)), color: hint_color, italics: false, line_height: Some(line_h), ..Default::default() };
                job.append(label, 0.0, fmt);
            };
            // Pushes `text[a..b]`, inserting inlay hints that start inside the range.
            let push = |job: &mut LayoutJob, a: usize, b: usize, tag: Option<HighlightTag>| {
                let mut cur = a;
                for (off, label) in line_hints.iter().filter(|(off, _)| *off >= a && *off < b) {
                    if cur < *off {
                        push_plain(job, cur, *off, tag);
                    }
                    push_hint(job, label);
                    cur = *off;
                }
                if cur < b {
                    push_plain(job, cur, b, tag);
                }
            };
            for (s, e, tag) in local_spans {
                if pos < s {
                    push(&mut job, pos, s, None);
                }
                push(&mut job, s, e, Some(tag));
                pos = e;
            }
            if pos < line_len {
                push(&mut job, pos, line_len, None);
            }
            for (off, label) in line_hints.iter().filter(|(off, _)| *off >= line_len) {
                let _ = off;
                push_hint(&mut job, label);
            }
            if job.sections.is_empty() {
                job.append("", 0.0, TextFormat { font_id: font_id.clone(), color: default_color, line_height: Some(line_h), ..Default::default() });
            }
            let job=if cfg.settings.unicode_bidi{super::unicode_view::shape(job)}else{job};
            let logical_galley=ui.fonts_mut(|f|f.layout_job(job.clone()));
            let (mut galley,visual)=if cfg.settings.unicode_bidi{let mut start=0;let row_ranges=logical_galley.rows.iter().map(|row|{let end=start+row.glyphs.len();let range=(start,end);start=end+usize::from(row.ends_with_newline);range}).collect::<Vec<_>>();let base=match cfg.settings.unicode_base_direction.as_str(){"LTR"=>Some(false),"RTL"=>Some(true),_=>None};let(job,map)=super::unicode_view::reorder(job,&row_ranges,base);if map.is_some(){(ui.fonts_mut(|f|f.layout_job(job)),map)}else{(logical_galley,None)}}else{(logical_galley,None)};
            if continuation>0 {let galley=std::sync::Arc::make_mut(&mut galley);for row in galley.rows.iter_mut().skip(1){row.pos.x+=continuation as f32*char_w;}galley.rect.max.x+=continuation as f32*char_w;galley.mesh_bounds.max.x+=continuation as f32*char_w;}
            if soft_wrap&&cfg.settings.preferences.show_soft_wrap_indent {for wrapped in galley.rows.iter().skip(1){painter.text(Pos2::new(text_origin.x+wrapped.pos.x-8.0,row_y(row)+wrapped.pos.y),egui::Align2::LEFT_TOP,"↪",FontId::monospace(font_size*0.75),cfg.theme.text_dim);}}
            let origin = Pos2::new(text_origin.x, row_y(row));
            let _ = display;
            layouts.push(LineLayout { line, galley, origin, map, visual, hints: hint_layout });
        }

        if crate::performance_trace::enabled() {
            crate::performance_trace::event("viewport", serde_json::json!({"first":first_row,"last":last_row,"laid_out":layouts.len(),"total_rows":rows.len(),"document_lines":state.doc.line_count(),"large_file":state.is_large_file(),"clip_height":viewport.height(),"line_height":line_h}));
        }

        // Boundaries are source positions; folding/wrapping determine their visible rows anew.
        for boundary in &methods {
            if let Some(ll) = layouts.iter().find(|ll| ll.line == boundary.line) {
                let y = ll.origin.y - 1.0;
                painter.line_segment([Pos2::new(text_origin.x, y), Pos2::new(clip.right(), y)], Stroke::new(1.0, rgb(cfg.scheme.editor.gutter_separator)));
            }
        }

        // ---- selection ----------------------------------------------------
        let sel_color = if focused { rgb(cfg.scheme.editor.selection) } else { rgb(cfg.scheme.editor.selection_inactive) };
        for (index, caret) in state.carets.iter().enumerate() {
            let virtual_caret = state.virtual_caret(index);
            if !caret.has_selection() && virtual_caret.anchor == virtual_caret.head {
                continue;
            }
            let (start, end) = if (virtual_caret.anchor.line, virtual_caret.anchor.column) <= (virtual_caret.head.line, virtual_caret.head.column) {
                (virtual_caret.anchor, virtual_caret.head)
            } else { (virtual_caret.head, virtual_caret.anchor) };
            for line in (start.line as usize)..=(end.line as usize) {
                let Some(ll) = layouts.iter().find(|l| l.line == line) else { continue };
                let text = state.doc.line_text(line);
                if ll.visual.is_some() {
                    let begin=if line==start.line as usize{start.column as usize}else{0};
                    let finish=if line==end.line as usize{end.column as usize}else{text.chars().count()};
                    for rect in visual_source_rects(ll,&text,begin,finish,tab_size){painter.rect_filled(rect.translate(ll.origin.to_vec2()),0.0,sel_color);}
                    continue;
                }
                let p0 = if line == start.line as usize { super::virtual_space_view::extend(pos_of_col(ll, &text, start.column as usize, tab_size), start.column as usize, text.chars().count(), char_w) } else { Pos2::new(0.0, 0.0) };
                let (p1, tail) = if line == end.line as usize { (super::virtual_space_view::extend(pos_of_col(ll, &text, end.column as usize, tab_size), end.column as usize, text.chars().count(), char_w), false) } else { (Pos2::new(ll.galley.size().x, ll.galley.size().y - line_h), true) };
                let gw = ll.galley.size().x.max(char_w);
                if (p0.y - p1.y).abs() < 0.5 {
                    let x1 = if tail { p1.x + char_w * 0.5 } else { p1.x };
                    let rect = Rect::from_min_max(Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y), Pos2::new(ll.origin.x + x1.max(p0.x + 1.0), ll.origin.y + p0.y + line_h));
                    painter.rect_filled(rect, 0.0, sel_color);
                } else {
                    // First row: from start to the row end; middle rows: full width; last row: from 0 to end.
                    painter.rect_filled(Rect::from_min_max(Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y), Pos2::new(ll.origin.x + gw + char_w * 0.5, ll.origin.y + p0.y + line_h)), 0.0, sel_color);
                    let mut y = p0.y + line_h;
                    while y < p1.y - 0.5 {
                        painter.rect_filled(Rect::from_min_max(Pos2::new(ll.origin.x, ll.origin.y + y), Pos2::new(ll.origin.x + gw + char_w * 0.5, ll.origin.y + y + line_h)), 0.0, sel_color);
                        y += line_h;
                    }
                    let x1 = if tail { p1.x + char_w * 0.5 } else { p1.x };
                    painter.rect_filled(Rect::from_min_max(Pos2::new(ll.origin.x, ll.origin.y + p1.y), Pos2::new(ll.origin.x + x1.max(1.0), ll.origin.y + p1.y + line_h)), 0.0, sel_color);
                }
            }
        }

        // ---- usage highlights (identifier under caret) -----------------------
        for (r, is_write) in &state.usage_highlights {
            let s = state.doc.offset_to_position(r.start());
            let e = state.doc.offset_to_position(r.end());
            if s.line != e.line {
                continue;
            }
            if let Some(ll) = layouts.iter().find(|l| l.line == s.line as usize) {
                let text = state.doc.line_text(s.line as usize);
                if ll.visual.is_some(){let color=if *is_write{rgb(cfg.scheme.editor.identifier_under_caret_write_bg)}else{rgb(cfg.scheme.editor.identifier_under_caret_bg)};for rect in visual_source_rects(ll,&text,s.column as usize,e.column as usize,tab_size){painter.rect_filled(rect.translate(ll.origin.to_vec2()),2.0,color);}continue;}
                let p0 = pos_of_col(ll, &text, s.column as usize, tab_size);
                let p1 = pos_of_col(ll, &text, e.column as usize, tab_size);
                let color = if *is_write { rgb(cfg.scheme.editor.identifier_under_caret_write_bg) } else { rgb(cfg.scheme.editor.identifier_under_caret_bg) };
                let x1 = if (p0.y - p1.y).abs() < 0.5 { p1.x } else { ll.galley.size().x };
                painter.rect_filled(Rect::from_min_max(Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y), Pos2::new(ll.origin.x + x1, ll.origin.y + p0.y + line_h)), 2.0, color);
            }
        }

        // ---- search highlights (matches may span lines) ----------------------
        if let (Some(first), Some(last)) = (layouts.first().map(|l| l.line), layouts.last().map(|l| l.line)) {
            let (vis_start, vis_end) = (state.doc.line_range(first).start(), state.doc.line_range(last).end());
            for r in cfg.search_highlights.iter().filter(|r| r.end() >= vis_start && r.start() <= vis_end && !r.is_empty()) {
                paint_text_range(&painter, state, &layouts, *r, rgb(cfg.scheme.editor.search_result_bg), tab_size, char_w, line_h);
            }
        }

        // ---- matching brace ---------------------------------------------
        if cfg.settings.highlight_matching_brace && !state.has_selection() {
            if let Some((a, b)) = matching_brace(state, state.primary().head()) {
                for off in [a, b] {
                    let p = state.doc.offset_to_position(off);
                    if let Some(ll) = layouts.iter().find(|l| l.line == p.line as usize) {
                        let text = state.doc.line_text(p.line as usize);
                        if ll.visual.is_some(){for rect in glyph_span_rects(ll,source_glyph_index(ll,&text,p.column as usize,tab_size),source_cluster_len(ll,&text,p.column as usize,tab_size)){painter.rect_filled(rect.translate(ll.origin.to_vec2()),2.0,rgb(cfg.scheme.editor.matched_brace_bg));}continue;}
                        let p0 = pos_of_col(ll, &text, p.column as usize, tab_size);
                        let x1 = p0.x + char_w;
                        painter.rect_filled(Rect::from_min_max(Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y), Pos2::new(ll.origin.x + x1, ll.origin.y + p0.y + line_h)), 2.0, rgb(cfg.scheme.editor.matched_brace_bg));
                    }
                }
            }
        }

        // ---- indent guides & right margin ----------------------------------
        if cfg.settings.show_right_margin {
            // The file's margin: EditorConfig `max_line_length` overrides the setting.
            for column in super::hard_wrap::visual_guide_columns(cfg.opts.right_margin, cfg.opts.visual_guides.as_deref().unwrap_or(&cfg.settings.visual_guides)) {
                let x = text_origin.x + column as f32 * char_w;
                painter.line_segment([Pos2::new(x, clip.top()), Pos2::new(x, clip.bottom())], Stroke::new(1.0, rgb(cfg.scheme.editor.right_margin)));
            }
        }
        if cfg.settings.show_indent_guides {
            let guide = Stroke::new(1.0, rgb(cfg.scheme.editor.indent_guide));
            for ll in &layouts {
                let levels = indent_levels(state, ll.line, cfg.opts.indent_size, tab_size);
                for lvl in 1..=levels {
                    let x = ll.origin.x + (lvl * cfg.opts.indent_size) as f32 * char_w - (cfg.opts.indent_size as f32 * char_w) + 0.5;
                    painter.line_segment([Pos2::new(x, ll.origin.y), Pos2::new(x, ll.origin.y + line_h)], guide);
                }
            }
        }

        // ---- rendered documentation comments -----------------------------------
        for p in &doc_placements {
            let Some(row) = line_row(p.region.first_line) else { continue };
            if row + p.rows <= first_row || row >= last_row {
                continue;
            }
            let top = row_y(row);
            let rect = p.rect(text_origin.x, top, line_h);
            painter.rect_filled(rect, 4.0, doc_style.background());
            painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 }, doc_style.border());
            if state.carets.iter().any(|c| c.has_selection() && c.range().start() < p.region.end && p.region.start < c.range().end()) {
                painter.rect_filled(rect, 4.0, sel_color.gamma_multiply(0.5));
            }
            p.doc.paint(&painter, p.content_origin(text_origin.x, top, line_h), &doc_style);
        }

        // ---- text -----------------------------------------------------------
        let mut placeholder_clicks: Vec<usize> = Vec::new();
        for ll in &layouts {
            for (_, dstart, dlen) in &ll.hints {
                if let Some(map)=&ll.visual{for logical in *dstart..dstart+dlen{let start=map.glyph_start.get(logical).copied().unwrap_or(logical);let len=map.glyph_len.get(logical).copied().unwrap_or(1);for rect in glyph_span_rects(ll,start,len){painter.rect_filled(rect.translate(ll.origin.to_vec2()),3.0,if cfg.theme.dark{Color32::from_white_alpha(20)}else{Color32::from_black_alpha(18)});}}continue;}
                let x0 = ll.galley.pos_from_cursor(CCursor::new(*dstart)).min.x + ll.origin.x;
                let x1 = ll.galley.pos_from_cursor(CCursor::new(dstart + dlen)).min.x + ll.origin.x;
                let r = Rect::from_min_max(Pos2::new(x0 - 1.0, ll.origin.y + 2.0), Pos2::new(x1 + 1.0, ll.origin.y + line_h - 2.0));
                painter.rect_filled(r, 3.0, if cfg.theme.dark { Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14) } else { Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x12) });
            }
            if cfg.settings.preferences.css_color_background {for color in colors_per_line.get(&ll.line).into_iter().flatten(){let range=state.doc.line_range(ll.line);let text=state.doc.line_text(ll.line);let start=usize::from(color.range.start()).saturating_sub(usize::from(range.start())).min(text.len());let end=usize::from(color.range.end().min(range.end())-range.start()).min(text.len());if text.is_char_boundary(start)&&text.is_char_boundary(end){let column=text[..start].chars().count();let count=text[start..end].chars().count();let first=source_glyph_index(ll,&text,column,tab_size);for rect in glyph_span_rects(ll,first,count){painter.rect_filled(rect.translate(ll.origin.to_vec2()),1.0,Color32::from_rgba_unmultiplied(color.rgba[0],color.rgba[1],color.rgba[2],70));}}}}
            eframe::text_rendering::editor_galley(&painter, ll.origin, ll.galley.clone(), default_color);
            // Use source-to-glyph mapping, so tabs/inlays and wrapping retain
            // the same coordinates as selections and the caret.
            let source_text=state.doc.line_text(ll.line);
            let line_range=state.doc.line_range(ll.line);
            // Spans are sorted and disjoint: binary search this line's, rather than
            // scanning every span of the file for each painted line.
            let first_span=spans.partition_point(|span|span.range.end()<=line_range.start());
            for span in spans[first_span..].iter().take_while(|span|span.range.start()<line_range.end()).filter(|span|span.range.end()>line_range.start()) {
                let style=cfg.scheme.style(span.tag);
                if !style.wave && !style.border {continue;}
                let start=usize::from(span.range.start().max(line_range.start())-line_range.start()).min(source_text.len());
                let end=usize::from(span.range.end().min(line_range.end())-line_range.start()).min(source_text.len());
                if !source_text.is_char_boundary(start)||!source_text.is_char_boundary(end){continue;}
                let start_col=source_text[..start].chars().count();
                let end_col=source_text[..end].chars().count();
                if ll.visual.is_some(){for rect in visual_source_rects(ll,&source_text,start_col,end_col,tab_size){super::scheme_effects::paint(&painter,rect.translate(ll.origin.to_vec2()),style,style.fg.map(rgb).unwrap_or(default_color));}continue;}
                let mut row_rect:Option<Rect>=None;
                for col in start_col..end_col {
                    let index=source_glyph_index(ll,&source_text,col,tab_size);
                    let begin=ll.galley.pos_from_cursor(CCursor::new(index));
                    let finish=ll.galley.pos_from_cursor(CCursor::new(index+source_cluster_len(ll,&source_text,col,tab_size)));
                    if (begin.top()-finish.top()).abs()>0.5 {continue;}
                    let glyph=Rect::from_min_max(begin.min+ll.origin.to_vec2(),Pos2::new(finish.left()+ll.origin.x,begin.bottom()+ll.origin.y));
                    if let Some(current)=row_rect {
                        if (current.top()-glyph.top()).abs()<0.5 {row_rect=Some(current.union(glyph));continue;}
                        super::scheme_effects::paint(&painter,current,style,style.fg.map(rgb).unwrap_or(default_color));
                    }
                    row_rect=Some(glyph);
                }
                if let Some(rect)=row_rect {super::scheme_effects::paint(&painter,rect,style,style.fg.map(rgb).unwrap_or(default_color));}
            }

            if let Some((true, placeholder)) = fold_starts.get(&ll.line) {
                let x = ll.origin.x + ll.galley.size().x + char_w * 0.5;
                let w = placeholder.chars().count() as f32 * char_w + 8.0;
                let rect = Rect::from_min_size(Pos2::new(x, ll.origin.y + 1.0), Vec2::new(w, line_h - 2.0));
                painter.rect_filled(rect, 3.0, rgb(cfg.scheme.editor.folded_text_bg));
                painter.rect_stroke(rect, 3.0, Stroke::new(1.0, rgb(cfg.scheme.editor.folded_text_fg)), egui::StrokeKind::Inside);
                painter.text(rect.center(), Align2::CENTER_CENTER, placeholder, font_id.clone(), rgb(cfg.scheme.editor.folded_text_fg));
                let r = ui.interact(rect, view.id.with(("fold_ph", ll.line)), Sense::click());
                if r.clicked() {
                    placeholder_clicks.push(ll.line);
                }
                if r.hovered() {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                }
            }
        if cfg.settings.show_whitespaces || cfg.settings.show_line_separators {
                let text = state.doc.line_text(ll.line);
                let hidden = super::doc_render::hidden_inline_comment_start(state, ll.line);
                let options = super::whitespace_marks::Options {
                    leading: cfg.settings.show_whitespaces && cfg.settings.show_leading_whitespaces,
                    inner: cfg.settings.show_whitespaces && cfg.settings.show_inner_whitespaces,
                    trailing: cfg.settings.show_whitespaces && cfg.settings.show_trailing_whitespaces,
                    line_endings: cfg.settings.show_line_separators,
                    tab_size,
                };
                let ws = rgb(cfg.scheme.editor.whitespace);
                for mark in super::whitespace_marks::line_markers(&state.doc, ll.line, options) {
                    let byte = usize::from(mark.offset - state.doc.line_range(ll.line).start());
                    if hidden.is_some_and(|start| byte >= start) { continue; }
                    let index = source_glyph_index(ll, &text, mark.char_index, tab_size);
                    match mark.kind {
                        super::whitespace_marks::Kind::Space => {
                            for rect in glyph_span_rects(ll, index, 1) {
                                painter.circle_filled(ll.origin + rect.center().to_vec2(), 1.0, ws);
                            }
                        }
                        super::whitespace_marks::Kind::Tab => {
                            for rect in glyph_span_rects(ll, index, mark.width) {
                                let rect = rect.translate(ll.origin.to_vec2());
                                let y = rect.center().y;
                                let left = rect.left() + 1.0;
                                let right = (rect.right() - 1.0).max(left);
                                painter.line_segment([Pos2::new(left, y), Pos2::new(right, y)], Stroke::new(1.0, ws));
                                if right - left > 3.0 {
                                    painter.line_segment([Pos2::new(right - 2.0, y - 2.0), Pos2::new(right, y)], Stroke::new(1.0, ws));
                                    painter.line_segment([Pos2::new(right - 2.0, y + 2.0), Pos2::new(right, y)], Stroke::new(1.0, ws));
                                }
                            }
                        }
                        super::whitespace_marks::Kind::LineEnding(_) => {
                            let rect = ll.galley.pos_from_cursor(CCursor::new(index));
                            painter.text(ll.origin + Vec2::new(rect.min.x + 2.0, rect.center().y), Align2::LEFT_CENTER, "↵", font_id.clone(), ws);
                        }
                    }
                }
            }
        }

        // Headers span the whole visible layout. Register and paint each once,
        // independently of how many source glyph rows are visible below it.
            let lens_ends:Vec<_>=layouts.iter().map(|layout|{let end=layout.galley.pos_from_cursor(CCursor::new(layout.galley.job.text.chars().count()));(layout.line,layout.origin+end.min.to_vec2()+Vec2::new(16.0,0.0))}).collect();
        events.extend(super::code_vision_view::paint(ui,state,cfg.code_vision,&vision_rows,text_origin,line_h,font_id.clone(),cfg.theme.text_dim,&lens_ends).into_iter().map(ViewEvent::CodeVisionFindUsages));
        if cfg.settings.preferences.code_vision_inheritors {events.extend(super::preferences::paint_inheritors(ui,state,cfg.gutter_markers,&vision_rows,text_origin,line_h,&lens_ends,cfg.theme.text_dim).into_iter().map(ViewEvent::GutterIntent));}
        // ---- debugger values after the line end -----------------------------------
        if !cfg.inline_values.is_empty() {
            let dim = if cfg.theme.dark { Color32::from_rgb(0x8C, 0x9A, 0xA8) } else { Color32::from_rgb(0x6A, 0x73, 0x7D) };
            let value_font = FontId::new(font_size * 0.92, mono_family(false, true));
            for ll in &layouts {
                let Some(v) = cfg.inline_values.iter().find(|v| v.line == ll.line) else { continue };
                if matches!(fold_starts.get(&ll.line), Some((true, _))) {
                    continue;
                }
                let Some(row) = ll.galley.rows.last() else { continue };
                let r = row.rect();
                let pos = Pos2::new(ll.origin.x + r.right() + char_w * 2.0, ll.origin.y + r.center().y);
                painter.text(pos, Align2::LEFT_CENTER, &v.text, value_font.clone(), if v.changed { cfg.theme.accent } else { dim });
            }
        }

        // ---- diagnostics: wavy underlines (strike-through for deprecations) ----
        // Only problems on the laid-out lines are painted: a range test first
        // keeps a file with thousands of them from mapping each one per frame.
        let laid_out = layouts.first().zip(layouts.last()).map(|(first, last)| {
            let (first, last) = (first.line.min(last.line), first.line.max(last.line));
            TextRange::new(state.doc.line_range(first).start(), state.doc.line_range_with_newline(last).end())
        });
        for d in &diagnostics {
            if laid_out.is_none_or(|r| d.range.end() < r.start() || d.range.start() > r.end()) {
                continue;
            }
            let look = super::diag_style::look(d);
            if look == super::diag_style::Look::Unused {
                continue;
            }
            let s = state.doc.offset_to_position(d.range.start());
            let e = state.doc.offset_to_position(d.range.end());
            let Some(ll) = layouts.iter().find(|l| l.line == s.line as usize) else { continue };
            let text = state.doc.line_text(s.line as usize);
            if ll.visual.is_some(){let end_col=if e.line==s.line{e.column as usize}else{text.chars().count()};let color=match d.severity{rustpy_core::Severity::Error=>rgb(cfg.scheme.editor.error_underline),rustpy_core::Severity::Warning=>rgb(cfg.scheme.editor.warning_underline),rustpy_core::Severity::Typo=>rgb(cfg.scheme.editor.typo_underline),_=>rgb(cfg.scheme.editor.weak_warning_underline)};for rect in visual_source_rects(ll,&text,s.column as usize,end_col,tab_size){let rect=rect.translate(ll.origin.to_vec2());if look==super::diag_style::Look::StrikeOut{painter.line_segment([Pos2::new(rect.left(),rect.center().y),Pos2::new(rect.right(),rect.center().y)],Stroke::new(1.0,default_color));}else{let mut points=Vec::new();let mut x=rect.left();while x<=rect.right(){points.push(Pos2::new(x,rect.bottom()-2.0-if points.len()%2==0{0.0}else{2.0}));x+=2.0;}painter.add(egui::Shape::line(points,Stroke::new(1.0,color)));}}continue;}
            let p0 = pos_of_col(ll, &text, s.column as usize, tab_size);
            let x0 = p0.x;
            let end_col = if e.line == s.line { e.column as usize } else { text.chars().count() };
            let p1 = pos_of_col(ll, &text, end_col, tab_size);
            let mut x1 = if (p0.y - p1.y).abs() < 0.5 { p1.x } else { ll.galley.size().x };
            if x1 - x0 < char_w * 0.6 {
                x1 = x0 + char_w;
            }
            if look == super::diag_style::Look::StrikeOut {
                let y = ll.origin.y + p0.y + line_h * 0.55;
                painter.line_segment([Pos2::new(ll.origin.x + x0, y), Pos2::new(ll.origin.x + x1, y)], Stroke::new(1.0, default_color));
                continue;
            }
            let color = match d.severity {
                rustpy_core::Severity::Error => rgb(cfg.scheme.editor.error_underline),
                rustpy_core::Severity::Warning => rgb(cfg.scheme.editor.warning_underline),
                rustpy_core::Severity::Typo => rgb(cfg.scheme.editor.typo_underline),
                _ => rgb(cfg.scheme.editor.weak_warning_underline),
            };
            let y = ll.origin.y + p0.y + line_h - 2.0;
            let mut pts = Vec::new();
            let mut x = ll.origin.x + x0;
            let mut up = false;
            while x <= ll.origin.x + x1 {
                pts.push(Pos2::new(x, if up { y - 2.0 } else { y }));
                up = !up;
                x += 2.0;
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.0, color)));
        }

        // ---- carets -----------------------------------------------------------
        let time = ui.input(|i| i.time);
        let blink_on = view.blink.visible(time, cfg.settings);
        let mut primary_caret_rect = None;
        for i in 0..state.carets.len() {
            let p = state.virtual_caret(i).head;
            let line = p.line as usize;
            let Some(row) = line_row(line) else { continue };
            let mut y = row_y(row);
            let x = match layouts.iter().find(|l| l.line == line) {
                Some(ll) => {
                    let text = state.doc.line_text(line);
                    let pc = super::virtual_space_view::extend(caret_pos_of_col(ll, &text, p.column as usize, tab_size), p.column as usize, text.chars().count(), char_w);
                    y = ll.origin.y + pc.y;
                    ll.origin.x + pc.x
                }
                None => text_origin.x + p.column as f32 * char_w,
            };
            let cell = layouts.iter().find(|ll| ll.line == line).and_then(|ll| {
                let text = state.doc.line_text(line);
                if p.column as usize > text.chars().count() { return None; }
                let index = source_glyph_index(ll, &text, p.column as usize, tab_size);
                let width = source_cluster_len(ll,&text,p.column as usize,tab_size);
                glyph_span_rects(ll, index, width).first().copied()
                    .map(|rect| rect.translate(ll.origin.to_vec2()))
            }).unwrap_or_else(|| Rect::from_min_size(Pos2::new(x, y), Vec2::new(char_w, line_h)));
            let caret_origin=if super::caret::effective_shape(cfg.settings,state.overwrite_mode)==rustpy_settings::CaretShape::Thin{Pos2::new(x,y)}else{cell.min};
            let rect = super::caret::caret_rect(caret_origin, Vec2::new(cell.width().max(1.0), line_h), cfg.settings, state.overwrite_mode);
            if i == 0 {
                primary_caret_rect = Some(rect);
            }
            if (focused || cfg.settings.preferences.caret_when_unfocused) && blink_on {
                painter.rect_filled(rect, 0.0, rgb(cfg.scheme.editor.caret));
            }
        }
        if let (Some(_), Some(t)) = (view.dnd, view.dnd_target) {
            let p = state.doc.offset_to_position(t);
            let line = p.line as usize;
            if let Some(row) = line_row(line) {
                let mut y = row_y(row);
                let x = match layouts.iter().find(|l| l.line == line) {
                    Some(ll) => {
                        let pc = caret_pos_of_col(ll, &state.doc.line_text(line), p.column as usize, tab_size);
                        y = ll.origin.y + pc.y;
                        ll.origin.x + pc.x
                    }
                    None => text_origin.x + p.column as f32 * char_w,
                };
                painter.rect_filled(Rect::from_min_size(Pos2::new(x - 1.0, y), Vec2::new(2.0, line_h)), 0.0, rgb(cfg.scheme.editor.caret).gamma_multiply(0.7));
            }
        }
        if focused {
            let predicted_dt = ui.input(|i| i.predicted_dt);
            if let Some(delay) = view.blink.repaint_delay(time, cfg.settings, predicted_dt) {
                ui.ctx().request_repaint_after(delay);
            }
        }
        view.caret_screen_rect = primary_caret_rect;
        if let Some(rect) = primary_caret_rect {
            if state.scroll_to_caret {
                // The fixed gutter is painted over the viewport's left edge.
                // Include its width so a revealed caret cannot remain underneath it.
                let reveal = rect.union(rect.translate(Vec2::new(-gutter_w, 0.0)));
                ui.scroll_to_rect(reveal.expand2(Vec2::new(char_w * 2.0, line_h)), None);
                state.scroll_to_caret = false;
            }
            if focused {
                let mut ime_rect=rect;
                if !view.ime.preedit.is_empty(){let preedit_job=super::unicode_view::shape(LayoutJob::simple(view.ime.preedit.clone(),font_id.clone(),default_color,f32::INFINITY));let count=preedit_job.text.chars().count();let(preedit_job,_)=super::unicode_view::reorder(preedit_job,&[(0,count)],None);let galley=ui.fonts_mut(|fonts|fonts.layout_job(preedit_job));let origin=Pos2::new(rect.min.x,rect.min.y);painter.rect_filled(Rect::from_min_size(origin,galley.size()),0.0,rgb(cfg.scheme.editor.background));eframe::text_rendering::editor_galley(&painter,origin,galley.clone(),default_color);ime_rect=Rect::from_min_size(origin+Vec2::new(galley.size().x,0.0),Vec2::new(1.0,line_h));painter.line_segment([origin+Vec2::new(0.0,line_h-1.0),origin+Vec2::new(galley.size().x,line_h-1.0)],Stroke::new(1.5,default_color));}
                let interrupt=std::mem::take(&mut view.ime.interrupt);
                ui.ctx().output_mut(|o| o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: content_rect, cursor_rect: ime_rect, should_interrupt_composition: interrupt }));
            }
        }

        // ---- gutter (painted last so it covers horizontally scrolled text) ----
        let gutter_rect = Rect::from_min_max(Pos2::new(clip.left(), clip.top()), Pos2::new(clip.left() + gutter_w - 6.0, clip.bottom()));
        painter.rect_filled(gutter_rect, 0.0, rgb(cfg.scheme.editor.gutter_background));
        painter.line_segment([Pos2::new(gutter_rect.right(), clip.top()), Pos2::new(gutter_rect.right(), clip.bottom())], Stroke::new(1.0, rgb(cfg.scheme.editor.gutter_separator)));
        // VCS change markers along the gutter's right edge.
        for ch in cfg.line_changes {
            let x0 = gutter_rect.right() - CHANGE_BAR_INSET;
            match ch.kind {
                rustpy_vcs::ChangeKind::Deleted => {
                    if let Some(row) = line_row(ch.start.min(line_count.saturating_sub(1))) {
                        let y = row_y(row);
                        let tri = [Pos2::new(x0 - 3.0, y - 4.0), Pos2::new(x0 + 3.0, y), Pos2::new(x0 - 3.0, y + 4.0)];
                        painter.add(egui::Shape::convex_polygon(tri.to_vec(), rgb(cfg.scheme.context_color("vcs.deleted",cfg.scheme.editor.deleted_line)), Stroke::NONE));
                    }
                }
                kind => {
                    let color = if kind == rustpy_vcs::ChangeKind::Added { rgb(cfg.scheme.context_color("vcs.added",cfg.scheme.editor.added_line)) } else { rgb(cfg.scheme.context_color("vcs.modified",cfg.scheme.editor.modified_line)) };
                    for l in ch.start..ch.start + ch.count {
                        if let Some(row) = line_row(l) {
                            let y = row_y(row);
                            painter.rect_filled(Rect::from_min_max(Pos2::new(x0 - 1.5, y), Pos2::new(x0 + 1.5, y + line_h)), 0.0, color);
                        }
                    }
                }
            }
        }
        let num_font = FontId::new(font_size * 0.95, mono_family(false, false));
        let mut gutter_pointer = view.gutter_press_owned;
        let mut fold_clicks: Vec<usize> = Vec::new();
        let gutter_hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| gutter_rect.contains(p));
        if !cfg.coverage.is_empty() {
            let x0 = gutter_rect.left() + annotation_w + color_preview_w + actions_w + 1.0;
            for (row, &line) in rows.iter().enumerate().take(last_row).skip(first_row) {
                if row_sub[row] != 0 {
                    continue;
                }
                let Ok(idx) = cfg.coverage.binary_search_by_key(&line, |(l, _)| *l) else { continue };
                let color = match cfg.coverage[idx].1 {
                    0 => Color32::from_rgb(0x4C, 0xA6, 0x4C),
                    1 => Color32::from_rgb(0xD0, 0x50, 0x50),
                    3 => Color32::from_rgb(0xD8, 0xB4, 0x3C),
                    _ => Color32::from_rgb(0x80, 0x80, 0x80),
                };
                let y = row_y(row);
                painter.rect_filled(Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x0 + 3.0, y + line_h)), 0.0, color);
            }
        }
        if let Some(ann) = cfg.annotations {
            let band_colors = [Color32::from_rgb(0x4A, 0x88, 0xC7), Color32::from_rgb(0x59, 0xA8, 0x69), Color32::from_rgb(0xC7, 0x7C, 0x3A), Color32::from_rgb(0xB0, 0x6A, 0xC9), Color32::from_rgb(0xC9, 0x5A, 0x5A), Color32::from_rgb(0x3A, 0xB0, 0xB0)];
            let ann_font = FontId::new(font_size * 0.85, mono_family(false, false));
            for (row, &line) in rows.iter().enumerate().take(last_row).skip(first_row) {
                if row_sub[row] != 0 {
                    continue;
                }
                let Some((text, band, tip)) = ann.get(line) else { continue };
                let y0 = row_y(row);
                let r = Rect::from_min_size(Pos2::new(gutter_rect.left(), y0), Vec2::new(annotation_w - 4.0, line_h));
                painter.rect_filled(Rect::from_min_size(r.min, Vec2::new(3.0, line_h)), 0.0, band_colors[*band as usize % band_colors.len()]);
                let clip_p = painter.with_clip_rect(r);
                clip_p.text(Pos2::new(r.left() + 8.0, y0 + line_h / 2.0), Align2::LEFT_CENTER, text, ann_font.clone(), rgb(cfg.scheme.editor.line_number));
                let annotation=ui.interact(r,view.id.with(("git-annotation",line)),Sense::click());
                if annotation.contains_pointer()&&ui.input(|input|input.pointer.any_pressed()){gutter_pointer=true;view.gutter_press_owned=true;}
                let revision=tip.split_whitespace().next().unwrap_or("");
                crate::a11y::bound_keyboard_context_menu(&annotation,cfg.keyboard,view.id.with(("git-annotation-owner",line))).show(|ui|{
                    if !revision.is_empty(){for (label,intent) in [("Show commit in Git Log",BlameIntent::Log),("Show diff",BlameIntent::Diff),("Annotate previous revision",BlameIntent::Previous)]{if ui.button(crate::i18n::tr_static(label)).clicked(){events.push(ViewEvent::BlameCommit{revision:revision.into(),intent});ui.close();}}if ui.button("Copy revision").clicked(){ui.ctx().copy_text(revision.into());ui.close();}}
                    if ui.button("Annotation options…").clicked(){events.push(ViewEvent::BlameCommit{revision:revision.into(),intent:BlameIntent::Options});ui.close();}
                    if ui.button("Hide annotations").clicked(){events.push(ViewEvent::Action(Action::Annotate));ui.close();}
                });
                if !tip.is_empty() && ui.rect_contains_pointer(r) {
                    egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), ui.id().with(("blame", line)), egui::PopupAnchor::Pointer).show(|ui| {
                        ui.label(tip);
                    });
                }
            }
            painter.line_segment([Pos2::new(gutter_rect.left() + annotation_w - 4.0, clip.top()), Pos2::new(gutter_rect.left() + annotation_w - 4.0, clip.bottom())], Stroke::new(1.0, rgb(cfg.scheme.editor.gutter_separator)));
        }
        for (row, &line) in rows.iter().enumerate().take(last_row).skip(first_row) {
            if row_sub[row] != 0 {
                continue;
            }
            let y = row_y(row) + line_h / 2.0;
            let rendered_doc = doc_at_line(line).is_some();
            if !rendered_doc {
                if let Some(colors) = colors_per_line.get(&line) {
                    let width = 12.0;
                    let visible = if colors.len() > 8 { 7 } else { colors.len() };
                    for (index, color) in colors.iter().take(visible).enumerate() {
                        let rect = Rect::from_center_size(Pos2::new(gutter_rect.left() + annotation_w + width * (index as f32 + 0.5), y), Vec2::new((width - 2.0).max(1.0), 10.0));
                        painter.rect_filled(rect, 0.0, Color32::from_gray(210));
                        for (x, y) in [(0.0, 0.0), (0.5, 0.5)] {
                            painter.rect_filled(Rect::from_min_size(rect.min + Vec2::new(rect.width() * x, rect.height() * y), rect.size() * 0.5), 0.0, Color32::from_gray(120));
                        }
                        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(color.rgba[0], color.rgba[1], color.rgba[2], color.rgba[3]));
                        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, rgb(cfg.scheme.editor.gutter_separator)), egui::StrokeKind::Inside);
                        ui.interact(rect, view.id.with(("css_color", color.range.start())), Sense::hover()).on_hover_text(format!("{}\nRGBA({}, {}, {}, {})", state.doc.slice(color.range), color.rgba[0], color.rgba[1], color.rgba[2], color.rgba[3]));
                    }
                    if colors.len() > visible {
                        let rect = Rect::from_center_size(Pos2::new(gutter_rect.left() + annotation_w + width * 7.5, y), Vec2::new(10.0, 10.0));
                        painter.text(rect.center(), Align2::CENTER_CENTER, "+", FontId::proportional(11.0), cfg.theme.text);
                        ui.interact(rect, view.id.with(("css_color_overflow", line)), Sense::hover()).on_hover_ui(|ui| {
                            ui.label(format!("{} more colors", colors.len() - visible));
                            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                                for color in colors.iter().skip(visible) {
                                    ui.horizontal(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(150));
                                        ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(color.rgba[0], color.rgba[1], color.rgba[2], color.rgba[3]));
                                        ui.label(state.doc.slice(color.range));
                                    });
                                }
                            });
                        });
                    }
                }
            }
            if !rendered_doc {
                let marker = cfg.gutter_markers.iter().find(|m| m.offset <= state.doc.len() && state.doc.line_of(m.offset) == line);
                // One offset per line in line order (`quick_fix_offsets`): a binary search per painted line.
                let line_start = state.doc.line_range(line).start();
                let quick = cfg.gutter_quick_fixes.get(cfg.gutter_quick_fixes.partition_point(|offset| *offset < line_start)).copied().filter(|offset| *offset <= state.doc.len() && state.doc.line_of(*offset) == line);
                let review=view.review_comments.iter().find(|marker|marker.document==state.doc.identity()&&marker.version==state.doc.version()&&marker.offset<=state.doc.len()&&state.doc.line_of(marker.offset)==line);
                for (lane, offset, kind) in [
                    (3,review.map(|marker|marker.offset),super::gutter_actions::Kind::ReviewComment(review.map(|marker|marker.id).unwrap_or_default())),
                    (0, quick, super::gutter_actions::Kind::QuickFix),
                    (1, marker.filter(|m| m.has_super).map(|m| m.offset), super::gutter_actions::Kind::Super),
                    (2, marker.filter(|m| m.has_implementations).map(|m| m.offset), super::gutter_actions::Kind::Implementations),
                ] {
                    let name=match lane{0=>"quickfix",1=>"super",2=>"implementations",_=>"review"};
                    if let Some(offset) = offset.filter(|_|super::preferences::gutter_visible(cfg.settings,name)) {
                        let rect = Rect::from_center_size(Pos2::new(gutter_rect.left() + annotation_w + color_preview_w + 9.0 + lane as f32 * 18.0, y), Vec2::new(16.0, line_h));
                        if ui.rect_contains_pointer(rect.intersect(ui.clip_rect()))
                            && ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary)
                                && input.pointer.press_origin().is_some_and(|origin| rect.intersect(ui.clip_rect()).contains(origin))) {
                            view.gutter_press_owned = true;
                            gutter_pointer = true;
                        }
                        let output = super::gutter_actions::show(ui, rect, view.id.with(("gutter_intent", lane, offset)), super::gutter_actions::Intent { offset, kind }, cfg.theme.text);
                        if lane == 3 { if let Some(review) = review { ui.interact(rect, view.id.with(("review_hover",review.id)), Sense::hover()).on_hover_text(&review.text); } }
                        gutter_pointer |= output.consumed_pointer;
                        if let Some(intent) = output.intent { events.push(ViewEvent::GutterIntent(intent)); }
                    }
                }
            }
            if cfg.settings.show_line_numbers && !rendered_doc {
                let is_current = line == primary_line;
                let n = if cfg.settings.relative_line_numbers && !is_current { (line as i64 - primary_line as i64).unsigned_abs() as usize } else { line + 1 };
                let color = if is_current { rgb(cfg.scheme.editor.line_number_current) } else { rgb(cfg.scheme.editor.line_number) };
                painter.text(Pos2::new(gutter_rect.right() - 18.0, y), Align2::RIGHT_CENTER, n.to_string(), num_font.clone(), color);
            }
            if let Some((_, glyph)) = cfg.breakpoints.iter().find(|(l, _)| *l == line) {
                let c = Pos2::new(gutter_rect.left() + annotation_w + color_preview_w + actions_w + 9.0, y);
                paint_breakpoint(&painter, c, *glyph, rgb(cfg.scheme.editor.gutter_background));
                // The glyph takes its own clicks: click removes, right-click edits.
                let r = ui.interact(Rect::from_center_size(c, Vec2::new(16.0, line_h)), view.id.with(("bp", line)), Sense::click());
                crate::a11y::button(&r, &format!("Remove breakpoint on line {}", line + 1));
                if r.clicked() {
                    events.push(ViewEvent::GutterClick(line));
                }
                if r.secondary_clicked() {
                    events.push(ViewEvent::GutterContext(line));
                }
                let what = match glyph {
                    BpGlyph::Enabled => "Line breakpoint",
                    BpGlyph::Conditional => "Conditional breakpoint",
                    BpGlyph::Log => "Logging breakpoint (does not suspend)",
                    BpGlyph::Temporary => "Temporary breakpoint (removed once hit)",
                    BpGlyph::Disabled => "Disabled breakpoint",
                    BpGlyph::Unverified => "Breakpoint not accepted by the debugger",
                    BpGlyph::Muted => "Breakpoints are muted",
                };
                r.on_hover_text(format!("{what}\nClick to remove, right-click to edit"));
            }
            if let Some((_, mnemonic)) = cfg.bookmarks.iter().find(|(l, _)| *l == line) {
                let r = Rect::from_center_size(Pos2::new(gutter_rect.right() - 8.0, y), Vec2::new(11.0, 13.0));
                painter.rect_filled(r, 2.0, Color32::from_rgb(0x4A, 0x88, 0xC7));
                if let Some(m) = mnemonic {
                    painter.text(r.center(), Align2::CENTER_CENTER, m.to_string(), egui::FontId::proportional(10.0), Color32::WHITE);
                } else {
                    // Small notch at the bottom to look like a bookmark ribbon.
                    let notch = [Pos2::new(r.left(), r.bottom()), Pos2::new(r.center().x, r.bottom() - 4.0), Pos2::new(r.right(), r.bottom())];
                    painter.add(egui::Shape::convex_polygon(notch.to_vec(), rgb(cfg.scheme.editor.gutter_background), Stroke::NONE));
                }
            }
            if cfg.exec_line == Some(line) {
                let x = gutter_rect.left() + annotation_w + color_preview_w + actions_w + 23.0;
                let tri = [Pos2::new(x - 4.0, y - 5.0), Pos2::new(x + 4.0, y), Pos2::new(x - 4.0, y + 5.0)];
                painter.add(egui::Shape::convex_polygon(tri.to_vec(), Color32::from_rgb(0xF0, 0xD0, 0x50), Stroke::NONE));
            }
            if cfg.run_lines.contains(&line) {
                let has_bp = cfg.breakpoints.iter().any(|(l, _)| *l == line);
                let icon = Rect::from_center_size(Pos2::new(gutter_rect.left() + annotation_w + color_preview_w + actions_w + if has_bp { 22.0 } else { 9.0 }, y), Vec2::splat(12.0));
                let r = ui.interact(icon, view.id.with(("run", line)), Sense::click());
                crate::a11y::button(&r, &format!("Run actions for line {}", line + 1));
                let c = if r.hovered() { Color32::from_rgb(0x6C, 0xC6, 0x6C) } else { Color32::from_rgb(0x59, 0xA8, 0x69) };
                let tri = [Pos2::new(icon.left() + 2.0, icon.top() + 1.0), Pos2::new(icon.right() - 1.0, icon.center().y), Pos2::new(icon.left() + 2.0, icon.bottom() - 1.0)];
                painter.add(egui::Shape::convex_polygon(tri.to_vec(), c, Stroke::NONE));
                if r.hovered() {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                }
                let cell = state.language == super::Language::Python && super::console_code::is_cell_marker(&state.doc.line_text(line));
                if state.language == super::Language::Python && !cell {
                    let owner = ui.data(|data| data.get_temp::<crate::run_context::ProjectOwner>(crate::run_context::owner_key(view.id))).unwrap_or_default();
                    let authority = egui::Id::new((&owner, state.doc.identity(), state.doc.version(), &state.path, line));
                    let popup_id = egui::Popup::default_response_id(&r).with(authority);
                    let retained = view.id.with(("run-popup-authority", line));
                    if let Some(previous) = ui.data(|data| data.get_temp::<egui::Id>(retained)) {
                        if previous != popup_id { egui::Popup::close_id(ui.ctx(), previous); }
                    }
                    ui.data_mut(|data| data.insert_temp(retained, popup_id));
                    let menu = if cfg.keyboard {
                        egui::Popup::menu(&r).id(popup_id).show(|ui| {
                            let mut chosen = None;
                            for action in crate::run_context::ContextAction::ALL {
                                if ui.button(action.label()).clicked() { chosen = Some(action); ui.close(); }
                            }
                            chosen
                        })
                    } else { egui::Popup::close_id(ui.ctx(), popup_id); None };
                    if let Some(action) = menu.and_then(|menu| menu.inner) {
                        if let Some(path) = state.path.clone() {
                            let text = state.doc.text();
                            if let Some(target) = crate::run_context::target_at(&owner.root, &path, &text, line) {
                                events.push(ViewEvent::ContextRun(Box::new(crate::run_context::EditorRequest {
                                    configuration: crate::run_context::config_for_target(&owner.root, &path, &target),
                                    owner, document: state.doc.identity(), version: state.doc.version(), path, action,
                                })));
                            }
                        }
                    }
                    if !egui::Popup::is_id_open(ui.ctx(), popup_id) { r.clone().on_hover_text("Run, debug, coverage, profile or configure this target"); }
                } else {
                    // Other languages, and `#%%` cells (run in the Python Console), run directly.
                    r.clone().on_hover_text(if cell { "Run Cell in Python Console" } else { "Run (Ctrl+Shift+F10)" });
                    if cell { r.context_menu(|ui| {for action in [Action::RunCell,Action::RunCellAndAdvance,Action::RunCellsAbove,Action::DebugCell] {if ui.button(action.text()).clicked(){events.push(ViewEvent::ScientificCell{line,action});ui.close();}}}); }
                    if r.clicked() {
                        events.push(ViewEvent::GutterRun(line));
                    }
                }
            }
            if let Some((folded, _)) = fold_starts.get(&line).filter(|_| !rendered_doc&&super::preferences::gutter_visible(cfg.settings,"folding")) {
                let marker = Rect::from_center_size(Pos2::new(gutter_rect.right() - 8.0, y), Vec2::splat(11.0));
                let r = ui.interact(marker, view.id.with(("fold", line)), Sense::click());
                crate::a11y::button(&r, &format!("{} region on line {}", if *folded { "Expand" } else { "Collapse" }, line + 1));
                if *folded || gutter_hovered || r.hovered() {
                    let c = rgb(cfg.scheme.editor.line_number_current);
                    painter.rect_stroke(marker, 2.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
                    painter.line_segment([Pos2::new(marker.left() + 2.5, marker.center().y), Pos2::new(marker.right() - 2.5, marker.center().y)], Stroke::new(1.0, c));
                    if *folded {
                        painter.line_segment([Pos2::new(marker.center().x, marker.top() + 2.5), Pos2::new(marker.center().x, marker.bottom() - 2.5)], Stroke::new(1.0, c));
                    }
                }
                if r.clicked() {
                    // The box is on top of the change bar: a click on the bar still opens the change popup.
                    let on_bar = r.interact_pointer_pos().is_some_and(|p| hits_change_bar(p.x, gutter_rect.right(), true)) && rustpy_vcs::change_at_line(cfg.line_changes, line, line_count).is_some();
                    if on_bar {
                        events.push(ViewEvent::ChangeMarkerClick(line));
                    } else {
                        fold_clicks.push(line);
                    }
                }
            }
        }
        for line in fold_clicks.into_iter().chain(placeholder_clicks) {
            state.toggle_fold_at_line(line);
        }
        // Rendered-view toggles of the docstrings, in the breakpoint column (shown while
        // rendered or on gutter hover; while nothing is rendered, finding the
        // docstrings waits for the hover).
        if super::doc_render::supported(state) && (gutter_hovered || !doc_placements.is_empty()) {
            let regions = super::doc_render::regions(state);
            let mut toggles: Vec<usize> = Vec::new();
            for r in regions.iter() {
                let Some(row) = line_row(r.first_line).filter(|row| (first_row..last_row).contains(row)) else { continue };
                // The icon shows what is drawn: a caret inside or a fold shows the source.
                let rendered = doc_at_line(r.first_line).is_some();
                let x = gutter_rect.left() + annotation_w + color_preview_w + actions_w + if cfg.breakpoints.iter().any(|(l, _)| *l == r.first_line) { 22.0 } else { 9.0 };
                let icon = Rect::from_center_size(Pos2::new(x, row_y(row) + line_h / 2.0), Vec2::new(9.0, 11.0));
                let resp = ui.interact(icon.expand(2.0), view.id.with(("doc_toggle", r.first_line)), Sense::click());
                if rendered || gutter_hovered || resp.hovered() {
                    paint_doc_toggle(&painter, icon, rendered, resp.hovered(), cfg.scheme);
                }
                if resp.hovered() {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                }
                let shortcut = cfg.keymap.shortcut_text(Action::ToggleRenderedDoc);
                let tip = if shortcut.is_empty() { "Toggle Rendered View".to_string() } else { format!("Toggle Rendered View ({shortcut})") };
                if resp.on_hover_text(tip).clicked() {
                    toggles.push(r.first_line);
                }
            }
            for line in toggles {
                super::doc_render::toggle_at_line(state, line);
            }
        }

        let mut stripe_consumed_pointer = widget.consumed_pointer || vision_pointer || gutter_pointer;
        let overview = Rect::from_min_max(
            Pos2::new(clip.right() - 24.0, clip.top()),
            Pos2::new(clip.right() - 4.0, clip.bottom()));
        let overview_y = |row: usize| overview.top()
            + row as f32 / row_count.max(1) as f32 * (overview.height() - 4.0).max(0.0) + 2.0;
        let vcs_markers = inspection_widget::vcs_markers(&state.doc, cfg.line_changes, &rows);
        let vcs_rect = |marker: &inspection_widget::VcsMarker| Rect::from_min_max(
            Pos2::new(clip.right() - 23.0, overview_y(marker.first_row)),
            Pos2::new(clip.right() - 20.0,
                (overview_y(marker.last_row.saturating_add(1)).max(overview_y(marker.first_row) + 2.0))
                    .min(clip.bottom())));
        // ---- error stripe (right margin markers) ------------------------------
        if overview.height() >= 4.0 {
            // Thousands of problems share a few hundred stripe pixels: paint each pixel and kind once.
            let mut painted = std::collections::HashSet::new();
            for d in &diagnostics {
                let line = state.doc.line_of(d.range.start());
                let row = session_scroll_row(line as u32, &line_to_row, &rows);
                let y = overview_y(row);
                let error = d.severity == rustpy_core::Severity::Error;
                if !painted.insert((y.round() as i32, error)) {
                    continue;
                }
                let color = if error { rgb(cfg.scheme.editor.error_stripe_error) } else { rgb(cfg.scheme.editor.error_stripe_warning) };
                painter.rect_filled(Rect::from_min_size(Pos2::new(clip.right() - 16.0, y), Vec2::new(10.0, cfg.settings.preferences.error_stripe_min_height.clamp(1.0,20.0))), 1.0, color);
            }
            // Search results (find bar matches, highlighted usages).
            let color = if cfg.theme.dark { Color32::from_rgb(0x5F, 0xA8, 0x6E) } else { Color32::from_rgb(0x4C, 0xA8, 0x4C) };
            let mut drawn_y = f32::NEG_INFINITY;
            for r in cfg.search_highlights {
                let row = session_scroll_row(state.doc.line_of(r.start()) as u32, &line_to_row, &rows);
                let y = overview_y(row);
                if (y - drawn_y).abs() < 2.0 { continue; }
                drawn_y = y;
                painter.rect_filled(Rect::from_min_size(Pos2::new(clip.right() - 14.0, y), Vec2::new(6.0, 2.0)), 0.0, color);
            }
            // A separate adjacent lane preserves diagnostic/search visibility.
            for marker in &vcs_markers {
                let color = match marker.kind {
                    rustpy_vcs::ChangeKind::Added => cfg.scheme.context_color("vcs.added",cfg.scheme.editor.added_line),
                    rustpy_vcs::ChangeKind::Modified => cfg.scheme.context_color("vcs.modified",cfg.scheme.editor.modified_line),
                    rustpy_vcs::ChangeKind::Deleted => cfg.scheme.context_color("vcs.deleted",cfg.scheme.editor.deleted_line),
                };
                painter.rect_filled(vcs_rect(marker), 0.0, rgb(color));
            }
        }
        // Reuse bounded source preview and actual-source reveal for all lanes.
        if overview.height() >= 4.0 && (!diagnostics.is_empty()
            || !cfg.search_highlights.is_empty() || !vcs_markers.is_empty()) {
            let target = ui.input(|input| input.pointer.hover_pos())
                .filter(|point| overview.contains(*point) && !summary_rect.contains(*point))
                .map(|point| {
                    let findings = diagnostics.iter().map(|diagnostic| (diagnostic.range.start(), diagnostic.message.as_str()))
                        .chain(cfg.search_highlights.iter().map(|range| (range.start(), "Search result")))
                        .map(|(offset, message)| {
                            let row = session_scroll_row(state.doc.line_of(offset) as u32, &line_to_row, &rows);
                            let rect = Rect::from_min_size(Pos2::new(clip.right() - 16.0, overview_y(row)), Vec2::new(10.0, 3.0));
                            (rect.distance_to_pos(point), offset, message)
                        });
                    let nearest = findings.chain(vcs_markers.iter().map(|marker|
                        (vcs_rect(marker).distance_to_pos(point), marker.target.clamp(marker.source.start(), marker.source.end()), marker.description())))
                        .min_by(|a,b| a.0.total_cmp(&b.0));
                    if let Some((_, offset, message)) = nearest.filter(|entry| entry.0 <= 6.0) {
                        (offset, message.to_owned())
                    } else {
                        let fraction = ((point.y-overview.top()) / overview.height().max(1.0)).clamp(0.0,1.0);
                        let row = ((fraction * rows.len() as f32) as usize).min(rows.len().saturating_sub(1));
                        let line = rows.get(row).copied().unwrap_or(0);
                        (state.doc.line_range(line).start(), "Source preview".into())
                    }
                });
            // Pointer-only: never steal editor Tab indentation/template traversal.
            let response = ui.interact(overview, view.id.with("stripe-preview"), Sense::CLICK);
            if let Some((offset, message)) = target {
                if cfg.settings.preferences.scrollbar_code_preview {
                let preview = scrollbar_preview::context(&state.doc, offset);
                response.clone().on_hover_ui(|ui| {
                    ui.label(message);
                    ui.monospace(preview);
                    ui.label("Click to navigate");
                });
                }
                stripe_consumed_pointer |= ui.input(|input|
                    input.pointer.primary_down() || input.pointer.button_released(egui::PointerButton::Primary));
                if response.clicked() {
                    state.reveal_offset(offset);
                    let position = state.doc.offset_to_position(offset);
                    state.go_to_line(position.line as usize + 1, Some(position.column as usize + 1));
                    view.blink.reset(ui.input(|input| input.time));
                    ui.memory_mut(|memory| memory.request_focus(view.id));
                }
            }
        }

        // ---- sticky lines (enclosing scopes pinned at the top) ----------------
        if cfg.settings.show_sticky_lines && cfg.settings.sticky_lines_limit > 0 && state.language == super::Language::Python && first_row > 0 {
            let first_line = rows.get(first_row).copied().unwrap_or(0);
            // Probe the first non-blank visible line at its first code character.
            let mut probe_line = first_line.min(line_count.saturating_sub(1));
            while probe_line + 1 < line_count && probe_line < first_line + 5 && state.doc.line_text(probe_line).trim().is_empty() {
                probe_line += 1;
            }
            let probe_text = state.doc.line_text(probe_line);
            let indent = probe_text.len() - probe_text.trim_start().len();
            // One char into the first word so the token belongs to the statement, not the indent.
            // Clamped: on an empty last line `start + 1` lies past the end of the text.
            let probe = (state.doc.line_range(probe_line).start() + TextSize::new((indent + 1).min(probe_text.len().max(1)) as u32)).min(state.doc.len());
            // The per-frame tree: a large file's may lag a keystroke behind.
            let crumbs = super::syntax_support::breadcrumbs(state.frame_syntax(), probe);
            let mut header_lines: Vec<usize> = crumbs
                .iter()
                .map(|(_, r)| {
                    // Skip decorator lines so the `class`/`def` line is shown.
                    let mut l = state.doc.line_of(r.start().min(state.doc.len()));
                    while l + 1 < line_count && state.doc.line_text(l).trim_start().starts_with('@') {
                        l += 1;
                    }
                    l
                })
                .filter(|l| *l < first_line)
                .collect();
            header_lines.dedup();
            let limit = cfg.settings.sticky_lines_limit;
            if header_lines.len() > limit {
                let skip = header_lines.len() - limit;
                header_lines.drain(0..skip);
            }
            if !header_lines.is_empty() {
                let sticky_bg = rgb(cfg.scheme.editor.sticky_lines_bg);
                let spans = state.highlight_spans().to_vec();
                let top = clip.top();
                let total_h = header_lines.len() as f32 * line_h;
                painter.rect_filled(Rect::from_min_max(Pos2::new(clip.left(), top), Pos2::new(clip.right(), top + total_h)), 0.0, sticky_bg);
                painter.line_segment([Pos2::new(clip.left(), top + total_h), Pos2::new(clip.right(), top + total_h)], Stroke::new(1.0, rgb(cfg.scheme.editor.gutter_separator)));
                for (k, &line) in header_lines.iter().enumerate() {
                    let y = top + k as f32 * line_h;
                    let lr = state.doc.line_range(line);
                    let text = state.doc.line_text(line);
                    let hidden_comment = super::doc_render::hidden_inline_comment_start(state, line);
                    let line_start = usize::from(lr.start());
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;
                    let mut pos = 0usize;
                    let si = spans.partition_point(|s| s.range.end() <= lr.start());
                    for s in spans[si..].iter().take_while(|s| usize::from(s.range.start()) < line_start + text.len()) {
                        let a = (usize::from(s.range.start()).max(line_start) - line_start).min(text.len());
                        let b = (usize::from(s.range.end()).min(line_start + text.len()) - line_start).min(text.len());
                        if a > pos {
                            append_reader_segment(&mut job, &text, pos, a, TextFormat { font_id: font_id.clone(), color: default_color, line_height: Some(line_h), ..Default::default() }, hidden_comment, tab_size);
                        }
                        if a < b {
                            let style = cfg.scheme.style(s.tag);
                            let color = style.fg.map(rgb).unwrap_or(default_color);
                            append_reader_segment(&mut job, &text, a, b, TextFormat { font_id: FontId::new(font_size, mono_family(style.bold, false)), color, italics: style.italic, line_height: Some(line_h), ..Default::default() }, hidden_comment, tab_size);
                            pos = b;
                        }
                    }
                    if pos < text.len() {
                        append_reader_segment(&mut job, &text, pos, text.len(), TextFormat { font_id: font_id.clone(), color: default_color, line_height: Some(line_h), ..Default::default() }, hidden_comment, tab_size);
                    }
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    let gutter_bg = rgb(cfg.scheme.editor.gutter_background);
                    painter.rect_filled(Rect::from_min_max(Pos2::new(clip.left(), y), Pos2::new(clip.left() + gutter_w - 6.0, y + line_h)), 0.0, gutter_bg);
                    if cfg.settings.show_line_numbers {
                        painter.text(Pos2::new(clip.left() + gutter_w - 6.0 - 18.0, y + line_h / 2.0), Align2::RIGHT_CENTER, (line + 1).to_string(), FontId::new(font_size * 0.95, mono_family(false, false)), rgb(cfg.scheme.editor.line_number));
                    }
                    let text_x = clip.left() + gutter_w + (text_origin.x - content_rect.left() - gutter_w);
                    let clip_painter = painter.with_clip_rect(Rect::from_min_max(Pos2::new(clip.left() + gutter_w - 6.0, y), Pos2::new(clip.right(), y + line_h)));
                    eframe::text_rendering::editor_galley(&clip_painter, Pos2::new(text_x, y), galley, default_color);
                    // Click on a sticky line jumps to it.
                    let row_rect = Rect::from_min_max(Pos2::new(clip.left(), y), Pos2::new(clip.right(), y + line_h));
                    if ui.input(|i| i.pointer.primary_clicked()) && ui.input(|i| i.pointer.interact_pos().is_some_and(|p| row_rect.contains(p))) {
                        state.set_carets(vec![Caret::at(lr.start())]);
                        state.scroll_to_caret = true;
                        view.sticky_consumed_click = true;
                    }
                }
            }
        }

        // ---- reader mode switch (top-right corner, library / read-only files) ----
        if super::doc_render::reader_mode_available(state) {
            let font = FontId::proportional((font_size * 0.9).max(11.0));
            let text_color = if reader_mode { cfg.theme.accent } else { cfg.theme.text_dim };
            let galley = painter.layout_no_wrap("Reader Mode".to_string(), font, text_color);
            let rect = Rect::from_min_size(Pos2::new(clip.right() - 24.0 - galley.size().x - 24.0, clip.top() + 4.0), Vec2::new(galley.size().x + 24.0, galley.size().y + 6.0));
            let r = ui.interact(rect, view.id.with("reader_mode"), Sense::click());
            let fill = if r.hovered() { cfg.theme.hover_bg } else { rgb(cfg.scheme.editor.background) };
            painter.rect_filled(rect, 4.0, fill);
            painter.rect_stroke(rect, 4.0, Stroke::new(1.0, if reader_mode { cfg.theme.accent.gamma_multiply(0.6) } else { cfg.theme.border }), egui::StrokeKind::Inside);
            // An open book.
            let c = Pos2::new(rect.left() + 11.0, rect.center().y);
            let book = Stroke::new(1.2, text_color);
            painter.line_segment([Pos2::new(c.x, c.y - 4.0), Pos2::new(c.x, c.y + 4.0)], book);
            painter.line_segment([Pos2::new(c.x, c.y - 4.0), Pos2::new(c.x - 5.0, c.y - 5.0)], book);
            painter.line_segment([Pos2::new(c.x, c.y - 4.0), Pos2::new(c.x + 5.0, c.y - 5.0)], book);
            painter.line_segment([Pos2::new(c.x - 5.0, c.y - 5.0), Pos2::new(c.x - 5.0, c.y + 3.0)], book);
            painter.line_segment([Pos2::new(c.x + 5.0, c.y - 5.0), Pos2::new(c.x + 5.0, c.y + 3.0)], book);
            painter.line_segment([Pos2::new(c.x - 5.0, c.y + 3.0), Pos2::new(c.x, c.y + 4.0)], book);
            painter.line_segment([Pos2::new(c.x + 5.0, c.y + 3.0), Pos2::new(c.x, c.y + 4.0)], book);
            eframe::text_rendering::editor_galley(&painter, Pos2::new(rect.left() + 20.0, rect.top() + 3.0), galley, text_color);
            if r.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            }
            let tip = if reader_mode { "Reader mode: documentation comments rendered, warnings hidden (Settings › Editor › Reader Mode). Click to leave it." } else { "Click to read this file in reader mode" };
            if r.on_hover_text(tip).clicked() {
                super::doc_render::toggle_reader_mode(state);
            }
        }

        // ---- input ------------------------------------------------------------
        let line_at_pointer = |pointer: Pos2| rows.get(((pointer.y - text_origin.y) / line_h).floor().max(0.0) as usize).copied();
        // A rendered docstring under the pointer and the pointer relative to its content.
        let doc_hit = |pointer: Pos2| -> Option<(&DocPlacement, Vec2)> {
            if pointer.x < text_origin.x - 6.0 {
                return None;
            }
            let line = line_at_pointer(pointer)?;
            let p = doc_at_line(line)?;
            let top = row_y(line_row(line)?);
            Some((p, pointer - p.content_origin(text_origin.x, top, line_h)))
        };
        let pos_from_pointer = |state: &EditorState, pointer: Pos2| -> Position {
            let row = (((pointer.y - text_origin.y) / line_h).floor().max(0.0) as usize).min(rows.len().saturating_sub(1));
            let line = rows.get(row).copied().unwrap_or(0);
            if let Some(p) = doc_at_line(line) {
                // A rendered docstring: the source line of the part under the pointer.
                let top = line_row(line).map_or(text_origin.y, row_y);
                let rel = pointer - p.content_origin(text_origin.x, top, line_h);
                return Position::new((line + p.doc.line_at(rel)).min(p.region.last_line) as u32, 0);
            }
            let col = match layouts.iter().find(|l| l.line == line) {
                Some(ll) => {
                    let rel_y = if soft_wrap { (pointer.y - ll.origin.y).clamp(0.0, (ll.galley.size().y - 1.0).max(0.0)) } else { (line_h / 2.0).max(0.0) };
                    let rel = Vec2::new(pointer.x - ll.origin.x, rel_y);
                    let visual_col = ll.galley.cursor_from_pos(rel).index.0;
                    let display_col=ll.visual.as_ref().and_then(|map|map.visual_to_logical.get(visual_col)).copied().unwrap_or(visual_col);
                    match &ll.map {
                        Some(map) => map.get(display_col).copied().unwrap_or(*map.last().unwrap_or(&0)),
                        None => display_col,
                    }
                }
                None => ((pointer.x - text_origin.x) / char_w).round().max(0.0) as usize,
            };
            let source_text=state.doc.line_text(line);let col=super::unicode_edit::floor_column(&source_text,col);
            let physical = source_text.chars().count();
            let end = layouts.iter().find(|ll| ll.line == line).map(|ll| {
                let p = caret_pos_of_col(ll, &state.doc.line_text(line), physical, tab_size);
                ll.origin + p.to_vec2()
            }).unwrap_or(Pos2::new(text_origin.x + physical as f32 * char_w, row_y(row)));
            let col = super::virtual_space_view::pointer_column(physical, end, pointer, char_w, line_h, virtual_enabled).unwrap_or(col.min(physical));
            Position::new(line as u32, col as u32)
        };

        // Hovered offset (for hover documentation) and diagnostic tooltips.
        let hover_doc = if resp.hovered() && !vision_pointer { ui.input(|i| i.pointer.hover_pos()).and_then(doc_hit) } else { None };
        if let Some((p, rel)) = hover_doc {
            // Rendered documentation: links open in the browser, other text edits the source.
            match p.doc.link_at(ui.ctx(), rel) {
                Some(url) => {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                    resp.clone().on_hover_text_at_pointer(url);
                }
                None => ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Default),
            }
        }
        if resp.hovered() && !vision_pointer && hover_doc.is_none() {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                if pointer.x >= text_origin.x {
                    let hp = pos_from_pointer(state, pointer);
                    let line_len = state.doc.line_text(hp.line as usize).chars().count() as u32;
                    if hp.column < line_len {
                        view.hovered_offset = Some(state.doc.position_to_offset(hp));
                    }
                }
            }
        }
        if cfg.settings.preferences.diagnostic_tooltips && resp.hovered() && !vision_pointer && hover_doc.is_none() && !diagnostics.is_empty() {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                if pointer.x >= text_origin.x {
                    let off = state.doc.position_to_offset(pos_from_pointer(state, pointer));
                    let hovered: Vec<String> = diagnostics.iter().filter(|d| d.range.contains_inclusive(off) && (d.range.len() > TextSize::new(0) || d.range.start() == off)).map(|d| d.message.clone()).collect();
                    if !hovered.is_empty() {
                        resp.clone().on_hover_ui_at_pointer(|ui| {
                            for m in hovered {
                                ui.label(m);
                            }
                        });
                    }
                }
            }
        }

        let mods = ui.input(|i| i.modifiers);
        // Right-click: move the caret there unless the click lands in the selection
        // (or on a rendered docstring, which keeps showing rendered under the menu).
        if resp.secondary_clicked() && !vision_pointer {
            if let Some(pointer) = resp.interact_pointer_pos() {
                if pointer.x >= text_origin.x - 2.0 && doc_hit(pointer).is_none() {
                    let off = state.doc.position_to_offset(pos_from_pointer(state, pointer));
                    let inside = state.carets.iter().any(|c| c.has_selection() && c.range().contains_inclusive(off));
                    if !inside {
                        state.set_carets(vec![Caret::at(off)]);
                    }
                }
            }
        }
        let is_python = state.language == super::Language::Python;
        let is_json = state.language == super::Language::Json;
        if !vision_pointer {
            let menu_carets: Vec<_> = (0..state.carets.len()).map(|index| {
                let caret = state.virtual_caret(index);
                (caret.anchor.line, caret.anchor.column, caret.head.line, caret.head.column)
            }).collect();
            let run_owner = ui.data(|data| data.get_temp::<crate::run_context::ProjectOwner>(crate::run_context::owner_key(view.id))).unwrap_or_default();
            let menu_authority = egui::Id::new((&run_owner, state.doc.identity(), state.doc.version(),
                &state.path, state.language, state.column_mode, menu_carets));
            crate::a11y::bound_keyboard_context_menu(&resp, cfg.keyboard, menu_authority).show(|ui| {
            let has_cells = is_python && state.doc.rope().lines().any(|l| l.chars().next() == Some('#') && super::console_code::is_cell_marker(&l.to_string()));
            // Translates the submenu titles (action texts translate themselves).
            let ui = &mut crate::i18n::LocalizedUi::new(ui);
            let source_configuration = state.path.as_ref().filter(|path| is_python && !path.starts_with("<typeshed>") && path.extension().is_some_and(|extension| extension == "py" || extension == "pyw"))
                .map(|path| crate::run_context::context_config(&run_owner.root, path, &state.doc.text(), state.doc.line_of(state.primary().head())));
            let mut run = |ui: &mut egui::Ui, a: Action| {
                let context_action = match a {
                    Action::RunContext => Some(crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Run)),
                    Action::DebugContext => Some(crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Debug)),
                    Action::CoverageContext => Some(crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Coverage)),
                    Action::ProfileContext => Some(crate::run_context::ContextAction::Launch(crate::run_context::RunMode::Profile)),
                    Action::ModifyContextConfiguration => Some(crate::run_context::ContextAction::Modify),
                    Action::CreateContextConfiguration => Some(crate::run_context::ContextAction::Create),
                    _ => None,
                };
                let configuration = context_action.and(source_configuration.clone());
                let label = match (&configuration, context_action) {
                    (Some(configuration), Some(crate::run_context::ContextAction::Launch(mode))) => format!("{} '{}'", mode.label(), configuration.name),
                    _ => a.text().to_string(),
                };
                if ui.add(egui::Button::new(label).shortcut_text(cfg.keymap.shortcut_text(a))).clicked() {
                    if let (Some(configuration), Some(action), Some(path)) = (configuration, context_action, state.path.clone()) {
                        events.push(ViewEvent::ContextRun(Box::new(crate::run_context::EditorRequest { owner:run_owner.clone(), document:state.doc.identity(), version:state.doc.version(), path, configuration, action })));
                    } else if is_editor_action(a) && handle_editor_action(a, state, &view_opts) {
                        events.push(ViewEvent::Performed(a));
                    } else {
                        events.push(ViewEvent::Action(a));
                    }
                    ui.close();
                }
            };
            if crate::custom_menus::render(ui,"editor.popup",|ui,a|run(ui,a)){return;}
            run(ui, Action::ShowIntentionActions);
            ui.separator();
            for a in [Action::Cut, Action::Copy, Action::CopyAsRichText, Action::Paste, Action::PasteFromHistory, Action::CopyPath] {
                run(ui, a);
            }
            ui.separator();
            run(ui, Action::ToggleColumnSelectionMode);
            run(ui, Action::FindUsages);
            ui.menu_button("Refactor", |ui| {
                for a in [Action::RefactorThis, Action::Rename, Action::ChangeSignature, Action::ExtractVariable, Action::ExtractMethod, Action::Inline] {
                    run(ui, a);
                }
            });
            ui.menu_button("Folding", |ui| {
                for a in [Action::ExpandRegion, Action::CollapseRegion, Action::ExpandAll, Action::CollapseAll] {
                    run(ui, a);
                }
            });
            ui.menu_button("Go To", |ui| {
                for a in [Action::GoToDeclaration, Action::GoToSourceOrStub, Action::GotoImplementation, Action::GotoSuperMethod, Action::GoToTypeDeclaration] {
                    run(ui, a);
                }
            });
            run(ui, Action::Generate);
            ui.menu_button("AI Actions", |ui| {
                for a in [Action::AiInlineCompletion, Action::AiExplainCode, Action::AiSuggestRefactoring, Action::AiWriteTests, Action::AiGenerateDocstring, Action::AiFixError] {
                    run(ui, a);
                }
            });
            ui.separator();
            run(ui, Action::RunContext);
            run(ui, Action::DebugContext);
            if is_python {
                for action in [Action::CoverageContext, Action::ProfileContext, Action::ModifyContextConfiguration, Action::CreateContextConfiguration] { run(ui, action); }
            }
            if !is_python {run(ui,Action::DataFileTools);if is_json{run(ui,Action::CopyJsonPath);}}
            if is_python {
                run(ui, Action::ExecuteInConsole);
                run(ui, Action::RunFileInConsole);
            }
            if has_cells {
                for action in [Action::RunCell,Action::RunCellAndAdvance,Action::RunCellsAbove,Action::DebugCell]{run(ui,action);}
            }
            ui.separator();
            run(ui, Action::LocalHistory);
            run(ui, Action::Annotate);
            run(ui, Action::CompareWithClipboard);
            crate::external_tools::context_submenu(ui, state.path.as_deref());
        }); }
        // Drag-and-drop: a drag that starts inside the (single) selection moves it (Ctrl copies).
        if cfg.settings.preferences.drag_and_drop && resp.drag_started_by(egui::PointerButton::Primary) && !view.sticky_consumed_click && !stripe_consumed_pointer && !mods.shift && !mods.alt && !state.column_mode && state.carets.len() == 1 && state.primary().has_selection() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                if origin.x >= text_origin.x - 2.0 {
                    let off = state.doc.position_to_offset(pos_from_pointer(state, origin));
                    let r = state.primary().range();
                    if off > r.start() && off < r.end() {
                        view.dnd = Some(r);
                        view.dnd_target = None;
                    }
                }
            }
        }
        // A drag selection starts where the button went down, not where egui
        // recognised the drag (a few pixels later).
        let press_pointer = if resp.drag_started_by(egui::PointerButton::Primary) { ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) } else { resp.interact_pointer_pos() };
        // A link in rendered documentation opens in the browser.
        let doc_link_click = if resp.clicked_by(egui::PointerButton::Primary) && !view.sticky_consumed_click && !stripe_consumed_pointer { resp.interact_pointer_pos().and_then(doc_hit).and_then(|(p, rel)| p.doc.link_at(ui.ctx(), rel)).map(str::to_string) } else { None };
        if let Some(url)=doc_link_click.as_ref().filter(|u|u.starts_with("python:")) {
            let offset=resp.interact_pointer_pos().and_then(doc_hit).map(|(placement,_)|placement.region.start).unwrap_or(state.primary().head());
            events.push(ViewEvent::DocumentationLink{target:url.clone(),offset,document:state.doc.identity(),version:state.doc.version()});
        }
        if let Some(url) = doc_link_click.as_ref().filter(|u| is_openable_doc_link(u)) {
            // Opened by the app (Settings › Tools › Web Browsers, else the system opener).
            events.push(ViewEvent::OpenUrl(url.clone()));
        }
        // The gutter next to a rendered docstring only has its toggle.
        let doc_gutter_click = press_pointer.is_some_and(|p| p.x < text_origin.x - 6.0 && line_at_pointer(p).is_some_and(|l| doc_at_line(l).is_some()));
        if doc_link_click.is_none() && !doc_gutter_click && !view.sticky_consumed_click && !stripe_consumed_pointer && view.dnd.is_none() && (resp.clicked_by(egui::PointerButton::Primary) || resp.drag_started_by(egui::PointerButton::Primary) || resp.clicked_by(egui::PointerButton::Middle) || resp.clicked_by(egui::PointerButton::Secondary) || resp.clicked_by(egui::PointerButton::Extra1) || resp.clicked_by(egui::PointerButton::Extra2)) {
            if let Some(pointer) = press_pointer {
                let pos = pos_from_pointer(state, pointer);
                // A click on a rendered docstring edits its source, at the clicked part's line.
                let doc = doc_hit(pointer).map(|(p, _)| &p.region);
                let off = match doc {
                    Some(region) => super::doc_render::source_offset(state, region, pos.line as usize),
                    None => state.doc.position_to_offset(pos),
                };
                view.blink.reset(time);
                let mut marker_click = false;
                if pointer.x < text_origin.x - 6.0 && resp.clicked() {
                    // Gutter click (line numbers and the breakpoint strip) toggles a
                    // breakpoint; the fold-marker column is separate.
                    // A VCS change marker (right edge) opens the change popup.
                    let line = pos.line as usize;
                    if hits_change_bar(pointer.x, gutter_rect.right(), false) && rustpy_vcs::change_at_line(cfg.line_changes, line, line_count).is_some() {
                        events.push(ViewEvent::ChangeMarkerClick(line));
                        marker_click = true;
                    } else if pointer.x < gutter_rect.right() - 14.0 {
                        events.push(ViewEvent::GutterClick(line));
                    }
                    view.click_count = 0;
                } else if let Some(action) = (doc.is_none() && cfg.keyboard).then(|| {
                    let button = if resp.clicked_by(egui::PointerButton::Secondary) { "Secondary" }
                        else if resp.clicked_by(egui::PointerButton::Extra1) { "Extra1" }
                        else if resp.clicked_by(egui::PointerButton::Extra2) { "Extra2" }
                        else if resp.clicked_by(egui::PointerButton::Middle) { "Middle" }
                        else if resp.double_clicked() { "PrimaryDouble" } else { "Primary" };
                    let candidates = cfg.keymap.mouse_candidates(button,mods);
                    if candidates.len() > 1 {
                        state.set_carets(vec![Caret::at(off)]);
                        events.push(ViewEvent::ShortcutChoices(candidates));
                        Some(None)
                    } else { candidates.first().copied().map(Some) }
                }).flatten() {
                    match action {
                        None => view.click_count = 0,
                        Some(Action::GoToDeclaration) => view.ctrl_click = Some(off),
                        Some(action) => {
                            state.set_carets(vec![Caret::at(off)]);
                            if handle_editor_action(action,state,cfg.opts) { events.push(ViewEvent::Performed(action)); }
                            else { events.push(ViewEvent::Action(action)); }
                        }
                    }
                    view.click_count = 0;
                } else if resp.clicked() {
                    if time - view.last_click_time < 0.4 && pointer.distance(view.last_click_pos) < 6.0 {
                        view.click_count += 1;
                    } else {
                        view.click_count = 1;
                    }
                    view.last_click_time = time;
                    view.last_click_pos = pointer;
                } else {
                    view.click_count = 1;
                }
                match view.click_count {
                    2 => {
                        state.set_carets(vec![Caret::at(off)]);
                        let mut select=cfg.opts.clone();select.camel_humps &= cfg.settings.preferences.double_click_camel_humps;state.select_word_with_opts(&select);
                    }
                    n if n >= 3 => {
                        let lr = state.doc.line_range_with_newline(pos.line as usize);
                        state.set_carets(vec![Caret::new(lr.start(), lr.end())]);
                    }
                    // The change popup leaves the caret and selection alone.
                    _ if marker_click => {}
                    _ => {
                        use super::state::virtual_space::VirtualCaret;
                        if mods.shift || state.sticky_selection {
                            let mut positions: Vec<_> = (0..state.carets.len()).map(|i| state.virtual_caret(i)).collect();
                            positions[0].head = pos;
                            state.set_virtual_carets(positions);
                        } else if mods.alt {
                            let mut positions: Vec<_> = (0..state.carets.len()).map(|i| state.virtual_caret(i)).collect();
                            positions.push(VirtualCaret { anchor: pos, head: pos });
                            state.set_virtual_carets(positions);
                        } else {
                            state.set_virtual_carets(vec![VirtualCaret { anchor: pos, head: pos }]);
                        }
                    }
                }
                view.drag_anchor = Some(if mods.shift { state.primary().sel.anchor } else { off });
                view.drag_virtual_anchor = Some(if mods.shift || state.sticky_selection { state.virtual_caret(0).anchor } else { pos });
                state.last_op = super::state::LastOp::Other;
                // The docstring's source replaces the rendered block: keep the caret in view.
                state.scroll_to_caret = doc.is_some();
            }
        }
        if let Some(src) = view.dnd {
            if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                view.dnd_target = Some(state.doc.position_to_offset(pos_from_pointer(state, pointer)));
                ui.ctx().set_cursor_icon(if mods.ctrl || mods.command { egui::CursorIcon::Copy } else { egui::CursorIcon::Grabbing });
            }
            if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                if let Some(t) = view.dnd_target.take() {
                    if t >= src.start() && t <= src.end() {
                        state.set_carets(vec![Caret::at(t)]);
                    } else {
                        let text = state.doc.slice(src);
                        let copy = mods.ctrl || mods.command;
                        let mut edits = vec![(TextRange::empty(t), text.clone())];
                        if !copy {
                            edits.push((src, String::new()));
                        }
                        if state.apply_text_edits(edits, if copy { "Copy text" } else { "Move text" }) {
                            let len = TextSize::of(text.as_str());
                            let start = if !copy && t > src.end() { t - src.len() } else { t };
                            state.set_carets(vec![Caret::new(start, start + len)]);
                        }
                    }
                }
                view.dnd = None;
                view.drag_anchor = None;
            view.drag_virtual_anchor = None;
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) && !stripe_consumed_pointer && view.click_count <= 1 && view.dnd.is_none() {
            if let (Some(anchor), Some(pointer)) = (view.drag_anchor, ui.input(|i| i.pointer.interact_pos())) {
                let pos = pos_from_pointer(state, pointer);
                if state.column_mode || mods.alt {
                    // Block selection: one caret per line between the anchor and the pointer.
                    let a = view.drag_virtual_anchor.unwrap_or_else(|| state.doc.offset_to_position(anchor));
                    state.set_block_selection(a, pos);
                } else {
                    let mut positions: Vec<_> = (0..state.carets.len()).map(|i| state.virtual_caret(i)).collect();
                    positions[0] = super::state::virtual_space::VirtualCaret { anchor: view.drag_virtual_anchor.unwrap_or_else(|| state.doc.offset_to_position(anchor)), head: pos };
                    state.set_virtual_carets(positions);
                }
                state.scroll_to_caret = true;
            }
        }
        if resp.drag_stopped() {
            view.drag_anchor = None;
            view.drag_virtual_anchor = None;
        }

        let mut consumed: Vec<(egui::Key, egui::Modifiers)> = Vec::new();
        if focused {
            let zoom = ui.input(|i| i.zoom_delta()) - 1.0;
            let wheel_actions = &keymap_wheel_actions;
            if wheel_actions.len() > 1 { events.push(ViewEvent::ShortcutChoices(wheel_actions.to_vec())); }
            else if let Some(action) = wheel_actions.first().copied() {
                if handle_editor_action(action,state,cfg.opts) { events.push(ViewEvent::Performed(action)); }
                else { events.push(ViewEvent::Action(action)); }
            }
            if wheel_actions.is_empty() && zoom != 0.0 && cfg.settings.mouse_wheel_zoom {
                events.push(ViewEvent::Zoom(zoom.signum()));
            }
            let evs = ui.input(|i| i.events.clone());
            let ime_commits=evs.iter().filter_map(|event|if let egui::Event::Ime(egui::ImeEvent::Commit(text))=event{Some(text.clone())}else{None}).collect::<Vec<_>>();
            for ev in evs {
                match ev {
                    egui::Event::Text(t) => {
                        if !mods.ctrl && !mods.alt&&!view.ime.composing()&&!ime_commits.contains(&t) {
                            state.insert_text(&t, cfg.opts);
                            view.typed.push_str(&t);
                            view.blink.reset(time);
                        }
                    }
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. })=>{view.ime.enabled();if text.is_empty(){view.ime.commit(state,"",cfg.opts);}else{view.ime.preedit(state,text);}ui.ctx().request_repaint();},
                    egui::Event::Ime(egui::ImeEvent::Commit(text))=>{if view.ime.commit(state,&text,cfg.opts){view.typed.push_str(&text);view.blink.reset(time);}},
                    egui::Event::Key{key,pressed:true,modifiers,..} if (view.ime.composing()||!ime_commits.is_empty())&&matches!(key,egui::Key::Enter|egui::Key::Backspace|egui::Key::Delete|egui::Key::Escape|egui::Key::ArrowLeft|egui::Key::ArrowRight|egui::Key::ArrowUp|egui::Key::ArrowDown)=>{if key==egui::Key::Escape{view.ime.cancel();}consumed.push((key,modifiers));},
                    // egui-winit reports every Ctrl+…+V as a paste: let the keymap have
                    // Ctrl+Shift+V (Paste from History) and Ctrl+Alt+Shift+V (Paste as Plain Text).
                    egui::Event::Paste(t) if mods.command && (mods.shift || mods.alt) => match cfg.keymap.lookup(crate::actions::KeyChord::new(egui::Key::V, mods)) {
                        Some(action) => events.push(ViewEvent::Action(action)),
                        None if t.len() > LARGE_PASTE_BYTES => events.push(ViewEvent::LargePaste(t)),
                        None => state.paste(&t, cfg.opts),
                    },
                    egui::Event::Paste(t) if t.len() > LARGE_PASTE_BYTES => events.push(ViewEvent::LargePaste(t)),
                    egui::Event::Paste(t) => {
                        if super::string_paste::prepare(state, &t).is_some() && t.chars().any(|c| c == '\\' || c == '\'' || c == '"' || c.is_control()) {
                            events.push(ViewEvent::StringPaste(t));
                            break;
                        }
                        state.paste_payload(&t, view.clipboard_payload.as_ref(), cfg.opts);
                    },
                    egui::Event::Copy => {
                        let payload = state.copy_payload();
                        let t = payload.text.clone();
                        ui.ctx().copy_text(t.clone());
                        events.push(ViewEvent::Copied(t));
                        view.clipboard_payload = Some(payload.clone());
                        events.push(ViewEvent::ClipboardPayload(payload));
                    }
                    egui::Event::Cut => {
                        let payload = state.copy_payload();
                        let t = state.cut();
                        ui.ctx().copy_text(t.clone());
                        events.push(ViewEvent::Copied(t));
                        view.clipboard_payload = Some(payload.clone());
                        events.push(ViewEvent::ClipboardPayload(payload));
                    }
                    egui::Event::Key { key: egui::Key::ControlLeft | egui::Key::ControlRight, pressed, repeat: false, modifiers, .. } => view.track_ctrl(pressed, modifiers, time),
                    egui::Event::Key { key, pressed: true, modifiers, .. } if view.clones_caret(key, modifiers) => {
                        let action = if key == egui::Key::ArrowUp { Action::AddCaretAbove } else { Action::AddCaretBelow };
                        view.blink.reset(time);
                        handle_editor_action(action, state, &view_opts);
                        events.push(ViewEvent::Performed(action));
                        // Ctrl+Up/Down must not also reach the IDE keymap (Scroll Up/Down).
                        consumed.push((key, modifiers));
                    }
                    egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                        if let Some(action) = cfg.keymap.lookup_key(key, physical_key, modifiers) {
                            view.blink.reset(time);
                            if action == Action::ScrollToCenter {
                                view.request_center(state);
                                ui.ctx().request_repaint();
                                events.push(ViewEvent::Performed(action));
                                consumed.push((key, modifiers));
                            } else if visual_horizontal(action,state,&layouts,tab_size) || handle_editor_action(action, state, &view_opts) {
                                events.push(ViewEvent::Performed(action));
                                // Ctrl chords the editor handled must not reach egui's own
                                // shortcuts (e.g. Ctrl+Minus/Equals GUI zoom) or other widgets.
                                if modifiers.command || modifiers.ctrl {
                                    consumed.push((key, modifiers));
                                }
                            } else {
                                events.push(ViewEvent::Action(action));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if !consumed.is_empty() {
            ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if consumed.contains(&(*key, *modifiers)))));
        }
        // Ctrl+Up/Down: scroll by lines, keeping the caret inside the viewport.
        if state.pending_scroll_lines != 0 {
            let lines = std::mem::take(&mut state.pending_scroll_lines);
            let max_top = (content_h - viewport.height()).max(0.0);
            let new_top = (viewport.min.y + lines as f32 * line_h).clamp(0.0, max_top);
            ui.scroll_with_delta(Vec2::new(0.0, viewport.min.y - new_top));
            if !rows.is_empty() {
                let first_visible = (((new_top - 2.0) / line_h).ceil().max(0.0) as usize).min(rows.len() - 1);
                let last_visible = ((((new_top + viewport.height() - 2.0) / line_h).floor() as usize).saturating_sub(1)).clamp(first_visible, rows.len() - 1);
                let caret = state.primary();
                let pos = state.doc.offset_to_position(caret.head());
                // With soft wraps the caret's own visual row counts, and the goal
                // is its column within the row.
                let (sub_row, x) = state.visual_row_col(caret.head()).unwrap_or((0, pos.column as usize));
                let goal = caret.goal_column.unwrap_or(x as u32);
                let row = line_row(pos.line as usize).unwrap_or(0) + sub_row;
                let target_row = if row < first_visible { Some(first_visible) } else if row > last_visible { Some(last_visible) } else { None };
                if let Some(r) = target_row {
                    let r = if vision_rows.is_header(r) { line_row(rows[r]).unwrap_or(r) } else { r };
                    let off = state.offset_at_visual_row(rows[r], row_sub[r] as usize, goal as usize).unwrap_or_else(|| state.doc.position_to_offset(Position::new(rows[r] as u32, goal)));
                    state.set_carets(vec![Caret { sel: rustpy_text::Selection::caret(off), goal_column: Some(goal) }]);
                }
            }
            state.scroll_to_caret = false;
        }
        super::accessibility::editor(&resp, state, focused);
    });
    // The line to keep in place next frame (unless this frame edited the text):
    // the caret's while it is on screen, else the first line shown.
    let offset = output.state.offset.y;
    view.viewport_height = output.inner_rect.height();
    // Derive centering from this frame's actual viewport (including resize and
    // scrollbar space), then apply it before visible layouts in the next frame.
    // The source-to-row map also works when the caret is currently offscreen.
    if view.center_request.is_pending() {
        let caret = state.doc.offset_to_position(state.primary().head());
        if let Some(row) = line_to_row.get(caret.line as usize).copied().flatten() {
            let sub = state
                .visual_row_col(state.primary().head())
                .map_or(0, |(row, _)| row);
            view.center_target = view.center_request.take_offset(
                (row + sub) as f32 * line_h + 2.0,
                line_h,
                view.viewport_height,
                output.content_size.y,
            );
            ui.ctx().request_repaint();
        }
    }

    if let Some(target) = view.center_target {
        let expected = target.clamp(0.0, (output.content_size.y - view.viewport_height).max(0.0));
        if (offset - expected).abs() <= 0.5 {
            view.center_target = None;
        } else {
            ui.ctx().request_repaint();
        }
    }
    if view.center_request.is_pending() {
        ui.ctx().request_repaint();
    }

    if let Some(target) = restore_offset {
        // egui may initially clamp against the previous frame's content size
        // (a placeholder, resize, or late doc layout). Retry using current rows.
        let expected = target.clamp(
            0.0,
            (output.content_size.y - output.inner_rect.height()).max(0.0),
        );
        if (offset - expected).abs() <= 0.5 {
            view.pending_scroll_line = None;
            view.session_restore_guard = None;
        } else {
            ui.ctx().request_repaint();
        }
    }
    let top_row =
        (((offset - 2.0) / line_h).floor().max(0.0) as usize).min(rows.len().saturating_sub(1));
    view.top_line = rows
        .get(top_row)
        .copied()
        .unwrap_or(0)
        .min(u32::MAX as usize) as u32;
    let bottom_row = ((offset + output.inner_rect.height() - 2.0) / line_h)
        .floor()
        .max(0.0) as usize;
    let caret_line = state.doc.line_of(state.primary().head());
    let caret_shown = line_to_row
        .get(caret_line)
        .copied()
        .flatten()
        .is_some_and(|r| top_row <= r && r < bottom_row);
    let anchor_line = if caret_shown {
        Some(caret_line)
    } else {
        rows.get(top_row).copied()
    };
    view.doc_anchor = anchor_line
        .filter(|_| state.doc.version() == version_at_start)
        .and_then(|line| {
            Some(DocAnchor {
                version: version_at_start,
                signature: doc_signature,
                line_h,
                line,
                row: line_to_row.get(line).copied().flatten()?,
                offset,
            })
        });
    events
}

/// Map a logical source line to its first visual row. Hidden lines restore to
/// their visible preceding header; out-of-range session lines clamp to the file.
fn session_scroll_row(line: u32, line_to_row: &[Option<usize>], rows: &[usize]) -> usize {
    let line = (line as usize).min(line_to_row.len().saturating_sub(1));
    if let Some(row) = line_to_row.get(line).copied().flatten() {
        return row;
    }
    let preceding = rows
        .partition_point(|&source_line| source_line <= line)
        .saturating_sub(1);
    rows.get(preceding)
        .and_then(|&source_line| line_to_row.get(source_line))
        .copied()
        .flatten()
        .unwrap_or(0)
}

/// Whether an action is handled inside the editor (as opposed to the app).
pub fn is_editor_action(action: Action) -> bool {
    use Action::*;
    matches!(
        action,
        ToggleFold
            | ExpandRegion
            | CollapseRegion
            | ExpandAll
            | CollapseAll
            | NextMethod
            | PreviousMethod
            | MatchingBrace
            | CaretLeft
            | CaretRight
            | CaretUp
            | CaretDown
            | SelectLeft
            | SelectRight
            | SelectUp
            | SelectDown
            | CaretWordLeft
            | CaretWordRight
            | SelectWordLeft
            | SelectWordRight
            | CaretLineStart
            | CaretLineEnd
            | SelectLineStart
            | SelectLineEnd
            | CaretPageUp
            | CaretPageDown
            | SelectPageUp
            | SelectPageDown
            | CaretTextStart
            | CaretTextEnd
            | SelectTextStart
            | SelectTextEnd
            | Backspace
            | Delete
            | Enter
            | StartNewLine
            | StartNewLineBefore
            | IndentSelection
            | UnindentSelection
            | Escape
            | SelectAll
            | ExtendSelection
            | ShrinkSelection
            | AddSelectionForNextOccurrence
            | UnselectOccurrence
            | SelectAllOccurrences
            | AddCaretAbove
            | AddCaretBelow
            | DuplicateLine
            | DeleteLine
            | JoinLines
            | MoveLineUp
            | MoveStatementUp
            | MoveLineDown
            | MoveStatementDown
            | CommentLine
            | ToggleCase
            | DeleteToWordStart
            | DeleteToWordEnd
            | DeleteToLineStart
            | DeleteToLineEnd
            | ToggleInsertState
            | ToggleStickySelection
            | ToggleColumnSelectionMode
            | Undo
            | Redo
            | ScrollUp
            | ScrollDown
            | CodeBlockStart
            | CodeBlockEnd
            | CommentBlock
            | MoveElementLeft
            | MoveElementRight
            | SortLines
            | ReverseLines
            | RemoveDuplicateLines
            | TrimTrailingWhitespace
            | ConvertIndentsToSpaces
            | ConvertIndentsToTabs
            | NextDocstringSection
            | PreviousDocstringSection
            | ToggleRenderedDoc
            | ToggleRenderedDocAll
            | ToggleReaderMode
    )
}

/// Horizontal arrow keys follow the visible grapheme order; selections remain UTF-8 ranges.
fn visual_horizontal(
    action: Action,
    state: &mut EditorState,
    layouts: &[LineLayout],
    tab_size: usize,
) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    let (right, select) = match action {
        Action::CaretLeft => (false, false),
        Action::CaretRight => (true, false),
        Action::SelectLeft => (false, true),
        Action::SelectRight => (true, true),
        _ => return false,
    };
    if !state.carets.iter().any(|c| {
        layouts
            .iter()
            .any(|l| l.line == state.doc.line_of(c.head()) && l.visual.is_some())
    }) {
        return false;
    }
    let mut carets = Vec::new();
    for caret in &state.carets {
        let line = state.doc.line_of(caret.head());
        let Some(ll) = layouts
            .iter()
            .find(|l| l.line == line && l.visual.is_some())
        else {
            let next = if right {
                super::unicode_edit::next(state, caret.head())
            } else {
                super::unicode_edit::previous(state, caret.head())
            };
            carets.push(super::state::Caret::new(
                if select || state.sticky_selection {
                    caret.sel.anchor
                } else {
                    next
                },
                next,
            ));
            continue;
        };
        let text = state.doc.line_text(line);
        let mut points = text
            .grapheme_indices(true)
            .map(|(byte, _)| {
                let col = text[..byte].chars().count();
                (
                    state.doc.line_range(line).start() + TextSize::new(byte as u32),
                    pos_of_col(ll, &text, col, tab_size),
                )
            })
            .collect::<Vec<_>>();
        points.push((
            state.doc.line_range(line).start() + TextSize::new(text.len() as u32),
            pos_of_col(ll, &text, text.chars().count(), tab_size),
        ));
        points.sort_by(|a, b| a.1.y.total_cmp(&b.1.y).then(a.1.x.total_cmp(&b.1.x)));
        let current = points.iter().position(|p| p.0 == caret.head()).unwrap_or(0);
        let next = if !select && !state.sticky_selection && caret.has_selection() {
            let anchor = points
                .iter()
                .position(|p| p.0 == caret.sel.anchor)
                .unwrap_or(current);
            points[if right {
                anchor.max(current)
            } else {
                anchor.min(current)
            }]
            .0
        } else {
            let index = if right {
                current.saturating_add(1).min(points.len() - 1)
            } else {
                current.saturating_sub(1)
            };
            if index == current {
                if right {
                    super::unicode_edit::next(state, caret.head())
                } else {
                    super::unicode_edit::previous(state, caret.head())
                }
            } else {
                points[index].0
            }
        };
        carets.push(super::state::Caret::new(
            if select || state.sticky_selection {
                caret.sel.anchor
            } else {
                next
            },
            next,
        ));
    }
    state.set_carets(carets);
    state.scroll_to_caret = true;
    true
}

/// Executes editor-local actions. Returns false if the action is not editor-local.
pub fn handle_editor_action(action: Action, state: &mut EditorState, opts: &EditOptions) -> bool {
    use Action::*;
    match action {
        CaretLeft => state.move_left(false),
        CaretRight => state.move_right(false),
        CaretUp => state.move_vertical(-1, false),
        CaretDown => state.move_vertical(1, false),
        // Column selection mode: Shift+arrows grow a block, not a stream selection.
        SelectLeft if state.column_mode => state.extend_block_selection(0, -1),
        SelectRight if state.column_mode => state.extend_block_selection(0, 1),
        SelectUp if state.column_mode => state.extend_block_selection(-1, 0),
        SelectDown if state.column_mode => state.extend_block_selection(1, 0),
        SelectLeft => state.move_left(true),
        SelectRight => state.move_right(true),
        SelectUp => state.move_vertical(-1, true),
        SelectDown => state.move_vertical(1, true),
        CaretWordLeft => state.move_word_left(false, opts),
        CaretWordRight => state.move_word_right(false, opts),
        SelectWordLeft => state.move_word_left(true, opts),
        SelectWordRight => state.move_word_right(true, opts),
        CaretLineStart => state.move_line_start(false, opts),
        CaretLineEnd => state.move_line_end_with_opts(false, opts),
        SelectLineStart => state.move_line_start(true, opts),
        SelectLineEnd => state.move_line_end_with_opts(true, opts),
        CaretPageUp => state.page(false, false, opts),
        CaretPageDown => state.page(true, false, opts),
        SelectPageUp => state.page(false, true, opts),
        SelectPageDown => state.page(true, true, opts),
        CaretTextStart => state.move_doc_start(false),
        CaretTextEnd => state.move_doc_end(false),
        SelectTextStart => state.move_doc_start(true),
        SelectTextEnd => state.move_doc_end(true),
        Backspace => state.backspace(opts),
        Delete => state.delete_forward(),
        Enter => state.newline(opts),
        StartNewLine => state.start_new_line(opts),
        StartNewLineBefore => state.start_new_line_before(opts),
        IndentSelection => state.tab(opts),
        UnindentSelection => state.unindent_lines(opts),
        Escape => {
            if !state.cancel_sticky_selection() {
                state.collapse_to_primary();
            }
        }
        SelectAll => state.select_all(),
        ExtendSelection => state.extend_selection_with_opts(opts),
        ShrinkSelection => state.shrink_selection(),
        AddSelectionForNextOccurrence => state.add_selection_for_next_occurrence(),
        UnselectOccurrence => state.unselect_occurrence(),
        SelectAllOccurrences => state.select_all_occurrences(),
        AddCaretAbove => state.clone_caret(false),
        AddCaretBelow => state.clone_caret(true),
        DuplicateLine => state.duplicate(),
        DeleteLine => state.delete_lines(),
        JoinLines => state.join_lines_with_options(opts),
        MoveLineUp => state.move_lines(false),
        MoveLineDown => state.move_lines(true),
        MoveStatementUp => state.move_statement(false),
        MoveStatementDown => state.move_statement(true),
        MoveElementLeft => state.move_element(false),
        MoveElementRight => state.move_element(true),
        CommentLine => state.toggle_line_comment_with_style(
            opts.paste_format_options.python.first_column_comments,
            opts.paste_format_options.python.space_after_comment,
        ),
        CommentBlock => state.toggle_block_comment(),
        SortLines => state.sort_lines(),
        ReverseLines => state.reverse_lines(),
        RemoveDuplicateLines => state.remove_duplicate_lines(),
        TrimTrailingWhitespace => state.trim_trailing_whitespace(),
        ConvertIndentsToSpaces => state.convert_indents(false, opts),
        ConvertIndentsToTabs => state.convert_indents(true, opts),
        ScrollUp => state.pending_scroll_lines -= 1,
        ScrollDown => state.pending_scroll_lines += 1,
        CodeBlockStart => state.move_to_code_block(false),
        CodeBlockEnd => state.move_to_code_block(true),
        ToggleCase => state.toggle_case(),
        DeleteToWordStart => state.delete_to_word_start(opts),
        DeleteToWordEnd => state.delete_to_word_end(opts),
        DeleteToLineStart => state.delete_to_line_start(),
        DeleteToLineEnd => state.delete_to_line_end(),
        ToggleInsertState => state.overwrite_mode = !state.overwrite_mode,
        ToggleStickySelection => state.toggle_sticky_selection(),
        ToggleColumnSelectionMode => state.column_mode = !state.column_mode,
        Undo => {
            if opts.confirm_refactoring_undo && state.doc.undo_is_refactoring() {
                state.pending_refactoring_undo =
                    Some((state.doc.current_history(), state.doc.version()));
            } else {
                state.undo();
            }
        }
        Redo => {
            state.redo();
        }
        ToggleFold => state.toggle_fold_at_caret(),
        ExpandRegion => state.expand_region_at_caret(),
        CollapseRegion => state.collapse_region_at_caret(),
        ExpandAll => state.expand_all_folds(),
        CollapseAll => state.collapse_all_folds(),
        NextDocstringSection => super::doc_render::navigate_section(state, true),
        PreviousDocstringSection => super::doc_render::navigate_section(state, false),
        ToggleRenderedDoc => {
            super::doc_render::toggle_at_caret(state);
        }
        ToggleRenderedDocAll => super::doc_render::toggle_all(state),
        ToggleReaderMode => super::doc_render::toggle_reader_mode(state),
        NextMethod => state.move_to_definition(true),
        PreviousMethod => state.move_to_definition(false),
        MatchingBrace => state.move_to_matching_brace(),
        _ => return false,
    }
    true
}

/// Padding inside a rendered docstring's box.
const DOC_PAD_X: f32 = 12.0;
const DOC_PAD_Y: f32 = 6.0;

/// A rendered docstring placed in the editor rows (all on its first line).
struct DocPlacement {
    region: super::doc_render::DocRegion,
    doc: Arc<super::doc_render::RenderedDoc>,
    /// Rows the block takes.
    rows: usize,
    /// Left edge of the box relative to the text origin.
    left: f32,
    /// Width of the box.
    width: f32,
}

impl DocPlacement {
    /// The box of a block whose first row starts at `top`.
    fn rect(&self, text_x: f32, top: f32, line_h: f32) -> Rect {
        Rect::from_min_size(
            Pos2::new(text_x + self.left, top + 2.0),
            Vec2::new(self.width, self.rows as f32 * line_h - 4.0),
        )
    }

    /// Top-left of the laid-out content.
    fn content_origin(&self, text_x: f32, top: f32, line_h: f32) -> Pos2 {
        let free = self.rows as f32 * line_h - self.doc.height;
        Pos2::new(
            text_x + self.left + DOC_PAD_X,
            top + (free / 2.0).max(DOC_PAD_Y).floor(),
        )
    }
}

/// Rendered docstrings within this many lines of the ones shown last frame are laid out right away.
const DOC_NEAR_LINES: usize = 30;
/// Time per frame for laying out the other rendered docstrings.
const DOC_LAYOUT_BUDGET: std::time::Duration = std::time::Duration::from_millis(6);
/// Rendered docstrings are laid out at widths in steps of this many pixels,
/// so that resizing the editor lays them out again only now and then.
const DOC_WIDTH_STEP: f32 = 24.0;

/// Lays out the docstrings to show rendered (see `doc_render::shown_regions`):
/// each box starts at its docstring's indentation and is as wide as the text
/// area allows, up to the right margin. Docstrings near the lines shown last
/// frame are laid out now, the others within a time budget per frame (a big
/// library file has megabytes of them): until then a docstring keeps a layout
/// made for another width, or shows its source.
#[allow(clippy::too_many_arguments)]
fn place_docs(
    ui: &egui::Ui,
    state: &mut EditorState,
    view: &EditorView,
    hidden: &[(usize, usize)],
    style: &super::doc_render::DocStyle,
    text_w: f32,
    cfg: &ViewConfig,
    char_w: f32,
    line_h: f32,
) -> Vec<DocPlacement> {
    let shown = super::doc_render::shown_regions(state, hidden);
    let (near_first, near_last) = match view.visible_lines {
        Some((first, last)) => (first.saturating_sub(DOC_NEAR_LINES), last + DOC_NEAR_LINES),
        // A new editor opens scrolled to its caret.
        None => {
            let line = state.doc.line_of(state.primary().head());
            (line.saturating_sub(100), line + 100)
        }
    };
    let deadline = std::time::Instant::now() + DOC_LAYOUT_BUDGET;
    let mut pending = false;
    let mut used = std::collections::HashSet::with_capacity(shown.len());
    let mut out = Vec::with_capacity(shown.len());
    for region in shown {
        let indent = rustpy_text::util::indent_width(
            &state.doc.line_text(region.first_line),
            cfg.opts.tab_size,
        ) as f32
            * char_w;
        let mut width = text_w - indent;
        if cfg.settings.show_right_margin {
            width = width.min(cfg.settings.right_margin as f32 * char_w - indent);
        }
        let content_w = ((width.max(char_w * 24.0) - 2.0 * DOC_PAD_X) / DOC_WIDTH_STEP)
            .floor()
            .max(1.0)
            * DOC_WIDTH_STEP;
        let near = region.first_line <= near_last && region.last_line >= near_first;
        let (doc, fresh) = super::doc_render::layout_region(
            ui,
            state,
            &region,
            content_w,
            style,
            &mut used,
            near || std::time::Instant::now() < deadline,
        );
        pending |= !fresh;
        let Some(doc) = doc else { continue };
        let rows = ((doc.height + 2.0 * DOC_PAD_Y + 4.0) / line_h)
            .ceil()
            .max(1.0) as usize;
        out.push(DocPlacement {
            region,
            rows,
            left: indent - 6.0,
            width: doc.width + 2.0 * DOC_PAD_X,
            doc,
        });
    }
    super::doc_render::retain_layouts(state, &used);
    if pending {
        ui.ctx().request_repaint();
    }
    out
}

/// Suppress reader comments while retaining source text and hit-test geometry.
fn append_reader_segment(
    job: &mut LayoutJob,
    text: &str,
    start: usize,
    end: usize,
    format: TextFormat,
    hidden_comment: Option<usize>,
    tab_size: usize,
) {
    let split = hidden_comment.unwrap_or(end).clamp(start, end);
    if start < split {
        job.append(
            &expand_source_segment(text, start, split, tab_size),
            0.0,
            format.clone(),
        );
    }
    if split < end {
        let mut hidden = format;
        hidden.color = Color32::TRANSPARENT;
        hidden.underline = Stroke::NONE;
        hidden.strikethrough = Stroke::NONE;
        hidden.background = Color32::TRANSPARENT;
        job.append(
            &expand_source_segment(text, split, end, tab_size),
            0.0,
            hidden,
        );
    }
}

/// The gutter icon toggling a docstring's rendered view: a page with lines,
/// filled while the docstring is rendered.
fn paint_doc_toggle(
    painter: &egui::Painter,
    r: Rect,
    rendered: bool,
    hovered: bool,
    scheme: &ColorScheme,
) {
    let c = if hovered || rendered {
        rgb(scheme.editor.line_number_current)
    } else {
        rgb(scheme.editor.line_number)
    };
    let lines = if rendered {
        rgb(scheme.editor.gutter_background)
    } else {
        c
    };
    if rendered {
        painter.rect_filled(r, 1.5, c);
    } else {
        painter.rect_stroke(r, 1.5, Stroke::new(1.0, c), egui::StrokeKind::Inside);
    }
    for k in 0..3 {
        let y = r.top() + 3.0 + k as f32 * 2.5;
        let right = r.right() - 2.5 - if k == 2 { 2.0 } else { 0.0 };
        painter.line_segment(
            [Pos2::new(r.left() + 2.5, y), Pos2::new(right, y)],
            Stroke::new(1.0, lines),
        );
    }
}

/// Visible rows (`rows[row] = line`, `row_sub[row]` = wrapped sub-row index) and
/// the line → first row mapping.
fn build_rows(
    line_count: usize,
    hidden: &[(usize, usize)],
    rows_per_line: Option<&[u32]>,
) -> (Vec<usize>, Vec<u32>, Vec<Option<usize>>) {
    let mut rows = Vec::with_capacity(line_count);
    let mut subs = Vec::with_capacity(line_count);
    let mut line_to_row = vec![None; line_count];
    let mut h = 0;
    for (line, slot) in line_to_row.iter_mut().enumerate() {
        while h < hidden.len() && hidden[h].1 < line {
            h += 1;
        }
        if h < hidden.len() && hidden[h].0 <= line && line <= hidden[h].1 {
            continue;
        }
        *slot = Some(rows.len());
        let n = rows_per_line
            .and_then(|r| r.get(line))
            .map(|n| (*n).max(1))
            .unwrap_or(1);
        for k in 0..n {
            rows.push(line);
            subs.push(k);
        }
    }
    (rows, subs, line_to_row)
}

/// Visual rows each line needs at `cols` columns (tabs expanded, inlay hints included).
fn compute_rows_per_line(
    state: &EditorState,
    tab_size: usize,
    cols: usize,
    hints: &[rustpy_semantic::features::InlayHint],
) -> Vec<u32> {
    let n = state.doc.line_count();
    let mut out = Vec::with_capacity(n);
    let mut hi = 0usize;
    for line in 0..n {
        let lr = state.doc.line_range(line);
        let text = state.doc.line_text(line);
        while hi < hints.len() && hints[hi].offset < lr.start() {
            hi += 1;
        }
        let mut hj = hi;
        let mut hint_chars = 0usize;
        while hj < hints.len() && hints[hj].offset <= lr.end() {
            hint_chars += hints[hj].label.chars().count();
            hj += 1;
        }
        let width = super::soft_wrap::line_width(&text, hint_chars, tab_size);
        let indent = if state.preferences.soft_wrap_indent {
            super::soft_wrap::continuation_indent(&text, cols, tab_size)
        } else {
            0
        };
        out.push(
            super::soft_wrap::rows_for_width(width, cols.saturating_sub(indent).max(1)) as u32,
        );
    }
    out
}

/// Fills the background of `r` on the laid-out lines: rounded on one visual
/// row; like a selection (to the row end, full rows, then the start of the last
/// row) when it spans rows or lines.
#[allow(clippy::too_many_arguments)]
fn paint_text_range(
    painter: &egui::Painter,
    state: &EditorState,
    layouts: &[LineLayout],
    r: TextRange,
    color: Color32,
    tab_size: usize,
    char_w: f32,
    line_h: f32,
) {
    let start = state.doc.offset_to_position(r.start());
    let end = state.doc.offset_to_position(r.end());
    let (Some(first), Some(last)) = (
        layouts.first().map(|l| l.line),
        layouts.last().map(|l| l.line),
    ) else {
        return;
    };
    for line in (start.line as usize).max(first)..=(end.line as usize).min(last) {
        let Some(ll) = layouts.iter().find(|l| l.line == line) else {
            continue;
        };
        let text = state.doc.line_text(line);
        if ll.visual.is_some() {
            let begin = if line == start.line as usize {
                start.column as usize
            } else {
                0
            };
            let finish = if line == end.line as usize {
                end.column as usize
            } else {
                text.chars().count()
            };
            for rect in visual_source_rects(ll, &text, begin, finish, tab_size) {
                painter.rect_filled(rect.translate(ll.origin.to_vec2()), 2.0, color);
            }
            continue;
        }
        let p0 = if line == start.line as usize {
            pos_of_col(ll, &text, start.column as usize, tab_size)
        } else {
            Pos2::new(0.0, 0.0)
        };
        let (p1, tail) = if line == end.line as usize {
            (pos_of_col(ll, &text, end.column as usize, tab_size), false)
        } else {
            (
                Pos2::new(ll.galley.size().x, ll.galley.size().y - line_h),
                true,
            )
        };
        let gw = ll.galley.size().x.max(char_w);
        let x1 = if tail { p1.x + char_w * 0.5 } else { p1.x };
        if (p0.y - p1.y).abs() < 0.5 {
            let rounding = if start.line == end.line { 2.0 } else { 0.0 };
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y),
                    Pos2::new(
                        ll.origin.x + x1.max(p0.x + 1.0),
                        ll.origin.y + p0.y + line_h,
                    ),
                ),
                rounding,
                color,
            );
            continue;
        }
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(ll.origin.x + p0.x, ll.origin.y + p0.y),
                Pos2::new(ll.origin.x + gw + char_w * 0.5, ll.origin.y + p0.y + line_h),
            ),
            0.0,
            color,
        );
        let mut y = p0.y + line_h;
        while y < p1.y - 0.5 {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(ll.origin.x, ll.origin.y + y),
                    Pos2::new(ll.origin.x + gw + char_w * 0.5, ll.origin.y + y + line_h),
                ),
                0.0,
                color,
            );
            y += line_h;
        }
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(ll.origin.x, ll.origin.y + p1.y),
                Pos2::new(ll.origin.x + x1.max(1.0), ll.origin.y + p1.y + line_h),
            ),
            0.0,
            color,
        );
    }
}

/// Source glyphs start after hints at the same source offset, including at wrap boundaries.
fn source_glyph_index(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> usize {
    let logical = logical_display_col_of(ll, text, col, tab_size)
        + ll.hints
            .iter()
            .filter(|(source, _, _)| *source == col)
            .map(|(_, _, len)| len)
            .sum::<usize>();
    ll.visual
        .as_ref()
        .and_then(|map| map.glyph_start.get(logical))
        .copied()
        .unwrap_or(logical)
}

fn source_cluster_len(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> usize {
    let logical = logical_display_col_of(ll, text, col, tab_size)
        + ll.hints
            .iter()
            .filter(|(source, _, _)| *source == col)
            .map(|(_, _, len)| len)
            .sum::<usize>();
    ll.visual
        .as_ref()
        .and_then(|m| m.glyph_len.get(logical))
        .copied()
        .unwrap_or_else(|| {
            use unicode_segmentation::UnicodeSegmentation;
            if text.chars().nth(col) == Some('\t') {
                source_glyph_len(text, col, tab_size)
            } else {
                let byte = text
                    .char_indices()
                    .nth(col)
                    .map(|(i, _)| i)
                    .unwrap_or(text.len());
                text[byte..]
                    .graphemes(true)
                    .next()
                    .map(|g| g.chars().count())
                    .unwrap_or(0)
            }
        })
}

fn source_glyph_len(text: &str, col: usize, tab_size: usize) -> usize {
    let column = text
        .chars()
        .take(col)
        .fold(0, |n, c| n + char_width(c, tab_size.max(1), n));
    match text.chars().nth(col) {
        Some('\t') => char_width('\t', tab_size.max(1), column),
        Some(_) => 1,
        None => 0,
    }
}
/// Logical selections can occupy disjoint visual runs in a mixed-direction line.
fn visual_source_rects(
    ll: &LineLayout,
    text: &str,
    start: usize,
    end: usize,
    tab_size: usize,
) -> Vec<Rect> {
    let mut output: Vec<Rect> = Vec::new();
    for col in start..end.min(text.chars().count()) {
        let logical = logical_display_col_of(ll, text, col, tab_size)
            + ll.hints
                .iter()
                .filter(|(source, _, _)| *source == col)
                .map(|(_, _, len)| len)
                .sum::<usize>();
        let index = ll
            .visual
            .as_ref()
            .and_then(|m| m.glyph_start.get(logical))
            .copied()
            .unwrap_or(logical);
        let len = ll
            .visual
            .as_ref()
            .and_then(|m| m.glyph_len.get(logical))
            .copied()
            .unwrap_or_else(|| source_glyph_len(text, col, tab_size));
        for rect in glyph_span_rects(ll, index, len) {
            if !output.contains(&rect) {
                output.push(rect);
            }
        }
    }
    output.sort_by(|a, b| {
        a.top()
            .total_cmp(&b.top())
            .then(a.left().total_cmp(&b.left()))
    });
    let mut merged: Vec<Rect> = Vec::new();
    for rect in output {
        if let Some(last) = merged.last_mut() {
            if (last.top() - rect.top()).abs() < 0.5 && rect.left() <= last.right() + 0.5 {
                *last = last.union(rect);
                continue;
            }
        }
        merged.push(rect);
    }
    merged
}

/// Actual glyph rectangles, grouped by visual row so expanded tabs never cross a row's clip.
fn glyph_span_rects(ll: &LineLayout, index: usize, len: usize) -> Vec<Rect> {
    let mut base = 0;
    let mut rects = Vec::new();
    for row in &ll.galley.rows {
        let start = index.saturating_sub(base).min(row.glyphs.len());
        let end = (index + len).saturating_sub(base).min(row.glyphs.len());
        if start < end {
            let first = &row.glyphs[start];
            let last = &row.glyphs[end - 1];
            rects.push(Rect::from_min_max(
                Pos2::new(row.pos.x + first.pos.x, row.pos.y),
                Pos2::new(
                    row.pos.x + last.pos.x + last.advance_width,
                    row.pos.y + row.size.y,
                ),
            ));
        }
        base += row.glyphs.len() + usize::from(row.ends_with_newline);
    }
    rects
}
fn expand_source_segment(text: &str, start: usize, end: usize, tab_size: usize) -> String {
    let mut column = text[..start]
        .chars()
        .fold(0, |n, c| n + char_width(c, tab_size.max(1), n));
    let mut output = String::new();
    for c in text[start..end].chars() {
        let width = char_width(c, tab_size.max(1), column);
        if c == '\t' {
            output.extend(std::iter::repeat_n(' ', width));
        } else {
            output.push(c);
        }
        column += width;
    }
    output
}

/// Position (x, y offset) of a source column inside a laid-out (possibly wrapped) line.
fn pos_of_col(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> Pos2 {
    let r = ll
        .galley
        .pos_from_cursor(CCursor::new(display_col_of(ll, text, col, tab_size)));
    Pos2::new(r.min.x, r.min.y)
}

/// Where a caret before source column `col` is drawn: like [`pos_of_col`],
/// except that at a soft-wrap boundary the caret starts the next row (where
/// typed text goes and where Down puts it) instead of ending the previous one.
fn caret_pos_of_col(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> Pos2 {
    let cursor = CCursor {
        prefer_next_row: true,
        ..CCursor::new(display_col_of(ll, text, col, tab_size))
    };
    let r = ll.galley.pos_from_cursor(cursor);
    Pos2::new(r.min.x, r.min.y)
}

/// Galley character index of source column `col` (tabs expanded, inlay hints before it).
fn logical_display_col_of(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> usize {
    if let Some(map) = &ll.map {
        return map
            .partition_point(|source| *source < col)
            .min(map.len().saturating_sub(1));
    }
    let mut display_col = 0usize;
    let mut vis = 0usize;
    for c in text.chars().take(col) {
        let w = char_width(c, tab_size, vis);
        vis += w;
        display_col += if c == '\t' { w } else { 1 };
    }
    for (src, _, len) in &ll.hints {
        if *src < col {
            display_col += len;
        }
    }
    display_col
}

fn display_col_of(ll: &LineLayout, text: &str, col: usize, tab_size: usize) -> usize {
    let logical = logical_display_col_of(ll, text, col, tab_size);
    ll.visual
        .as_ref()
        .and_then(|map| map.logical_to_visual.get(logical))
        .copied()
        .unwrap_or(logical)
}

fn expand_tabs(text: &str, tab_size: usize) -> (String, Option<Vec<usize>>) {
    if !text.contains('\t') {
        return (text.to_string(), None);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut map = Vec::with_capacity(text.len() + 8);
    let mut col = 0usize;
    for (ci, c) in text.chars().enumerate() {
        if c == '\t' {
            let w = tab_size - (col % tab_size);
            for _ in 0..w {
                out.push(' ');
                map.push(ci);
            }
            col += w;
        } else {
            out.push(c);
            map.push(ci);
            col += char_width(c, tab_size, col);
        }
    }
    map.push(text.chars().count());
    (out, Some(map))
}

/// Display → source char map for a line with inlay hints (tabs expanded to
/// `tab_size` spaces like the layout does). Returns the map and the hints as
/// `(source char index, display start, label chars)`.
type HintLayout = Vec<(usize, usize, usize)>;

fn display_map_with_hints(
    text: &str,
    tab_size: usize,
    hints: &[(usize, &str)],
) -> (Option<Vec<usize>>, HintLayout) {
    let mut map = Vec::with_capacity(text.len() + 16);
    let mut out = Vec::new();
    let mut hi = 0usize;
    let mut ci = 0usize;
    let mut column = 0usize;
    for (bi, c) in text.char_indices() {
        while hi < hints.len() && hints[hi].0 <= bi {
            let len = hints[hi].1.chars().count();
            out.push((ci, map.len(), len));
            for _ in 0..len {
                map.push(ci);
            }
            hi += 1;
        }
        if c == '\t' {
            for _ in 0..char_width(c, tab_size.max(1), column) {
                map.push(ci);
            }
        } else {
            map.push(ci);
        }
        column += char_width(c, tab_size.max(1), column);
        ci += 1;
    }
    while hi < hints.len() {
        let len = hints[hi].1.chars().count();
        out.push((ci, map.len(), len));
        for _ in 0..len {
            map.push(ci);
        }
        hi += 1;
    }
    map.push(ci);
    (Some(map), out)
}

/// [`estimate_max_columns`], cached per document version. After an edit only
/// the caret lines are measured (the estimate only grows; it sizes the
/// horizontal scroll range), with a full scan every so many versions.
fn cached_max_columns(
    cache: &mut Option<(rustpy_text::DocumentId, u64, usize, usize, u32)>,
    state: &EditorState,
    tab_size: usize,
) -> usize {
    const FULL_SCAN_EVERY: u32 = 64;
    let (document, version) = (state.doc.identity(), state.doc.version());
    match cache {
        Some((d, v, t, columns, _)) if *d == document && *v == version && *t == tab_size => {
            return *columns;
        }
        Some((d, v, t, columns, edits))
            if *d == document && *t == tab_size && *edits < FULL_SCAN_EVERY =>
        {
            let caret_lines = state.carets.iter().map(|c| state.doc.line_of(c.head()));
            let widest = caret_lines
                .map(|line| {
                    state
                        .doc
                        .line_text(line)
                        .chars()
                        .fold(0, |c, ch| c + char_width(ch, tab_size, c))
                })
                .max()
                .unwrap_or(0);
            *columns = (*columns).max(widest);
            *v = version;
            *edits += 1;
            return *columns;
        }
        _ => {}
    }
    let columns = estimate_max_columns(state, tab_size);
    *cache = Some((document, version, tab_size, columns, 0));
    columns
}

fn estimate_max_columns(state: &EditorState, tab_size: usize) -> usize {
    let doc = &state.doc;
    let n = doc.line_count();
    // Sample for large files; exact for small ones.
    let step = (n / 2000).max(1);
    let mut max = 0;
    let mut i = 0;
    while i < n {
        let t = doc.line_text(i);
        let w = t.chars().fold(0, |c, ch| c + char_width(ch, tab_size, c));
        max = max.max(w);
        i += step;
    }
    max.max(80)
}

fn indent_levels(state: &EditorState, line: usize, indent_size: usize, tab_size: usize) -> usize {
    let text = state.doc.line_text(line);
    let width = if text.trim().is_empty() {
        // Blank line: inherit the indent of the next non-blank line.
        let mut w = 0;
        for l in line + 1..(line + 50).min(state.doc.line_count()) {
            let t = state.doc.line_text(l);
            if !t.trim().is_empty() {
                w = rustpy_text::util::indent_width(&t, tab_size);
                break;
            }
        }
        w
    } else {
        rustpy_text::util::indent_width(&text, tab_size)
    };
    width.checked_div(indent_size).unwrap_or(0)
}

/// Finds the bracket pair at/before the caret: returns (open, close) offsets.
fn matching_brace(state: &EditorState, caret: TextSize) -> Option<(TextSize, TextSize)> {
    let doc = &state.doc;
    let candidates = [caret, doc.prev_boundary(caret)];
    for at in candidates {
        let Some(c) = doc.char_at(at) else { continue };
        let (open, close, forward) = match c {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => continue,
        };
        let text = doc.text();
        let start = u32::from(at) as usize;
        let mut depth = 0i32;
        if forward {
            for (i, ch) in text[start..].char_indices() {
                if ch == open {
                    depth += 1;
                } else if ch == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((at, TextSize::new((start + i) as u32)));
                    }
                }
            }
        } else {
            for (i, ch) in text[..=start].char_indices().rev() {
                if ch == close {
                    depth += 1;
                } else if ch == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((TextSize::new(i as u32), at));
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod clone_caret_gesture_tests {
    use super::*;
    use egui::{Key, Modifiers};

    #[test]
    fn ctrl_pressed_twice_and_held_turns_arrows_into_clone_caret() {
        let mut v = EditorView::new(egui::Id::new("gesture"));
        let ctrl = Modifiers::CTRL;
        assert!(
            !v.clones_caret(Key::ArrowUp, ctrl),
            "plain Ctrl+Up is Scroll Up"
        );
        v.track_ctrl(true, ctrl, 1.0);
        assert!(!v.clones_caret(Key::ArrowUp, ctrl), "one Ctrl press");
        v.track_ctrl(false, Modifiers::NONE, 1.1);
        v.track_ctrl(true, ctrl, 1.25);
        assert!(v.clones_caret(Key::ArrowUp, ctrl) && v.clones_caret(Key::ArrowDown, ctrl));
        assert!(!v.clones_caret(Key::ArrowLeft, ctrl));
        assert!(
            !v.clones_caret(Key::ArrowUp, ctrl | Modifiers::SHIFT | Modifiers::ALT),
            "Ctrl+Alt+Shift+Up is Previous Change"
        );
        v.track_ctrl(false, Modifiers::NONE, 1.6);
        assert!(!v.clones_caret(Key::ArrowUp, ctrl), "released");
        // Two presses too far apart do not arm it.
        v.track_ctrl(true, ctrl, 5.0);
        v.track_ctrl(false, Modifiers::NONE, 5.1);
        v.track_ctrl(true, ctrl, 5.6);
        assert!(!v.clones_caret(Key::ArrowDown, ctrl));
    }

    #[test]
    fn reader_documentation_remains_visible_when_default_docstring_folding_is_enabled() {
        let mut state = EditorState::from_text(
            "def f():\n    \"\"\"First line.\n    More documentation.\n    \"\"\"\n    return 1\n",
            super::super::Language::Python,
        );
        state.doc.set_read_only(true);
        let settings = EditorSettings {
            fold_docstrings_by_default: true,
            ..Default::default()
        };
        let scheme = ColorScheme::dark();
        let theme = UiTheme::dark();
        let opts = EditOptions::default();
        let keymap = Keymap::default_keymap();
        let mut view = EditorView::new(egui::Id::new("reader-fold-test"));
        let cfg = ViewConfig {
            scheme: &scheme,
            theme: &theme,
            settings: &settings,
            opts: &opts,
            keymap: &keymap,
            font_size: 14.0,
            search_highlights: &[],
            request_focus: false,
            run_lines: &[],
            line_changes: &[],
            breakpoints: &[],
            exec_line: None,
            inline_values: &[],
            bookmarks: &[],
            gutter_markers: &[],
            gutter_quick_fixes: &[],
            code_vision: None,
            annotations: None,
            coverage: &[],
            keyboard: false,
        };
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut state, &mut view, &cfg);
        })
        .drop_without_applying_deltas();
        assert!(super::super::doc_render::reader_mode(&state));
        assert!(state.hidden_line_ranges().is_empty());
        assert_eq!(
            super::super::doc_render::shown_regions(&mut state, &[]).len(),
            1
        );
    }

    #[test]
    fn actual_view_draws_distinct_file_margin_and_auxiliary_guides() {
        let mut state = EditorState::from_text("value = 1\n", super::super::Language::Python);
        let mut settings = EditorSettings {
            right_margin: 120,
            visual_guides: vec![120, 79, 80, 80, 0],
            ..Default::default()
        };
        let scheme = ColorScheme::dark();
        let theme = UiTheme::dark();
        let opts = EditOptions {
            right_margin: 79,
            ..Default::default()
        };
        let keymap = Keymap::default_keymap();
        let mut view = EditorView::new(egui::Id::new("reader-fold-test"));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let guide_xs = |settings: &EditorSettings,
                        state: &mut EditorState,
                        view: &mut EditorView| {
            let cfg = ViewConfig {
                scheme: &scheme,
                theme: &theme,
                settings,
                opts: &opts,
                keymap: &keymap,
                font_size: 14.0,
                search_highlights: &[],
                request_focus: false,
                run_lines: &[],
                line_changes: &[],
                breakpoints: &[],
                exec_line: None,
                inline_values: &[],
                bookmarks: &[],
                gutter_markers: &[],
                gutter_quick_fixes: &[],
                code_vision: None,
                annotations: None,
                coverage: &[],
                keyboard: false,
            };
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    show(ui, state, view, &cfg);
                },
            );
            let mut xs = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, stroke }
                        if stroke.color == rgb(scheme.editor.right_margin)
                            && points[0].x == points[1].x
                            && points[1].y - points[0].y > 100.0 =>
                    {
                        Some(points[0].x)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            xs.sort_by(f32::total_cmp);
            output.drop_without_applying_deltas();
            xs
        };
        let xs = guide_xs(&settings, &mut state, &mut view);
        assert_eq!(
            xs.len(),
            3,
            "zero and duplicate columns must not draw extra guides"
        );
        let char_w = ctx.fonts_mut(|fonts| {
            fonts.glyph_width(&FontId::new(14.0, mono_family(false, false)), 'M')
        });
        assert!(
            (xs[1] - xs[0] - char_w).abs() < 0.1,
            "79 and 80 are adjacent columns"
        );
        assert!(
            (xs[2] - xs[1] - 40.0 * char_w).abs() < 0.1,
            "configured 120 remains an auxiliary guide"
        );
        settings.show_right_margin = false;
        assert!(guide_xs(&settings, &mut state, &mut view).is_empty());
    }

    #[test]
    fn actual_view_imports_editorconfig_guides_without_mutating_global_settings_or_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.py");
        let config = root.path().join(".editorconfig");
        let source = "\tπ = '🙂'\n";
        std::fs::write(&path, source).unwrap();
        std::fs::write(
            &config,
            "root=true\n[*]\nmax_line_length=40\nij_visual_guides=20,30,30,0\n",
        )
        .unwrap();
        let mut state = EditorState::from_text(source, super::super::Language::Python);
        state.path = Some(path.clone());
        let original_carets = state.carets.clone();
        let original_version = state.doc.version();
        let mut settings = EditorSettings {
            right_margin: 120,
            visual_guides: vec![120, 79, 80, 80, 0],
            ..Default::default()
        };
        let scheme = ColorScheme::dark();
        let theme = UiTheme::dark();

        let keymap = Keymap::default_keymap();
        let mut view = EditorView::new(egui::Id::new("reader-fold-test"));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let guide_xs = |settings: &EditorSettings,
                        state: &mut EditorState,
                        view: &mut EditorView| {
            let opts = super::super::indent_options::file_edit_options(settings, state).0;
            let cfg = ViewConfig {
                scheme: &scheme,
                theme: &theme,
                settings,
                opts: &opts,
                keymap: &keymap,
                font_size: 14.0,
                search_highlights: &[],
                request_focus: false,
                run_lines: &[],
                line_changes: &[],
                breakpoints: &[],
                exec_line: None,
                inline_values: &[],
                bookmarks: &[],
                gutter_markers: &[],
                gutter_quick_fixes: &[],
                code_vision: None,
                annotations: None,
                coverage: &[],
                keyboard: false,
            };
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    show(ui, state, view, &cfg);
                },
            );
            let mut xs = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, stroke }
                        if stroke.color == rgb(scheme.editor.right_margin)
                            && points[0].x == points[1].x
                            && points[1].y - points[0].y > 100.0 =>
                    {
                        Some(points[0].x)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            xs.sort_by(f32::total_cmp);
            output.drop_without_applying_deltas();
            xs
        };
        let xs = guide_xs(&settings, &mut state, &mut view);
        assert_eq!(
            xs.len(),
            3,
            "file guides20/30 plus margin40, not global79/80/120"
        );
        let char_w = ctx.fonts_mut(|fonts| {
            fonts.glyph_width(&FontId::new(14.0, mono_family(false, false)), 'M')
        });
        assert!((xs[1] - xs[0] - 10.0 * char_w).abs() < 0.1);
        assert!((xs[2] - xs[1] - 10.0 * char_w).abs() < 0.1);
        std::fs::write(
            &config,
            "root=true\n[*]\nmax_line_length=40\nij_visual_guides=\n",
        )
        .unwrap();
        rustpy_settings::editorconfig::invalidate_cache();
        let cleared = guide_xs(&settings, &mut state, &mut view);
        assert_eq!(
            cleared.len(),
            1,
            "empty clears auxiliary guides but retains margin"
        );
        assert!((cleared[0] - xs[2]).abs() < 0.1);
        std::fs::write(
            &config,
            "root=true\n[*]\nmax_line_length=40\nij_visual_guides=unset\n",
        )
        .unwrap();
        rustpy_settings::editorconfig::invalidate_cache();
        assert_eq!(guide_xs(&settings, &mut state, &mut view).len(), 4);
        assert_eq!(settings.visual_guides, vec![120, 79, 80, 80, 0]);
        settings.show_right_margin = false;
        assert!(guide_xs(&settings, &mut state, &mut view).is_empty());
        assert_eq!(state.doc.text(), source);
        assert_eq!(state.carets, original_carets);
        assert_eq!(state.doc.version(), original_version);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn actual_reader_view_hides_comments_and_reveals_caret_line() {
        let source = "# hidden whole-line marker\nα = '# visible string'  # hidden inline marker\nresult = α\n";
        let mut state = EditorState::from_text(source, super::super::Language::Python);
        state.doc.set_read_only(true);
        state.set_carets(vec![super::super::Caret::at(
            state.doc.line_range(2).start(),
        )]);
        let settings = EditorSettings::default();
        let scheme = ColorScheme::dark();
        let theme = UiTheme::dark();
        let opts = EditOptions::default();
        let keymap = Keymap::default_keymap();
        let mut view = EditorView::new(egui::Id::new("reader-comments-view"));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let paint = |state: &mut EditorState, view: &mut EditorView| {
            let cfg = ViewConfig {
                scheme: &scheme,
                theme: &theme,
                settings: &settings,
                opts: &opts,
                keymap: &keymap,
                font_size: 14.0,
                search_highlights: &[],
                request_focus: false,
                run_lines: &[],
                line_changes: &[],
                breakpoints: &[],
                exec_line: None,
                inline_values: &[],
                bookmarks: &[],
                gutter_markers: &[],
                gutter_quick_fixes: &[],
                code_vision: None,
                annotations: None,
                coverage: &[],
                keyboard: false,
            };

            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    show(ui, state, view, &cfg);
                },
            );
            let jobs = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.job.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            output.drop_without_applying_deltas();
            jobs
        };
        let jobs = paint(&mut state, &mut view);
        assert!(!jobs
            .iter()
            .any(|job| job.text.contains("hidden whole-line marker")));
        let inline = jobs
            .iter()
            .find(|job| job.text.contains("hidden inline marker"))
            .unwrap();
        let comment = egui::text::ByteIndex(inline.text.find("# hidden inline").unwrap());
        assert!(inline
            .sections
            .iter()
            .filter(|section| section.byte_range.start >= comment)
            .all(|section| section.format.color == Color32::TRANSPARENT));
        assert!(inline
            .sections
            .iter()
            .filter(|section| section.byte_range.end <= comment)
            .all(|section| section.format.color != Color32::TRANSPARENT));
        state.set_carets(vec![super::super::Caret::at(
            state.doc.line_range(1).start(),
        )]);
        let jobs = paint(&mut state, &mut view);
        let inline = jobs
            .iter()
            .find(|job| job.text.contains("hidden inline marker"))
            .unwrap();
        assert!(inline
            .sections
            .iter()
            .all(|section| section.format.color != Color32::TRANSPARENT));
        super::super::doc_render::toggle_reader_mode(&mut state);
        let jobs = paint(&mut state, &mut view);
        assert!(jobs
            .iter()
            .any(|job| job.text.contains("hidden whole-line marker")));
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn only_web_and_mail_doc_links_open() {
        assert!(
            is_openable_doc_link("https://docs.python.org/3/")
                && is_openable_doc_link("HTTP://example.org")
                && is_openable_doc_link("mailto:a@b.c")
        );
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ssh://host",
            "/tmp/x",
            "relative/page.html",
        ] {
            assert!(!is_openable_doc_link(bad), "{bad}");
        }
    }
}

#[cfg(test)]
mod session_scroll_tests {
    use super::super::Language;
    use super::*;

    fn frame(
        ctx: &egui::Context,
        state: &mut EditorState,
        view: &mut EditorView,
        settings: &EditorSettings,
    ) {
        let scheme = ColorScheme::dark();
        let theme = UiTheme::dark();
        let opts = EditOptions::default();
        let keymap = Keymap::default_keymap();
        let cfg = ViewConfig {
            scheme: &scheme,
            theme: &theme,
            settings,
            opts: &opts,
            keymap: &keymap,
            font_size: 14.0,
            search_highlights: &[],
            request_focus: false,
            run_lines: &[],
            line_changes: &[],
            breakpoints: &[],
            exec_line: None,
            inline_values: &[],
            bookmarks: &[],
            gutter_markers: &[],
            gutter_quick_fixes: &[],
            code_vision: None,
            annotations: None,
            coverage: &[],
            keyboard: false,
        };
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(480.0, 220.0))),
                ..Default::default()
            },
            |ui| {
                show(ui, state, view, &cfg);
            },
        )
        .drop_without_applying_deltas();
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx
    }

    #[test]
    fn actual_viewport_roundtrips_workspace_without_moving_caret_to_top_line() {
        let source = (0..120)
            .map(|i| format!("value_{i} = {i}\n"))
            .collect::<String>();
        let mut state = EditorState::from_text(&source, Language::Python);
        let mut view = EditorView::new(egui::Id::new("session-source"));
        let settings = EditorSettings::default();
        let ctx = context();
        let position = SessionPosition {
            caret: Position::new(3, 2),
            scroll_line: 45,
        };
        view.restore_session_position(&mut state, position, true);
        frame(&ctx, &mut state, &mut view, &settings);
        frame(&ctx, &mut state, &mut view, &settings);
        assert_eq!(view.session_position(&state), position);
        assert!(view.pending_scroll_line.is_none());
        assert_eq!(view.top_line, 45);
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("m.py");
        std::fs::write(&path, &source).unwrap();
        let mut workspace = rustpy_settings::Workspace::default();
        workspace.open_files.push(
            view.session_position(&state)
                .into_open_file_state(path, true),
        );
        workspace.save(fixture.path()).unwrap();
        let saved = rustpy_settings::Workspace::load(fixture.path())
            .open_files
            .remove(0);
        assert!(saved.pinned);
        let mut reopened = EditorState::from_text(&source, Language::Python);
        let mut new_view = EditorView::new(egui::Id::new("session-reopened"));
        new_view.restore_session_position(&mut reopened, SessionPosition::from(&saved), true);
        let reopened_ctx = context();
        frame(&reopened_ctx, &mut reopened, &mut new_view, &settings);
        frame(&reopened_ctx, &mut reopened, &mut new_view, &settings);
        assert_eq!(new_view.session_position(&reopened), position);
        assert_eq!(reopened.primary_position(), Position::new(3, 2));
    }

    #[test]
    fn wraps_and_rendered_documentation_restore_logical_source_lines() {
        for wrapped in [false, true] {
            let source = format!("def documented():\n    \"\"\"Heading.\n\n    Detailed documentation with enough text to wrap and change visual height.\n    \"\"\"\n    pass\n{}", (0..100).map(|i| format!("# line {i} {}\n", "long comment ".repeat(15))).collect::<String>());
            let mut state = EditorState::from_text(&source, Language::Python);
            let settings = EditorSettings {
                soft_wrap: wrapped,
                render_doc_comments: true,
                ..Default::default()
            };
            let mut view = EditorView::new(egui::Id::new(("session-docs", wrapped)));
            let ctx = context();
            let position = SessionPosition {
                caret: Position::new(0, 0),
                scroll_line: 30,
            };
            view.restore_session_position(&mut state, position, true);
            for _ in 0..4 {
                frame(&ctx, &mut state, &mut view, &settings);
            }
            assert!(view.pending_scroll_line.is_none(), "wrapped={wrapped}");
            assert_eq!(view.top_line, 30);
            assert_eq!(view.session_position(&state), position, "wrapped={wrapped}");
            assert!(!super::super::doc_render::shown_regions(&mut state, &[]).is_empty());
            if wrapped {
                assert!(view
                    .wrap_cache
                    .as_ref()
                    .unwrap()
                    .2
                    .iter()
                    .any(|&rows| rows > 1));
            }
        }
    }

    #[test]
    fn deferred_large_document_positions_survive_placeholder_and_state_replacement() {
        let mut placeholder = EditorState::from_text("", Language::Python);
        let mut view = EditorView::new(egui::Id::new("session-deferred"));
        let position = SessionPosition {
            caret: Position::new(60, 3),
            scroll_line: 40,
        };
        view.restore_session_position(&mut placeholder, position, false);
        assert_eq!(
            view.session_position(&placeholder),
            position,
            "saving before load must retain both positions"
        );
        let ctx = context();
        frame(
            &ctx,
            &mut placeholder,
            &mut view,
            &EditorSettings::default(),
        );
        assert_eq!(view.session_position(&placeholder), position);
        let source = (0..120)
            .map(|i| format!("value_{i} = {i}\n"))
            .collect::<String>();
        let mut loaded = EditorState::from_text(&source, Language::Python);
        assert_eq!(
            view.session_position(&loaded),
            position,
            "background replacement must not lose the pending caret"
        );
        view.complete_session_load(&mut loaded);
        for _ in 0..4 {
            frame(&ctx, &mut loaded, &mut view, &EditorSettings::default());
        }
        assert!(view.pending_scroll_line.is_none());
        assert_eq!(view.top_line, 40);
        assert_eq!(view.session_position(&loaded), position);
        assert_eq!(loaded.primary_position(), position.caret);
    }

    #[test]
    fn explicit_load_completion_restores_empty_document_and_cancellation_wins() {
        let position = SessionPosition {
            caret: Position::new(60, 3),
            scroll_line: 40,
        };
        let mut state = EditorState::from_text("", Language::Python);
        let mut view = EditorView::new(egui::Id::new("session-empty-loaded"));
        view.restore_session_position(&mut state, position, false);
        let mut loaded = EditorState::from_text("", Language::Python);
        view.complete_session_load(&mut loaded);
        let ctx = context();
        for _ in 0..3 {
            frame(&ctx, &mut loaded, &mut view, &EditorSettings::default());
        }
        assert!(view.pending_scroll_line.is_none() && view.pending_session_caret.is_none());
        assert_eq!(view.session_position(&loaded), SessionPosition::default());

        view.restore_session_position(&mut state, position, false);
        view.cancel_session_restore();
        let mut loaded = EditorState::from_text(&"x = 1\n".repeat(100), Language::Python);
        loaded.move_primary_to(Position::new(12, 1));
        view.complete_session_load(&mut loaded);
        assert_eq!(loaded.primary_position(), Position::new(12, 1));
        assert!(view.pending_scroll_line.is_none());
    }

    #[test]
    fn editing_and_navigation_before_layout_supersede_saved_viewport() {
        let source = "x = 1\n".repeat(100);
        for edit in [false, true] {
            let mut state = EditorState::from_text(&source, Language::Python);
            let mut view = EditorView::new(egui::Id::new(("session-superseded", edit)));
            view.restore_session_position(
                &mut state,
                SessionPosition {
                    caret: Position::new(3, 2),
                    scroll_line: 45,
                },
                true,
            );
            if edit {
                state.apply_text_edits(
                    vec![(TextRange::empty(TextSize::new(0)), "# change\n".into())],
                    "User edit",
                );
            } else {
                state.move_primary_to(Position::new(12, 1));
            }
            let caret = state.primary_position();
            assert_eq!(view.session_position(&state).caret, caret);
            assert_eq!(view.session_position(&state).scroll_line, 0);
            frame(
                &context(),
                &mut state,
                &mut view,
                &EditorSettings::default(),
            );
            assert!(view.pending_scroll_line.is_none() && view.pending_session_caret.is_none());
            assert_eq!(state.primary_position(), caret);
        }
    }

    #[test]
    fn editing_placeholder_cancels_deferred_session_before_completion() {
        let mut state = EditorState::from_text("", Language::Python);
        let mut view = EditorView::new(egui::Id::new("session-placeholder-edited"));
        view.restore_session_position(
            &mut state,
            SessionPosition {
                caret: Position::new(60, 3),
                scroll_line: 40,
            },
            false,
        );
        state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "x = 1\n".into())],
            "User edit",
        );
        let caret = state.primary_position();
        frame(
            &context(),
            &mut state,
            &mut view,
            &EditorSettings::default(),
        );
        assert!(view.pending_session_caret.is_none() && view.pending_scroll_line.is_none());
        view.complete_session_load(&mut state);
        assert_eq!(state.primary_position(), caret);
    }

    #[test]
    fn hidden_and_out_of_range_lines_choose_visible_header_and_first_wrapped_row() {
        let (rows, _, line_to_row) = build_rows(8, &[(2, 4)], Some(&[3, 2, 1, 1, 1, 4, 1, 1]));
        assert_eq!(session_scroll_row(1, &line_to_row, &rows), 3);
        assert_eq!(session_scroll_row(4, &line_to_row, &rows), 3);
        assert_eq!(session_scroll_row(5, &line_to_row, &rows), 5);
        assert_eq!(session_scroll_row(u32::MAX, &line_to_row, &rows), 10);
        assert_eq!(session_scroll_row(10, &[], &[]), 0);
    }
}

#[cfg(test)]
#[path = "view_data_syntax_tests.rs"]
mod data_syntax_tests;

#[cfg(test)]
#[path = "view_appearance_tests.rs"]
mod appearance_tests;

#[cfg(test)]
#[path = "view_navigation_tests.rs"]
mod navigation_tests;

#[cfg(test)]
#[path = "view_column_tests.rs"]
mod column_tests;

#[cfg(test)]
#[path = "view_multicaret_tests.rs"]
mod multicaret_tests;

#[cfg(test)]
#[path = "view_smartkeys_tests.rs"]
mod smartkeys_tests;

#[cfg(test)]
#[path = "view_scrollbar_tests.rs"]
mod scrollbar_tests;

#[cfg(test)]
#[path = "view_virtual_space_tests.rs"]
mod virtual_space_view_tests;

#[cfg(test)]
#[path = "view_inspection_stripe_tests.rs"]
mod inspection_stripe_tests;

#[cfg(test)]
#[path = "view_code_vision_tests.rs"]
mod code_vision_tests;

#[cfg(test)]
#[path = "view_gutter_actions_tests.rs"]
mod gutter_actions_tests;

#[cfg(test)]
#[path = "view_todo_tests.rs"]
mod todo_tests;

#[cfg(test)]
#[path = "view_accessibility_tests.rs"]
mod accessibility_tests;

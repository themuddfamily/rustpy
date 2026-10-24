//! Rendered documentation comments and reader mode (roadmap §1.1).
//!
//! A docstring that occupies whole lines can be drawn as formatted text in
//! place of its source lines:
//! the gutter icon next to it or *Toggle Rendered View* (Ctrl+Alt+Q) switches
//! one docstring, Settings › Editor › General › Appearance › "Render
//! documentation comments" renders all of them by default, and reader mode
//! (Settings › Editor › Reader Mode) renders them in library and read-only
//! files. A rendered docstring shows its source again while a caret is in it,
//! so clicking it or moving the caret into it edits the source; it renders
//! again once the caret leaves.
//! Reader mode also hides ordinary comment-only lines; a caret on a comment
//! reveals its source, and leaving reader mode restores every comment.
//!
//! The docstring → blocks conversion lives in [`rustpy_semantic::doc_blocks`];
//! this module tracks which docstrings are rendered (the editor's default plus
//! the docstrings switched against it, anchored at the start of their string
//! literal so they follow edits), lays the blocks out with egui and hit-tests
//! clicks on them.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use rustpy_core::TextSize;
use rustpy_highlight::{ColorScheme, HighlightTag};
use rustpy_semantic::doc_blocks::{self, BlockKind, DocBlock, Span, SpanStyle};
use rustpy_settings::EditorSettings;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use super::state::{map_offset, Caret, EditorState, Language};
use crate::theme::{mono_family, rgb};

/// A docstring on lines of its own, which can be shown rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRegion {
    /// Start of the string literal: identifies the docstring (follows edits).
    pub start: TextSize,
    /// End of the string literal.
    pub end: TextSize,
    pub first_line: usize,
    pub last_line: usize,
    /// The literal as written (prefix, quotes, escapes).
    pub literal: Arc<str>,
    /// Hash of `literal` (part of its layout's cache key).
    pub hash: u64,
}

impl DocRegion {
    /// Whether a caret at `offset` (on `line`) is inside the docstring, i.e. the
    /// docstring must show its source. A caret before the opening quote is not.
    pub fn contains_caret(&self, line: usize, offset: TextSize) -> bool {
        self.first_line <= line && line <= self.last_line && offset > self.start
    }
}

/// Rendered documentation comments and reader mode of one editor.
#[derive(Default)]
pub struct DocRender {
    /// Reader mode chosen for this editor (the switch or the *Reader Mode*
    /// action); `None` follows the settings: on for read-only files.
    reader_pinned: Option<bool>,
    /// Reader mode in effect (updated by [`sync`]).
    reader: bool,
    /// A library source identified by the interpreter/project paths (not a bundled stub).
    library_source: bool,
    /// The user made this read-only library file writable ([`make_writable`]).
    unlocked: bool,
    /// Filesystem/viewer guards are independent of reader-mode protection.
    file_read_only: bool,
    /// The settings' "render by default" value last applied (`None`: apply it again).
    settings_default: Option<bool>,
    /// Whether docstrings render unless switched: the settings' default, or the
    /// last *Render All Doc Comments*.
    default: bool,
    /// Docstrings switched against `default`, by the start offset of their literal.
    flipped: HashSet<TextSize>,
    /// Document version the `flipped` anchors are valid for.
    anchors_version: u64,
    /// Docstring regions of a document version.
    regions: Option<(u64, Arc<Vec<DocRegion>>)>,
    /// Syntax-confirmed comments for this document version.
    comments: Option<(rustpy_text::DocumentId, u64, Arc<Vec<ReaderComment>>)>,
    /// Laid-out docstrings by [`layout_key`] (each knows the width it was laid
    /// out for), kept for the docstrings drawn last frame.
    layouts: HashMap<u64, Arc<RenderedDoc>>,
    blocks: HashMap<u64, Arc<Vec<DocBlock>>>,
}

impl std::fmt::Debug for DocRender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocRender")
            .field("reader", &self.reader)
            .field("default", &self.default)
            .field("flipped", &self.flipped.len())
            .finish()
    }
}

/// Whether docstrings of this editor can be rendered (Python, not a large file).
pub fn supported(state: &EditorState) -> bool {
    matches!(state.language, Language::Python) && !state.is_large_file()
}

/// Docstrings of the current document version that occupy whole lines, sorted.
pub fn regions(state: &mut EditorState) -> Arc<Vec<DocRegion>> {
    let version = state.doc.version();
    if let Some((v, r)) = &state.doc_render.regions {
        if *v == version {
            return r.clone();
        }
    }
    let regions = Arc::new(if supported(state) {
        compute_regions(state)
    } else {
        Vec::new()
    });
    state.doc_render.regions = Some((version, regions.clone()));
    regions
}

fn compute_regions(state: &mut EditorState) -> Vec<DocRegion> {
    let parse = state.syntax().clone();
    let root = parse.syntax_node();
    let mut docs: Vec<ast::StringLike> = Vec::new();
    if let Some(m) = ast::Module::cast(root.clone()) {
        docs.extend(m.docstring());
    }
    for n in root.descendants() {
        match n.kind() {
            SyntaxKind::FUNC_DEF => docs.extend(ast::FuncDef::cast(n).and_then(|f| f.docstring())),
            SyntaxKind::CLASS_DEF => {
                docs.extend(ast::ClassDef::cast(n).and_then(|c| c.docstring()))
            }
            _ => {}
        }
    }
    let doc = &state.doc;
    let mut out: Vec<DocRegion> = docs
        .into_iter()
        .filter_map(|d| {
            let ast::StringLike::Single(s) = d else {
                return None;
            };
            if s.is_bytes() {
                return None;
            }
            let range = s.token()?.text_range();
            let (start, end) = (range.start(), range.end());
            let (first_line, last_line) = (doc.line_of(start), doc.line_of(end));
            // Only whitespace may share the docstring's lines.
            let first = doc.line_range(first_line);
            let last = doc.line_range(last_line);
            let before = doc.slice(rustpy_core::TextRange::new(first.start(), start));
            let after = doc.slice(rustpy_core::TextRange::new(end.min(last.end()), last.end()));
            if !before.trim().is_empty() || !after.trim().is_empty() {
                return None;
            }
            let literal: Arc<str> = doc.slice(range).into();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            literal.hash(&mut h);
            Some(DocRegion {
                start,
                end,
                first_line,
                last_line,
                literal,
                hash: h.finish(),
            })
        })
        .collect();
    out.sort_by_key(|r| r.start);
    out.dedup_by_key(|r| r.start);
    out
}

/// Per-frame bookkeeping: decides reader mode, applies the "render by
/// default" setting of the mode when it changes (or the editor is new) and
/// moves the switched docstrings through the edits made since the last frame.
pub fn sync(state: &mut EditorState, settings: &EditorSettings) {
    if !supported(state) {
        return;
    }
    if state.doc_render.library_source && !state.doc_render.unlocked {
        if !settings.reader_mode.protect_library_sources {
            state.doc.set_read_only(state.doc_render.file_read_only);
        } else if !state.doc.is_modified() {
            state.doc.set_read_only(true);
        }
    }
    let read_only = state.doc.is_read_only();
    let dr = &mut state.doc_render;
    dr.reader = dr.reader_pinned.unwrap_or(
        settings.reader_mode.enabled && (read_only || (dr.library_source && !dr.unlocked)),
    );
    let want = if dr.reader {
        settings.reader_mode.render_docs
    } else {
        settings.render_doc_comments
    };
    let changed = dr.settings_default != Some(want);
    if changed {
        dr.settings_default = Some(want);
        dr.default = want;
        dr.flipped.clear();
    }
    remap_anchors(state);
    if changed && want {
        let docs = regions(state);
        unfold(state, &docs);
    }
}

/// Moves the switched-docstring anchors through the document changes made
/// since they were valid; anchors inside replaced text are dropped (those
/// docstrings follow the default again).
fn remap_anchors(state: &mut EditorState) {
    let version = state.doc.version();
    let dr = &mut state.doc_render;
    if dr.anchors_version == version {
        return;
    }
    if !dr.flipped.is_empty() {
        match state.doc.changes_since(dr.anchors_version) {
            Some(changes) => {
                let mut anchors: Vec<TextSize> = dr.flipped.drain().collect();
                for edits in changes {
                    anchors = anchors
                        .into_iter()
                        .filter(|&a| {
                            !edits
                                .iter()
                                .any(|e| e.range.start() < a && a < e.range.end())
                        })
                        .map(|a| map_offset(&edits, a, true))
                        .collect();
                }
                dr.flipped = anchors.into_iter().collect();
            }
            None => dr.flipped.clear(),
        }
    }
    dr.anchors_version = version;
}

/// Whether reader mode is on for this editor.
pub fn reader_mode(state: &EditorState) -> bool {
    state.doc_render.reader
}

struct ReaderComment {
    line: usize,
    /// UTF-8 byte offset in the source line, before tab expansion or hints.
    start: usize,
    whole_line: bool,
}

fn reader_comments(state: &mut EditorState) -> Arc<Vec<ReaderComment>> {
    let version = state.doc.version();
    let identity = state.doc.identity();
    if let Some((document, v, comments)) = &state.doc_render.comments {
        if *document == identity && *v == version {
            return comments.clone();
        }
    }
    let syntax = state.syntax().syntax_node().clone();
    let comments: Arc<Vec<ReaderComment>> = Arc::new(
        syntax
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::COMMENT)
            .map(|token| {
                let start = token.text_range().start();
                let line = state.doc.line_of(start);
                let line_start = state.doc.line_range(line).start();
                let before = state
                    .doc
                    .slice(rustpy_core::TextRange::new(line_start, start));
                ReaderComment {
                    line,
                    start: usize::from(start - line_start),
                    whole_line: before.trim().is_empty(),
                }
            })
            .collect(),
    );
    state.doc_render.comments = Some((identity, version, comments.clone()));
    comments
}

/// Start of an inline comment to suppress in painting, in UTF-8 source-line
/// bytes. Keep its text and layout intact so selection, wrapping and hit tests
/// still use the unchanged document. Any caret on the line reveals it.
pub fn hidden_inline_comment_start(state: &mut EditorState, line: usize) -> Option<usize> {
    if !supported(state)
        || !reader_mode(state)
        || state
            .carets
            .iter()
            .any(|caret| state.doc.line_of(caret.head()) == line)
    {
        return None;
    }
    if folded_first_lines(state).contains(&line) {
        return None;
    }
    reader_comments(state)
        .iter()
        .find(|comment| comment.line == line && !comment.whole_line)
        .map(|comment| comment.start)
}

/// Hide ordinary comment-only lines while reading, without hiding code,
/// docstring contents or the line of a caret. Leaving reader mode shows them
/// again; the document itself is never changed.
pub fn hidden_comment_lines(state: &mut EditorState) -> Vec<(usize, usize)> {
    if !supported(state) || !reader_mode(state) {
        return Vec::new();
    }
    let comments = reader_comments(state);
    // A folded custom region uses its comment header as the visible
    // placeholder and unfold control. Reader presentation must retain it.
    let fold_headers = folded_first_lines(state);
    let caret_lines: HashSet<_> = state
        .carets
        .iter()
        .map(|caret| state.doc.line_of(caret.head()))
        .collect();
    merge_hidden(
        &[],
        comments
            .iter()
            .filter(|comment| comment.whole_line)
            .map(|comment| comment.line)
            .filter(|line| !caret_lines.contains(line) && !fold_headers.contains(line))
            .map(|line| (line, line)),
    )
}

/// Whether the reader-mode switch is offered: Python files that are read-only
/// (libraries, stubs) or in reader mode.
pub fn reader_mode_available(state: &EditorState) -> bool {
    supported(state) && (state.doc.is_read_only() || reader_mode(state))
}

/// Whether any docstring of this editor is set to render.
pub fn renders_any(state: &EditorState) -> bool {
    supported(state) && (state.doc_render.default || !state.doc_render.flipped.is_empty())
}

/// Whether the docstring starting at `start` is set to render (it shows
/// rendered unless a caret is inside or it is folded).
pub fn is_rendered(state: &EditorState, start: TextSize) -> bool {
    state.doc_render.default != state.doc_render.flipped.contains(&start)
}

/// Line and offset of every caret.
fn caret_positions(state: &EditorState) -> Vec<(usize, TextSize)> {
    state
        .carets
        .iter()
        .map(|c| (state.doc.line_of(c.head()), c.head()))
        .collect()
}

/// First lines of the folded fold regions.
fn folded_first_lines(state: &mut EditorState) -> HashSet<usize> {
    let folded = state.folded_starts();
    if folded.is_empty() {
        return HashSet::new();
    }
    state
        .fold_regions()
        .iter()
        .filter(|f| folded.contains(&f.range.start()))
        .map(|f| f.first_line)
        .collect()
}

/// The docstrings to draw rendered now: set to render, not inside a folded
/// region, not folded themselves and without a caret inside.
pub fn shown_regions(state: &mut EditorState, hidden: &[(usize, usize)]) -> Vec<DocRegion> {
    if !supported(state) || (!state.doc_render.default && state.doc_render.flipped.is_empty()) {
        return Vec::new();
    }
    let all = regions(state);
    let carets = caret_positions(state);
    let folded_lines = folded_first_lines(state);
    all.iter()
        .filter(|r| is_rendered(state, r.start))
        .filter(|r| {
            !hidden
                .iter()
                .any(|(a, b)| *a <= r.last_line && r.first_line <= *b)
        })
        .filter(|r| !folded_lines.contains(&r.first_line))
        .filter(|r| {
            !carets
                .iter()
                .any(|(line, off)| r.contains_caret(*line, *off))
        })
        .cloned()
        .collect()
}

/// Whether `region` is drawn rendered now (its own line being visible): set
/// to render, not folded and without a caret inside.
fn displayed(state: &mut EditorState, region: &DocRegion) -> bool {
    is_rendered(state, region.start)
        && !caret_positions(state)
            .iter()
            .any(|(line, off)| region.contains_caret(*line, *off))
        && !folded_first_lines(state).contains(&region.first_line)
}

/// Where a click on line `line` of a rendered docstring puts the caret: after
/// the opening quotes on the first line, at the text of the others.
pub fn source_offset(state: &EditorState, region: &DocRegion, line: usize) -> TextSize {
    let line = line.clamp(region.first_line, region.last_line);
    if line == region.first_line {
        let opening = region
            .literal
            .bytes()
            .take_while(|b| b.is_ascii_alphabetic())
            .count();
        let quotes = region.literal[opening..]
            .bytes()
            .take_while(|b| *b == b'"' || *b == b'\'')
            .count()
            .min(3);
        return (region.start + TextSize::new((opening + quotes) as u32)).min(region.end);
    }
    let text = state.doc.line_text(line);
    state.doc.line_range(line).start()
        + TextSize::new((text.len() - text.trim_start().len()) as u32)
}

/// Sorted, merged hidden line ranges with `extra` (inclusive ranges) added.
pub fn merge_hidden(
    hidden: &[(usize, usize)],
    extra: impl Iterator<Item = (usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut all: Vec<(usize, usize)> = hidden.iter().copied().chain(extra).collect();
    all.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(all.len());
    for r in all {
        match out.last_mut() {
            Some(last) if r.0 <= last.1 + 1 => last.1 = last.1.max(r.1),
            _ => out.push(r),
        }
    }
    out
}

/// Renders `region` (unfolding it, and moving carets inside it to its start
/// so it shows rendered right away) or shows its source.
pub fn set_rendered(state: &mut EditorState, region: &DocRegion, render: bool) {
    remap_anchors(state);
    let dr = &mut state.doc_render;
    if render == dr.default {
        dr.flipped.remove(&region.start);
    } else {
        dr.flipped.insert(region.start);
    }
    if render {
        unfold(state, std::slice::from_ref(region));
        move_carets_out(state, std::slice::from_ref(region));
    }
}

/// Unfolds the fold regions starting on the first line of `regions` (the docstrings' own folds).
fn unfold(state: &mut EditorState, regions: &[DocRegion]) {
    let lines: HashSet<usize> = regions.iter().map(|r| r.first_line).collect();
    let starts: Vec<TextSize> = state
        .fold_regions()
        .iter()
        .filter(|f| lines.contains(&f.first_line))
        .map(|f| f.range.start())
        .collect();
    for s in starts {
        state.unfold_start(s);
    }
}

fn move_carets_out(state: &mut EditorState, regions: &[DocRegion]) {
    let mut moved = false;
    let carets: Vec<Caret> = state
        .carets
        .iter()
        .map(|k| {
            let line = state.doc.line_of(k.head());
            match regions.iter().find(|r| r.contains_caret(line, k.head())) {
                Some(r) => {
                    moved = true;
                    Caret::at(r.start)
                }
                None => *k,
            }
        })
        .collect();
    if moved {
        state.set_carets(carets);
    }
}

/// The docstring on `line` (any of its lines).
pub fn region_at_line(state: &mut EditorState, line: usize) -> Option<DocRegion> {
    regions(state)
        .iter()
        .find(|r| r.first_line <= line && line <= r.last_line)
        .cloned()
}

/// Shows `region` the other way from how it shows now: its source while it
/// is drawn rendered, else rendered (also when a caret inside or a fold shows
/// its source although it is set to render).
fn flip(state: &mut EditorState, region: &DocRegion) {
    remap_anchors(state);
    let render = !displayed(state, region);
    set_rendered(state, region, render);
}

/// Gutter icon: toggles the rendered view of the docstring on `line`.
pub fn toggle_at_line(state: &mut EditorState, line: usize) {
    if let Some(r) = region_at_line(state, line) {
        flip(state, &r);
    }
}

/// *Toggle Rendered View*: the docstring containing the caret, or the one that
/// starts on the next non-blank line (the caret on a `def` / `class` line).
/// Returns false when there is no docstring there.
pub fn toggle_at_caret(state: &mut EditorState) -> bool {
    if !supported(state) {
        return false;
    }
    let line = state.doc.line_of(state.primary().head());
    let regions = regions(state);
    let mut next = line + 1;
    while next < state.doc.line_count() && state.doc.line_text(next).trim().is_empty() {
        next += 1;
    }
    let Some(r) = regions
        .iter()
        .find(|r| r.first_line <= line && line <= r.last_line)
        .or_else(|| regions.iter().find(|r| r.first_line == next))
    else {
        return false;
    };
    flip(state, r);
    true
}

/// *Render All Doc Comments*: shows the source of every docstring when some
/// are set to render, else renders them all (and the ones written later).
pub fn toggle_all(state: &mut EditorState) {
    if !supported(state) {
        return;
    }
    remap_anchors(state);
    let regions = regions(state);
    let render = !regions.iter().any(|r| is_rendered(state, r.start));
    state.doc_render.default = render;
    state.doc_render.flipped.clear();
    if render {
        unfold(state, &regions);
        move_carets_out(state, &regions);
    }
}

/// Whether `path` is library code: a bundled stub, a file in `site-packages` /
/// `dist-packages`, or a file in one of the interpreter's `search_paths` (the
/// standard library, …) outside the project `roots` (an editable install
/// puts a project directory on the search path). Library directory names above
/// the deepest project root do not classify its own sources as libraries;
/// installed packages inside that project's virtual environment still do.
pub fn is_library_file(path: &Path, search_paths: &[PathBuf], roots: &[PathBuf]) -> bool {
    if path.starts_with("<typeshed>") {
        return true;
    }
    let under = |dirs: &[PathBuf]| {
        dirs.iter()
            .any(|d| !d.as_os_str().is_empty() && path.starts_with(d))
    };
    let project_root = roots
        .iter()
        .filter(|r| !r.as_os_str().is_empty() && path.starts_with(r))
        .max_by_key(|r| r.components().count());
    let relative = project_root
        .and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path);
    relative.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("site-packages" | "dist-packages")
        )
    }) || (project_root.is_none() && under(search_paths))
}

/// Whether a file opened from disk starts read-only, and so in reader mode:
/// Python library sources (see [`is_library_file`]; the project roots are
/// also compared in their canonical form, as `path` is canonical).
pub fn opens_read_only(path: &Path, search_paths: &[PathBuf], roots: &[PathBuf]) -> bool {
    if Language::from_path(path) != Language::Python {
        return false;
    }
    let mut all_roots = roots.to_vec();
    all_roots.extend(roots.iter().filter_map(|r| dunce::canonicalize(r).ok()));
    is_library_file(path, search_paths, &all_roots)
}

/// The status bar's read-only switch: makes a library source writable (bundled
/// stubs and hex or image views stay read-only), which also ends reader mode
/// unless it was chosen for this editor. Returns whether it became writable.
pub fn make_writable(state: &mut EditorState) -> bool {
    let library_source = state.language == Language::Python
        && state
            .path
            .as_ref()
            .is_some_and(|p| !p.starts_with("<typeshed>"));
    if !library_source || !state.doc.is_read_only() {
        return false;
    }
    state.doc.set_read_only(state.doc_render.file_read_only);
    state.doc_render.unlocked = true;
    true
}

/// Explicit reader protection, distinct from the filesystem lock.
pub fn library_protected(state: &EditorState, settings: &EditorSettings) -> bool {
    state.doc_render.library_source
        && !state.doc_render.unlocked
        && settings.reader_mode.protect_library_sources
}

/// Refresh a filesystem/viewer guard without allowing reader-mode sync to
/// unlock it. The caller applies any independent library protection afterward.
pub fn file_read_only(state: &EditorState) -> bool {
    state.doc_render.file_read_only
}

pub fn set_file_read_only(state: &mut EditorState, read_only: bool) {
    state.doc_render.file_read_only = read_only;
    state.doc.set_read_only(read_only);
}

/// Once the interpreter's search paths are known, an open and unmodified
/// library source becomes read-only like the ones opened afterwards (editors
/// restored at startup open before the interpreter is found), unless the
/// user made it writable.
pub fn lock_library_file(
    state: &mut EditorState,
    search_paths: &[PathBuf],
    roots: &[PathBuf],
    protect: bool,
) {
    let Some(path) = &state.path else { return };
    if path.starts_with("<typeshed>") {
        return; // Bundled stubs always remain protected.
    }
    state.doc_render.library_source = opens_read_only(path, search_paths, roots);
    if !state.doc_render.library_source || state.doc_render.unlocked {
        return;
    }
    if !protect {
        state.doc.set_read_only(state.doc_render.file_read_only);
    } else if !state.doc.is_modified() {
        state.doc.set_read_only(true);
    }
}

/// Switches reader mode for this editor (it then keeps the choice); the
/// docstrings follow the new mode's "render by default" setting.
pub fn toggle_reader_mode(state: &mut EditorState) {
    if !supported(state) {
        return;
    }
    let dr = &mut state.doc_render;
    let on = !dr.reader;
    dr.reader = on;
    dr.reader_pinned = Some(on);
    dr.settings_default = None;
    if on {
        let regions = regions(state);
        move_carets_out(state, &regions);
    }
}

// ---------------------------------------------------------------------------
// Layout and painting
// ---------------------------------------------------------------------------

/// Colors and fonts of rendered documentation.
pub struct DocStyle<'a> {
    pub scheme: &'a ColorScheme,
    pub font_size: f32,
    /// Identifies the font size and colors in layout cache keys.
    key: u64,
}

impl<'a> DocStyle<'a> {
    pub fn new(scheme: &'a ColorScheme, font_size: f32) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        font_size.to_bits().hash(&mut h);
        scheme.dark.hash(&mut h);
        scheme.editor.background.hash(&mut h);
        scheme.editor.foreground.hash(&mut h);
        scheme.styles.hash(&mut h);
        Self {
            scheme,
            font_size,
            key: h.finish(),
        }
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl DocStyle<'_> {
    fn bg_editor(&self) -> Color32 {
        rgb(self.scheme.editor.background)
    }
    fn fg_editor(&self) -> Color32 {
        rgb(self.scheme.editor.foreground)
    }
    /// Background of a rendered docstring.
    pub fn background(&self) -> Color32 {
        mix(
            self.bg_editor(),
            self.fg_editor(),
            if self.scheme.dark { 0.055 } else { 0.035 },
        )
    }
    /// The bar along the left edge of a rendered docstring.
    pub fn border(&self) -> Color32 {
        mix(self.bg_editor(), self.fg_editor(), 0.22)
    }
    pub fn text(&self) -> Color32 {
        mix(self.fg_editor(), self.bg_editor(), 0.08)
    }
    fn strong(&self) -> Color32 {
        mix(
            self.fg_editor(),
            if self.scheme.dark {
                Color32::WHITE
            } else {
                Color32::BLACK
            },
            0.45,
        )
    }
    fn dim(&self) -> Color32 {
        mix(self.fg_editor(), self.bg_editor(), 0.35)
    }
    fn code_bg(&self) -> Color32 {
        mix(
            self.background(),
            if self.scheme.dark {
                Color32::BLACK
            } else {
                Color32::from_rgb(0x60, 0x70, 0x90)
            },
            if self.scheme.dark { 0.22 } else { 0.08 },
        )
    }
    fn link(&self) -> Color32 {
        if self.scheme.dark {
            Color32::from_rgb(0x58, 0x9D, 0xF6)
        } else {
            Color32::from_rgb(0x28, 0x6C, 0xD0)
        }
    }
    fn line_height(&self) -> f32 {
        (self.font_size * 1.42).round()
    }
}

/// What a laid-out piece of a rendered docstring is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Text,
    /// Section titles (drawn twice, one pixel apart, for a bold look).
    Heading,
    /// Code with a background box.
    Code,
}

/// A laid-out piece of a rendered docstring.
pub struct DocItem {
    /// Top-left relative to the content origin.
    pub pos: Vec2,
    /// The text, laid out again each frame it is drawn or hit-tested: a galley
    /// kept across frames would point into a font atlas that egui rebuilds when
    /// it fills up or the fonts change, while egui's galley cache makes the
    /// repeated layout of a piece drawn every frame cheap.
    job: LayoutJob,
    /// Size of the laid-out text.
    pub size: Vec2,
    pub kind: ItemKind,
    /// Docstring line (relative to its first line) the piece comes from.
    pub line: usize,
    /// Links as `(character range in the text, url)`.
    pub links: Vec<(Range<usize>, String)>,
    image: Option<egui::TextureHandle>,
}

/// A docstring laid out for drawing.
pub struct RenderedDoc {
    pub items: Vec<DocItem>,
    pub width: f32,
    pub height: f32,
}

impl RenderedDoc {
    /// The docstring line (relative to its first line) at `rel` (content coordinates).
    pub fn line_at(&self, rel: Vec2) -> usize {
        self.items
            .iter()
            .take_while(|it| it.pos.y <= rel.y)
            .last()
            .or(self.items.first())
            .map_or(0, |it| it.line)
    }

    /// The URL of the link at `rel`, if any.
    pub fn link_at(&self, ctx: &egui::Context, rel: Vec2) -> Option<&str> {
        self.items
            .iter()
            .filter(|it| !it.links.is_empty())
            .find_map(|it| {
                let local = rel - it.pos;
                if !Rect::from_min_size(Pos2::ZERO, it.size).contains(local.to_pos2()) {
                    return None;
                }
                let galley = ctx.fonts_mut(|f| f.layout_job(it.job.clone()));
                let idx = galley.cursor_from_pos(local).index.0;
                // The cursor lands between characters: check the character under the pointer.
                let glyph_left = galley.pos_from_cursor(CCursor::new(idx)).min.x;
                let ch = if local.x < glyph_left {
                    idx.saturating_sub(1)
                } else {
                    idx
                };
                it.links
                    .iter()
                    .find(|(r, _)| r.contains(&ch))
                    .map(|(_, url)| url.as_str())
            })
    }

    /// Paints the docstring content with its top-left at `origin`.
    pub fn paint(&self, painter: &Painter, origin: Pos2, style: &DocStyle) {
        let text = style.text();
        let clip = painter.clip_rect();
        for it in &self.items {
            let pos = origin + it.pos;
            if !clip.intersects(Rect::from_min_size(pos, it.size).expand(8.0)) {
                continue;
            }
            if let Some(image) = &it.image {
                painter.image(
                    image.id(),
                    Rect::from_min_size(pos, it.size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                continue;
            }
            let galley = painter.layout_job(it.job.clone());
            match it.kind {
                ItemKind::Code => {
                    let r = Rect::from_min_size(pos, it.size).expand2(Vec2::new(6.0, 3.0));
                    let r = Rect::from_min_max(
                        r.min,
                        Pos2::new(origin.x + self.width.max(r.width()), r.max.y),
                    );
                    painter.rect_filled(r, 3.0, style.code_bg());
                    painter.galley(pos, galley, text);
                }
                ItemKind::Heading => {
                    painter.galley(pos, galley.clone(), text);
                    painter.galley(pos + Vec2::new(0.6, 0.0), galley, text);
                }
                ItemKind::Text => painter.galley(pos, galley, text),
            }
        }
    }
}

/// Cache key of a laid-out docstring: its text and the look it was laid out with.
fn layout_key(region: &DocRegion, style: &DocStyle) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    region.hash.hash(&mut h);
    style.key.hash(&mut h);
    h.finish()
}

/// The docstring of `region` laid out `width` pixels wide, cached per editor
/// (call [`retain_layouts`] once a frame to drop the ones no longer drawn).
/// Laying out is costly (egui lays out some 0.5 MB of text a second), so it
/// happens only when `now` allows; otherwise a layout made for another width
/// stands in, if there is one. The flag tells whether the result is up to date.
pub fn layout_region(
    ui: &egui::Ui,
    state: &mut EditorState,
    region: &DocRegion,
    width: f32,
    style: &DocStyle,
    used: &mut HashSet<u64>,
    now: bool,
) -> (Option<Arc<RenderedDoc>>, bool) {
    let blocks = if let Some(blocks) = state.doc_render.blocks.get(&region.hash) {
        blocks.clone()
    } else {
        if state.doc_render.blocks.len() >= 32 {
            state.doc_render.blocks.clear();
        }
        let blocks = Arc::new(doc_blocks::render_literal(&region.literal));
        state.doc_render.blocks.insert(region.hash, blocks.clone());
        blocks
    };
    let mut key = layout_key(region, style);
    if let Some(source) = state.path.as_deref() {
        for block in blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Image { .. }))
            .take(4)
        {
            if let BlockKind::Image { target, .. } = &block.kind {
                key = key.rotate_left(7) ^ crate::documentation_assets::fingerprint(source, target);
            }
        }
    }
    used.insert(key);
    match state.doc_render.layouts.get(&key) {
        Some(doc) if doc.width == width => return (Some(doc.clone()), true),
        cached if !now => return (cached.cloned(), false),
        _ => {}
    }
    let doc = Arc::new(layout_with_source(
        ui,
        &blocks,
        width,
        style,
        state.path.as_deref(),
    ));
    state.doc_render.layouts.insert(key, doc.clone());
    (Some(doc), true)
}

/// Drops cached layouts not used this frame.
pub fn retain_layouts(state: &mut EditorState, used: &HashSet<u64>) {
    if state.doc_render.layouts.len() > used.len() {
        state.doc_render.layouts.retain(|k, _| used.contains(k));
    }
}

/// Appends spans to `job`, recording link ranges (in characters).
fn append_spans(
    job: &mut LayoutJob,
    links: &mut Vec<(Range<usize>, String)>,
    chars: &mut usize,
    spans: &[Span],
    style: &DocStyle,
    base: &TextFormat,
) {
    for s in spans {
        let mut f = base.clone();
        match s.style {
            SpanStyle::Text => {}
            SpanStyle::Emphasis => f.italics = true,
            SpanStyle::Strong => f.color = style.strong(),
            SpanStyle::Code => {
                f.font_id = FontId::new(style.font_size * 0.92, mono_family(false, false));
                f.background = style.code_bg();
            }
        }
        if let Some(url) = &s.link {
            f.color = style.link();
            f.underline = Stroke::new(1.0, style.link());
            let n = s.text.chars().count();
            links.push((*chars..*chars + n, url.clone()));
        }
        *chars += s.text.chars().count();
        job.append(&s.text, 0.0, f);
    }
}

fn append_plain(job: &mut LayoutJob, chars: &mut usize, text: &str, format: TextFormat) {
    *chars += text.chars().count();
    job.append(text, 0.0, format);
}

/// Syntax-highlighted Python (or plain) code.
fn code_job(text: &str, python: bool, style: &DocStyle, width: f32) -> LayoutJob {
    let font = FontId::new(style.font_size * 0.92, mono_family(false, false));
    let fg = rgb(style.scheme.editor.foreground);
    let plain = TextFormat {
        font_id: font.clone(),
        color: fg,
        line_height: Some((style.font_size * 1.3).round()),
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let mut pos = 0;
    if python {
        for s in rustpy_highlight::highlight_python(text) {
            let (a, b) = (
                usize::from(s.range.start()).min(text.len()),
                usize::from(s.range.end()).min(text.len()),
            );
            if a < pos || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                continue;
            }
            if a > pos {
                job.append(&text[pos..a], 0.0, plain.clone());
            }
            let st = style.scheme.style(s.tag);
            let mut f = plain.clone();
            if s.tag != HighlightTag::Default {
                f.color = st.fg.map(rgb).unwrap_or(fg);
                f.font_id = FontId::new(font.size, mono_family(st.bold, st.italic));
            }
            job.append(&text[a..b], 0.0, f);
            pos = b;
        }
    }
    if pos < text.len() {
        job.append(&text[pos..], 0.0, plain);
    }
    job
}

/// Lays out docstring blocks `width` pixels wide.
pub fn layout(ui: &egui::Ui, blocks: &[DocBlock], width: f32, style: &DocStyle) -> RenderedDoc {
    layout_with_source(ui, blocks, width, style, None)
}
pub fn layout_with_source(
    ui: &egui::Ui,
    blocks: &[DocBlock],
    width: f32,
    style: &DocStyle,
    source: Option<&Path>,
) -> RenderedDoc {
    let lh = style.line_height();
    let indent_step = (style.font_size * 1.4).round();
    let base = TextFormat {
        font_id: FontId::new(style.font_size, FontFamily::Proportional),
        color: style.text(),
        line_height: Some(lh),
        ..Default::default()
    };
    let mut items: Vec<DocItem> = Vec::new();
    let mut y = 0.0f32;
    let mut prev: Option<&BlockKind> = None;
    // The size of a piece of text (its galley is not kept: see `DocItem::job`).
    let measure = |job: LayoutJob| {
        let size = ui.fonts_mut(|f| f.layout_job(job.clone())).size();
        (job, size)
    };
    let mut images = 0usize;
    for b in blocks {
        let x = b.depth as f32 * indent_step;
        let w = (width - x).max(40.0);
        let gap = match (&b.kind, prev) {
            (_, None) => 0.0,
            (BlockKind::Heading(_), _) => lh * 0.55,
            // A definition right below its term.
            (_, Some(BlockKind::Term(_))) => 1.0,
            (BlockKind::Field { .. }, Some(BlockKind::Field { .. } | BlockKind::Heading(_))) => 1.0,
            (BlockKind::Item { .. }, Some(BlockKind::Item { .. })) => 1.0,
            (BlockKind::Code { .. }, _) | (_, Some(BlockKind::Code { .. })) => lh * 0.45,
            _ => lh * 0.4,
        };
        y += gap;
        let mut links = Vec::new();
        let mut chars = 0usize;
        match &b.kind {
            BlockKind::Paragraph(spans) | BlockKind::Term(spans) => {
                // A definition-list term shows bold, like a heading.
                let term = matches!(b.kind, BlockKind::Term(_));
                let mut job = LayoutJob::default();
                job.wrap.max_width = w;
                let format = if term {
                    TextFormat {
                        color: style.strong(),
                        ..base.clone()
                    }
                } else {
                    base.clone()
                };
                append_spans(&mut job, &mut links, &mut chars, spans, style, &format);
                let (job, size) = measure(job);
                items.push(DocItem {
                    pos: Vec2::new(x, y),
                    job,
                    size,
                    kind: if term {
                        ItemKind::Heading
                    } else {
                        ItemKind::Text
                    },
                    line: b.line,
                    links,
                    image: None,
                });
                y += size.y;
            }
            BlockKind::Heading(text) => {
                let mut job = LayoutJob::default();
                job.wrap.max_width = w;
                append_plain(
                    &mut job,
                    &mut chars,
                    text,
                    TextFormat {
                        color: style.strong(),
                        ..base.clone()
                    },
                );
                let (job, size) = measure(job);
                items.push(DocItem {
                    pos: Vec2::new(x, y),
                    job,
                    size,
                    kind: ItemKind::Heading,
                    line: b.line,
                    links,
                    image: None,
                });
                y += size.y;
            }
            BlockKind::Field { name, ty, desc } => {
                let mut job = LayoutJob::default();
                job.wrap.max_width = w;
                let mono = |bold: bool, italic: bool| {
                    FontId::new(style.font_size * 0.92, mono_family(bold, italic))
                };
                if !name.is_empty() {
                    append_plain(
                        &mut job,
                        &mut chars,
                        name,
                        TextFormat {
                            font_id: mono(true, false),
                            color: style.strong(),
                            ..base.clone()
                        },
                    );
                }
                if let Some(t) = ty {
                    let shown = if name.is_empty() {
                        t.clone()
                    } else {
                        format!(" ({t})")
                    };
                    if !t.is_empty()
                        && t.len() <= 1024
                        && t.split('.').all(|part| {
                            !part.is_empty()
                                && part.chars().all(|c| c == '_' || c.is_alphanumeric())
                        })
                    {
                        let start = chars + if name.is_empty() { 0 } else { 2 };
                        links.push((start..start + t.chars().count(), format!("python:{t}")));
                    }
                    append_plain(
                        &mut job,
                        &mut chars,
                        &shown,
                        TextFormat {
                            font_id: mono(false, true),
                            color: style.dim(),
                            ..base.clone()
                        },
                    );
                }
                if !desc.is_empty() {
                    let sep = if name.is_empty() && ty.is_none() {
                        ""
                    } else {
                        " – "
                    };
                    append_plain(&mut job, &mut chars, sep, base.clone());
                    append_spans(&mut job, &mut links, &mut chars, desc, style, &base);
                }
                let (job, size) = measure(job);
                items.push(DocItem {
                    pos: Vec2::new(x, y),
                    job,
                    size,
                    kind: ItemKind::Text,
                    line: b.line,
                    links,
                    image: None,
                });
                y += size.y;
            }
            BlockKind::Item { marker, spans } => {
                let mut mjob = LayoutJob::default();
                append_plain(
                    &mut mjob,
                    &mut 0,
                    marker,
                    TextFormat {
                        color: style.dim(),
                        ..base.clone()
                    },
                );
                let (mjob, msize) = measure(mjob);
                let mw = msize.x.max(style.font_size * 0.6) + 6.0;
                items.push(DocItem {
                    pos: Vec2::new(x, y),
                    job: mjob,
                    size: msize,
                    kind: ItemKind::Text,
                    line: b.line,
                    links: Vec::new(),
                    image: None,
                });
                let mut job = LayoutJob::default();
                job.wrap.max_width = (w - mw).max(40.0);
                append_spans(&mut job, &mut links, &mut chars, spans, style, &base);
                let (job, size) = measure(job);
                items.push(DocItem {
                    pos: Vec2::new(x + mw, y),
                    job,
                    size,
                    kind: ItemKind::Text,
                    line: b.line,
                    links,
                    image: None,
                });
                y += size.y;
            }
            BlockKind::Image { target, alt } => {
                images += 1;
                if let Some(image) = source
                    .filter(|_| images <= 4)
                    .and_then(|path| crate::documentation_assets::texture(ui.ctx(), path, target))
                {
                    let native = image.size_vec2();
                    let scale = (w / native.x.max(1.0)).min(1.0);
                    let size = native * scale;
                    items.push(DocItem {
                        pos: Vec2::new(x, y),
                        job: LayoutJob::default(),
                        size,
                        kind: ItemKind::Text,
                        line: b.line,
                        links: vec![],
                        image: Some(image),
                    });
                    y += size.y + lh * 0.4;
                } else {
                    let mut job = LayoutJob::default();
                    job.append(&format!("[Image: {alt}] {target}"), 0.0, base.clone());
                    let size = ui.fonts_mut(|f| f.layout_job(job.clone())).size();
                    items.push(DocItem {
                        pos: Vec2::new(x, y),
                        job,
                        size,
                        kind: ItemKind::Text,
                        line: b.line,
                        links: if doc_blocks::looks_like_url(target) {
                            vec![(
                                0..format!("[Image: {alt}] {target}").chars().count(),
                                target.clone(),
                            )]
                        } else {
                            vec![]
                        },
                        image: None,
                    });
                    y += size.y;
                }
            }
            BlockKind::Code { text, python } => {
                y += 3.0;
                let (job, size) = measure(code_job(text, *python, style, (w - 12.0).max(40.0)));
                items.push(DocItem {
                    pos: Vec2::new(x + 6.0, y),
                    job,
                    size,
                    kind: ItemKind::Code,
                    line: b.line,
                    links,
                    image: None,
                });
                y += size.y + 3.0;
            }
        }
        prev = Some(&b.kind);
    }
    RenderedDoc {
        items,
        width,
        height: y.max(lh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{Edit, EditGroup};

    const SRC: &str = "\"\"\"Module doc.\"\"\"\n\nimport os\n\n\ndef f(x):\n    \"\"\"Do f.\n\n    Args:\n        x: the x\n    \"\"\"\n    return x\n\n\nclass C:\n    '''Class doc.'''\n\n    def g(self): \"\"\"Same-line docstring.\"\"\"\n\n    def h(self):\n        \"\"\"Doc.\"\"\"  # trailing comment\n\n    def k(self):\n        b\"\"\"bytes are not docstrings\"\"\"\n";

    fn editor(src: &str) -> EditorState {
        EditorState::from_text(src, Language::Python)
    }

    fn rendering_on() -> EditorSettings {
        EditorSettings {
            render_doc_comments: true,
            ..Default::default()
        }
    }

    /// First lines of the docstrings drawn rendered now.
    fn shown(s: &mut EditorState) -> Vec<usize> {
        shown_regions(s, &[]).iter().map(|r| r.first_line).collect()
    }

    fn insert(s: &mut EditorState, at: TextSize, text: &str, caret: TextSize) {
        s.apply(
            vec![Edit::insert(at, text)],
            vec![Caret::at(caret)],
            EditGroup::Separate,
            None,
        );
    }

    #[test]
    fn finds_docstrings_on_lines_of_their_own() {
        let mut s = editor(SRC);
        let lines: Vec<(usize, usize)> = regions(&mut s)
            .iter()
            .map(|r| (r.first_line, r.last_line))
            .collect();
        // Module, function and class docstrings; not the one after `def g(self):`, not
        // the one with a trailing comment, not a bytes literal.
        assert_eq!(lines, vec![(0, 0), (6, 10), (15, 15)]);
        assert_eq!(
            &*regions(&mut s)[1].literal,
            "\"\"\"Do f.\n\n    Args:\n        x: the x\n    \"\"\""
        );
        assert!(regions(&mut editor("x = 1\n")).is_empty());
        assert!(regions(&mut EditorState::from_text(
            "\"\"\"Doc.\"\"\"\n",
            Language::PlainText
        ))
        .is_empty());
    }

    #[test]
    fn render_by_default_and_caret_inside_shows_source() {
        let mut s = editor(SRC);
        sync(&mut s, &EditorSettings::default());
        assert!(shown(&mut s).is_empty(), "off by default");
        sync(&mut s, &rendering_on());
        // The caret at offset 0 sits before the module docstring's quotes: all render.
        assert_eq!(shown(&mut s), vec![0, 6, 15]);
        // A caret inside the function docstring shows its source.
        let inside = s.doc.line_range(8).start() + TextSize::new(6);
        s.set_carets(vec![Caret::at(inside)]);
        assert_eq!(shown(&mut s), vec![0, 15]);
        // Folded or hidden regions are not drawn rendered.
        s.set_carets(vec![Caret::at(TextSize::new(0))]);
        assert_eq!(
            shown_regions(&mut s, &[(7, 11)])
                .iter()
                .map(|r| r.first_line)
                .collect::<Vec<_>>(),
            vec![0, 15]
        );
        // Switching the setting off shows every source again.
        sync(&mut s, &EditorSettings::default());
        assert!(shown(&mut s).is_empty());
    }

    #[test]
    fn render_by_default_covers_docstrings_written_later() {
        let mut s = editor(SRC);
        sync(&mut s, &rendering_on());
        // A new function: its docstring shows its source while the caret is in it...
        let end = s.doc.len();
        insert(
            &mut s,
            end,
            "\n\ndef z():\n    \"\"\"New.\n    \"\"\"\n",
            end + TextSize::new(22),
        );
        sync(&mut s, &rendering_on());
        assert_eq!(shown(&mut s), vec![0, 6, 15]);
        // ...and renders once the caret leaves, like the others.
        s.set_carets(vec![Caret::at(TextSize::new(0))]);
        assert_eq!(shown(&mut s), vec![0, 6, 15, 27]);
    }

    #[test]
    fn toggles_flip_what_is_shown() {
        let mut s = editor(SRC);
        sync(&mut s, &EditorSettings::default());
        let inside = s.doc.line_range(9).start() + TextSize::new(9);
        s.set_carets(vec![Caret::at(inside)]);
        // The gutter icon renders the docstring; the caret in it moves before it.
        toggle_at_line(&mut s, 6);
        let r = region_at_line(&mut s, 6).unwrap();
        assert!(is_rendered(&s, r.start));
        assert_eq!(
            s.primary().head(),
            r.start,
            "the caret moves before the docstring"
        );
        assert_eq!(shown(&mut s), vec![6]);
        toggle_at_line(&mut s, 7);
        assert!(!is_rendered(&s, r.start));
        // Ctrl+Alt+Q on the `def` line toggles the docstring below it.
        s.set_carets(vec![Caret::at(s.doc.line_range(5).start())]);
        assert!(toggle_at_caret(&mut s));
        assert_eq!(shown(&mut s), vec![6]);
        // A caret inside shows the source although the docstring is set to render:
        // toggling renders it again (rather than doing nothing visible).
        s.set_carets(vec![Caret::at(inside)]);
        assert!(shown(&mut s).is_empty());
        assert!(toggle_at_caret(&mut s));
        assert_eq!(shown(&mut s), vec![6]);
        assert_eq!(s.primary().head(), r.start);
        // Now on its first line, before the quotes: the next toggle shows the source.
        assert!(toggle_at_caret(&mut s));
        assert!(shown(&mut s).is_empty() && !is_rendered(&s, r.start));
        s.set_carets(vec![Caret::at(s.doc.line_range(2).start())]);
        assert!(!toggle_at_caret(&mut s), "no docstring at `import os`");
    }

    #[test]
    fn render_all_toggles_every_docstring_and_later_ones() {
        let mut s = editor(SRC);
        sync(&mut s, &EditorSettings::default());
        toggle_at_line(&mut s, 15);
        // Some are rendered: all show their source.
        toggle_all(&mut s);
        assert!(shown(&mut s).is_empty());
        // None are rendered: all render, including docstrings written afterwards.
        toggle_all(&mut s);
        assert_eq!(shown(&mut s), vec![0, 6, 15]);
        let end = s.doc.len();
        insert(
            &mut s,
            end,
            "\n\ndef z():\n    \"\"\"New.\"\"\"\n",
            TextSize::new(0),
        );
        sync(&mut s, &EditorSettings::default());
        assert_eq!(shown(&mut s), vec![0, 6, 15, 27]);
    }

    #[test]
    fn rendering_a_folded_docstring_unfolds_it() {
        let mut s = editor(SRC);
        sync(&mut s, &rendering_on());
        // "Fold docstrings by default": the folded function docstring is not drawn rendered.
        s.apply_default_folds(false, true);
        assert_eq!(shown(&mut s), vec![0, 15]);
        // Its gutter icon shows it is not rendered; clicking it renders it.
        toggle_at_line(&mut s, 6);
        assert_eq!(shown(&mut s), vec![0, 6, 15]);
        assert!(
            s.hidden_line_ranges().is_empty(),
            "the docstring fold opened"
        );
    }

    #[test]
    fn switched_docstrings_follow_edits() {
        let mut s = editor(SRC);
        sync(&mut s, &EditorSettings::default());
        toggle_at_line(&mut s, 0);
        toggle_at_line(&mut s, 6);
        // Insert lines above the function: its docstring stays rendered.
        let at = s.doc.line_range(3).start();
        insert(&mut s, at, "X = 1\nY = 2\n", TextSize::new(0));
        sync(&mut s, &EditorSettings::default());
        assert_eq!(shown(&mut s), vec![0, 8]);
        // Indenting the module docstring's line keeps it (the anchor moves with the quotes)...
        insert(&mut s, TextSize::new(0), "  ", TextSize::new(0));
        sync(&mut s, &EditorSettings::default());
        let module_doc = regions(&mut s)[0].clone();
        assert_eq!(
            (module_doc.start, module_doc.first_line),
            (TextSize::new(2), 0)
        );
        assert!(is_rendered(&s, module_doc.start));
        // ...and deleting a docstring's text drops it.
        let r = region_at_line(&mut s, 8).unwrap();
        s.apply(
            vec![Edit::replace(
                rustpy_core::TextRange::new(r.start - TextSize::new(1), r.end),
                String::new(),
            )],
            vec![Caret::at(TextSize::new(0))],
            EditGroup::Separate,
            None,
        );
        sync(&mut s, &EditorSettings::default());
        assert_eq!(
            s.doc_render.flipped.len(),
            1,
            "only the module docstring is still switched"
        );
    }

    #[test]
    fn reader_hides_only_comment_lines_and_keeps_caret_source_visible() {
        let source = "\"\"\"Docs\n# doc text\n\"\"\"\n# first\n    # second\nvalue = \"# string\"  # inline\n# last\n";
        let mut state = editor(source);
        state.carets = vec![Caret::at(state.doc.line_range(5).start())];
        sync(&mut state, &EditorSettings::default());
        assert!(hidden_comment_lines(&mut state).is_empty());
        state.doc.set_read_only(true);
        sync(&mut state, &EditorSettings::default());
        assert_eq!(hidden_comment_lines(&mut state), vec![(3, 4), (6, 6)]);
        assert_eq!(shown(&mut state), vec![0], "docstrings still render");
        state
            .carets
            .push(Caret::at(state.doc.line_range(3).start()));
        assert_eq!(hidden_comment_lines(&mut state), vec![(4, 4), (6, 6)]);
        toggle_reader_mode(&mut state);
        assert!(hidden_comment_lines(&mut state).is_empty());
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn reader_comment_cache_follows_document_edits() {
        let mut state = editor("# old\nvalue = 1\n");
        state.carets = vec![Caret::at(state.doc.line_range(1).start())];
        sync(&mut state, &EditorSettings::default());
        toggle_reader_mode(&mut state);
        assert_eq!(hidden_comment_lines(&mut state), vec![(0, 0)]);
        let prefix = "text = '# literal'\n";
        let caret = state.carets[0].head() + TextSize::new(prefix.len() as u32);
        insert(&mut state, TextSize::new(0), prefix, caret);
        assert_eq!(hidden_comment_lines(&mut state), vec![(1, 1)]);
    }

    #[test]
    fn reader_inline_comments_preserve_code_strings_selection_and_source() {
        let source =
            "\"\"\"Docs # preserved\"\"\"\nvalue = 'é # string'\t# hidden inline\nother = 2\n";
        let mut state = editor(source);
        let code = state.doc.line_range(2).start();
        state.carets = vec![Caret::at(code)];
        sync(&mut state, &EditorSettings::default());
        assert_eq!(hidden_inline_comment_start(&mut state, 1), None);
        state.doc.set_read_only(true);
        sync(&mut state, &EditorSettings::default());
        assert_eq!(hidden_inline_comment_start(&mut state, 0), None);
        assert_eq!(hidden_inline_comment_start(&mut state, 2), None);
        let expected = "value = 'é # string'\t".len();
        assert_eq!(hidden_inline_comment_start(&mut state, 1), Some(expected));
        let range = state.doc.line_range(1);
        state.carets.push(Caret::at(range.start()));
        assert_eq!(hidden_inline_comment_start(&mut state, 1), None);
        state.carets.pop();
        state.carets[0] = Caret::new(range.start() + TextSize::new(expected as u32), code);
        assert!(state.carets[0].has_selection());
        let carets = state.carets.clone();
        assert_eq!(hidden_inline_comment_start(&mut state, 1), Some(expected));
        assert_eq!(state.carets, carets);
        toggle_reader_mode(&mut state);
        assert_eq!(hidden_inline_comment_start(&mut state, 1), None);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn reader_inline_comment_cache_follows_edits() {
        let mut state = editor("value = 1 # comment\nother = 2\n");
        state.carets = vec![Caret::at(state.doc.line_range(1).start())];
        sync(&mut state, &EditorSettings::default());
        toggle_reader_mode(&mut state);
        assert_eq!(hidden_inline_comment_start(&mut state, 0), Some(10));
        let caret = state.carets[0].head() + TextSize::new(2);
        insert(&mut state, TextSize::new(0), "é", caret);
        assert_eq!(hidden_inline_comment_start(&mut state, 0), Some(12));
    }

    #[test]
    fn reader_keeps_folded_custom_region_control_visible() {
        let mut state = editor("# region Example\nvalue = 1\nother = 2\n# endregion\ntail = 3\n");
        state.carets = vec![Caret::at(state.doc.line_range(4).start())];
        sync(&mut state, &EditorSettings::default());
        toggle_reader_mode(&mut state);
        state.toggle_fold_at_line(0);
        let folded = state.hidden_line_ranges();
        assert_eq!(folded, vec![(1, 3)]);
        let comments = hidden_comment_lines(&mut state);
        assert_eq!(comments, vec![(3, 3)], "the region header remains visible");
        assert_eq!(merge_hidden(&folded, comments.into_iter()), vec![(1, 3)]);
        state.toggle_fold_at_line(0);
        assert!(state.hidden_line_ranges().is_empty());
        assert_eq!(hidden_comment_lines(&mut state), vec![(0, 0), (3, 3)]);
        assert_eq!(state.doc.line_text(1), "value = 1");
        assert_eq!(state.doc.line_text(2), "other = 2");
    }

    #[test]
    fn reader_comment_cache_expires_on_equal_version_document_replacement() {
        let mut state = editor("# old\nvalue = 1 # inline\ntail = 2\n");
        state.carets = vec![Caret::at(state.doc.line_range(2).start())];
        sync(&mut state, &EditorSettings::default());
        toggle_reader_mode(&mut state);
        assert_eq!(hidden_comment_lines(&mut state), vec![(0, 0)]);
        assert_eq!(hidden_inline_comment_start(&mut state, 1), Some(10));
        let other = editor("first = 1\nsecond = 2\ntail = 3\n");
        assert_eq!(state.doc.version(), other.doc.version());
        state.doc = other.doc;
        assert!(hidden_comment_lines(&mut state).is_empty());
        assert_eq!(hidden_inline_comment_start(&mut state, 1), None);
    }

    #[test]
    fn reader_mode_for_read_only_files() {
        let mut settings = EditorSettings::default();
        let mut s = editor(SRC);
        s.doc.set_read_only(true);
        sync(&mut s, &settings);
        assert!(reader_mode(&s) && reader_mode_available(&s));
        assert_eq!(shown(&mut s).len(), 3, "reader mode renders docs");
        // Reader mode off in the settings: no longer applies (and docs follow the global option).
        settings.reader_mode.enabled = false;
        sync(&mut s, &settings);
        assert!(!reader_mode(&s));
        assert!(shown(&mut s).is_empty());
        // The user's choice for this editor wins over the settings.
        toggle_reader_mode(&mut s);
        sync(&mut s, &settings);
        assert!(reader_mode(&s));
        assert_eq!(shown(&mut s).len(), 3);
        // Writable files do not start in reader mode.
        let mut w = editor(SRC);
        sync(&mut w, &EditorSettings::default());
        assert!(!reader_mode(&w) && !reader_mode_available(&w));
    }

    #[test]
    fn making_a_library_file_writable_leaves_reader_mode() {
        let settings = EditorSettings::default();
        let mut s = editor(SRC);
        s.doc.set_read_only(true);
        sync(&mut s, &settings);
        assert!(reader_mode(&s));
        // The status bar's read-only switch unlocks the file for editing: its source shows.
        s.doc.set_read_only(false);
        sync(&mut s, &settings);
        assert!(!reader_mode(&s) && !reader_mode_available(&s));
        assert!(shown(&mut s).is_empty());
    }

    #[test]
    fn library_files() {
        let search = vec![
            PathBuf::from("/usr/lib/python3.12"),
            PathBuf::from("/home/u/proj/src"),
        ];
        let roots = vec![PathBuf::from("/home/u/proj")];
        let lib = |p: &str| is_library_file(Path::new(p), &search, &roots);
        assert!(lib("/usr/lib/python3.12/os.py"));
        assert!(lib(
            "/home/u/proj/.venv/lib/python3.12/site-packages/requests/api.py"
        ));
        assert!(lib("C:/Python312/Lib/site-packages/x.py"));
        assert!(lib("<typeshed>/os/path.pyi"));
        // An editable install puts the project's `src` on the search path: still project code.
        assert!(!lib("/home/u/proj/src/pkg/mod.py"));
        assert!(!lib("/home/u/other/x.py"));
        // A project inside a search path is project code.
        assert!(!is_library_file(
            Path::new("/usr/lib/python3.12/mine/a.py"),
            &search,
            &[PathBuf::from("/usr/lib/python3.12/mine")]
        ));
        // Only Python sources open read-only (and so in reader mode).
        assert!(opens_read_only(
            Path::new("/usr/lib/python3.12/os.py"),
            &search,
            &roots
        ));
        assert!(opens_read_only(
            Path::new("/usr/lib/python3.12/site-packages/x/__init__.pyi"),
            &search,
            &roots
        ));
        assert!(!opens_read_only(
            Path::new("/usr/lib/python3.12/site-packages/x/README.md"),
            &search,
            &roots
        ));
        assert!(!opens_read_only(
            Path::new("/home/u/proj/src/pkg/mod.py"),
            &search,
            &roots
        ));
    }

    #[test]
    fn explicit_project_roots_inside_library_named_directories_remain_editable() {
        for component in ["site-packages", "dist-packages"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join(component).join("project");
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("main.py");
            std::fs::write(&path, "x = 1\n").unwrap();
            assert!(!opens_read_only(
                &path,
                std::slice::from_ref(&root),
                std::slice::from_ref(&root)
            ));
            let installed = root.join(".venv/lib/python3.12/site-packages/vendor.py");
            assert!(opens_read_only(
                &installed,
                &[],
                std::slice::from_ref(&root)
            ));
            // An explicitly selected installed package becomes project source.
            assert!(!opens_read_only(
                &installed,
                &[],
                &[root.clone(), installed.parent().unwrap().to_path_buf()]
            ));
            assert!(opens_read_only(
                &dir.path().join(component).join("other.py"),
                &[],
                std::slice::from_ref(&root)
            ));
            let mut state = editor("x = 1\n");
            state.path = Some(path.clone());
            lock_library_file(
                &mut state,
                std::slice::from_ref(&root),
                std::slice::from_ref(&root),
                true,
            );
            assert!(!state.doc.is_read_only());
            assert!(opens_read_only(
                Path::new("<typeshed>/abc.pyi"),
                &[],
                &[PathBuf::from("<typeshed>")]
            ));
        }
    }

    #[test]
    fn library_editors_lock_once_the_interpreter_is_known_and_unlock_on_request() {
        let search = vec![PathBuf::from("/usr/lib/python3.12")];
        let roots = vec![PathBuf::from("/home/u/proj")];
        let at = |path: &str| {
            let mut s = editor(SRC);
            s.path = Some(PathBuf::from(path));
            s
        };
        // A standard-library editor restored before the interpreter was found.
        let mut s = at("/usr/lib/python3.12/textwrap.py");
        lock_library_file(&mut s, &search, &roots, true);
        assert!(s.doc.is_read_only());
        // The status bar switch makes it writable, and it stays so when the interpreter changes.
        assert!(make_writable(&mut s));
        assert!(!s.doc.is_read_only());
        lock_library_file(&mut s, &search, &roots, true);
        assert!(!s.doc.is_read_only());
        // Project files, modified editors and bundled stubs are left alone.
        let mut p = at("/home/u/proj/a.py");
        lock_library_file(&mut p, &search, &roots, true);
        assert!(!p.doc.is_read_only());
        let mut m = at("/usr/lib/python3.12/m.py");
        insert(&mut m, TextSize::new(0), "# x\n", TextSize::new(0));
        lock_library_file(&mut m, &search, &roots, true);
        assert!(!m.doc.is_read_only());
        let mut stub = at("<typeshed>/os/path.pyi");
        stub.doc.set_read_only(true);
        assert!(!make_writable(&mut stub));
        assert!(stub.doc.is_read_only());
    }

    #[test]
    fn global_library_editing_policy_is_independent_of_reader_mode_and_survives_reload() {
        let mut s = editor(SRC);
        s.path = Some("/tmp/site-packages/demo.py".into());
        let mut settings = EditorSettings::default();
        lock_library_file(&mut s, &[], &[], true);
        sync(&mut s, &settings);
        assert!(s.doc.is_read_only() && reader_mode(&s));
        settings.reader_mode.enabled = false;
        sync(&mut s, &settings);
        assert!(
            s.doc.is_read_only() && !reader_mode(&s),
            "rendering preference does not remove protection"
        );
        settings.reader_mode.protect_library_sources = false;
        sync(&mut s, &settings);
        assert!(!s.doc.is_read_only());
        let src = s.doc.text();
        s.doc.reset(&src);
        lock_library_file(&mut s, &[], &[], false);
        sync(&mut s, &settings);
        assert!(
            !s.doc.is_read_only(),
            "reload and interpreter discovery honor the global policy"
        );
        settings.reader_mode.enabled = true;
        sync(&mut s, &settings);
        assert!(
            reader_mode(&s) && !s.doc.is_read_only(),
            "editing and reader presentation are independent"
        );
        let mut reopened = editor(SRC);
        reopened.path = s.path.clone();
        lock_library_file(&mut reopened, &[], &[], false);
        sync(&mut reopened, &settings);
        assert!(
            !reopened.doc.is_read_only(),
            "the persisted setting applies to reopened files"
        );
        settings.reader_mode.protect_library_sources = true;
        sync(&mut s, &settings);
        assert!(
            s.doc.is_read_only(),
            "reenabling protection locks clean library sources"
        );
        let mut stub = editor(SRC);
        stub.path = Some("<typeshed>/demo.pyi".into());
        stub.doc.set_read_only(true);
        settings.reader_mode.protect_library_sources = false;
        lock_library_file(&mut stub, &[], &[], false);
        sync(&mut stub, &settings);
        assert!(stub.doc.is_read_only(), "bundled stubs remain protected");
    }

    #[test]
    fn reenabling_library_protection_preserves_unsaved_edits_and_explicit_unlocks() {
        let mut s = editor(SRC);
        s.path = Some("/tmp/site-packages/demo.py".into());
        lock_library_file(&mut s, &[], &[], false);
        insert(&mut s, TextSize::new(0), "# edited\n", TextSize::new(0));
        sync(&mut s, &EditorSettings::default());
        assert!(!s.doc.is_read_only() && s.doc.is_modified());
        let mut unlocked = editor(SRC);
        unlocked.path = s.path.clone();
        lock_library_file(&mut unlocked, &[], &[], true);
        assert!(make_writable(&mut unlocked));
        sync(&mut unlocked, &EditorSettings::default());
        assert!(
            !unlocked.doc.is_read_only(),
            "the explicit per-file unlock remains honored"
        );
    }

    #[test]
    fn enabling_rendering_reveals_existing_docstring_folds() {
        let mut s = editor(SRC);
        s.apply_default_folds(false, true);
        assert!(!s.hidden_line_ranges().is_empty());
        s.doc.set_read_only(true);
        sync(&mut s, &EditorSettings::default());
        assert!(reader_mode(&s) && s.hidden_line_ranges().is_empty());
        assert_eq!(shown(&mut s), vec![0, 6, 15]);
    }

    #[test]
    fn layouts_are_made_when_allowed_and_stand_in_for_other_widths() {
        let mut s = editor(SRC);
        let region = regions(&mut s)[1].clone();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let scheme = ColorScheme::dark();
        let full = ctx.run_ui(egui::RawInput::default(), |ui| {
            let style = DocStyle::new(&scheme, 13.0);
            let mut used = HashSet::new();
            // Out of time this frame: nothing to show yet (the docstring shows its source).
            assert!(matches!(
                layout_region(ui, &mut s, &region, 400.0, &style, &mut used, false),
                (None, false)
            ));
            let (doc, fresh) = layout_region(ui, &mut s, &region, 400.0, &style, &mut used, true);
            assert!(fresh && doc.is_some_and(|d| d.width == 400.0));
            // Another width: the old layout stands in until there is time to lay it out again.
            let (doc, fresh) = layout_region(ui, &mut s, &region, 480.0, &style, &mut used, false);
            assert!(!fresh && doc.is_some_and(|d| d.width == 400.0));
            let (doc, fresh) = layout_region(ui, &mut s, &region, 480.0, &style, &mut used, true);
            assert!(fresh && doc.is_some_and(|d| d.width == 480.0));
            // Another font size or color scheme needs its own layout.
            assert!(matches!(
                layout_region(
                    ui,
                    &mut s,
                    &region,
                    480.0,
                    &DocStyle::new(&scheme, 16.0),
                    &mut used,
                    false
                ),
                (None, false)
            ));
            let light = ColorScheme::light();
            assert!(matches!(
                layout_region(
                    ui,
                    &mut s,
                    &region,
                    480.0,
                    &DocStyle::new(&light, 13.0),
                    &mut used,
                    false
                ),
                (None, false)
            ));
        });
        full.drop_without_applying_deltas();
        // Layouts not used in a frame are dropped.
        retain_layouts(&mut s, &HashSet::new());
        assert!(s.doc_render.layouts.is_empty());
    }

    #[test]
    fn layout_maps_clicks_to_source_lines() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let scheme = ColorScheme::dark();
        let mut out = None;
        let full = ctx.run_ui(egui::RawInput::default(), |ui| {
            let style = DocStyle::new(&scheme, 13.0);
            let blocks = doc_blocks::render_literal("\"\"\"Summary line.\n\n    Args:\n        x (int): the `x` and https://example.org\n    \"\"\"");
            out = Some(layout(ui, &blocks, 400.0, &style));
        });
        full.drop_without_applying_deltas();
        let doc = out.unwrap();
        assert!(doc.height > 30.0);
        assert_eq!(doc.line_at(Vec2::new(5.0, 2.0)), 0);
        assert_eq!(doc.line_at(Vec2::new(5.0, doc.height - 2.0)), 3);
        // The URL at the end of the field is a link.
        let field = doc.items.last().unwrap();
        let end = field.size;
        assert_eq!(
            doc.link_at(&ctx, field.pos + Vec2::new(end.x - 4.0, end.y / 2.0)),
            Some("https://example.org")
        );
        assert_eq!(
            doc.link_at(&ctx, field.pos + Vec2::new(2.0, end.y / 2.0)),
            None
        );
    }
}

/// Source-backed section offsets; navigation never modifies the document.
pub fn sections(state: &mut EditorState) -> Vec<(String, TextSize)> {
    let mut result = Vec::new();
    for region in regions(state).iter() {
        for block in doc_blocks::render_literal(&region.literal) {
            if let BlockKind::Heading(title) = block.kind {
                result.push((
                    title,
                    source_offset(state, region, region.first_line + block.line),
                ));
            }
        }
    }
    result.sort_by_key(|(_, offset)| *offset);
    result.dedup_by_key(|(_, offset)| *offset);
    result
}
pub fn navigate_section(state: &mut EditorState, forward: bool) {
    let caret = state.primary().head();
    let all = sections(state);
    let target = if forward {
        all.iter().find(|(_, at)| *at > caret).or(all.first())
    } else {
        all.iter().rev().find(|(_, at)| *at < caret).or(all.last())
    };
    if let Some((_, at)) = target {
        state.reveal_offset(*at);
        state.set_carets(vec![Caret::at(*at)]);
        state.scroll_to_caret = true;
    }
}

#[cfg(test)]
mod section_navigation_tests {
    use super::*;
    #[test]
    fn sections_use_absolute_unicode_crlf_source_lines_without_editing_readonly_docs() {
        let src="# π\r\n\r\ndef f():\r\n    \"\"\"Summary.\r\n\r\n    Args:\r\n        value: π\r\n\r\n    Returns:\r\n        str: value\r\n    \"\"\"\r\n    pass\r\n";
        let mut state = EditorState::from_text(src, Language::Python);
        state.doc.set_read_only(true);
        let version = state.doc.version();
        let text = state.doc.text();
        let bytes = state.doc.to_bytes().unwrap();
        assert_eq!(state.doc.line_ending(), rustpy_text::LineEnding::CrLf);
        assert_eq!(bytes, src.as_bytes());
        let found = sections(&mut state);
        assert_eq!(
            found
                .iter()
                .map(|(title, _)| title.as_str())
                .collect::<Vec<_>>(),
            vec!["Parameters", "Returns"]
        );
        assert_eq!(state.doc.line_of(found[0].1), 5);
        assert_eq!(state.doc.line_of(found[1].1), 8);
        // Navigation uses absolute UTF-8 offsets in the normalized source,
        // including the Unicode prefix and indentation before each heading.
        assert_eq!(usize::from(found[0].1), text.find("Args:").unwrap());
        assert_eq!(usize::from(found[1].1), text.find("Returns:").unwrap());
        navigate_section(&mut state, true);
        assert_eq!(state.primary().head(), found[0].1);
        navigate_section(&mut state, true);
        assert_eq!(state.primary().head(), found[1].1);
        navigate_section(&mut state, false);
        assert_eq!(state.primary().head(), found[0].1);
        assert!(state.doc.is_read_only());
        assert_eq!(state.doc.version(), version);
        assert_eq!(state.doc.text(), text);
        assert_eq!(state.doc.line_ending(), rustpy_text::LineEnding::CrLf);
        assert_eq!(state.doc.to_bytes().unwrap(), bytes);
    }
}

#[cfg(test)]
mod rendered_resource_paint_tests {
    use super::*;
    #[test]
    fn actual_shared_layout_paints_image_role_code_table_and_admonition_without_editing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.py");
        let literal="\"\"\"See :class:`Thing`.\n\n.. image:: diagram.png\n   :alt: Diagram\n\n.. warning:: Caution.\n\n```python\nvalue = 1\n```\n\n+---+---+\n| x | y |\n+===+===+\n| 1 | 2 |\n+---+---+\n\"\"\"";
        std::fs::write(&path, literal).unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .save(dir.path().join("diagram.png"))
            .unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let scheme = ColorScheme::dark();
        let mut result = None;
        let full = ctx.run_ui(egui::RawInput::default(), |root_ui| {
            egui::CentralPanel::default().show(root_ui, |ui| {
                let blocks = doc_blocks::render_literal(literal);
                let doc = layout_with_source(
                    ui,
                    &blocks,
                    500.0,
                    &DocStyle::new(&scheme, 13.0),
                    Some(&path),
                );
                result = Some((
                    doc.items.iter().filter(|item| item.image.is_some()).count(),
                    doc.items.iter().any(|item| {
                        item.links
                            .iter()
                            .any(|(_, target)| target == "python:Thing")
                    }),
                    doc.items
                        .iter()
                        .filter(|item| item.kind == ItemKind::Code)
                        .count(),
                    doc.items.iter().any(|item| item.kind == ItemKind::Heading),
                ));
                doc.paint(
                    ui.painter(),
                    ui.min_rect().min,
                    &DocStyle::new(&scheme, 13.0),
                );
            });
        });
        full.drop_without_applying_deltas();
        let (images, role, codes, heading) = result.unwrap();
        assert_eq!(images, 1);
        assert!(role);
        assert!(codes >= 2);
        assert!(heading);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), literal);
    }
}

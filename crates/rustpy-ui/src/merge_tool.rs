//! `rustpy diff` and `rustpy merge` (roadmap §8 "Command-line launcher"): the
//! side-by-side diff viewer and a three-way merge window, on their own without
//! a project, for `git difftool` / `git mergetool` and other tools.
//!
//! The merge result starts as Git's three-way merge (`git merge-file`, with the
//! base lines in each conflict) when a base is given, and otherwise with every
//! difference between the two files as a conflict. Changes made on one side
//! only are merged; conflicts are resolved with Accept Left / Right / Both /
//! Base or by editing the result, and Apply writes it to the output file.

use crate::a11y::DialogWindow;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use rustpy_cli::args::{DiffArgs, MergeArgs};
use rustpy_text::LineEnding;
use similar::{ChangeTag, TextDiff};

use crate::diff_view::SideBySide;
use crate::theme::UiTheme;

/// Labels of the conflict markers.
pub const LEFT_LABEL: &str = "left";
pub const BASE_LABEL: &str = "base";
pub const RIGHT_LABEL: &str = "right";

/// A conflict block in the merge result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Bytes of the whole block, markers included.
    pub range: Range<usize>,
    /// 1-based line of its `<<<<<<<` marker.
    pub line: usize,
    pub left: String,
    pub base: Option<String>,
    pub right: String,
}

/// How to resolve a conflict.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Left,
    Right,
    /// Left, then right.
    Both,
    Base,
}

fn is_marker(line: &str, marker: &str) -> bool {
    line.strip_prefix(marker)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\r', '\n']))
}

/// The conflict blocks of `text` (Git's markers, optionally with a base section).
pub fn conflicts(text: &str) -> Vec<Conflict> {
    #[derive(PartialEq)]
    enum State {
        Outside,
        Left,
        Base,
        Right,
    }
    let mut out = Vec::new();
    let mut state = State::Outside;
    let (mut start, mut start_line) = (0usize, 0usize);
    let (mut left, mut base, mut right) = (String::new(), None::<String>, String::new());
    let mut offset = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let line_start = offset;
        offset += line.len();
        match state {
            State::Outside => {
                if is_marker(line, "<<<<<<<") {
                    state = State::Left;
                    start = line_start;
                    start_line = i + 1;
                    left.clear();
                    right.clear();
                    base = None;
                }
            }
            State::Left => {
                if is_marker(line, "|||||||") {
                    state = State::Base;
                    base = Some(String::new());
                } else if is_marker(line, "=======") {
                    state = State::Right;
                } else if is_marker(line, "<<<<<<<") {
                    // An unterminated block: start over at this one.
                    start = line_start;
                    start_line = i + 1;
                    left.clear();
                } else {
                    left.push_str(line);
                }
            }
            State::Base => {
                if is_marker(line, "=======") {
                    state = State::Right;
                } else if let Some(b) = &mut base {
                    b.push_str(line);
                }
            }
            State::Right => {
                if is_marker(line, ">>>>>>>") {
                    out.push(Conflict {
                        range: start..offset,
                        line: start_line,
                        left: left.clone(),
                        base: base.clone(),
                        right: right.clone(),
                    });
                    state = State::Outside;
                } else {
                    right.push_str(line);
                }
            }
        }
    }
    out
}

/// `text` with `conflict` replaced by the chosen side(s).
pub fn resolve(text: &str, conflict: &Conflict, how: Resolution) -> String {
    let replacement = match how {
        Resolution::Left => conflict.left.clone(),
        Resolution::Right => conflict.right.clone(),
        Resolution::Both => format!("{}{}", conflict.left, conflict.right),
        Resolution::Base => conflict.base.clone().unwrap_or_default(),
    };
    let range = conflict.range.start.min(text.len())..conflict.range.end.min(text.len());
    format!(
        "{}{}{}",
        &text[..range.start],
        replacement,
        &text[range.end..]
    )
}

fn push_side(out: &mut String, lines: &str) {
    out.push_str(lines);
    if !lines.is_empty() && !lines.ends_with('\n') {
        out.push('\n');
    }
}

/// Every difference between `left` and `right` as a conflict (a merge without
/// a base); returns the text and the number of conflicts.
pub fn two_way(left: &str, right: &str) -> (String, usize) {
    let diff = TextDiff::from_lines(left, right);
    let mut out = String::new();
    let (mut l, mut r) = (String::new(), String::new());
    let mut n = 0;
    let flush = |out: &mut String, l: &mut String, r: &mut String, n: &mut usize| {
        if l.is_empty() && r.is_empty() {
            return;
        }
        *n += 1;
        out.push_str(&format!("<<<<<<< {LEFT_LABEL}\n"));
        push_side(out, l);
        out.push_str("=======\n");
        push_side(out, r);
        out.push_str(&format!(">>>>>>> {RIGHT_LABEL}\n"));
        l.clear();
        r.clear();
    };
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            match change.tag() {
                ChangeTag::Equal => {
                    flush(&mut out, &mut l, &mut r, &mut n);
                    out.push_str(change.value());
                }
                ChangeTag::Delete => l.push_str(change.value()),
                ChangeTag::Insert => r.push_str(change.value()),
            }
        }
    }
    flush(&mut out, &mut l, &mut r, &mut n);
    (out, n)
}

/// The merge result the window starts with, and its number of conflicts.
pub fn initial_result(left: &str, right: &str, base: Option<&str>) -> (String, usize) {
    match base {
        Some(base) => match rustpy_vcs::merge_texts(
            base,
            left,
            right,
            [LEFT_LABEL, BASE_LABEL, RIGHT_LABEL],
            true,
        ) {
            Ok((text, _)) => {
                let n = conflicts(&text).len();
                (text, n)
            }
            Err(e) => {
                log::warn!("three-way merge failed ({e}); every difference becomes a conflict");
                two_way(left, right)
            }
        },
        None => two_way(left, right),
    }
}

/// A file's text for the diff and merge windows (`None`: binary), with `\n` line breaks.
fn read_text(path: &Path) -> anyhow::Result<Option<(String, LineEnding, rustpy_text::Encoding)>> {
    if path.is_dir() {
        anyhow::bail!(
            "{} is a folder; comparing folders is not supported",
            path.display()
        );
    }
    let snapshot = rustpy_core::vfs::Vfs::new()
        .read(path, merge_read_limits())
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let bytes = snapshot.bytes.as_ref();
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    if !utf16 && bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(None);
    }
    let (text, encoding) =
        rustpy_text::Encoding::decode_with_policy(bytes, rustpy_text::SourcePolicy::Generic, None)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let (ending, _) = LineEnding::detect(&text);
    Ok(Some((
        LineEnding::normalize_to_lf(&text).into_owned(),
        ending,
        encoding,
    )))
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn native_options(title: &str) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([600.0, 360.0])
            .with_app_id("rustpy"),
        // Their own state file: the IDE's window layout (ui_state) stays untouched.
        persist_window: false,
        persistence_path: Some(rustpy_settings::Settings::config_dir().join("ui_state_tools")),
        ..Default::default()
    }
}

fn setup(ctx: &egui::Context, theme: &UiTheme) {
    crate::theme::install_fonts(ctx);
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    theme.apply(ctx);
}

/// `--screenshot <png>` (headless UI tests): captures the window once
/// `RUSTPY_SMOKE_DELAY` seconds (default 2.5) have passed, then closes it.
struct SmokeShot {
    path: PathBuf,
    requested: bool,
}

impl SmokeShot {
    fn new(path: Option<PathBuf>) -> Option<Self> {
        path.map(|path| Self {
            path,
            requested: false,
        })
    }

    /// Returns true when the window should close.
    fn tick(&mut self, ctx: &egui::Context) -> bool {
        ctx.request_repaint();
        let elapsed = ctx.input(|i| i.time);
        let delay = std::env::var("RUSTPY_SMOKE_DELAY")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(2.5);
        if elapsed > delay && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let buf: Vec<u8> = img
                .pixels
                .iter()
                .flat_map(|c| [c.r(), c.g(), c.b(), 255u8])
                .collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, buf)
                .map(|rgba| rgba.save(&self.path))
            {
                Some(Ok(())) => log::info!("screenshot saved to {}", self.path.display()),
                Some(Err(e)) => log::error!("cannot save screenshot: {e}"),
                None => log::error!("screenshot buffer size mismatch"),
            }
            return true;
        }
        if elapsed > 20.0 {
            log::error!("screenshot timed out");
            return true;
        }
        false
    }
}

fn editor_font() -> (UiTheme, FontId) {
    let settings = rustpy_settings::Settings::load();
    crate::diff_view::configure(&settings.diff_merge);
    let theme = UiTheme::for_name(&settings.appearance.theme);
    let mono = FontId::new(
        settings.editor.font_size.max(10.0),
        crate::theme::mono_family(false, false),
    );
    (theme, mono)
}

/// One pair of files in the diff view.
struct DiffPane {
    tab: String,
    left_title: String,
    right_title: String,
    diff: SideBySide,
}

fn pane(tab: &str, a: (&str, &str), b: (&str, &str)) -> DiffPane {
    DiffPane {
        tab: tab.to_string(),
        left_title: a.0.to_string(),
        right_title: b.0.to_string(),
        diff: SideBySide::new(a.1, b.1),
    }
}

/// Tab strip for several panes; returns the selected one.
fn pane_tabs(ui: &mut egui::Ui, panes: &[DiffPane], selected: &mut usize) {
    if panes.len() > 1 {
        ui.horizontal(|ui| {
            for (i, p) in panes.iter().enumerate() {
                if ui
                    .selectable_label(*selected == i, format!("{} ({})", p.tab, p.diff.changes))
                    .clicked()
                {
                    *selected = i;
                }
            }
        });
    }
}

struct DiffApp {
    directory: Option<crate::app::directory_diff::DirectoryDiff>,
    draining: Vec<crate::app::directory_diff::DirectoryJob>,
    closing: bool,
    theme: UiTheme,
    mono: FontId,
    panes: Vec<DiffPane>,
    selected: usize,
    /// Shown instead of the panes (binary files).
    message: Option<String>,
    header: String,
    shot: Option<SmokeShot>,
}

impl eframe::App for DiffApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let shot_done = self.shot.as_mut().is_some_and(|s| s.tick(ui.ctx()));
        let native_close = ui.input(|input| input.viewport().close_requested());
        if native_close {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.closing |= shot_done || native_close || ui.input(|i| i.key_pressed(egui::Key::Escape));
        if self.closing {
            if let Some(mut directory) = self.directory.take() {
                if let Some(job) = directory.take_job_for_drain() {
                    self.draining.push(job);
                }
            }
            self.draining.retain_mut(|job| !job.drained());
            if self.draining.is_empty() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(20));
            }
        }
        let theme = self.theme.clone();
        egui::Panel::top("diff_header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.header).strong());
            if let Some(p) = self.panes.get(self.selected) {
                let text = if p.diff.changes == 0 {
                    "Contents are identical".to_string()
                } else {
                    format!("{} changed line(s)", p.diff.changes)
                };
                ui.label(egui::RichText::new(text).color(theme.text_dim));
            }
            pane_tabs(ui, &self.panes, &mut self.selected);
            ui.add_space(2.0);
        });
        if let Some(directory) = &mut self.directory {
            let selected = egui::Panel::left("standalone_directory_inventory")
                .resizable(true)
                .default_size(460.0)
                .show(ui, |ui| directory.show(ui))
                .inner;
            if let Some((left, right)) = selected {
                match prepare_diff(&DiffArgs {
                    left,
                    right,
                    base: None,
                    screenshot: None,
                }) {
                    Ok(preview) => {
                        self.panes = preview.panes;
                        self.message = preview.message;
                        self.selected = 0;
                    }
                    Err(error) => self.message = Some(error.to_string()),
                }
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            match (&self.message, self.panes.get(self.selected)) {
                (Some(m), _) => {
                    ui.label(m);
                }
                (None, Some(p)) => {
                    p.diff
                        .ui(ui, &theme, self.mono.clone(), &p.left_title, &p.right_title)
                }
                (None, None) => {}
            }
        });
    }
}

/// `rustpy diff <left> <right> [<base>]`: shows the diff until the window is
/// closed. Exit status 0, or an error (status 2) when a file cannot be read.
fn prepare_diff(args: &DiffArgs) -> anyhow::Result<DiffApp> {
    if args.left.is_dir() || args.right.is_dir() {
        anyhow::ensure!(
            args.left.is_dir() && args.right.is_dir(),
            "Compare two directories or two files"
        );
        anyhow::ensure!(
            args.base.is_none(),
            "Directory comparison takes two roots; a third base is only supported for files"
        );
        let (theme, mono) = editor_font();
        return Ok(DiffApp {
            directory: Some(crate::app::directory_diff::DirectoryDiff::with_domain(
                rustpy_core::TaskManager::global(),
                &rustpy_core::CancellationToken::new(),
                args.left.clone(),
                args.right.clone(),
            )),
            draining: Vec::new(),
            closing: false,
            theme,
            mono,
            panes: Vec::new(),
            selected: 0,
            message: None,
            header: format!("{} ↔ {}", args.left.display(), args.right.display()),
            shot: SmokeShot::new(args.screenshot.clone()),
        });
    }
    let mut files = vec![("Left", args.left.clone()), ("Right", args.right.clone())];
    if let Some(b) = &args.base {
        files.push(("Base", b.clone()));
    }
    let mut texts = Vec::new();
    for (_, path) in &files {
        texts.push(read_text(path)?);
    }
    let names: Vec<String> = files.iter().map(|(_, p)| p.display().to_string()).collect();
    let (theme, mono) = editor_font();
    let header = match &args.base {
        Some(_) => format!(
            "Left: {}   Base: {}   Right: {}",
            names[0], names[2], names[1]
        ),
        None => format!("{}  ↔  {}", names[0], names[1]),
    };
    let mut app = DiffApp {
        directory: None,
        draining: Vec::new(),
        closing: false,
        theme,
        mono,
        panes: Vec::new(),
        selected: 0,
        message: None,
        header,
        shot: SmokeShot::new(args.screenshot.clone()),
    };
    if texts.iter().any(Option::is_none) {
        let sizes: Vec<String> = files
            .iter()
            .map(|(label, p)| {
                format!(
                    "{label}: {} bytes",
                    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
                )
            })
            .collect();
        let identities = files
            .iter()
            .map(|(_, path)| merge_output_revision(path))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let same = identities
            .windows(2)
            .all(|pair| pair[0].is_some() && pair[0] == pair[1]);
        app.message = Some(format!(
            "{} ({})",
            if same {
                "Binary files are identical"
            } else {
                "Binary files differ"
            },
            sizes.join(", ")
        ));
    } else {
        let t: Vec<String> = texts
            .into_iter()
            .map(|t| t.map(|(s, _, _)| s).unwrap_or_default())
            .collect();
        let (l, r) = (t[0].as_str(), t[1].as_str());
        let left = (names[0].as_str(), l);
        let right = (names[1].as_str(), r);
        match t.get(2) {
            Some(b) => {
                let base = (names[2].as_str(), b.as_str());
                app.panes.push(pane("Left | Base", left, base));
                app.panes.push(pane("Base | Right", base, right));
                app.panes.push(pane("Left | Right", left, right));
            }
            None => app.panes.push(pane("Left | Right", left, right)),
        }
    }
    Ok(app)
}

/// Standalone files or directory inventory with guarded synchronization and file previews.
pub fn run_diff(args: DiffArgs) -> anyhow::Result<i32> {
    let app = prepare_diff(&args)?;
    let title = format!(
        "{} vs {} – RustPy Diff",
        file_title(&args.left),
        file_title(&args.right)
    );
    let theme = app.theme.clone();
    eframe::run_native(
        "RustPy Diff",
        native_options(&title),
        Box::new(move |cc| {
            setup(&cc.egui_ctx, &theme);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(0)
}

/// Builds the colored layout of the merge result: marker lines and the lines
/// of each side get their own background.
fn result_layout(text: &str, mono: &FontId, theme: &UiTheme) -> LayoutJob {
    let (left_bg, base_bg, right_bg, marker_bg) = if theme.dark {
        (
            Color32::from_rgb(0x2D, 0x3B, 0x52),
            Color32::from_rgb(0x3A, 0x3A, 0x3A),
            Color32::from_rgb(0x29, 0x45, 0x2B),
            Color32::from_rgb(0x4B, 0x2B, 0x2B),
        )
    } else {
        (
            Color32::from_rgb(0xDD, 0xE8, 0xFA),
            Color32::from_rgb(0xEE, 0xEE, 0xEE),
            Color32::from_rgb(0xD8, 0xF5, 0xD8),
            Color32::from_rgb(0xFF, 0xD8, 0xD8),
        )
    };
    let mut job = LayoutJob::default();
    let mut bg: Option<Color32> = None;
    for line in text.split_inclusive('\n') {
        let (format_bg, color) = if is_marker(line, "<<<<<<<") {
            bg = Some(left_bg);
            (marker_bg, theme.error)
        } else if is_marker(line, "|||||||") && bg.is_some() {
            bg = Some(base_bg);
            (marker_bg, theme.error)
        } else if is_marker(line, "=======") && bg.is_some() {
            bg = Some(right_bg);
            (marker_bg, theme.error)
        } else if is_marker(line, ">>>>>>>") && bg.is_some() {
            bg = None;
            (marker_bg, theme.error)
        } else {
            (bg.unwrap_or(Color32::TRANSPARENT), theme.text)
        };
        job.append(
            line,
            0.0,
            TextFormat {
                font_id: mono.clone(),
                color,
                background: format_bg,
                ..Default::default()
            },
        );
    }
    if text.is_empty() {
        job.append(
            "",
            0.0,
            TextFormat {
                font_id: mono.clone(),
                color: theme.text,
                ..Default::default()
            },
        );
    }
    job
}

struct MergeApp {
    visual: crate::diff_view::merge::MergeViewer,
    advanced: bool,
    three_way: bool,
    theme: UiTheme,
    mono: FontId,
    output: PathBuf,
    expected_output: Option<rustpy_core::vfs::ContentRevision>,
    header: String,
    left: String,
    right: String,
    initial: String,
    result: String,
    line_ending: LineEnding,
    encoding: rustpy_text::Encoding,
    panes: Vec<DiffPane>,
    selected: usize,
    /// Asking whether to apply a result that still has conflicts.
    confirm_apply: bool,
    error: Option<String>,
    exit: Arc<AtomicI32>,
    /// `(text, job)` of the last layout of the result.
    layout_cache: Option<(String, LayoutJob)>,
    shot: Option<SmokeShot>,
}

fn merge_output_revision(path: &Path) -> anyhow::Result<Option<rustpy_core::vfs::ContentRevision>> {
    match rustpy_core::vfs::Vfs::new().read(path, merge_read_limits()) {
        Ok(snapshot) => Ok(Some(snapshot.revision)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn merge_read_limits() -> rustpy_core::vfs::ReadLimits {
    rustpy_core::vfs::ReadLimits {
        max_file_bytes: 32 * 1024 * 1024,
        max_cached_bytes: 0,
        max_cached_files: 0,
    }
}
#[cfg(test)]
fn write_merge_result(
    path: &Path,
    result: &str,
    ending: LineEnding,
    encoding: rustpy_text::Encoding,
) -> anyhow::Result<()> {
    write_captured_merge_result(path, result, ending, encoding, merge_output_revision(path)?)
}
fn write_captured_merge_result(
    path: &Path,
    result: &str,
    ending: LineEnding,
    encoding: rustpy_text::Encoding,
    expected: Option<rustpy_core::vfs::ContentRevision>,
) -> anyhow::Result<()> {
    let bytes =
        encoding.encode_with_policy(&ending.apply(result), rustpy_text::SourcePolicy::Generic)?;
    crate::editor::save_actions::checked_write(
        &rustpy_core::vfs::Vfs::new(),
        path,
        &bytes,
        expected,
        merge_read_limits(),
    )?;
    Ok(())
}

impl MergeApp {
    fn apply(&mut self, ctx: &egui::Context) {
        match write_captured_merge_result(
            &self.output,
            &self.result,
            self.line_ending,
            self.encoding,
            self.expected_output,
        ) {
            Ok(()) => {
                self.exit.store(0, Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = Some(format!("Cannot write {}: {e}", self.output.display())),
        }
    }

    fn cancel(&mut self, ctx: &egui::Context) {
        self.exit.store(1, Ordering::SeqCst);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for MergeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let theme = self.theme.clone();
        let found = conflicts(&self.result);
        if self.shot.as_mut().is_some_and(|s| s.tick(&ctx)) {
            self.cancel(&ctx);
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !self.confirm_apply {
            self.cancel(&ctx);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.three_way,
                    egui::Checkbox::new(&mut self.advanced, "Three-way visual merge"),
                )
                .changed()
                && self.advanced
            {
                self.visual.set_result(self.result.clone());
            }
        });
        if self.advanced {
            let mut apply = false;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label(&self.header);
                if let Some(error) = &self.error {
                    ui.colored_label(theme.error, error);
                }
                apply = self.visual.ui(ui, &theme);
            });
            self.result = self.visual.result.clone();
            if apply {
                self.apply(&ctx);
            }
            return;
        }
        egui::Panel::top("merge_header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Merge Revisions for {}", self.output.display()))
                    .strong(),
            );
            ui.label(
                egui::RichText::new(&self.header)
                    .color(theme.text_dim)
                    .small(),
            );
            ui.add_space(2.0);
        });
        egui::Panel::bottom("merge_buttons").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Accept Left")
                    .on_hover_text("Use the left file as the result")
                    .clicked()
                {
                    self.result = self.left.clone();
                }
                if ui
                    .button("Accept Right")
                    .on_hover_text("Use the right file as the result")
                    .clicked()
                {
                    self.result = self.right.clone();
                }
                if ui
                    .button("Reset")
                    .on_hover_text("Start over from the automatic merge")
                    .clicked()
                {
                    self.result = self.initial.clone();
                }
                if found.is_empty() {
                    ui.label(
                        egui::RichText::new("All conflicts are resolved").color(theme.success),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(format!("{} conflict(s) left", found.len()))
                            .color(theme.warning),
                    );
                }
                if let Some(e) = &self.error {
                    ui.label(egui::RichText::new(e).color(theme.error));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        self.cancel(&ctx);
                    }
                    if ui.button(egui::RichText::new("Apply").strong()).clicked() {
                        if found.is_empty() {
                            self.apply(&ctx);
                        } else {
                            self.confirm_apply = true;
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });
        let h = ui.available_height();
        egui::Panel::bottom("merge_diff")
            .resizable(true)
            .default_size((h * 0.4).max(120.0))
            .min_size(80.0)
            .show(ui, |ui| {
                pane_tabs(ui, &self.panes, &mut self.selected);
                if let Some(p) = self.panes.get(self.selected) {
                    p.diff
                        .ui(ui, &theme, self.mono.clone(), &p.left_title, &p.right_title);
                }
            });
        let mut resolve_one: Option<(usize, Resolution)> = None;
        egui::Panel::left("merge_conflicts")
            .resizable(true)
            .default_size(250.0)
            .min_size(160.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Conflicts").strong());
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("merge_conflict_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if found.is_empty() {
                            ui.label(egui::RichText::new("No conflicts").color(theme.text_dim));
                        }
                        for (i, c) in found.iter().enumerate() {
                            ui.label(format!(
                                "Line {}: {} | {} line(s)",
                                c.line,
                                c.left.lines().count(),
                                c.right.lines().count()
                            ));
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .small_button("Left")
                                    .on_hover_text("Accept the left side")
                                    .clicked()
                                {
                                    resolve_one = Some((i, Resolution::Left));
                                }
                                if ui
                                    .small_button("Right")
                                    .on_hover_text("Accept the right side")
                                    .clicked()
                                {
                                    resolve_one = Some((i, Resolution::Right));
                                }
                                if ui
                                    .small_button("Both")
                                    .on_hover_text("Left, then right")
                                    .clicked()
                                {
                                    resolve_one = Some((i, Resolution::Both));
                                }
                                if c.base.is_some()
                                    && ui
                                        .small_button("Base")
                                        .on_hover_text("Keep the base version")
                                        .clicked()
                                {
                                    resolve_one = Some((i, Resolution::Base));
                                }
                            });
                            ui.add_space(4.0);
                        }
                    });
            });
        if let Some((i, how)) = resolve_one {
            if let Some(c) = found.get(i) {
                self.result = resolve(&self.result, c, how);
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(egui::RichText::new("Result (editable)").color(theme.text_dim));
            let mono = self.mono.clone();
            let cache = &mut self.layout_cache;
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                let text = buf.as_str();
                let mut job = match cache {
                    Some((t, job)) if t == text => job.clone(),
                    _ => {
                        let job = result_layout(text, &mono, &theme);
                        *cache = Some((text.to_string(), job.clone()));
                        job
                    }
                };
                job.wrap.max_width = wrap_width;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            egui::ScrollArea::both()
                .id_salt("merge_result")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.result)
                            .font(self.mono.clone())
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(20)
                            .layouter(&mut layouter),
                    );
                });
        });
        if self.confirm_apply {
            let n = found.len();
            let mut close = false;
            egui::Window::new("Apply Merge Result").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show_modal(&ctx, |ui| {
                ui.label(format!("The result still has {n} unresolved conflict(s). Save it with the conflict markers?"));
                ui.horizontal(|ui| {
                    if ui.button("Apply Anyway").clicked() {
                        close = true;
                        self.apply(&ctx);
                    }
                    if ui.button("Continue Merging").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_apply = false;
            }
        }
    }
}

/// `rustpy merge <left> <right> [<base>] <output>`: runs the merge window.
/// Exit status 0 when the result was written to `output`, 1 when cancelled.
pub fn run_merge(args: MergeArgs) -> anyhow::Result<i32> {
    let read = |p: &Path| -> anyhow::Result<(String, LineEnding, rustpy_text::Encoding)> {
        read_text(p)?
            .ok_or_else(|| anyhow::anyhow!("{} is a binary file; it cannot be merged", p.display()))
    };
    let expected_output = merge_output_revision(&args.output)?;
    let (left, mut ending, mut encoding) = read(&args.left)?;
    let (right, _, _) = read(&args.right)?;
    let base = match &args.base {
        Some(b) => Some(read(b)?.0),
        None => None,
    };
    if args.output.is_dir() {
        anyhow::bail!("{} is a folder", args.output.display());
    }
    if args.output.is_file() {
        let (_, output_ending, output_encoding) = read(&args.output)?;
        ending = output_ending;
        encoding = output_encoding;
    }
    let (initial, _) = initial_result(&left, &right, base.as_deref());
    let names = (
        args.left.display().to_string(),
        args.right.display().to_string(),
        args.base.as_ref().map(|b| b.display().to_string()),
    );
    let (theme, mono) = editor_font();
    let mut panes = vec![pane("Left | Right", (&names.0, &left), (&names.1, &right))];
    if let (Some(b), Some(bn)) = (&base, &names.2) {
        panes.push(pane("Left | Base", (&names.0, &left), (bn, b)));
        panes.push(pane("Base | Right", (bn, b), (&names.1, &right)));
    }
    let header = match &names.2 {
        Some(b) => format!("Left: {}   Base: {}   Right: {}", names.0, b, names.1),
        None => format!("Left: {}   Right: {}", names.0, names.1),
    };
    let exit = Arc::new(AtomicI32::new(1));
    let visual = crate::diff_view::merge::MergeViewer::new(
        base.as_deref().unwrap_or(""),
        &left,
        &right,
        rustpy_settings::Settings::load().diff_merge,
    );
    let app = MergeApp {
        visual,
        advanced: base.is_some(),
        three_way: base.is_some(),
        theme: theme.clone(),
        mono,
        output: args.output.clone(),
        expected_output,
        header,
        left,
        right,
        result: initial.clone(),
        initial,
        line_ending: ending,
        encoding,
        panes,
        selected: 0,
        confirm_apply: false,
        error: None,
        exit: exit.clone(),
        layout_cache: None,
        shot: SmokeShot::new(args.screenshot.clone()),
    };
    let title = format!("Merge {} – RustPy", file_title(&args.output));
    eframe::run_native(
        "RustPy Merge",
        native_options(&title),
        Box::new(move |cc| {
            setup(&cc.egui_ctx, &theme);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(exit.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_output_preserves_formats_and_refuses_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.txt");
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
            rustpy_text::Encoding::Utf16Le,
            rustpy_text::Encoding::Utf16Be,
        ] {
            let source = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\ns='é'\r\n"
            } else {
                "s='é'\r\n"
            };
            let original = encoding.encode(source).unwrap();
            std::fs::write(&path, &original).unwrap();
            let (text, ending, detected) = read_text(&path).unwrap().unwrap();
            assert_eq!(detected, encoding);
            write_merge_result(&path, &text.replace("é", "à"), ending, detected).unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                encoding.encode(&source.replace("é", "à")).unwrap()
            );
            if encoding == rustpy_text::Encoding::Latin1 {
                let bytes = std::fs::read(&path).unwrap();
                assert!(write_merge_result(&path, &(text + "😀"), ending, detected).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
        }
        std::fs::write(&path, b"\xff\xfea").unwrap();
        assert!(read_text(&path).is_err());
    }

    #[test]
    fn standalone_merge_refuses_changed_or_newly_created_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merge.txt");
        std::fs::write(&path, "original\n").unwrap();
        let original = merge_output_revision(&path).unwrap();
        std::fs::write(&path, "concurrent\n").unwrap();
        assert!(write_captured_merge_result(
            &path,
            "resolved\n",
            LineEnding::Lf,
            rustpy_text::Encoding::Utf8,
            original
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "concurrent\n");
        let missing = dir.path().join("new.txt");
        assert_eq!(merge_output_revision(&missing).unwrap(), None);
        std::fs::write(&missing, "created by another process").unwrap();
        assert!(write_captured_merge_result(
            &missing,
            "resolved",
            LineEnding::Lf,
            rustpy_text::Encoding::Utf8,
            None
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&missing).unwrap(),
            "created by another process"
        );
    }
    #[test]
    fn standalone_diff_dispatches_directories_and_keeps_the_scan_owned_until_drained() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        std::fs::create_dir(&left).unwrap();
        std::fs::create_dir(&right).unwrap();
        std::fs::write(left.join("same.txt"), "same").unwrap();
        std::fs::write(right.join("same.txt"), "same").unwrap();
        let mut app = prepare_diff(&DiffArgs {
            left: left.clone(),
            right: right.clone(),
            ..Default::default()
        })
        .unwrap();
        let mut directory = app.directory.take().unwrap();
        let mut job = directory.take_job_for_drain().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !job.drained() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            job.drained(),
            "standalone close must observe task retirement"
        );
        assert!(prepare_diff(&DiffArgs {
            left: left.clone(),
            right: right.join("same.txt"),
            ..Default::default()
        })
        .is_err());
        assert!(prepare_diff(&DiffArgs {
            left,
            right,
            base: Some(dir.path().into()),
            screenshot: None
        })
        .is_err());
    }

    #[test]
    fn generic_diff_and_merge_ignore_python_cookie_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        let source = "# coding: latin-1\r\ntext = é😀\r\n";
        std::fs::write(&path, source.as_bytes()).unwrap();
        let (text, ending, encoding) = read_text(&path).unwrap().unwrap();
        assert_eq!(encoding, rustpy_text::Encoding::Utf8);
        assert_eq!(text, "# coding: latin-1\ntext = é😀\n");
        write_merge_result(&path, &text.replace("é", "à"), ending, encoding).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            source.replace("é", "à").as_bytes()
        );
    }

    #[test]
    fn finds_conflicts_with_and_without_base() {
        let text = "a\n<<<<<<< left\nL1\nL2\n||||||| base\nB\n=======\nR\n>>>>>>> right\nb\n<<<<<<< left\nx\n=======\ny\n>>>>>>> right\n";
        let c = conflicts(text);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (
                c[0].line,
                c[0].left.as_str(),
                c[0].base.as_deref(),
                c[0].right.as_str()
            ),
            (2, "L1\nL2\n", Some("B\n"), "R\n")
        );
        assert_eq!(
            &text[c[0].range.clone()],
            "<<<<<<< left\nL1\nL2\n||||||| base\nB\n=======\nR\n>>>>>>> right\n"
        );
        assert_eq!((c[1].line, c[1].base.as_deref()), (11, None));
        // A line that only starts like a marker is text.
        assert!(conflicts("<<<<<<<<<< not a marker\n=======\n>>>>>>>>\n").is_empty());
        // An unterminated block is not a conflict.
        assert!(conflicts("<<<<<<< left\nx\n=======\n").is_empty());
    }

    #[test]
    fn resolutions() {
        let text = "a\n<<<<<<< left\nL\n||||||| base\nB\n=======\nR\n>>>>>>> right\nz\n";
        let c = &conflicts(text)[0];
        assert_eq!(resolve(text, c, Resolution::Left), "a\nL\nz\n");
        assert_eq!(resolve(text, c, Resolution::Right), "a\nR\nz\n");
        assert_eq!(resolve(text, c, Resolution::Both), "a\nL\nR\nz\n");
        assert_eq!(resolve(text, c, Resolution::Base), "a\nB\nz\n");
    }

    #[test]
    fn two_way_merge_marks_every_difference() {
        let (text, n) = two_way("a\nb\nc\n", "a\nB\nc\nd");
        assert_eq!(n, 2);
        assert_eq!(text, "a\n<<<<<<< left\nb\n=======\nB\n>>>>>>> right\nc\n<<<<<<< left\n=======\nd\n>>>>>>> right\n");
        let c = conflicts(&text);
        assert_eq!(c.len(), 2);
        let resolved = resolve(
            &resolve(&text, &c[1], Resolution::Right),
            &c[0],
            Resolution::Left,
        );
        assert_eq!(resolved, "a\nb\nc\nd\n");
        assert_eq!(two_way("same\n", "same\n"), ("same\n".to_string(), 0));
    }

    #[test]
    fn three_way_merge_keeps_one_sided_changes() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let (text, n) = initial_result(
            "ONE\ntwo\nthree\nfour\nfive\n",
            "one\ntwo\nthree\nfour\nFIVE\n",
            Some(base),
        );
        assert_eq!((text.as_str(), n), ("ONE\ntwo\nthree\nfour\nFIVE\n", 0));
        let (text, n) = initial_result(
            "one\nTWO-left\nthree\nfour\nfive\n",
            "one\nTWO-right\nthree\nfour\nfive\n",
            Some(base),
        );
        assert_eq!(n, 1);
        let c = &conflicts(&text)[0];
        assert_eq!(
            (c.left.as_str(), c.base.as_deref(), c.right.as_str()),
            ("TWO-left\n", Some("two\n"), "TWO-right\n")
        );
    }

    #[test]
    fn reading_files() {
        let dir = tempfile::tempdir().unwrap();
        let crlf = dir.path().join("crlf.txt");
        std::fs::write(&crlf, "a\r\nb\r\n").unwrap();
        assert_eq!(
            read_text(&crlf).unwrap(),
            Some((
                "a\nb\n".to_string(),
                LineEnding::CrLf,
                rustpy_text::Encoding::Utf8
            ))
        );
        let bin = dir.path().join("x.bin");
        std::fs::write(&bin, [0u8, 1, 2]).unwrap();
        assert_eq!(read_text(&bin).unwrap(), None);
        assert!(read_text(dir.path())
            .unwrap_err()
            .to_string()
            .contains("folder"));
        assert!(read_text(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn result_coloring_marks_sections() {
        let theme = UiTheme::dark();
        let mono = FontId::monospace(13.0);
        let job = result_layout(
            "a\n<<<<<<< left\nL\n=======\nR\n>>>>>>> right\n",
            &mono,
            &theme,
        );
        assert_eq!(job.sections.len(), 6);
        assert_eq!(job.sections[0].format.background, Color32::TRANSPARENT);
        assert_eq!(
            job.sections[1].format.color, theme.error,
            "markers stand out"
        );
        assert_ne!(
            job.sections[2].format.background, job.sections[4].format.background,
            "left and right differ"
        );
    }
}

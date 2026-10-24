//! Headless UI snapshot harness (tests only).
//!
//! Renders a surface in a plain `egui::Context` (no window, no GPU, bundled
//! fonts only) at a fixed screen size, scale and theme, reduces the frame's
//! paint output to a stable text digest (shapes and text runs with rounded
//! positions and exact colours) and compares it with a golden under
//! `qa/goldens/ui/<name>.txt`.
//!
//! * `RUSTPY_UPDATE_SNAPSHOTS=1` (re)writes every golden that a test renders.
//! * A missing golden is written and reported as new; with
//!   `RUSTPY_REQUIRE_SNAPSHOTS=1` (CI) a missing golden fails instead and the
//!   rendering is left next to it as `<name>.new.txt`.
//! * A mismatch fails with a unified diff and leaves `<name>.new.txt` for
//!   review; accepting it is a rename (or a rerun with the update variable).
use egui::epaint::{ClippedShape, ColorMode, Shape, TextShape};
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::fmt::Write as _;
use std::path::PathBuf;

/// Rewrites goldens instead of comparing.
pub(crate) const UPDATE_VAR: &str = "RUSTPY_UPDATE_SNAPSHOTS";
/// Fails on missing goldens instead of creating them (CI).
pub(crate) const REQUIRE_VAR: &str = "RUSTPY_REQUIRE_SNAPSHOTS";
/// Frames rendered before the captured one, one simulated second apart, so
/// window fade-ins, collapsing animations and first-frame layout passes settle.
const SETTLE_FRAMES: usize = 4;
/// Longest diff printed on a mismatch; the full rendering is in `.new.txt`.
const MAX_DIFF_LINES: usize = 300;

/// A headless rendering context for one snapshot.
pub(crate) struct Harness {
    ctx: egui::Context,
    name: String,
    size: Vec2,
    pixels_per_point: f32,
    theme: String,
    time: f64,
    replacements: Vec<(String, String)>,
}

impl Harness {
    /// A fresh context with the bundled fonts, `theme` applied, 1:1 scale.
    pub(crate) fn new(name: &str, size: Vec2, theme: &crate::theme::UiTheme) -> Self {
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "snapshot names are [a-z0-9_]+: {name:?}"
        );
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.set_zoom_factor(1.0);
        theme.apply(&ctx);
        Self {
            ctx,
            name: name.to_owned(),
            size,
            pixels_per_point: 1.0,
            theme: theme.name().to_owned(),
            time: 0.0,
            replacements: Vec::new(),
        }
    }

    /// Replaces machine-specific text (a temporary project root) in the digest.
    pub(crate) fn replace(&mut self, from: impl Into<String>, to: impl Into<String>) {
        let from = from.into();
        if !from.is_empty() {
            self.replacements.push((from, to.into()));
            // Longest first, so a path is not partly replaced by its prefix.
            self.replacements
                .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        }
    }

    fn input(&self) -> egui::RawInput {
        let mut viewports = egui::ViewportIdMap::default();
        viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(self.pixels_per_point),
                focused: Some(true),
                ..Default::default()
            },
        );
        egui::RawInput {
            viewports,
            time: Some(self.time),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            max_texture_side: Some(8192),
            ..Default::default()
        }
    }

    fn frame(&mut self, run_ui: &mut dyn FnMut(&mut egui::Ui)) -> Vec<ClippedShape> {
        let mut output = self.ctx.run_ui(self.input(), |ui| run_ui(ui));
        let shapes = std::mem::take(&mut output.shapes);
        output.drop_without_applying_deltas();
        shapes
    }

    /// Settles `run_ui`, then renders it twice at the same instant and returns
    /// the digest; differing renders mean the surface is not snapshot-stable.
    pub(crate) fn render(&mut self, mut run_ui: impl FnMut(&mut egui::Ui)) -> String {
        for _ in 0..SETTLE_FRAMES {
            self.frame(&mut run_ui);
            self.time += 1.0;
        }
        let shapes = self.frame(&mut run_ui);
        let first = self.digest(&shapes);
        let shapes = self.frame(&mut run_ui);
        let second = self.digest(&shapes);
        assert!(
            first == second,
            "UI snapshot {:?} is not deterministic between two identical frames:\n{}",
            self.name,
            unified_diff(&first, &second)
        );
        second
    }

    fn digest(&self, shapes: &[ClippedShape]) -> String {
        let header = format!(
            "# rustpy ui snapshot: {}\n# screen {}x{} @{}x, theme {:?}\n",
            self.name,
            self.size.x.round() as i64,
            self.size.y.round() as i64,
            self.pixels_per_point,
            self.theme,
        );
        let mut text = header + &digest(shapes);
        for (from, to) in &self.replacements {
            text = text.replace(from.as_str(), to);
            // Texts are printed with `{:?}`, which escapes Windows separators.
            let escaped = from.escape_default().to_string();
            if escaped != *from {
                text = text.replace(&escaped, to);
            }
        }
        let placeholders: Vec<&str> = self
            .replacements
            .iter()
            .map(|(_, to)| to.as_str())
            .collect();
        mask_placeholder_text(&text, &placeholders)
    }

    /// Renders and compares against (or writes) the golden.
    pub(crate) fn assert_snapshot(&mut self, run_ui: impl FnMut(&mut egui::Ui)) {
        let actual = self.render(run_ui);
        assert_matches_golden(&self.name, &actual);
    }
}

/// `qa/goldens/ui` in the repository.
pub(crate) fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../qa/goldens/ui")
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

/// What to do with a rendering, given the golden on disk (if any).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Matches,
    /// Write the golden (update mode, or a first rendering outside CI).
    WriteGolden {
        new: bool,
    },
    /// Missing golden under `RUSTPY_REQUIRE_SNAPSHOTS`.
    Missing,
    Mismatch,
}

pub(crate) fn verdict(golden: Option<&str>, actual: &str, update: bool, require: bool) -> Verdict {
    match golden {
        Some(expected) if normalize_newlines(expected) == actual => Verdict::Matches,
        _ if update => Verdict::WriteGolden {
            new: golden.is_none(),
        },
        Some(_) => Verdict::Mismatch,
        None if require => Verdict::Missing,
        None => Verdict::WriteGolden { new: true },
    }
}

/// A text whose content was replaced by a placeholder (a project path, for
/// example) has a machine-specific width, elision and style-run length: those
/// are masked so the snapshot is the same on every machine.
fn mask_placeholder_text(text: &str, placeholders: &[&str]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut masking = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if let Some(rest) = body.strip_prefix("text ") {
            masking = placeholders
                .iter()
                .any(|p| !p.is_empty() && rest.contains(p));
            if masking {
                // `text (x,y) WxH "..."`: the width becomes `~`, elision is dropped.
                let mut parts = rest.splitn(3, ' ');
                let (Some(pos), Some(size), Some(tail)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    out.push_str(line);
                    continue;
                };
                let height = size.split_once('x').map(|(_, h)| h).unwrap_or(size);
                let tail = tail.strip_suffix(" elided").unwrap_or(tail);
                out.push_str(&format!("text {pos} ~x{height} {tail}\n"));
                continue;
            }
        } else if masking && body.starts_with("  [") {
            // Style runs `  [a..b] style`: the end offset depends on the text.
            if let Some((range, style)) = body[2..].split_once("] ") {
                if let Some((start, _)) = range.trim_start_matches('[').split_once("..") {
                    out.push_str(&format!("  [{start}..~] {style}\n"));
                    continue;
                }
            }
        } else {
            masking = false;
        }
        out.push_str(line);
    }
    out
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub(crate) fn assert_matches_golden(name: &str, actual: &str) {
    let dir = golden_dir();
    let path = dir.join(format!("{name}.txt"));
    let pending = dir.join(format!("{name}.new.txt"));
    let golden = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("reading UI golden {}: {e}", path.display()),
    };
    let write = |target: &PathBuf| {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(target, actual)
            .unwrap_or_else(|e| panic!("writing UI snapshot {}: {e}", target.display()));
    };
    match verdict(
        golden.as_deref(),
        actual,
        env_flag(UPDATE_VAR),
        env_flag(REQUIRE_VAR),
    ) {
        Verdict::Matches => {
            let _ = std::fs::remove_file(&pending);
        }
        Verdict::WriteGolden { new } => {
            write(&path);
            let _ = std::fs::remove_file(&pending);
            eprintln!(
                "UI snapshot {name}: {} golden {}",
                if new { "wrote new" } else { "updated" },
                path.display()
            );
        }
        Verdict::Missing => {
            write(&pending);
            panic!(
                "UI snapshot {name}: golden {} is missing ({REQUIRE_VAR} is set); \
                 the rendering is in {}. Review it and rerun with {UPDATE_VAR}=1 to accept.",
                path.display(),
                pending.display()
            );
        }
        Verdict::Mismatch => {
            write(&pending);
            let expected = normalize_newlines(golden.as_deref().unwrap_or_default());
            panic!(
                "UI snapshot {name} differs from {}\n(full rendering: {}; rerun with \
                 {UPDATE_VAR}=1 to accept)\n{}",
                path.display(),
                pending.display(),
                unified_diff(&expected, actual)
            );
        }
    }
}

/// A unified diff (golden → actual), truncated to `MAX_DIFF_LINES`.
pub(crate) fn unified_diff(expected: &str, actual: &str) -> String {
    let diff = similar::TextDiff::from_lines(expected, actual)
        .unified_diff()
        .context_radius(3)
        .header("golden", "actual")
        .to_string();
    let total = diff.lines().count();
    if total <= MAX_DIFF_LINES {
        return diff;
    }
    let mut text: String = diff
        .lines()
        .take(MAX_DIFF_LINES)
        .flat_map(|line| [line, "\n"])
        .collect();
    let _ = writeln!(text, "... {} more diff lines", total - MAX_DIFF_LINES);
    text
}

/// The stable text form of a frame's paint output, one line per visible
/// primitive in paint order; a `clip` line precedes primitives whose clip
/// rectangle differs from the previous one.
pub(crate) fn digest(shapes: &[ClippedShape]) -> String {
    let mut out = String::new();
    let mut clip = None;
    for clipped in shapes {
        shape_lines(&mut out, &clipped.shape, clipped.clip_rect, &mut clip);
    }
    out
}

fn shape_lines(out: &mut String, shape: &Shape, clip_rect: Rect, clip: &mut Option<Rect>) {
    if let Shape::Vec(shapes) = shape {
        for shape in shapes {
            shape_lines(out, shape, clip_rect, clip);
        }
        return;
    }
    let Some(line) = shape_line(shape) else {
        return;
    };
    let bounds = shape.visual_bounding_rect();
    if !bounds.is_positive() && !matches!(shape, Shape::LineSegment { .. }) {
        return;
    }
    if !clip_rect.intersects(bounds.expand(0.5)) {
        return;
    }
    if *clip != Some(clip_rect) {
        *clip = Some(clip_rect);
        let _ = writeln!(out, "clip {}", rect(clip_rect));
    }
    out.push_str(&line);
    out.push('\n');
}

/// One primitive, or `None` when it paints nothing.
fn shape_line(shape: &Shape) -> Option<String> {
    Some(match shape {
        Shape::Noop | Shape::Vec(_) => return None,
        Shape::Rect(r) => {
            if r.fill.a() == 0 && invisible(r.stroke) {
                return None;
            }
            let mut line = format!("rect {} fill={}", rect(r.rect), color(r.fill));
            if !invisible(r.stroke) {
                let _ = write!(line, " stroke={}", stroke(r.stroke));
            }
            let c = r.corner_radius;
            if c.nw != 0 || c.ne != 0 || c.sw != 0 || c.se != 0 {
                let _ = write!(line, " radius={},{},{},{}", c.nw, c.ne, c.sw, c.se);
            }
            line
        }
        Shape::Circle(c) => {
            if c.fill.a() == 0 && invisible(c.stroke) {
                return None;
            }
            format!(
                "circle {} r={} fill={} stroke={}",
                pos(c.center),
                num(c.radius),
                color(c.fill),
                stroke(c.stroke)
            )
        }
        Shape::LineSegment { points, stroke: s } => {
            if invisible(*s) {
                return None;
            }
            format!(
                "line {}-{} stroke={}",
                pos(points[0]),
                pos(points[1]),
                stroke(*s)
            )
        }
        Shape::Path(p) => {
            let stroke_color = match &p.stroke.color {
                ColorMode::Solid(c) => *c,
                _ => Color32::WHITE,
            };
            if p.fill.a() == 0 && (p.stroke.width <= 0.0 || stroke_color.a() == 0) {
                return None;
            }
            let stroke = match &p.stroke.color {
                ColorMode::Solid(c) => format!("{}{}", num(p.stroke.width), color(*c)),
                _ => format!("{}uv", num(p.stroke.width)),
            };
            format!(
                "path {} points={}{} fill={} stroke={}",
                rect(shape.visual_bounding_rect()),
                p.points.len(),
                if p.closed { " closed" } else { "" },
                color(p.fill),
                stroke
            )
        }
        Shape::Text(t) => text_line(t)?,
        Shape::Mesh(m) => {
            if m.vertices.is_empty() {
                return None;
            }
            let texture = match m.texture_id {
                egui::TextureId::Managed(0) => "font".to_owned(),
                id => format!("{id:?}"),
            };
            format!(
                "mesh {} vertices={} texture={texture}",
                rect(shape.visual_bounding_rect()),
                m.vertices.len()
            )
        }
        Shape::Ellipse(_) => format!("ellipse {}", rect(shape.visual_bounding_rect())),
        Shape::QuadraticBezier(_) | Shape::CubicBezier(_) => {
            format!("bezier {}", rect(shape.visual_bounding_rect()))
        }
        Shape::Callback(_) => format!("callback {}", rect(shape.visual_bounding_rect())),
    })
}

fn text_line(t: &TextShape) -> Option<String> {
    let galley = &t.galley;
    if galley.text().is_empty() || t.opacity_factor <= 0.0 {
        return None;
    }
    // An elided galley's width depends on the text it cut, which for paths
    // includes the machine-specific part later replaced by placeholders.
    let width = if galley.elided {
        "~".to_string()
    } else {
        num(galley.size().x)
    };
    let mut line = format!(
        "text {} {}x{} {:?}",
        pos(t.pos),
        width,
        num(galley.size().y),
        galley.text()
    );
    if galley.elided {
        line.push_str(" elided");
    }
    if let Some(c) = t.override_text_color {
        let _ = write!(line, " override={}", color(c));
    }
    if t.angle != 0.0 {
        let _ = write!(line, " angle={:.3}", t.angle);
    }
    if t.opacity_factor < 1.0 {
        let _ = write!(line, " opacity={:.2}", t.opacity_factor);
    }
    // Style runs; adjacent runs with the same style are merged.
    let mut runs: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for section in &galley.job.sections {
        let f = &section.format;
        let fg = if f.color == Color32::PLACEHOLDER {
            t.fallback_color
        } else {
            f.color
        };
        let mut style = format!(
            "{:?} {} {}",
            f.font_id.family,
            num(f.font_id.size),
            color(fg)
        );
        if f.background.a() != 0 {
            let _ = write!(style, " bg={}", color(f.background));
        }
        if f.italics {
            style.push_str(" italic");
        }
        if !invisible(f.underline) {
            let _ = write!(style, " underline={}", stroke(f.underline));
        }
        if !invisible(f.strikethrough) {
            let _ = write!(style, " strike={}", stroke(f.strikethrough));
        }
        match runs.last_mut() {
            Some((range, last)) if *last == style && range.end == section.byte_range.start.0 => {
                range.end = section.byte_range.end.0;
            }
            _ => runs.push((section.byte_range.start.0..section.byte_range.end.0, style)),
        }
    }
    for (range, style) in runs {
        let _ = write!(line, "\n  [{}..{}] {style}", range.start, range.end);
    }
    Some(line)
}

fn invisible(s: Stroke) -> bool {
    s.width <= 0.0 || s.color.a() == 0
}

/// Rounds to whole points; `-0` prints as `0`.
fn num(v: f32) -> String {
    if !v.is_finite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let r = v.round();
    if r == 0.0 {
        "0".to_owned()
    } else {
        format!("{r}")
    }
}

fn pos(p: Pos2) -> String {
    format!("({},{})", num(p.x), num(p.y))
}

fn rect(r: Rect) -> String {
    format!(
        "[{},{} {},{}]",
        num(r.min.x),
        num(r.min.y),
        num(r.max.x),
        num(r.max.y)
    )
}

fn color(c: Color32) -> String {
    if c.a() == 0 {
        return "none".to_owned();
    }
    let [r, g, b, a] = c.to_array();
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

fn stroke(s: Stroke) -> String {
    format!("{}{}", num(s.width), color(s.color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::UiTheme;

    fn sample(harness: &mut Harness) -> String {
        harness.render(|ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label("Hello, π");
                let _ = ui.button("OK");
                ui.label(egui::RichText::new("/tmp/fixture/root").monospace());
            });
        })
    }

    #[test]
    fn digest_is_stable_textual_and_normalised() {
        let mut harness =
            Harness::new("harness_sample", egui::vec2(320.0, 200.0), &UiTheme::dark());
        harness.replace("/tmp/fixture/root", "$ROOT");
        let first = sample(&mut harness);
        assert!(
            first.starts_with("# rustpy ui snapshot: harness_sample\n# screen 320x200 @1x, theme ")
        );
        assert!(first.contains("\"Hello, π\""), "{first}");
        assert!(first.contains("\"OK\""), "{first}");
        assert!(first.contains("\"$ROOT\""), "{first}");
        assert!(!first.contains("/tmp/fixture"), "{first}");
        assert!(first.lines().any(|l| l.starts_with("rect [")), "{first}");
        assert!(first.lines().any(|l| l.starts_with("clip [")), "{first}");
        // No fractional coordinates leak into the text.
        for line in first.lines().filter(|l| l.starts_with("text ")) {
            let position = line.split_whitespace().nth(1).unwrap();
            assert!(!position.contains('.'), "{line}");
        }
        // A fresh context renders the same digest.
        let mut again = Harness::new("harness_sample", egui::vec2(320.0, 200.0), &UiTheme::dark());
        again.replace("/tmp/fixture/root", "$ROOT");
        assert_eq!(sample(&mut again), first);
        // The theme changes colours, so it is part of the oracle.
        let mut light = Harness::new(
            "harness_sample",
            egui::vec2(320.0, 200.0),
            &UiTheme::light(),
        );
        light.replace("/tmp/fixture/root", "$ROOT");
        assert_ne!(sample(&mut light), first);
    }

    #[test]
    fn primitive_formatting_rounds_and_skips_invisible_shapes() {
        assert_eq!(num(-0.3), "0");
        assert_eq!(num(12.5), "13");
        assert_eq!(num(f32::INFINITY), "inf");
        assert_eq!(color(Color32::from_rgb(1, 2, 255)), "#0102ff");
        assert_eq!(color(Color32::TRANSPARENT), "none");
        let shapes = vec![
            ClippedShape {
                clip_rect: Rect::EVERYTHING,
                shape: Shape::rect_filled(
                    Rect::from_min_max(Pos2::new(0.4, 1.6), Pos2::new(10.0, 10.0)),
                    0.0,
                    Color32::RED,
                ),
            },
            // Transparent: paints nothing.
            ClippedShape {
                clip_rect: Rect::EVERYTHING,
                shape: Shape::rect_filled(
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(5.0, 5.0)),
                    0.0,
                    Color32::TRANSPARENT,
                ),
            },
            // Clipped away entirely.
            ClippedShape {
                clip_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(5.0, 5.0)),
                shape: Shape::line_segment(
                    [Pos2::new(50.0, 50.0), Pos2::new(60.0, 50.0)],
                    Stroke::new(1.0, Color32::WHITE),
                ),
            },
        ];
        assert_eq!(
            digest(&shapes),
            "clip [-inf,-inf inf,inf]\nrect [0,2 10,10] fill=#ff0000\n"
        );
    }

    #[test]
    fn golden_verdicts_cover_update_require_and_line_endings() {
        assert_eq!(
            verdict(Some("a\nb\n"), "a\nb\n", false, true),
            Verdict::Matches
        );
        assert_eq!(
            verdict(Some("a\r\nb\r\n"), "a\nb\n", false, true),
            Verdict::Matches
        );
        assert_eq!(verdict(Some("a\n"), "b\n", false, false), Verdict::Mismatch);
        assert_eq!(
            verdict(Some("a\n"), "b\n", true, true),
            Verdict::WriteGolden { new: false }
        );
        assert_eq!(
            verdict(None, "b\n", false, false),
            Verdict::WriteGolden { new: true }
        );
        assert_eq!(verdict(None, "b\n", false, true), Verdict::Missing);
        assert_eq!(
            verdict(None, "b\n", true, true),
            Verdict::WriteGolden { new: true }
        );
    }

    #[test]
    fn mismatch_diff_is_readable_and_bounded() {
        let diff = unified_diff("text (1,2) \"a\"\nrect x\n", "text (1,3) \"a\"\nrect x\n");
        assert!(diff.contains("--- golden"), "{diff}");
        assert!(diff.contains("+++ actual"), "{diff}");
        assert!(diff.contains("-text (1,2) \"a\""), "{diff}");
        assert!(diff.contains("+text (1,3) \"a\""), "{diff}");
        let long_a: String = (0..1000).map(|i| format!("a{i}\n")).collect();
        let long_b: String = (0..1000).map(|i| format!("b{i}\n")).collect();
        let diff = unified_diff(&long_a, &long_b);
        assert!(diff.lines().count() <= MAX_DIFF_LINES + 1);
        assert!(
            diff.ends_with("more diff lines\n"),
            "{}",
            &diff[diff.len() - 80..]
        );
    }
}

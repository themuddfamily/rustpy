//! Pixel boundaries for the raster viewer, independent of its chessboard background.
use egui::{Color32, Pos2, Rect, Stroke, Ui};

const MIN_PIXEL_SIZE: f32 = 4.0;
const MAX_BOUNDARIES_PER_AXIS: usize = 2048;

/// Paint only visible source-pixel boundaries at useful magnification.
/// Returns the number of boundaries (each has a light halo and dark center).
pub(super) fn paint(ui: &Ui, image: Rect, pixels: [usize; 2], zoom: f32) -> usize {
    if !zoom.is_finite() || zoom < MIN_PIXEL_SIZE || pixels.contains(&0) {
        return 0;
    }
    let visible = image.intersect(ui.clip_rect());
    if !visible.is_positive() || !visible.is_finite() {
        return 0;
    }
    let axis = |low: f32, high: f32, origin: f32, count: usize| {
        let first = ((low - origin) / zoom).ceil().max(0.0) as usize;
        let last = ((high - origin) / zoom).floor().max(0.0) as usize;
        (first.min(count), last.min(count))
    };
    let (x0, x1) = axis(visible.left(), visible.right(), image.left(), pixels[0]);
    let (y0, y1) = axis(visible.top(), visible.bottom(), image.top(), pixels[1]);
    let nx = x1.saturating_sub(x0) + usize::from(x0 <= x1);
    let ny = y1.saturating_sub(y0) + usize::from(y0 <= y1);
    // An unusually huge render surface must not produce an unbounded shape list.
    if nx > MAX_BOUNDARIES_PER_AXIS || ny > MAX_BOUNDARIES_PER_AXIS {
        return 0;
    }
    let painter = ui.painter().with_clip_rect(visible);
    let line = |a, b| {
        painter.line_segment([a, b], Stroke::new(2.0, Color32::from_white_alpha(90)));
        painter.line_segment([a, b], Stroke::new(1.0, Color32::from_black_alpha(170)));
    };
    for column in x0..=x1 {
        let x = image.left() + column as f32 * zoom;
        line(Pos2::new(x, visible.top()), Pos2::new(x, visible.bottom()));
    }
    for row in y0..=y1 {
        let y = image.top() + row as f32 * zoom;
        line(Pos2::new(visible.left(), y), Pos2::new(visible.right(), y));
    }
    nx + ny
}

#[cfg(test)]
mod tests {
    use super::*;
    fn painted(
        image: Rect,
        clip: Rect,
        pixels: [usize; 2],
        zoom: f32,
    ) -> Vec<egui::epaint::ClippedShape> {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_clip_rect(clip);
            paint(ui, image, pixels, zoom);
        });
        let shapes = output.shapes.clone();
        output.drop_without_applying_deltas();
        shapes
            .into_iter()
            .filter(|shape| matches!(shape.shape, egui::Shape::LineSegment { .. }))
            .collect()
    }
    #[test]
    fn actual_shapes_follow_source_pixel_boundaries_after_clipped_scroll() {
        let image = Rect::from_min_size(Pos2::new(-7.0, -5.0), egui::vec2(80.0, 80.0));
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(21.0, 19.0));
        let shapes = painted(image, clip, [10, 10], 8.0);
        assert_eq!(shapes.len(), 12); // Three columns and three rows, each halo+center.
        for shape in shapes {
            assert_eq!(shape.clip_rect, clip);
            if let egui::Shape::LineSegment { points, .. } = shape.shape {
                assert!(points.iter().all(|point| clip.contains(*point)));
                if points[0].x == points[1].x {
                    assert!([1.0, 9.0, 17.0].contains(&points[0].x));
                } else {
                    assert!([3.0, 11.0, 19.0].contains(&points[0].y));
                }
            }
        }
    }
    #[test]
    fn large_image_work_depends_on_visible_clip_not_image_pixel_count() {
        let clip = Rect::from_min_size(Pos2::ZERO, egui::vec2(120.0, 80.0));
        let shapes = painted(
            Rect::from_min_size(
                Pos2::new(-4000.0, -8000.0),
                egui::vec2(4_000_000.0, 4_000_000.0),
            ),
            clip,
            [1_000_000, 1_000_000],
            4.0,
        );
        assert_eq!(shapes.len(), 104); // 31 columns +21 rows; no million-pixel loop.
        assert!(shapes.iter().all(|shape| shape.clip_rect == clip));
    }
    #[test]
    fn grid_reveals_only_at_useful_zoom_and_never_outside_image() {
        let image = Rect::from_min_size(Pos2::ZERO, egui::vec2(32.0, 32.0));
        assert!(painted(image, image, [8, 8], 3.99).is_empty());
        assert!(!painted(image, image, [8, 8], 4.0).is_empty());
        assert!(painted(image, image.translate(egui::vec2(100.0, 0.0)), [8, 8], 4.0).is_empty());
        assert!(painted(image, image, [8, 8], f32::NAN).is_empty());
    }
    #[test]
    fn pathological_surface_has_a_fixed_shape_budget() {
        let image = Rect::from_min_size(Pos2::ZERO, egui::vec2(100_000.0, 100_000.0));
        assert!(painted(image, image, [25_000, 25_000], 4.0).is_empty());
    }
    #[test]
    fn actual_viewer_checkbox_enables_overlay_without_mutating_pixels_or_peer_zoom() {
        use super::super::{IdeApp, ImageTab, Settings};
        use crate::editor::{EditorState, Language};
        let mut app = IdeApp::new(Settings::default(), None);
        for zoom in [8.0, 2.0] {
            app.push_tab(EditorState::from_text("", Language::Other));
            app.tabs.last_mut().unwrap().image = Some(ImageTab {
                rgba: vec![255; 8 * 8 * 4],
                size: [8, 8],
                texture: None,
                zoom,
                checkerboard: false,
                pixel_grid: false,
            });
        }
        let original = app.tabs[0].image.as_ref().unwrap().rgba.clone();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut frame = |time, events| {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 400.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| app.image_viewer(ui, 0));
                },
            );
            let tree = output.platform_output.accesskit_update.clone().unwrap();
            let lines = output.shapes.iter().filter(|shape| matches!(&shape.shape,
                egui::Shape::LineSegment { stroke, .. } if stroke.color == Color32::from_black_alpha(170))).count();
            output.drop_without_applying_deltas();
            (tree, lines)
        };
        let (tree, before) = frame(0.0, vec![]);
        assert_eq!(before, 0);
        let bounds = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Pixel grid"))
            .expect("actual named grid checkbox")
            .1
            .bounds()
            .unwrap();
        let pos = Pos2::new(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        frame(
            0.1,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        let (_, after) = frame(
            0.2,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert_eq!(after, 18); // Actual 8x8 image has nine boundaries on each axis.
        assert!(app.tabs[0].image.as_ref().unwrap().pixel_grid);
        assert_eq!(app.tabs[0].image.as_ref().unwrap().rgba, original);
        assert_eq!(app.tabs[0].image.as_ref().unwrap().zoom, 8.0);
        assert_eq!(app.tabs[1].image.as_ref().unwrap().zoom, 2.0);
        assert!(!app.tabs[1].image.as_ref().unwrap().pixel_grid);
    }
}

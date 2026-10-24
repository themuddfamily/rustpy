//! Glyph-only coverage tags for the native Glow renderer.
//! Modes: 0 grayscale, 1 monochrome, 2 RGB LCD, 3 BGR LCD.
//! LCD samples the existing hinted grayscale atlas at physical third-pixel
//! offsets; this is filtered LCD coverage, not a platform LCD font rasterizer.
use egui::{
    Context,
    epaint::{ClippedShape, Shape},
};
use std::sync::Arc;
#[derive(Clone, Default)]
struct Controls {
    ide: u8,
    editor: u8,
    editors: std::collections::HashSet<usize>,
}
fn key(ctx: &Context) -> egui::Id {
    egui::Id::new(("rustpy-text-coverage", ctx.viewport_id()))
}
/// Start a viewport frame. Editor rectangles are registered as they are painted.
pub fn begin_frame(ctx: &Context, ide: u8, editor: u8) {
    let key = key(ctx);
    ctx.data_mut(|data| {
        data.insert_temp(
            key,
            Controls {
                ide: ide.min(3),
                editor: editor.min(3),
                editors: Default::default(),
            },
        )
    });
}
/// Paint an editor-owned galley without classifying overlapping popups by position.
/// A fresh immutable header gives this paint operation an unambiguous identity;
/// shaped rows, text and cached layout remain shared until coverage tagging.
pub fn editor_galley(
    painter: &egui::Painter,
    pos: egui::Pos2,
    galley: Arc<egui::Galley>,
    color: egui::Color32,
) {
    let ctx = painter.ctx();
    let key = key(ctx);
    if ctx.data(|data| {
        data.get_temp::<Controls>(key)
            .is_none_or(|controls| controls.ide == controls.editor)
    }) {
        painter.galley(pos, galley, color);
        return;
    }
    let owned = Arc::new((*galley).clone());
    ctx.data_mut(|data| {
        if let Some(mut controls) = data.get_temp::<Controls>(key) {
            controls.editors.insert(Arc::as_ptr(&owned) as usize);
            data.insert_temp(key, controls);
        }
    });
    painter.galley(pos, owned, color);
}
pub fn prepare_shapes(ctx: &Context, shapes: Vec<ClippedShape>, opaque: bool) -> Vec<ClippedShape> {
    prepare_shapes_for_viewport(ctx, ctx.viewport_id(), shapes, opaque)
}
/// The renderer calls this after end_pass has restored the parent viewport.
/// Keep the completed viewport's coverage and editor identities explicit.
pub fn prepare_shapes_for_viewport(
    ctx: &Context,
    viewport: egui::ViewportId,
    mut shapes: Vec<ClippedShape>,
    opaque: bool,
) -> Vec<ClippedShape> {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("rustpy-glow-text-coverage"), true));
    let mut controls = ctx
        .data(|data| data.get_temp::<Controls>(egui::Id::new(("rustpy-text-coverage", viewport))))
        .unwrap_or_default();
    if !opaque {
        if controls.ide >= 2 {
            controls.ide = 0;
        }
        if controls.editor >= 2 {
            controls.editor = 0;
        }
    }
    for clipped in &mut shapes {
        prepare(&mut clipped.shape, &controls);
    }
    shapes
}
fn prepare(shape: &mut Shape, controls: &Controls) {
    match shape {
        Shape::Vec(children) => {
            for child in children {
                prepare(child, controls);
            }
        }
        Shape::Text(text) => {
            let mut mode = if controls
                .editors
                .contains(&(Arc::as_ptr(&text.galley) as usize))
            {
                controls.editor
            } else {
                controls.ide
            };
            // Horizontal LCD stripes cannot be applied to rotated glyphs.
            if mode >= 2 && (text.angle != 0.0 || text.opacity_factor < 1.0) {
                mode = 0;
            }
            if mode == 0 {
                return;
            }
            // Color emoji may contain neutral pixels; do not split those pixels
            // into LCD passes. Keep their complete shaped run in its original form.
            if text
                .galley
                .job
                .text
                .chars()
                .any(|c| matches!(c as u32, 0x1f000..=0x1faff | 0x2600..=0x27ff | 0xfe0f | 0x200d))
            {
                return;
            }

            // Copy-on-write preserves canonical layout caches and source offsets.
            let override_text_color = text.override_text_color;
            let fallback_color = text.fallback_color;
            let galley = Arc::make_mut(&mut text.galley);
            for row in &mut galley.rows {
                let visuals = &mut Arc::make_mut(&mut row.row).visuals;
                for vertex in &mut visuals.mesh.vertices[visuals.glyph_vertex_range.clone()] {
                    // Never tag decorations/backgrounds, or translucent glyphs for LCD.
                    let color = override_text_color.unwrap_or_else(|| {
                        if vertex.color == egui::Color32::PLACEHOLDER {
                            fallback_color
                        } else {
                            vertex.color
                        }
                    });
                    if mode >= 2 && color.a() != 255 {
                        continue;
                    }
                    if mode == 1 || mode == 3 {
                        vertex.uv.x = -vertex.uv.x;
                    }
                    if mode >= 2 {
                        vertex.uv.y = -vertex.uv.y;
                    }
                }
            }
        }
        _ => {}
    }
}
/// Consecutive triangle runs preserve the original painter's ordering.
pub(crate) fn coverage_runs(mesh: &egui::Mesh) -> Vec<(usize, usize, bool)> {
    if mesh.indices.is_empty() {
        return vec![];
    }
    if mesh.texture_id != egui::TextureId::Managed(0)
        || !mesh.vertices.iter().any(|vertex| vertex.uv.y < 0.0)
    {
        return vec![(0, mesh.indices.len(), false)];
    }
    let mut runs: Vec<(usize, usize, bool)> = Vec::new();
    for (triangle, indices) in mesh.indices.chunks_exact(3).enumerate() {
        let lcd = indices
            .iter()
            .all(|index| mesh.vertices[*index as usize].uv.y < 0.0);
        if let Some(last) = runs.last_mut().filter(|last| last.2 == lcd) {
            last.1 += 3;
        } else {
            runs.push((triangle * 3, 3, lcd));
        }
    }
    runs
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_child_coverage_uses_explicit_viewport_after_parent_resumes() {
        let ctx = Context::default();
        let child = egui::ViewportId::from_hash_of("child-project");
        let shape = text_fixture(&ctx, "e\u{301}日本語");
        begin_frame(&ctx, 0, 0);
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("rustpy-text-coverage", child)),
                Controls {
                    ide: 1,
                    ..Default::default()
                },
            )
        });
        let tagged = prepare_shapes_for_viewport(
            &ctx,
            child,
            vec![ClippedShape {
                clip_rect: egui::Rect::EVERYTHING,
                shape,
            }],
            true,
        );
        let Shape::Text(text) = &tagged[0].shape else {
            panic!()
        };
        assert!(
            text.galley.rows.iter().any(|row| row
                .visuals
                .mesh
                .vertices
                .iter()
                .any(|v| v.uv.x < 0.0))
        );
        assert_eq!(
            ctx.data(|data| data.get_temp::<Controls>(egui::Id::new((
                "rustpy-text-coverage",
                egui::ViewportId::ROOT
            ))))
            .unwrap()
            .ide,
            0
        );
    }
    #[test]
    fn viewport_controls_do_not_leak_between_frames() {
        let ctx = Context::default();
        begin_frame(&ctx, 1, 2);
        let key = key(&ctx);
        ctx.data_mut(|data| {
            let mut controls = data.get_temp::<Controls>(key).unwrap();
            controls.editors.insert(42);
            data.insert_temp(key, controls);
        });
        begin_frame(&ctx, 0, 0);
        let controls = ctx.data(|data| data.get_temp::<Controls>(key)).unwrap();
        assert!(controls.editors.is_empty());
        assert_eq!(controls.ide, 0);
    }
    #[test]
    fn grayscale_shapes_preserve_source_layout_identity() {
        let mut shape = Shape::Noop;
        prepare(&mut shape, &Controls::default());
        assert!(matches!(shape, Shape::Noop));
    }
    fn text_fixture(ctx: &Context, source: &str) -> Shape {
        let mut shape = Shape::Noop;
        let output = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let galley = ctx.fonts_mut(|fonts| {
                fonts.layout_no_wrap(
                    source.into(),
                    egui::FontId::monospace(16.0),
                    egui::Color32::WHITE,
                )
            });
            shape = Shape::Text(egui::epaint::TextShape::new(
                egui::pos2(10.0, 10.0),
                galley,
                egui::Color32::WHITE,
            ));
        });
        // This CPU-only fixture retains the galley, not a GPU atlas upload.
        output.drop_without_applying_deltas();
        shape
    }
    #[test]
    fn glyph_tags_preserve_decorations_source_and_cached_layout() {
        let ctx = Context::default();
        let mut shape = text_fixture(&ctx, "e\u{301}日本語");
        let Shape::Text(original) = &shape else {
            panic!()
        };
        let cached = original.galley.clone();
        let source = cached.job.clone();
        prepare(
            &mut shape,
            &Controls {
                ide: 2,
                ..Default::default()
            },
        );
        let Shape::Text(tagged) = shape else { panic!() };
        assert!(Arc::ptr_eq(&source, &tagged.galley.job));
        assert_eq!(cached.rect, tagged.galley.rect);
        for (before, after) in cached.rows.iter().zip(&tagged.galley.rows) {
            for (i, (a, b)) in before
                .visuals
                .mesh
                .vertices
                .iter()
                .zip(&after.visuals.mesh.vertices)
                .enumerate()
            {
                assert_eq!(a.pos, b.pos);
                assert_eq!(a.color, b.color);
                if before.visuals.glyph_vertex_range.contains(&i) {
                    assert_eq!(b.uv.y, -a.uv.y);
                } else {
                    assert_eq!(a.uv, b.uv);
                }
            }
        }
        assert!(
            cached
                .rows
                .iter()
                .flat_map(|row| &row.visuals.mesh.vertices)
                .all(|vertex| vertex.uv.y >= 0.0)
        );
    }
    #[test]
    fn emoji_and_rotated_lcd_text_keep_original_coverage() {
        let ctx = Context::default();
        for source in ["hello 🦀", "rotated"] {
            let mut shape = text_fixture(&ctx, source);
            if source == "rotated" {
                if let Shape::Text(text) = &mut shape {
                    text.angle = 0.5;
                }
            }
            let original = shape.clone();
            prepare(
                &mut shape,
                &Controls {
                    ide: 2,
                    ..Default::default()
                },
            );
            assert_eq!(shape, original);
        }
    }
    #[test]
    fn independent_coverage_does_not_attenuate_uncovered_channels() {
        let background = [0.2_f32, 0.4, 0.6];
        let foreground = [1.0_f32, 1.0, 1.0];
        let coverage = [1.0_f32, 0.0, 0.5];
        let blended: Vec<_> = (0..3)
            .map(|i| foreground[i] * coverage[i] + background[i] * (1.0 - coverage[i]))
            .collect();
        assert_eq!(blended, vec![1.0, 0.4, 0.8]);
        // One max-alpha RGBA pass would erase the untouched green destination.
        assert_ne!(blended[1], foreground[1] * coverage[1]);
    }
    #[test]
    fn lcd_runs_cannot_overtake_a_later_overlay_in_one_mesh() {
        let mut mesh = egui::Mesh::default();
        for y in [0.5, -0.5, 0.5] {
            let index = mesh.vertices.len() as u32;
            mesh.vertices.extend((0..3).map(|_| egui::epaint::Vertex {
                pos: egui::Pos2::ZERO,
                uv: egui::pos2(0.5, y),
                color: egui::Color32::WHITE,
            }));
            mesh.indices.extend([index, index + 1, index + 2]);
        }
        assert_eq!(
            coverage_runs(&mesh),
            vec![(0, 3, false), (3, 3, true), (6, 3, false)]
        );
    }
    #[test]
    fn editor_mode_does_not_reclassify_an_overlapping_popup() {
        let ctx = Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            begin_frame(ctx, 0, 1);
            let painter = ctx.layer_painter(egui::LayerId::background());
            let galley = ctx.fonts_mut(|fonts| {
                fonts.layout_no_wrap(
                    "same text".into(),
                    egui::FontId::monospace(16.0),
                    egui::Color32::WHITE,
                )
            });
            editor_galley(
                &painter,
                egui::pos2(10.0, 10.0),
                galley.clone(),
                egui::Color32::WHITE,
            );
            painter.galley(egui::pos2(10.0, 10.0), galley, egui::Color32::WHITE);
        });
        let shapes = prepare_shapes(&ctx, output.shapes, true);
        // Shapes now own the CPU data under test. No renderer consumes the
        // atlas updates; clear them before assertions can unwind.
        output.textures_delta.clear();
        let text: Vec<_> = shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Text(text) = &shape.shape {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text.len(), 2);
        assert!(
            text[0].galley.rows.iter().any(|row| row
                .visuals
                .mesh
                .vertices
                .iter()
                .any(|v| v.uv.x < 0.0))
        );
        assert!(
            text[1].galley.rows.iter().all(|row| row
                .visuals
                .mesh
                .vertices
                .iter()
                .all(|v| v.uv.x >= 0.0))
        );
    }
}

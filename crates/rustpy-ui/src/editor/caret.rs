//! Shared caret painting policy: insertion shapes, overwrite blocks and blinking.
//!
//! Coordinates and widths are logical pixels; egui applies viewport DPI scaling.
//! Geometry is separate from visibility so scrolling/IME keep a stable caret
//! rectangle while the painted caret is in its off phase.

use std::time::Duration;

use egui::{Pos2, Rect, Vec2};
use rustpy_settings::{CaretShape, EditorSettings};

/// Preserve the old block preference and make overwrite mode unmistakable.
pub fn effective_shape(settings: &EditorSettings, overwrite: bool) -> CaretShape {
    if overwrite || settings.block_caret {
        CaretShape::Block
    } else {
        settings.caret_shape
    }
}

/// The painted rectangle for the caret's text-cell origin and measured size.
/// Thin width and underscore thickness are bounded, including hand-edited JSON.
pub fn caret_rect(origin: Pos2, cell: Vec2, settings: &EditorSettings, overwrite: bool) -> Rect {
    let width = f32::from(settings.caret_width.clamp(1, 10));
    match effective_shape(settings, overwrite) {
        CaretShape::Thin => Rect::from_min_size(
            Pos2::new(origin.x - width / 2.0, origin.y),
            Vec2::new(width, cell.y),
        ),
        CaretShape::Block => Rect::from_min_size(origin, cell),
        CaretShape::Underscore => {
            let thickness = width.min(cell.y);
            Rect::from_min_size(
                Pos2::new(origin.x, origin.y + cell.y - thickness),
                Vec2::new(cell.x, thickness),
            )
        }
    }
}

/// Each editor view has its own activity clock. Reset after typing, caret
/// movement or focus acquisition to show the caret for a full on phase.
#[derive(Copy, Clone, Debug, Default)]
pub struct BlinkState {
    anchor: f64,
}

impl BlinkState {
    pub fn reset(&mut self, now: f64) {
        if now.is_finite() {
            self.anchor = now;
        }
    }

    pub fn visible(&self, now: f64, settings: &EditorSettings) -> bool {
        crate::a11y::reduced_motion()
            || !settings.caret_blinking
            || self.phase(now, settings) < self.period(settings)
    }

    /// Repaint exactly at the next phase boundary, rather than delaying a
    /// boundary by a whole period when a frame arrives midway through a phase.
    pub fn next_repaint(&self, now: f64, settings: &EditorSettings) -> Option<Duration> {
        if crate::a11y::reduced_motion() || !settings.caret_blinking {
            return None;
        }
        let period = self.period(settings);
        let phase = self.phase(now, settings);
        let remaining = if phase < period {
            period - phase
        } else {
            2.0 * period - phase
        };
        Some(Duration::from_secs_f64(remaining.max(0.001)))
    }

    /// The delay to pass to `request_repaint_after` for the next phase
    /// boundary. egui wakes `predicted_dt` (one frame time) before the delay
    /// it is given: without adding it back, the frame came ~17 ms before the
    /// boundary, drew the unchanged caret and asked for another frame at the
    /// boundary, so an idle editor painted twice per blink. A few more
    /// milliseconds absorb early timer wake-ups.
    pub fn repaint_delay(
        &self,
        now: f64,
        settings: &EditorSettings,
        predicted_dt: f32,
    ) -> Option<Duration> {
        const MARGIN: Duration = Duration::from_millis(4);
        let lead = Duration::try_from_secs_f32(predicted_dt).unwrap_or_default();
        self.next_repaint(now, settings)
            .map(|delay| delay + lead.min(Duration::from_millis(250)) + MARGIN)
    }

    fn period(&self, settings: &EditorSettings) -> f64 {
        settings.caret_blink_period_ms.clamp(100, 2000) as f64 / 1000.0
    }

    fn phase(&self, now: f64, settings: &EditorSettings) -> f64 {
        let elapsed = if now.is_finite() {
            (now - self.anchor).max(0.0)
        } else {
            0.0
        };
        if elapsed.is_finite() {
            elapsed % (2.0 * self.period(settings))
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_geometry_is_the_existing_centered_two_pixel_caret() {
        let settings = EditorSettings::default();
        assert_eq!(
            caret_rect(
                Pos2::new(20.0, 30.0),
                Vec2::new(9.0, 18.0),
                &settings,
                false
            ),
            Rect::from_min_max(Pos2::new(19.0, 30.0), Pos2::new(21.0, 48.0))
        );
    }

    #[test]
    fn underscore_sits_at_cell_bottom_and_tracks_font_metrics() {
        let settings = EditorSettings {
            caret_shape: CaretShape::Underscore,
            caret_width: 3,
            ..EditorSettings::default()
        };
        assert_eq!(
            caret_rect(
                Pos2::new(10.0, 20.0),
                Vec2::new(8.0, 16.0),
                &settings,
                false
            ),
            Rect::from_min_max(Pos2::new(10.0, 33.0), Pos2::new(18.0, 36.0))
        );
        assert_eq!(
            caret_rect(
                Pos2::new(10.0, 20.0),
                Vec2::new(16.0, 32.0),
                &settings,
                false
            ),
            Rect::from_min_max(Pos2::new(10.0, 49.0), Pos2::new(26.0, 52.0))
        );
        // A short cell must not paint the underscore above its first pixel.
        assert_eq!(
            caret_rect(Pos2::ZERO, Vec2::new(8.0, 2.0), &settings, false),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 2.0))
        );
    }

    #[test]
    fn every_insert_shape_becomes_a_cell_block_in_overwrite_mode() {
        let origin = Pos2::new(15.0, 23.0);
        let cell = Vec2::new(11.0, 21.0);
        for shape in [CaretShape::Thin, CaretShape::Block, CaretShape::Underscore] {
            let settings = EditorSettings {
                caret_shape: shape,
                caret_width: 7,
                ..EditorSettings::default()
            };
            assert_eq!(
                caret_rect(origin, cell, &settings, true),
                Rect::from_min_size(origin, cell)
            );
        }
        let settings = EditorSettings {
            caret_shape: CaretShape::Underscore,
            block_caret: true,
            ..EditorSettings::default()
        };
        assert_eq!(
            caret_rect(origin, cell, &settings, false),
            Rect::from_min_size(origin, cell)
        );
    }

    #[test]
    fn persisted_width_bounds_apply_without_changing_the_text_cell() {
        for (configured, painted) in [(0, 1.0), (1, 1.0), (5, 5.0), (10, 10.0), (255, 10.0)] {
            let settings = EditorSettings {
                caret_width: configured,
                ..EditorSettings::default()
            };
            let rect = caret_rect(Pos2::new(20.0, 4.0), Vec2::new(9.0, 18.0), &settings, false);
            assert_eq!(rect.width(), painted);
            assert_eq!(rect.center().x, 20.0);
            assert_eq!(rect.height(), 18.0);
        }
    }

    #[test]
    fn activity_restarts_the_full_on_phase_and_repaint_targets_the_boundary() {
        let settings = EditorSettings::default();
        let mut blink = BlinkState::default();
        blink.reset(10.0);
        assert!(blink.visible(10.0, &settings));
        assert!(blink.visible(10.25, &settings));
        assert_eq!(
            blink.next_repaint(10.25, &settings),
            Some(Duration::from_millis(250))
        );
        assert!(!blink.visible(10.5, &settings));
        assert!(blink.visible(11.0, &settings));
        // Typing/movement while off makes the caret immediately visible again.
        blink.reset(10.75);
        assert!(blink.visible(10.75, &settings));
        assert!(!blink.visible(11.25, &settings));
    }

    #[test]
    fn blink_repaints_land_after_the_boundary_once_egui_subtracts_a_frame() {
        let settings = EditorSettings::default();
        let mut blink = BlinkState::default();
        blink.reset(0.0);
        let dt = 1.0 / 60.0;
        // Idle blinking: each wake-up is the requested delay minus the
        // predicted frame time (as egui schedules it).
        let mut now = 0.0;
        let mut frames = 0;
        let mut shown = blink.visible(now, &settings);
        while now < 10.0 {
            let delay = blink.repaint_delay(now, &settings, dt).unwrap();
            now += (delay.as_secs_f64() - dt as f64).max(0.0);
            frames += 1;
            let visible = blink.visible(now, &settings);
            assert_ne!(
                visible, shown,
                "frame at {now:.3} s did not change the caret"
            );
            shown = visible;
        }
        // One frame per half period (500 ms): 2 frames a second, no extras.
        assert!((19..=21).contains(&frames), "{frames} frames in 10 s");
        // No predicted frame time: the delay is the boundary plus the margin.
        assert_eq!(
            blink.repaint_delay(0.25, &settings, 0.0),
            Some(Duration::from_millis(254))
        );
        let disabled = EditorSettings {
            caret_blinking: false,
            ..EditorSettings::default()
        };
        assert_eq!(blink.repaint_delay(0.25, &disabled, dt), None);
    }

    #[test]
    fn disabled_and_malformed_blink_preferences_do_not_spin_or_hide_the_caret() {
        let disabled = EditorSettings {
            caret_blinking: false,
            ..EditorSettings::default()
        };
        let blink = BlinkState::default();
        assert!(blink.visible(0.75, &disabled));
        assert_eq!(blink.next_repaint(0.75, &disabled), None);
        let zero = EditorSettings {
            caret_blink_period_ms: 0,
            ..EditorSettings::default()
        };
        assert_eq!(
            blink.next_repaint(0.0, &zero),
            Some(Duration::from_millis(100))
        );
        assert!(!blink.visible(0.125, &zero));
        assert!(blink.visible(f64::NAN, &zero));
        let mut restarted = blink;
        restarted.reset(f64::NAN);
        assert!(restarted.visible(-1.0, &zero));
    }
}

//! Ordered, clean Double Shift detection. The deadline measures Shift presses,
//! but firing waits for the second release so neither tap can be a typing chord.

use egui::{Event, Key, Modifiers};

const DOUBLE_SHIFT_SECONDS: f64 = 0.35;

#[derive(Clone, Copy)]
struct Tap {
    key: Key,
    started: f64,
    second: bool,
}

#[derive(Default)]
pub(super) struct SearchEverywhereGesture {
    held_shifts: u8,
    pending: Option<Tap>,
    clean_press: Option<f64>,
}

impl SearchEverywhereGesture {
    /// Forget candidates and held-key history when another UI owns input or
    /// the native window loses focus. A later unmatched release cannot arm it.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    fn invalidate(&mut self) {
        self.pending = None;
        self.clean_press = None;
        // Keep physically held Shift keys until their releases. A duplicate
        // down after a typing event must not be mistaken for a fresh tap.
    }

    /// Observe the original event slice without consuming editor input. Use
    /// egui InputState.time; process even multiple taps within a single frame.
    /// Returns true only after completing the second clean, non-overlapping tap.
    pub(super) fn update(&mut self, now: f64, events: &[Event]) -> bool {
        let mut fired = false;
        for event in events {
            match event {
                Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } if shift_bit(*key).is_some() => {
                    let bit = shift_bit(*key).unwrap();
                    if *pressed {
                        if forbidden_modifiers(*modifiers) {
                            self.held_shifts |= bit;
                            self.invalidate();
                            continue;
                        }
                        if *repeat || self.held_shifts & bit != 0 {
                            continue;
                        }
                        let previously_held = self.held_shifts;
                        self.held_shifts |= bit;
                        if previously_held != 0 {
                            // Holding left and pressing right is one chord,
                            // rather than two separate Shift taps.
                            self.invalidate();
                            continue;
                        }
                        let second = self.clean_press.take().is_some_and(|first| {
                            let elapsed = now - first;
                            (0.0..DOUBLE_SHIFT_SECONDS).contains(&elapsed)
                        });
                        self.pending = Some(Tap {
                            key: *key,
                            started: now,
                            second,
                        });
                    } else {
                        let was_held = self.held_shifts & bit != 0;
                        self.held_shifts &= !bit;
                        if forbidden_modifiers(*modifiers) {
                            self.invalidate();
                            continue;
                        }
                        if !was_held || self.held_shifts != 0 {
                            continue;
                        }
                        if let Some(tap) = self.pending.take().filter(|tap| tap.key == *key) {
                            if tap.second {
                                fired = true;
                                self.clean_press = None;
                            } else {
                                self.clean_press = Some(tap.started);
                            }
                        }
                    }
                }
                Event::ModifiersChanged(modifiers) if forbidden_modifiers(*modifiers) => {
                    self.invalidate()
                }
                Event::Key { .. }
                | Event::Text(_)
                | Event::Paste(_)
                | Event::Copy
                | Event::Cut
                | Event::Ime(_)
                | Event::PointerButton { .. }
                | Event::MouseWheel { .. } => self.invalidate(),
                Event::WindowFocused(false) => {
                    self.reset();
                    fired = false;
                }
                // Modifier changes for Shift, pointer motion, redraws and
                // screenshot replies are not extra taps or typing chords.
                _ => {}
            }
        }
        fired
    }
}

fn shift_bit(key: Key) -> Option<u8> {
    match key {
        Key::ShiftLeft => Some(1),
        Key::ShiftRight => Some(2),
        _ => None,
    }
}

fn forbidden_modifiers(modifiers: Modifiers) -> bool {
    modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: if pressed {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        }
    }

    fn tap(gesture: &mut SearchEverywhereGesture, time: f64, key: Key) -> bool {
        assert!(
            !gesture.update(time, &[shift(key, true)]),
            "a press alone cannot complete a clean tap"
        );
        gesture.update(time + 0.0005, &[shift(key, false)])
    }

    #[test]
    fn complete_taps_allow_either_shift_and_keep_strict_press_deadline() {
        for second in [Key::ShiftLeft, Key::ShiftRight] {
            let mut gesture = SearchEverywhereGesture::default();
            assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
            assert!(tap(&mut gesture, 1.349, second));
        }
        for second_time in [1.350, 1.351] {
            let mut gesture = SearchEverywhereGesture::default();
            assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
            assert!(!tap(&mut gesture, second_time, Key::ShiftRight));
            // An expired second tap becomes a new first tap.
            assert!(tap(&mut gesture, second_time + 0.10, Key::ShiftLeft));
        }
    }

    #[test]
    fn one_frame_preserves_order_and_second_release_controls_firing() {
        let mut gesture = SearchEverywhereGesture::default();
        assert!(gesture.update(
            1.0,
            &[
                shift(Key::ShiftLeft, true),
                shift(Key::ShiftLeft, false),
                shift(Key::ShiftRight, true),
                shift(Key::ShiftRight, false),
            ]
        ));
        assert!(!gesture.update(1.01, &[]));
        let mut gesture = SearchEverywhereGesture::default();
        assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
        assert!(!gesture.update(1.20, &[shift(Key::ShiftRight, true)]));
        // The strict deadline was satisfied by the second press: holding it
        // longer does not change that press-to-press interval.
        assert!(gesture.update(1.60, &[shift(Key::ShiftRight, false)]));
    }

    #[test]
    fn text_during_either_held_tap_invalidates_it_across_frames() {
        let mut gesture = SearchEverywhereGesture::default();
        assert!(!gesture.update(1.0, &[shift(Key::ShiftLeft, true)]));
        assert!(!gesture.update(1.01, &[Event::Text("R".into())]));
        assert!(!gesture.update(1.02, &[shift(Key::ShiftLeft, false)]));
        assert!(!tap(&mut gesture, 1.04, Key::ShiftLeft));
        assert!(!gesture.update(1.20, &[shift(Key::ShiftRight, true)]));
        assert!(!gesture.update(1.21, &[Event::Text("U".into())]));
        assert!(!gesture.update(1.22, &[shift(Key::ShiftRight, false)]));
        assert!(!tap(&mut gesture, 1.24, Key::ShiftLeft));
    }

    #[test]
    fn intervening_key_text_and_click_remove_a_completed_candidate() {
        for event in [
            Event::Key {
                key: Key::ArrowRight,
                physical_key: Some(Key::ArrowRight),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Event::Text("x".into()),
            Event::PointerButton {
                pos: egui::pos2(12.0, 18.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ] {
            let mut gesture = SearchEverywhereGesture::default();
            assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
            assert!(!gesture.update(1.10, &[event]));
            assert!(!tap(&mut gesture, 1.20, Key::ShiftRight));
        }
    }

    #[test]
    fn repeated_and_duplicate_shift_presses_cannot_count_as_extra_taps() {
        let mut gesture = SearchEverywhereGesture::default();
        assert!(!gesture.update(1.0, &[shift(Key::ShiftLeft, true)]));
        let mut repeat = shift(Key::ShiftLeft, true);
        if let Event::Key { repeat, .. } = &mut repeat {
            *repeat = true;
        }
        assert!(!gesture.update(1.01, &[repeat]));
        assert!(!gesture.update(1.02, &[shift(Key::ShiftLeft, true)]));
        assert!(!gesture.update(1.03, &[shift(Key::ShiftLeft, false)]));
        assert!(tap(&mut gesture, 1.20, Key::ShiftLeft));
    }

    #[test]
    fn overlapping_left_right_shift_is_a_chord_and_requires_both_releases() {
        let mut gesture = SearchEverywhereGesture::default();
        assert!(!gesture.update(1.0, &[shift(Key::ShiftLeft, true)]));
        assert!(!gesture.update(1.01, &[shift(Key::ShiftRight, true)]));
        assert!(!gesture.update(1.02, &[shift(Key::ShiftLeft, false)]));
        assert!(!gesture.update(1.03, &[shift(Key::ShiftRight, false)]));
        assert!(!tap(&mut gesture, 1.10, Key::ShiftRight));
        assert!(tap(&mut gesture, 1.20, Key::ShiftLeft));
    }

    #[test]
    fn other_modifiers_invalidate_candidates_and_never_make_shift_taps() {
        for modifiers in [
            Modifiers::SHIFT | Modifiers::CTRL,
            Modifiers::SHIFT | Modifiers::ALT,
            Modifiers {
                shift: true,
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
        ] {
            let mut gesture = SearchEverywhereGesture::default();
            assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
            assert!(!gesture.update(1.10, &[Event::ModifiersChanged(modifiers)]));
            let mut press = shift(Key::ShiftLeft, true);
            if let Event::Key {
                modifiers: held, ..
            } = &mut press
            {
                *held = modifiers;
            }
            assert!(!gesture.update(1.11, &[press]));
            assert!(!gesture.update(1.12, &[shift(Key::ShiftLeft, false)]));
            assert!(!tap(&mut gesture, 1.20, Key::ShiftRight));
        }
    }

    #[test]
    fn focus_loss_and_explicit_reset_discard_candidates_and_held_history() {
        let mut gesture = SearchEverywhereGesture::default();
        assert!(!tap(&mut gesture, 1.0, Key::ShiftLeft));
        assert!(!gesture.update(1.10, &[Event::WindowFocused(false)]));
        assert!(!tap(&mut gesture, 1.20, Key::ShiftRight));
        gesture.reset();
        assert!(!gesture.update(1.21, &[shift(Key::ShiftLeft, false)]));
        assert!(!gesture.update(1.22, &[shift(Key::ShiftLeft, true)]));
        gesture.reset();
        assert!(!gesture.update(1.23, &[shift(Key::ShiftLeft, false)]));
        assert!(!tap(&mut gesture, 1.24, Key::ShiftRight));
    }
}

//! Scripted pointer/keyboard input for headless smoke tests (`--input-script`).
//!
//! One command per line (`#` comments allowed):
//!
//! ```text
//! wait 0.5                 # pause (seconds)
//! move 320 96              # pointer move
//! click 320 96 [ctrl]      # press + release
//! dclick 320 96            # double click
//! rclick 320 96            # right click (context menu)
//! down 320 96 [ctrl|shift|alt]...
//! up 600 137
//! drag 320 96 600 137 [ctrl]   # press, interpolated moves, release
//! key S ctrl               # key press + release (egui key names)
//! key C ctrl               # Copy event, as winit sends for Ctrl+C (Ctrl+X: Cut)
//! hold ctrl                # keep modifiers held (e.g. Ctrl for Ctrl+Tab, Tab, …)
//! release                  # release held modifiers
//! type hello               # text input, one character per frame
//! typefast hello           # the whole string in one event
//! paste echo a\necho b     # clipboard paste (\n = newline)
//! mark power-save          # records that the script reached this point
//! ```
//!
//! `mark` emits a `script_step` row into the opt-in performance trace, so an
//! acceptance driver can tell a step that ran from one that was never reached.

use std::collections::VecDeque;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput};

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Wait(f64),
    Move(Pos2),
    Button {
        pos: Pos2,
        pressed: bool,
        mods: Modifiers,
    },
    /// Secondary (right) button press + release.
    RightClick(Pos2),
    Key {
        key: Key,
        mods: Modifiers,
    },
    Text(String),
    /// Clipboard paste (`\n` escapes allowed).
    Paste(String),
    /// Sets the held modifiers without other input.
    Mods(Modifiers),
    /// A named progress marker for the performance trace; no input.
    Mark(String),
}

fn parse_mods(words: &[&str]) -> Modifiers {
    let mut m = Modifiers::NONE;
    for w in words {
        match w.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                m.ctrl = true;
                m.command = !cfg!(target_os = "macos");
            }
            "cmd" | "command" => m.command = true,
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            _ => {}
        }
    }
    m
}

/// Parses a script into steps; unknown lines are reported as errors.
pub fn parse(src: &str) -> Result<VecDeque<Step>, String> {
    let mut out = VecDeque::new();
    for (n, raw) in src.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| -> Result<f32, String> {
            words
                .get(i)
                .and_then(|w| w.parse::<f32>().ok())
                .ok_or_else(|| format!("line {}: expected a number in `{line}`", n + 1))
        };
        match words[0] {
            "wait" => out.push_back(Step::Wait(num(1)? as f64)),
            "move" => out.push_back(Step::Move(Pos2::new(num(1)?, num(2)?))),
            "down" | "up" => {
                let pos = Pos2::new(num(1)?, num(2)?);
                let mods = parse_mods(&words[3..]);
                out.push_back(Step::Mods(mods));
                out.push_back(Step::Move(pos));
                out.push_back(Step::Button {
                    pos,
                    pressed: words[0] == "down",
                    mods,
                });
            }
            "click" | "dclick" => {
                let pos = Pos2::new(num(1)?, num(2)?);
                let mods = parse_mods(&words[3..]);
                let times = if words[0] == "dclick" { 2 } else { 1 };
                out.push_back(Step::Mods(mods));
                out.push_back(Step::Move(pos));
                for _ in 0..times {
                    out.push_back(Step::Button {
                        pos,
                        pressed: true,
                        mods,
                    });
                    out.push_back(Step::Button {
                        pos,
                        pressed: false,
                        mods,
                    });
                }
                out.push_back(Step::Mods(Modifiers::NONE));
            }
            "rclick" => {
                let pos = Pos2::new(num(1)?, num(2)?);
                out.push_back(Step::Move(pos));
                out.push_back(Step::RightClick(pos));
            }
            "drag" => {
                let (a, b) = (Pos2::new(num(1)?, num(2)?), Pos2::new(num(3)?, num(4)?));
                let mods = parse_mods(&words[5..]);
                out.push_back(Step::Mods(mods));
                out.push_back(Step::Move(a));
                out.push_back(Step::Button {
                    pos: a,
                    pressed: true,
                    mods,
                });
                for i in 1..=8 {
                    out.push_back(Step::Move(a + (b - a) * (i as f32 / 8.0)));
                }
                out.push_back(Step::Button {
                    pos: b,
                    pressed: false,
                    mods,
                });
                out.push_back(Step::Mods(Modifiers::NONE));
            }
            "hold" => out.push_back(Step::Mods(parse_mods(&words[1..]))),
            "release" => out.push_back(Step::Mods(Modifiers::NONE)),
            "key" => {
                let name = words
                    .get(1)
                    .ok_or_else(|| format!("line {}: key name missing", n + 1))?;
                let key = Key::from_name(name)
                    .ok_or_else(|| format!("line {}: unknown key `{name}`", n + 1))?;
                out.push_back(Step::Key {
                    key,
                    mods: parse_mods(&words[2..]),
                });
            }
            // One character per frame, like a real keyboard.
            "type" => {
                for c in line.trim_start_matches("type").trim_start().chars() {
                    out.push_back(Step::Text(c.to_string()));
                }
            }
            "typefast" => out.push_back(Step::Text(
                line.trim_start_matches("typefast").trim_start().to_string(),
            )),
            "paste" => out.push_back(Step::Paste(
                line.trim_start_matches("paste")
                    .trim_start()
                    .replace("\\n", "\n"),
            )),
            "mark" => {
                let label = line.trim_start_matches("mark").trim();
                if label.is_empty() {
                    return Err(format!("line {}: mark label missing", n + 1));
                }
                out.push_back(Step::Mark(label.to_string()));
            }
            other => return Err(format!("line {}: unknown command `{other}`", n + 1)),
        }
    }
    Ok(out)
}

/// The event winit sends instead of a key press for a clipboard shortcut
/// (mirrors egui-winit's `is_copy_command` / `is_cut_command`): widgets never
/// see Ctrl+C or Ctrl+X (Cmd on macOS) as keys. Ctrl+V would carry the
/// clipboard contents, so it stays a key press here; use `paste` for that.
fn clipboard_event(key: Key, mods: Modifiers) -> Option<Event> {
    let windows = cfg!(target_os = "windows");
    if key == Key::Cut
        || (mods.command && key == Key::X)
        || (windows && mods.shift && key == Key::Delete)
    {
        Some(Event::Cut)
    } else if key == Key::Copy
        || (mods.command && key == Key::C)
        || (windows && mods.ctrl && key == Key::Insert)
    {
        Some(Event::Copy)
    } else {
        None
    }
}

/// Feeds scripted steps into egui, one step per frame.
#[derive(Default)]
pub struct InputPlayer {
    steps: VecDeque<Step>,
    wait_until: f64,
    mods: Modifiers,
    last_pos: Option<Pos2>,
    finished: bool,
}

impl InputPlayer {
    pub fn new(steps: VecDeque<Step>) -> Self {
        Self {
            steps,
            ..Default::default()
        }
    }

    pub fn is_active(&self) -> bool {
        !self.steps.is_empty()
    }

    /// Whether every step has been replayed (the player then only keeps the
    /// window focused).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Applies the next step to `raw`; returns true while more steps remain.
    pub fn feed(&mut self, raw: &mut RawInput) -> bool {
        let now = raw.time.unwrap_or(0.0);
        // A headless X server has no window manager, so the window never gets
        // focus; scripted input stands in for a user at a focused window. That
        // user stays at the window after the last step: a screenshot taken
        // later must still see focus-bound popups (completion lookups close
        // when the window loses focus), not a window the user left.
        raw.focused = true;
        // Window-level shortcuts read the viewport's focus, not `raw.focused`.
        let viewport = raw.viewport_id;
        raw.viewports.entry(viewport).or_default().focused = Some(true);
        if self.finished || (self.steps.is_empty() && now >= self.wait_until) {
            self.finished = true;
            return false;
        }
        if now < self.wait_until {
            return true;
        }
        // Consume modifier changes eagerly, then exactly one input step.
        while let Some(Step::Mods(m)) = self.steps.front() {
            let m = *m;
            self.set_mods(raw, m);
            self.steps.pop_front();
        }
        let Some(step) = self.steps.pop_front() else {
            self.finished = true;
            return false;
        };
        match step {
            Step::Wait(s) => self.wait_until = now + s,
            Step::Move(p) => {
                raw.events.push(Event::PointerMoved(p));
                self.last_pos = Some(p);
            }
            Step::Button { pos, pressed, mods } => {
                if self.last_pos != Some(pos) {
                    raw.events.push(Event::PointerMoved(pos));
                    self.last_pos = Some(pos);
                }
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: mods,
                });
            }
            Step::RightClick(pos) => {
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: true,
                    modifiers: self.mods,
                });
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: false,
                    modifiers: self.mods,
                });
            }
            Step::Key { key, mods } => {
                let held = self.mods;
                if mods != held {
                    raw.events.push(Event::ModifiersChanged(mods));
                }
                if let Some(ev) = clipboard_event(key, mods) {
                    raw.events.push(ev);
                } else {
                    raw.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: mods,
                    });
                    raw.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: mods,
                    });
                }
                if mods != held {
                    raw.events.push(Event::ModifiersChanged(held));
                }
            }
            Step::Text(t) => raw.events.push(Event::Text(t)),
            Step::Paste(t) => raw.events.push(Event::Paste(t)),
            Step::Mods(m) => self.set_mods(raw, m),
            Step::Mark(label) => {
                crate::performance_trace::event("script_step", serde_json::json!({"label":label}))
            }
        }
        !self.steps.is_empty() || now < self.wait_until
    }

    fn set_mods(&mut self, raw: &mut RawInput, m: Modifiers) {
        if m != self.mods {
            raw.events.push(Event::ModifiersChanged(m));
            self.mods = m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let steps =
            parse("wait 0.5\nclick 10 20 ctrl # comment\nkey S ctrl\ntype hi there\n").unwrap();
        assert_eq!(steps[0], Step::Wait(0.5));
        assert!(matches!(steps[1], Step::Mods(m) if m.ctrl));
        assert!(steps
            .iter()
            .any(|s| matches!(s, Step::Key { key: Key::S, mods } if mods.ctrl)));
        assert_eq!(
            steps.back(),
            Some(&Step::Text("e".into())),
            "type sends one character per step"
        );
        assert!(parse("bogus 1").is_err());
        assert_eq!(parse("paste a\\nb").unwrap()[0], Step::Paste("a\nb".into()));
        let held = parse("hold ctrl\nkey Tab ctrl\nrelease").unwrap();
        assert!(matches!(held[0], Step::Mods(m) if m.ctrl));
        assert_eq!(held[2], Step::Mods(Modifiers::NONE));
    }

    #[test]
    fn held_modifiers_stay_down_across_keys() {
        let mut p = InputPlayer::new(parse("hold ctrl\nkey Tab ctrl\nrelease\nwait 0").unwrap());
        let mut raw = RawInput {
            time: Some(0.0),
            ..Default::default()
        };
        p.feed(&mut raw);
        // Ctrl goes down and stays down: no release after the Tab press.
        assert!(matches!(raw.events.first(), Some(Event::ModifiersChanged(m)) if m.ctrl));
        assert!(!raw
            .events
            .iter()
            .any(|e| matches!(e, Event::ModifiersChanged(m) if !m.ctrl)));
        let mut raw = RawInput {
            time: Some(0.1),
            ..Default::default()
        };
        p.feed(&mut raw);
        assert_eq!(
            raw.events,
            vec![Event::ModifiersChanged(Modifiers::NONE)],
            "release lets go of Ctrl"
        );
    }

    #[test]
    fn the_window_stays_focused_during_and_after_the_script() {
        let mut p = InputPlayer::new(parse("key A\nwait 1").unwrap());
        // The window of a headless X server never has focus.
        let mut feed = |t: f64| {
            let mut raw = RawInput {
                time: Some(t),
                focused: false,
                ..Default::default()
            };
            let more = p.feed(&mut raw);
            // Global shortcuts check the viewport's focus too.
            let viewport = raw.viewports.get(&raw.viewport_id).and_then(|v| v.focused);
            assert_eq!(viewport == Some(true), raw.focused);
            (more, raw.focused)
        };
        assert_eq!(feed(0.0), (true, true), "the key");
        assert_eq!(feed(0.1), (true, true), "the wait starts");
        assert_eq!(
            feed(0.6),
            (true, true),
            "still waiting: focus-bound popups stay open"
        );
        assert_eq!(
            feed(1.2),
            (false, true),
            "done: the scripted user has not left the window"
        );
        assert_eq!(feed(5.0), (false, true), "later frames stay focused");
    }

    #[test]
    fn drag_expands_to_press_moves_release() {
        let steps = parse("drag 0 0 80 0").unwrap();
        let presses = steps
            .iter()
            .filter(|s| matches!(s, Step::Button { pressed: true, .. }))
            .count();
        let releases = steps
            .iter()
            .filter(|s| matches!(s, Step::Button { pressed: false, .. }))
            .count();
        let moves = steps.iter().filter(|s| matches!(s, Step::Move(_))).count();
        assert_eq!((presses, releases, moves), (1, 1, 9));
    }

    #[test]
    fn clipboard_shortcuts_arrive_as_copy_and_cut_events() {
        let frame = |script: &str| {
            let mut p = InputPlayer::new(parse(script).unwrap());
            let mut raw = RawInput {
                time: Some(0.0),
                ..Default::default()
            };
            p.feed(&mut raw);
            raw.events
        };
        let is_key = |e: &Event| matches!(e, Event::Key { .. });
        let copy = frame("key C ctrl");
        assert!(
            copy.contains(&Event::Copy) && !copy.iter().any(is_key),
            "{copy:?}"
        );
        let copy_shift = frame("key C ctrl shift");
        assert!(
            copy_shift.contains(&Event::Copy),
            "Ctrl+Shift+C is a copy too: {copy_shift:?}"
        );
        // The shift state is visible to the widget at the time of the event.
        assert!(
            matches!(copy_shift.first(), Some(Event::ModifiersChanged(m)) if m.shift && m.ctrl)
        );
        let cut = frame("key X ctrl");
        assert!(
            cut.contains(&Event::Cut) && !cut.iter().any(is_key),
            "{cut:?}"
        );
        assert!(
            frame("key C").iter().any(is_key),
            "a plain C is a key press"
        );
        assert!(
            frame("key V ctrl").iter().any(is_key),
            "paste stays a key press"
        );
    }

    #[test]
    fn player_feeds_one_step_per_frame_and_honours_waits() {
        let mut p = InputPlayer::new(parse("move 5 5\nwait 1\nmove 6 6").unwrap());
        let mut raw = RawInput {
            time: Some(0.0),
            ..Default::default()
        };
        assert!(p.feed(&mut raw));
        assert_eq!(raw.events.len(), 1);
        let mut raw = RawInput {
            time: Some(0.1),
            ..Default::default()
        };
        p.feed(&mut raw); // wait starts
        let mut raw = RawInput {
            time: Some(0.5),
            ..Default::default()
        };
        p.feed(&mut raw);
        assert!(raw.events.is_empty(), "still waiting");
        let mut raw = RawInput {
            time: Some(1.2),
            ..Default::default()
        };
        assert!(!p.feed(&mut raw));
        assert_eq!(raw.events, vec![Event::PointerMoved(Pos2::new(6.0, 6.0))]);
    }

    #[test]
    fn marks_are_named_steps_without_input() {
        let steps = parse("mark power save on\nkey Escape").unwrap();
        assert_eq!(steps[0], Step::Mark("power save on".into()));
        assert!(parse("mark").is_err(), "a mark needs a label");
        let mut p = InputPlayer::new(steps);
        let mut raw = RawInput {
            time: Some(0.0),
            ..Default::default()
        };
        assert!(p.feed(&mut raw), "the key step remains");
        assert!(raw.events.is_empty(), "a mark sends no input");
    }
}

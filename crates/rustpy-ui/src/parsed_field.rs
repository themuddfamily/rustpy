//! Single-line text fields for values that are edited as text and parsed
//! back: environment variables (`KEY=VALUE;…`), comma-separated lists, a name
//! that must stay valid. Rebuilding such a field from its value every frame
//! loses the keystrokes that do not parse yet (a `;` or `,` separator, a
//! `KEY` before its `=`, an emptied name before the new one is typed), so
//! while the field has keyboard focus it shows exactly what was typed; once
//! focus leaves it shows the value's own text again.

use std::collections::BTreeMap;

/// A field drawn by [`singleline`].
pub struct ParsedField {
    pub response: egui::Response,
    /// The text in the field: what was typed while it has focus, else the
    /// value's text.
    pub text: String,
    /// The user edited the text this frame (parse [`ParsedField::text`]).
    pub changed: bool,
}

/// A single-line field for a value whose text is `value`. `id` must be the
/// same every frame (it keys the typed text in egui memory); `style` sets
/// the width, hint and so on.
pub fn singleline(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &str,
    style: impl for<'t> FnOnce(egui::TextEdit<'t>) -> egui::TextEdit<'t>,
) -> ParsedField {
    let typed_id = id.with("typed");
    // Keyboard focus as egui remembers it: unlike `Response::has_focus` it
    // stays while the IDE window is in the background.
    let focused = |ui: &egui::Ui| ui.memory(|m| m.has_focus(id));
    let typed = if focused(ui) {
        ui.data(|d| d.get_temp::<String>(typed_id))
    } else {
        None
    };
    let mut text = typed.unwrap_or_else(|| value.to_string());
    let response = ui.add(style(egui::TextEdit::singleline(&mut text).id(id)));
    let changed = response.changed();
    if focused(ui) {
        ui.data_mut(|d| d.insert_temp(typed_id, text.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(typed_id));
    }
    ParsedField {
        response,
        text,
        changed,
    }
}

/// The text of an environment variables field: `KEY=VALUE` pairs joined by `;`.
pub fn env_text(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Parses an environment variables field (`KEY=VALUE;KEY2=VALUE2`). Names
/// and values are trimmed; an entry without `=` or without a name (one that
/// is still being typed) is skipped.
pub fn parse_env_text(text: &str) -> BTreeMap<String, String> {
    text.split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// Why `name` (trimmed) cannot name a run configuration: it is empty, or
/// one of the `others` already has it.
pub fn config_name_problem(name: &str, others: &[String]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        Some("Enter a name".into())
    } else if others.iter().any(|o| o == name) {
        Some(format!("'{name}' is already used"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One headless frame with `events`; `ui` draws the field.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, ui: impl FnMut(&mut egui::Ui)) {
        frame_in(ctx, true, events, ui);
    }

    /// [`frame`] with the window focused (`window`) or in the background.
    fn frame_in(
        ctx: &egui::Context,
        window: bool,
        events: Vec<egui::Event>,
        mut ui: impl FnMut(&mut egui::Ui),
    ) {
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: window,
            ..Default::default()
        };
        ctx.run_ui(input, |root| {
            egui::Frame::new().show(root, |inner| ui(inner));
        })
        .drop_without_applying_deltas();
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Types `text` one character per frame into the environment field and
    /// returns what the field showed last.
    fn type_env(
        ctx: &egui::Context,
        id: egui::Id,
        env: &mut BTreeMap<String, String>,
        text: &str,
    ) -> String {
        let mut shown = String::new();
        for c in text.chars() {
            frame(ctx, vec![egui::Event::Text(c.to_string())], |ui| {
                let field = singleline(ui, id, &env_text(env), |t| t);
                if field.changed {
                    *env = parse_env_text(&field.text);
                }
                shown = field.text;
            });
        }
        shown
    }

    fn focus(ctx: &egui::Context, id: egui::Id, value: &str) {
        frame(ctx, Vec::new(), |ui| {
            ui.memory_mut(|m| m.request_focus(id));
            singleline(ui, id, value, |t| t);
        });
    }

    /// QA: typing `FOO=1;BAR=2` gave `=1BAR=2` and the environment {"": "1BAR=2"}.
    #[test]
    fn environment_field_keeps_every_keystroke() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("env");
        let mut env = BTreeMap::new();
        focus(&ctx, id, "");
        assert_eq!(type_env(&ctx, id, &mut env, "FOO=1;BAR=2"), "FOO=1;BAR=2");
        assert_eq!(
            env,
            BTreeMap::from([
                ("BAR".to_string(), "2".to_string()),
                ("FOO".to_string(), "1".to_string())
            ])
        );
        // Switching to another application keeps what was typed.
        frame_in(&ctx, false, Vec::new(), |ui| {
            assert_eq!(
                singleline(ui, id, &env_text(&env), |t| t).text,
                "FOO=1;BAR=2"
            )
        });
        assert_eq!(
            type_env(&ctx, id, &mut env, ";DEBUG"),
            "FOO=1;BAR=2;DEBUG",
            "an entry without '=' stays in the field"
        );
        assert_eq!(env.len(), 2);
        // Without focus the field shows the parsed value.
        frame(&ctx, Vec::new(), |ui| {
            ui.memory_mut(|m| m.surrender_focus(id));
        });
        frame(&ctx, Vec::new(), |ui| {
            assert_eq!(
                singleline(ui, id, &env_text(&env), |t| t).text,
                "BAR=2;FOO=1"
            )
        });
    }

    /// QA: clearing the Name field was refused, so typing `Server` gave `ServerPython 1`.
    #[test]
    fn a_name_can_be_cleared_and_retyped() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("name");
        let others = vec!["Other".to_string()];
        let mut name = "Python 1".to_string();
        let mut problem = None;
        focus(&ctx, id, &name);
        let step = |events: Vec<egui::Event>, name: &mut String, problem: &mut Option<String>| {
            let mut shown = String::new();
            frame(&ctx, events, |ui| {
                let field = singleline(ui, id, name, |t| t);
                *problem = config_name_problem(&field.text, &others);
                if field.changed && problem.is_none() {
                    *name = field.text.trim().to_string();
                }
                shown = field.text;
            });
            shown
        };
        // Ctrl+A, Backspace.
        let clear = || {
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
                key(egui::Key::Backspace),
            ]
        };
        assert_eq!(step(clear(), &mut name, &mut problem), "");
        assert_eq!(
            step(Vec::new(), &mut name, &mut problem),
            "",
            "the emptied field stays empty"
        );
        assert_eq!(problem.as_deref(), Some("Enter a name"));
        assert_eq!(name, "Python 1", "an empty name is not applied");
        for c in "Server".chars() {
            step(
                vec![egui::Event::Text(c.to_string())],
                &mut name,
                &mut problem,
            );
        }
        assert_eq!((name.as_str(), problem.as_deref()), ("Server", None));
        // A name another configuration has is shown but not applied.
        step(clear(), &mut name, &mut problem);
        for c in "Other".chars() {
            step(
                vec![egui::Event::Text(c.to_string())],
                &mut name,
                &mut problem,
            );
        }
        assert_eq!(step(Vec::new(), &mut name, &mut problem), "Other");
        assert_eq!(problem.as_deref(), Some("'Other' is already used"));
        assert_eq!(name, "Othe");
    }

    #[test]
    fn environment_text_round_trip() {
        let env = parse_env_text(" A = 1 ;B=x=y;;C;=orphan;D=");
        assert_eq!(
            env,
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "x=y".to_string()),
                ("D".to_string(), String::new())
            ])
        );
        assert_eq!(env_text(&env), "A=1;B=x=y;D=");
        assert_eq!(parse_env_text(&env_text(&env)), env);
        assert!(parse_env_text("").is_empty());
    }

    #[test]
    fn configuration_names() {
        let others = vec!["a".to_string()];
        assert_eq!(config_name_problem("b", &others), None);
        assert_eq!(
            config_name_problem("  ", &others).as_deref(),
            Some("Enter a name")
        );
        assert_eq!(
            config_name_problem(" a ", &others).as_deref(),
            Some("'a' is already used")
        );
    }
}

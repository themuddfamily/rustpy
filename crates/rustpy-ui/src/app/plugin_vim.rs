//! Deliberately bounded modal editing, implemented with native editor commands
//! and undo history. No vimrc evaluation, shell commands or Vimscript runtime.
use super::*;
#[derive(Default)]
pub(super) struct State {
    owner: Option<(rustpy_text::DocumentId, egui::Id)>,
    insert: bool,
    command: Option<String>,
}

fn normal(state: &mut crate::editor::EditorState, character: char, insert: &mut bool) -> bool {
    match character {
        'i' => *insert = true,
        'a' => {
            state.move_right(false);
            *insert = true;
        }
        'h' => state.move_left(false),
        'l' => state.move_right(false),
        'j' => state.move_vertical(1, false),
        'k' => state.move_vertical(-1, false),
        '0' => state.move_line_start(false, &Default::default()),
        '$' => state.move_line_end(false),
        'x' => state.delete_forward(),
        'u' => {
            state.undo();
        }
        _ => return false,
    }
    true
}
impl IdeApp {
    /// Call from raw_input_hook after shortcut normalization, before the editor
    /// sees events. Only the actually focused text editor owns modal input.
    pub(super) fn plugin_modal_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if !self.settings.plugins.vim_enabled
            || !self.editor_keyboard()
            || !self.component_menu_owner_available(false)
        {
            return;
        }
        let Some(index) = self.active else { return };
        if !ctx.memory(|memory| memory.has_focus(self.tabs[index].view.id)) {
            return;
        }
        let owner = (
            self.tabs[index].state.doc.identity(),
            self.tabs[index].view.id,
        );
        let mut vim = std::mem::take(&mut self.plugin_runtime.vim);
        if vim.owner != Some(owner) {
            vim = State {
                owner: Some(owner),
                ..Default::default()
            };
        }
        let mut output = Vec::new();
        let mut actions = Vec::new();
        let mut note = None;
        for event in std::mem::take(&mut input.events) {
            match event {
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                } => {
                    vim.insert = false;
                    vim.command = None;
                    note = Some("NORMAL".to_string());
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } if vim.command.is_some() => {
                    let command = vim.command.take().unwrap();
                    match command.as_str() {
                        "w" => actions.push(Action::Save),
                        "q" => actions.push(Action::CloseEditor),
                        "wq" => {
                            actions.push(Action::Save);
                            actions.push(Action::CloseEditor);
                        }
                        _ => note = Some(format!("Unsupported modal command: :{command}")),
                    }
                }
                egui::Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    ..
                } if vim.command.is_some() => {
                    vim.command.as_mut().unwrap().pop();
                }
                egui::Event::Key {
                    key: egui::Key::R,
                    pressed: true,
                    modifiers,
                    ..
                } if !vim.insert && modifiers.ctrl && !modifiers.alt && !modifiers.shift => {
                    self.tabs[index].state.redo();
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } if !vim.insert => {
                    self.tabs[index].state.move_vertical(1, false);
                }
                egui::Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    ..
                } if !vim.insert => {
                    self.tabs[index].state.move_left(false);
                }
                egui::Event::Key {
                    key: egui::Key::Delete,
                    pressed: true,
                    ..
                } if !vim.insert => {
                    self.tabs[index].state.delete_forward();
                }
                egui::Event::Key {
                    key: egui::Key::Tab,
                    pressed: true,
                    ..
                } if !vim.insert => {}
                egui::Event::Paste(_) if !vim.insert => {
                    note = Some("Use Insert mode to paste".into());
                }
                egui::Event::Text(text) if !vim.insert => {
                    if let Some(command) = vim.command.as_mut() {
                        if command.len() + text.len() <= 128 {
                            command.push_str(&text);
                        }
                        note = Some(format!(":{command}"));
                    } else {
                        let mut characters = text.chars();
                        while let Some(character) = characters.next() {
                            if character == ':' {
                                vim.command = Some(characters.take(128).collect());
                                note = Some(":".into());
                                break;
                            }
                            if !normal(&mut self.tabs[index].state, character, &mut vim.insert) {
                                note = Some(format!("Unsupported modal command: {character}"));
                            }
                            if vim.insert {
                                let rest = characters.collect::<String>();
                                if !rest.is_empty() {
                                    output.push(egui::Event::Text(rest));
                                }
                                note = Some("INSERT".into());
                                break;
                            }
                        }
                    }
                }
                egui::Event::Key { key, modifiers, .. }
                    if !vim.insert
                        && !modifiers.ctrl
                        && !modifiers.command
                        && !modifiers.alt
                        && matches!(
                            key,
                            egui::Key::A
                                | egui::Key::B
                                | egui::Key::C
                                | egui::Key::D
                                | egui::Key::E
                                | egui::Key::F
                                | egui::Key::G
                                | egui::Key::H
                                | egui::Key::I
                                | egui::Key::J
                                | egui::Key::K
                                | egui::Key::L
                                | egui::Key::M
                                | egui::Key::N
                                | egui::Key::O
                                | egui::Key::P
                                | egui::Key::Q
                                | egui::Key::R
                                | egui::Key::S
                                | egui::Key::T
                                | egui::Key::U
                                | egui::Key::V
                                | egui::Key::W
                                | egui::Key::X
                                | egui::Key::Y
                                | egui::Key::Z
                        ) => {}
                event => output.push(event),
            }
        }
        input.events = output;
        self.plugin_runtime.vim = vim;
        if let Some(note) = note {
            self.set_status(note);
        }
        for action in actions {
            if self
                .active
                .and_then(|index| self.tabs.get(index))
                .is_none_or(|tab| tab.state.doc.identity() != owner.0)
            {
                break;
            }
            if action == Action::CloseEditor
                && self.tabs[index].has_unsaved_changes()
                && !self.save_all()
            {
                break;
            }
            self.handle_action(action, ctx);
        }
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn bounded_normal_commands_use_native_undo_and_never_interpret_shell_or_vimscript() {
        let mut editor = crate::editor::EditorState::from_text("abc\n", Language::Python);
        let mut insert = false;
        assert!(normal(&mut editor, 'l', &mut insert));
        assert!(normal(&mut editor, 'x', &mut insert));
        assert_eq!(editor.doc.text(), "ac\n");
        assert!(normal(&mut editor, 'u', &mut insert));
        assert_eq!(editor.doc.text(), "abc\n");
        assert!(!normal(&mut editor, '!', &mut insert));
        assert!(normal(&mut editor, 'i', &mut insert));
        assert!(insert);
    }
}

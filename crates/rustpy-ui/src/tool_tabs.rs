//! Presentation of a retained live content identity; closing is delegated to its owner.
#[derive(Default)]
pub(crate) struct Appearance {
    pub title: String,
    pub color: Option<[u8; 3]>,
    pub pinned: bool,
}
#[derive(Clone, Copy, Default)]
pub(crate) enum Command {
    #[default]
    None,
    Close,
    CloseOthers,
}
impl Appearance {
    pub fn label(&self, default: &str) -> egui::RichText {
        let name = if self.title.is_empty() {
            default
        } else {
            &self.title
        };
        let text = egui::RichText::new(format!("{}{}", if self.pinned { "📌 " } else { "" }, name));
        if let Some([r, g, b]) = self.color {
            text.color(egui::Color32::from_rgb(r, g, b))
        } else {
            text
        }
    }
    pub fn menu(&mut self, ui: &mut egui::Ui) -> Command {
        ui.checkbox(&mut self.pinned, "Pin tab");
        ui.label("Rename tab");
        ui.text_edit_singleline(&mut self.title);
        let mut color = self.color.unwrap_or([120, 160, 220]);
        if ui.color_edit_button_srgb(&mut color).changed() {
            self.color = Some(color);
        }
        if ui.button("Reset color").clicked() {
            self.color = None;
        }
        if ui.button("Close Other Tabs").clicked() {
            ui.close();
            return Command::CloseOthers;
        }
        if ui.button("Close").clicked() {
            ui.close();
            return Command::Close;
        }
        Command::None
    }
}

/// Typing from an actual tree/list row starts its real filter. A focused text
/// field and editor retain their events; this never acts on mere panel visibility.
pub(crate) fn type_to_filter(
    ctx: &egui::Context,
    owner: Option<egui::Id>,
    filter: &mut String,
) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    if ctx.memory(|memory| memory.focused()) != Some(owner) || egui::Popup::is_any_open(ctx) {
        return false;
    }
    let text = ctx.input(|input| {
        if input.modifiers.ctrl || input.modifiers.alt || input.modifiers.command {
            return String::new();
        }
        input
            .events
            .iter()
            .filter_map(|event| {
                if let egui::Event::Text(text) = event {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<String>()
    });
    if text.trim().is_empty() {
        return false;
    }
    filter.push_str(&text);
    ctx.input_mut(|input| {
        input
            .events
            .retain(|event| !matches!(event, egui::Event::Text(_)))
    });
    true
}

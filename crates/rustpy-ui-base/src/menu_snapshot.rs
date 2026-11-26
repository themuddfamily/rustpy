//! The menu appearance settings and keymap that `custom_menus` (in `rustpy-ui`)
//! publishes per project viewport, and the readers the foundation widgets need:
//! tooltip shortcuts (`a11y`) and main menu mnemonics (`i18n`).
use crate::actions::{KeyChord, Keymap};
use rustpy_settings::menus::Settings;
#[doc(hidden)]
#[derive(Clone)]
pub struct Snapshot {
    pub settings: Settings,
    pub keymap: Keymap,
    pub owner_available: bool,
}
#[doc(hidden)]
pub fn key(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("custom-menu-settings", ctx.viewport_id()))
}
/// Resolve only the current viewport's actual ancestry. A snapshot marks an
/// actual project owner; inherited preferences are never republished as owners.
pub fn appearance_owner(ctx: &egui::Context) -> Option<egui::ViewportId> {
    let current = ctx.viewport_id();
    let parents = ctx.input(|input| {
        input
            .raw
            .viewports
            .iter()
            .map(|(id, info)| (*id, info.parent))
            .collect::<std::collections::HashMap<_, _>>()
    });
    let mut candidate = current;
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if !visited.insert(candidate) {
            return None;
        }
        let key = egui::Id::new(("custom-menu-settings", candidate));
        if ctx.data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(key).is_some()) {
            return Some(candidate);
        }
        candidate = parents.get(&candidate).copied().flatten()?;
    }
    None
}
/// Shared, so the menus drawn each frame do not copy the settings and keymap.
pub fn snapshot(ctx: &egui::Context) -> Option<std::sync::Arc<Snapshot>> {
    let owner = appearance_owner(ctx)?;
    let key = egui::Id::new(("custom-menu-settings", owner));
    ctx.data(|data| data.get_temp::<std::sync::Arc<Snapshot>>(key))
}
pub fn mnemonic(response: &egui::Response, source: &str) {
    let Some(snapshot) = snapshot(&response.ctx) else {
        return;
    };
    if !snapshot.settings.appearance.menu_mnemonics
        || !snapshot.owner_available
        || !response.enabled()
        || response
            .ctx
            .input(|input| input.viewport().focused == Some(false))
    {
        return;
    }
    let (letter, key) = match source {
        "File" => ("F", egui::Key::F),
        "Edit" => ("E", egui::Key::E),
        "View" => ("V", egui::Key::V),
        "Navigate" => ("N", egui::Key::N),
        "Code" => ("C", egui::Key::C),
        "Refactor" => ("O", egui::Key::O),
        "Run" => ("R", egui::Key::R),
        "Tools" => ("T", egui::Key::T),
        "Git" => ("G", egui::Key::G),
        "Window" => ("W", egui::Key::W),
        "Help" => ("H", egui::Key::H),
        _ => return,
    };
    let chord = KeyChord::new(key, egui::Modifiers::ALT);
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_keyboard_shortcut(format!("Alt+{letter}"))
    });
    if snapshot.keymap.lookup(chord).is_some() {
        return;
    }
    if response
        .ctx
        .input_mut(|input| input.consume_key(egui::Modifiers::ALT, key))
    {
        response.request_focus();
        egui::Popup::open_id(&response.ctx, egui::Popup::default_response_id(response));
        response.ctx.request_repaint();
    }
}
pub fn tooltip_shortcuts(ctx: &egui::Context) -> bool {
    snapshot(ctx).is_none_or(|snapshot| snapshot.settings.appearance.tooltip_shortcuts)
}

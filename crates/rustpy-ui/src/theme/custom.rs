//! UI chrome theme loading. Editor syntax schemes remain separate.
use std::path::Path;

use egui::Color32;
use rustpy_settings::ui_themes::{self, Base, Theme};

use super::UiTheme;

pub fn load(config: &Path, id: &str) -> Result<UiTheme, String> {
    from_definition(id, &ui_themes::load(config, id)?)
}

pub fn from_definition(id: &str, definition: &Theme) -> Result<UiTheme, String> {
    definition.validate()?;
    let mut theme = match definition.base {
        Base::Dark => UiTheme::dark(),
        Base::Light => UiTheme::light(),
        Base::HighContrast => UiTheme::high_contrast(),
    };
    theme.custom_id = Some(id.to_owned());
    for (key, value) in &definition.colors {
        let [r, g, b] = ui_themes::rgb(value)?;
        let color = Color32::from_rgb(r, g, b);
        macro_rules! colors { ($($field:ident),*) => { match key.as_str() { $(stringify!($field) => theme.$field = color,)* _ => unreachable!("validated color key"), } } }
        colors!(
            panel_bg,
            panel_bg_darker,
            header_bg,
            tab_bar_bg,
            tab_active_bg,
            tab_hover_bg,
            tab_underline,
            border,
            text,
            text_dim,
            text_disabled,
            accent,
            selection_bg,
            selection_inactive_bg,
            hover_bg,
            error,
            warning,
            success,
            modified
        );
    }
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_chrome_inherits_base_preserves_identity_and_applies_overrides() {
        let def = Theme::parse(br##"{"name":"Night","base":"Light","colors":{"panel_bg":"#123456","accent":"#654321","text":"#aabbcc"}}"##).unwrap();
        let theme = from_definition("custom:night.json", &def).unwrap();
        assert!(!theme.dark);
        assert_eq!(theme.name(), "custom:night.json");
        assert_eq!(theme.header_bg, UiTheme::light().header_bg);
        assert_eq!(theme.panel_bg, Color32::from_rgb(18, 52, 86));
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        assert_eq!(
            ctx.style_of(if theme.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .visuals
            .panel_fill,
            theme.panel_bg
        );
        assert_eq!(
            ctx.style_of(if theme.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .visuals
            .override_text_color,
            Some(theme.text)
        );
        assert_eq!(
            ctx.style_of(if theme.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .visuals
            .hyperlink_color,
            theme.accent
        );
    }

    #[test]
    fn high_contrast_custom_colors_are_not_overwritten_by_preset_colors() {
        let def = Theme::parse(br##"{"name":"Custom contrast","base":"High Contrast","colors":{"text":"#123456","panel_bg_darker":"#abcdef","border":"#fedcba"}}"##).unwrap();
        let theme = from_definition("custom:contrast.json", &def).unwrap();
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let v = &ctx
            .style_of(if theme.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .visuals;
        assert!(theme.high_contrast);
        assert_eq!(v.window_stroke.color, theme.border);
        assert_eq!(v.extreme_bg_color, theme.panel_bg_darker);
        assert_eq!(v.widgets.active.fg_stroke.color, theme.text);
        assert_eq!(v.widgets.active.bg_stroke.color, theme.border);
        assert_eq!(UiTheme::for_name("Dark").name(), "Dark");
        assert_eq!(UiTheme::for_name("custom:../missing.json").name(), "Dark");
    }
}

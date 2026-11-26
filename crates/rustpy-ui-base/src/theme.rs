//! UI theme (*Dark* / *Light* look) and font setup.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

pub mod custom;

use egui::{Color32, FontData, FontDefinitions, FontFamily, Stroke, Visuals};
use rustpy_highlight::Rgb;

pub const MONO_REGULAR: &str = "Hack";
pub const MONO_BOLD: &str = "Hack-Bold";
pub const MONO_ITALIC: &str = "Hack-Italic";
pub const MONO_BOLD_ITALIC: &str = "Hack-BoldItalic";

/// Hack Regular. egui already registers it as its default monospace font
/// (font data `"Hack"`), so its bytes are reused rather than embedded twice.
pub fn hack_regular() -> &'static [u8] {
    static BYTES: OnceLock<&'static [u8]> = OnceLock::new();
    BYTES.get_or_init(|| {
        let font = FontDefinitions::default()
            .font_data
            .get(MONO_REGULAR)
            .map(|data| data.font.clone());
        match font {
            Some(Cow::Borrowed(bytes)) => bytes,
            Some(Cow::Owned(bytes)) => Box::leak(bytes.into_boxed_slice()),
            None => &[],
        }
    })
}

/// The bundled Hack faces: `(font and family name, bold, italic, bytes)`.
pub fn bundled_mono_faces() -> [(&'static str, bool, bool, &'static [u8]); 4] {
    [
        (MONO_REGULAR, false, false, hack_regular()),
        (
            MONO_BOLD,
            true,
            false,
            include_bytes!("../assets/fonts/Hack-Bold.ttf"),
        ),
        (
            MONO_ITALIC,
            false,
            true,
            include_bytes!("../assets/fonts/Hack-Italic.ttf"),
        ),
        (
            MONO_BOLD_ITALIC,
            true,
            true,
            include_bytes!("../assets/fonts/Hack-BoldItalic.ttf"),
        ),
    ]
}

/// egui's default fonts plus the bundled Hack faces, which the monospace
/// family and the editor families (see [`mono_family`]) use.
pub fn bundled_font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, _, _, bytes) in bundled_mono_faces() {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        fonts.families.insert(
            FontFamily::Name(name.into()),
            vec![
                name.to_owned(),
                "NotoEmoji-Regular".to_owned(),
                "emoji-icon-font".to_owned(),
            ],
        );
    }
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.retain(|name| name != MONO_REGULAR);
        mono.insert(0, MONO_REGULAR.to_owned());
    }
    crate::fonts::install_reader_faces(&mut fonts);
    crate::fonts::install_console_faces(&mut fonts);
    crate::fonts::install_emoji_fallback(&mut fonts);
    fonts
}

/// Registers the bundled fonts only (`crate::fonts` adds the installed ones).
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(bundled_font_definitions());
}

/// The IDE's bundled fonts; `crate::i18n` appends CJK fallback fonts to every family.
pub fn font_definitions() -> FontDefinitions {
    bundled_font_definitions()
}

pub fn mono_family(bold: bool, italic: bool) -> FontFamily {
    FontFamily::Name(
        match (bold, italic) {
            (false, false) => MONO_REGULAR,
            (true, false) => MONO_BOLD,
            (false, true) => MONO_ITALIC,
            (true, true) => MONO_BOLD_ITALIC,
        }
        .into(),
    )
}

pub fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Colors for custom-painted chrome (tabs, gutters, tool-window headers).
#[derive(Clone, Debug)]
pub struct UiTheme {
    /// Persistent selection identity; independent of the display name.
    custom_id: Option<String>,
    scheme_colors: std::collections::BTreeMap<String, Color32>,
    pub dark: bool,
    /// The High Contrast theme: white on black, strong borders and focus rings.
    pub high_contrast: bool,
    pub panel_bg: Color32,
    pub panel_bg_darker: Color32,
    pub header_bg: Color32,
    pub tab_bar_bg: Color32,
    pub tab_active_bg: Color32,
    pub tab_hover_bg: Color32,
    pub tab_underline: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_disabled: Color32,
    pub accent: Color32,
    pub selection_bg: Color32,
    pub selection_inactive_bg: Color32,
    pub hover_bg: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub modified: Color32,
}

impl UiTheme {
    pub fn bind_editor_scheme(&mut self, scheme: &rustpy_highlight::ColorScheme) {
        self.scheme_colors = scheme
            .context_colors
            .iter()
            .take(128)
            .map(|(key, color)| (key.clone(), Color32::from_rgb(color.0, color.1, color.2)))
            .collect();
    }
    pub fn scheme_color(&self, key: &str, fallback: Color32) -> Color32 {
        self.scheme_colors.get(key).copied().unwrap_or(fallback)
    }
    pub fn scheme_override(&self, key: &str) -> Option<Color32> {
        self.scheme_colors.get(key).copied()
    }

    pub fn dark() -> Self {
        Self {
            custom_id: None,
            scheme_colors: Default::default(),
            dark: true,
            high_contrast: false,
            panel_bg: Color32::from_rgb(0x3C, 0x3F, 0x41),
            panel_bg_darker: Color32::from_rgb(0x2B, 0x2B, 0x2B),
            header_bg: Color32::from_rgb(0x3C, 0x3F, 0x41),
            tab_bar_bg: Color32::from_rgb(0x3C, 0x3F, 0x41),
            tab_active_bg: Color32::from_rgb(0x4E, 0x52, 0x54),
            tab_hover_bg: Color32::from_rgb(0x45, 0x48, 0x4A),
            tab_underline: Color32::from_rgb(0x4A, 0x88, 0xC7),
            border: Color32::from_rgb(0x32, 0x32, 0x32),
            text: Color32::from_rgb(0xBB, 0xBB, 0xBB),
            text_dim: Color32::from_rgb(0x8C, 0x8C, 0x8C),
            text_disabled: Color32::from_rgb(0x6E, 0x6E, 0x6E),
            accent: Color32::from_rgb(0x4A, 0x88, 0xC7),
            selection_bg: Color32::from_rgb(0x2F, 0x65, 0xCA),
            selection_inactive_bg: Color32::from_rgb(0x0D, 0x29, 0x3E),
            hover_bg: Color32::from_rgb(0x46, 0x4A, 0x4D),
            error: Color32::from_rgb(0xFF, 0x64, 0x64),
            warning: Color32::from_rgb(0xF0, 0xA7, 0x32),
            success: Color32::from_rgb(0x49, 0x9C, 0x54),
            modified: Color32::from_rgb(0x6C, 0x9E, 0xE2),
        }
    }

    pub fn light() -> Self {
        Self {
            custom_id: None,
            scheme_colors: Default::default(),
            dark: false,
            high_contrast: false,
            panel_bg: Color32::from_rgb(0xF2, 0xF2, 0xF2),
            panel_bg_darker: Color32::from_rgb(0xE8, 0xE8, 0xE8),
            header_bg: Color32::from_rgb(0xF2, 0xF2, 0xF2),
            tab_bar_bg: Color32::from_rgb(0xF2, 0xF2, 0xF2),
            tab_active_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            tab_hover_bg: Color32::from_rgb(0xE6, 0xE6, 0xE6),
            tab_underline: Color32::from_rgb(0x40, 0x83, 0xC9),
            border: Color32::from_rgb(0xD1, 0xD1, 0xD1),
            text: Color32::from_rgb(0x00, 0x00, 0x00),
            text_dim: Color32::from_rgb(0x6E, 0x6E, 0x6E),
            text_disabled: Color32::from_rgb(0x99, 0x99, 0x99),
            accent: Color32::from_rgb(0x40, 0x83, 0xC9),
            selection_bg: Color32::from_rgb(0x27, 0x75, 0xD7),
            selection_inactive_bg: Color32::from_rgb(0xD5, 0xD5, 0xD5),
            hover_bg: Color32::from_rgb(0xE0, 0xE6, 0xEE),
            error: Color32::from_rgb(0xDB, 0x3B, 0x3B),
            warning: Color32::from_rgb(0xC9, 0x8B, 0x12),
            success: Color32::from_rgb(0x36, 0x8C, 0x40),
            modified: Color32::from_rgb(0x22, 0x5F, 0xB0),
        }
    }

    /// High Contrast (paired with the High Contrast editor scheme): white text
    /// on black, a bright cyan accent, white borders, thick focus rings. Text
    /// colors keep a contrast ratio of at least 7:1 against the background.
    pub fn high_contrast() -> Self {
        Self {
            custom_id: None,
            scheme_colors: Default::default(),
            dark: true,
            high_contrast: true,
            panel_bg: Color32::BLACK,
            panel_bg_darker: Color32::BLACK,
            header_bg: Color32::BLACK,
            tab_bar_bg: Color32::BLACK,
            tab_active_bg: Color32::from_rgb(0x1C, 0x3F, 0x9F),
            tab_hover_bg: Color32::from_rgb(0x26, 0x26, 0x26),
            tab_underline: Color32::from_rgb(0x1A, 0xEB, 0xFF),
            border: Color32::WHITE,
            text: Color32::WHITE,
            text_dim: Color32::from_rgb(0xD0, 0xD0, 0xD0),
            text_disabled: Color32::from_rgb(0xA6, 0xA6, 0xA6),
            accent: Color32::from_rgb(0x1A, 0xEB, 0xFF),
            selection_bg: Color32::from_rgb(0x1C, 0x3F, 0x9F),
            selection_inactive_bg: Color32::from_rgb(0x4A, 0x4A, 0x4A),
            hover_bg: Color32::from_rgb(0x26, 0x26, 0x26),
            error: Color32::from_rgb(0xFF, 0x70, 0x70),
            warning: Color32::from_rgb(0xFF, 0xD7, 0x00),
            success: Color32::from_rgb(0x5B, 0xE3, 0x5B),
            modified: Color32::from_rgb(0x6F, 0xB8, 0xFF),
        }
    }

    /// `"Light"`, `"High Contrast"`, otherwise Dark.
    pub fn for_name(name: &str) -> Self {
        if name.starts_with(rustpy_settings::ui_themes::PREFIX) {
            return custom::load(&rustpy_settings::Settings::config_dir(), name).unwrap_or_else(
                |e| {
                    log::warn!("Cannot load UI theme {name}: {e}; using Dark");
                    Self::dark()
                },
            );
        }
        match name {
            "Light" => Self::light(),
            "High Contrast" => Self::high_contrast(),
            _ => Self::dark(),
        }
    }

    /// The name `for_name` reads back (`appearance.theme`).
    pub fn name(&self) -> &str {
        if let Some(id) = &self.custom_id {
            return id;
        }
        if self.high_contrast {
            "High Contrast"
        } else if self.dark {
            "Dark"
        } else {
            "Light"
        }
    }

    /// Applies matching egui visuals.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut v = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        v.panel_fill = self.panel_bg;
        v.window_fill = self.panel_bg;
        v.extreme_bg_color = self.panel_bg_darker;
        v.faint_bg_color = self.hover_bg;
        v.override_text_color = Some(self.text);
        v.selection.bg_fill = self.selection_bg;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.noninteractive.bg_fill = self.panel_bg;
        v.widgets.inactive.bg_fill = if self.dark {
            Color32::from_rgb(0x4C, 0x50, 0x52)
        } else {
            Color32::from_rgb(0xE6, 0xE6, 0xE6)
        };
        v.widgets.hovered.bg_fill = self.hover_bg;
        v.widgets.active.bg_fill = self.selection_bg;
        v.window_stroke = Stroke::new(1.0, self.border);
        v.window_corner_radius = 4.0.into();
        v.menu_corner_radius = 4.0.into();
        v.widgets.inactive.corner_radius = 3.0.into();
        v.widgets.hovered.corner_radius = 3.0.into();
        v.widgets.active.corner_radius = 3.0.into();
        v.popup_shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 12,
            spread: 0,
            color: Color32::from_black_alpha(96),
        };
        if self.high_contrast {
            // Every control has a visible outline; hover, focus (egui draws a focused
            // widget in its active style) and open menus get a thick accent border.
            let white = Color32::WHITE;
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, white);
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.fg_stroke = Stroke::new(1.5, white);
            }
            // Buttons stay black (`weak_bg_fill`); slider rails and check boxes (`bg_fill`) must show on black.
            v.widgets.inactive.bg_fill = Color32::from_rgb(0x40, 0x40, 0x40);
            v.widgets.inactive.weak_bg_fill = Color32::BLACK;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xC0, 0xC0, 0xC0));
            v.widgets.hovered.weak_bg_fill = self.hover_bg;
            v.widgets.hovered.bg_stroke = Stroke::new(2.0, self.accent);
            v.widgets.active.weak_bg_fill = self.selection_bg;
            v.widgets.active.bg_stroke = Stroke::new(2.0, self.accent);
            v.widgets.open.weak_bg_fill = self.selection_bg;
            v.widgets.open.bg_stroke = Stroke::new(2.0, self.accent);
            v.selection.stroke = Stroke::new(2.0, white);
            v.window_stroke = Stroke::new(2.0, white);
            v.text_cursor.stroke = Stroke::new(2.0, self.accent);
            v.hyperlink_color = self.accent;
            v.extreme_bg_color = Color32::BLACK;
            v.faint_bg_color = Color32::from_rgb(0x1A, 0x1A, 0x1A);
            v.popup_shadow = egui::epaint::Shadow::NONE;
        }
        if self.custom_id.is_some() {
            // Preset accessibility geometry stays; custom colors are the user's
            // choice, including High Contrast overrides.
            v.window_stroke.color = self.border;
            v.extreme_bg_color = self.panel_bg_darker;
            v.faint_bg_color = self.hover_bg;
            v.selection.stroke.color = self.text;
            v.hyperlink_color = self.accent;
            v.text_cursor.stroke.color = self.accent;
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.fg_stroke.color = self.text;
                w.bg_stroke.color = self.border;
            }
            v.widgets.inactive.bg_fill = self.panel_bg_darker;
            v.widgets.inactive.weak_bg_fill = self.panel_bg;
            v.widgets.hovered.bg_fill = self.hover_bg;
            v.widgets.hovered.weak_bg_fill = self.hover_bg;
            v.widgets.active.bg_fill = self.selection_bg;
            v.widgets.active.weak_bg_fill = self.selection_bg;
            v.widgets.open.bg_fill = self.selection_bg;
            v.widgets.open.weak_bg_fill = self.selection_bg;
        }
        // Appearance is an explicit selection, independent of OS light/dark changes.
        ctx.set_theme(if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        ctx.set_visuals(v);
        ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(8.0, 3.0);
            style.spacing.menu_margin = egui::Margin::same(6);
        });
    }
}

/// Selects all text of the single-line `TextEdit` behind `resp` (so typing
/// replaces a pre-filled query, as search fields do).
pub fn select_all_text(ui: &egui::Ui, resp: &egui::Response, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
        let n = text.chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(n),
            )));
        state.store(ui.ctx(), resp.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an sRGB color.
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn high_contrast_text_meets_wcag_aaa() {
        let t = UiTheme::high_contrast();
        assert!(t.high_contrast && t.dark);
        for (name, fg) in [
            ("text", t.text),
            ("text_dim", t.text_dim),
            ("text_disabled", t.text_disabled),
            ("accent", t.accent),
            ("error", t.error),
            ("warning", t.warning),
            ("success", t.success),
            ("modified", t.modified),
        ] {
            let c = contrast(fg, t.panel_bg);
            assert!(c >= 7.0, "{name}: {c:.1}:1 on the panel");
        }
        for bg in [
            t.selection_bg,
            t.selection_inactive_bg,
            t.tab_active_bg,
            t.hover_bg,
        ] {
            assert!(contrast(t.text, bg) >= 7.0, "white text on {bg:?}");
        }
        assert!(contrast(t.border, t.panel_bg) >= 7.0);
    }

    #[test]
    fn theme_names_round_trip() {
        for name in ["Dark", "Light", "High Contrast"] {
            assert_eq!(UiTheme::for_name(name).name(), name);
        }
        assert_eq!(UiTheme::for_name("No Such Theme").name(), "Dark");
        assert!(!UiTheme::dark().high_contrast && !UiTheme::light().high_contrast);
    }
}

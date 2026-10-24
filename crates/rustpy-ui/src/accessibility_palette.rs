//! Semantic palettes supplement labels, glyphs and VCS marker shapes.
use crate::theme::UiTheme;
use egui::Color32;
use rustpy_highlight::{ColorScheme, Rgb};
use rustpy_settings::accessibility::ColorBlindness;
#[derive(Default)]
pub struct PaletteState {
    theme: Option<([Color32; 5], [Color32; 5])>,
    scheme: Option<([Rgb; 9], [Rgb; 9])>,
}
fn theme_colors(t: &UiTheme) -> [Color32; 5] {
    [t.error, t.warning, t.success, t.modified, t.accent]
}
fn scheme_colors(s: &ColorScheme) -> [Rgb; 9] {
    let e = &s.editor;
    [
        e.error_stripe_error,
        e.error_stripe_warning,
        e.error_underline,
        e.warning_underline,
        e.weak_warning_underline,
        e.typo_underline,
        e.added_line,
        e.modified_line,
        e.deleted_line,
    ]
}
pub fn colors(mode: ColorBlindness, dark: bool) -> [Color32; 5] {
    let values = match (mode, dark) {
        (ColorBlindness::Protanopia, true) => [0xFFAA55, 0xFFFF88, 0x55DDEE, 0xBB99FF, 0x66BBFF],
        (ColorBlindness::Deuteranopia, true) => [0xFFAA55, 0xFFEE77, 0x55CCFF, 0xCC99FF, 0x88CCFF],
        (ColorBlindness::Tritanopia, true) => [0xFF8899, 0xFFCC88, 0x66DDCC, 0xCCAAFF, 0xAAEEFF],
        (ColorBlindness::Protanopia, false) => [0xA34500, 0x756000, 0x006B88, 0x684A99, 0x005A9C],
        (ColorBlindness::Deuteranopia, false) => [0xA34300, 0x776000, 0x006A94, 0x6E438C, 0x005D94],
        (ColorBlindness::Tritanopia, false) => [0xA72C50, 0x815400, 0x006C60, 0x6D438F, 0x006777],
        _ => [0; 5],
    };
    values.map(|c| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8))
}
impl PaletteState {
    pub fn apply(&mut self, theme: &mut UiTheme, scheme: &mut ColorScheme, mode: ColorBlindness) {
        let current = theme_colors(theme);
        let base = self
            .theme
            .take()
            .filter(|(_, last)| *last == current)
            .map(|(base, _)| base)
            .unwrap_or(current);
        let applied = if mode == ColorBlindness::None {
            base
        } else {
            colors(mode, theme.dark)
        };
        [
            theme.error,
            theme.warning,
            theme.success,
            theme.modified,
            theme.accent,
        ] = applied;
        self.theme = Some((base, applied));
        let current = scheme_colors(scheme);
        let base = self
            .scheme
            .take()
            .filter(|(_, last)| *last == current)
            .map(|(base, _)| base)
            .unwrap_or(current);
        let applied = if mode == ColorBlindness::None {
            base
        } else {
            let c = colors(mode, scheme.dark).map(|c| Rgb(c.r(), c.g(), c.b()));
            [c[0], c[1], c[0], c[1], c[1], c[3], c[2], c[3], c[0]]
        };
        let e = &mut scheme.editor;
        [
            e.error_stripe_error,
            e.error_stripe_warning,
            e.error_underline,
            e.warning_underline,
            e.weak_warning_underline,
            e.typo_underline,
            e.added_line,
            e.modified_line,
            e.deleted_line,
        ] = applied;
        self.scheme = Some((base, applied));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_switching_never_accumulates_or_changes_source_theme_identity() {
        let mut t = UiTheme::dark();
        let mut s = ColorScheme::dark();
        let original = theme_colors(&t);
        let editor = scheme_colors(&s);
        let mut state = PaletteState::default();
        for mode in [
            ColorBlindness::Protanopia,
            ColorBlindness::Deuteranopia,
            ColorBlindness::Tritanopia,
            ColorBlindness::Protanopia,
        ] {
            state.apply(&mut t, &mut s, mode);
            let once = theme_colors(&t);
            state.apply(&mut t, &mut s, mode);
            assert_eq!(theme_colors(&t), once);
            assert_ne!(once[0], once[2]);
            assert_eq!(t.name(), "Dark");
        }
        state.apply(&mut t, &mut s, ColorBlindness::None);
        assert_eq!(theme_colors(&t), original);
        assert_eq!(scheme_colors(&s), editor);
    }
}

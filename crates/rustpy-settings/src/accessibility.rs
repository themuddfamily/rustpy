//! Platform accessibility preferences; AccessKit remains the native bridge.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenReader {
    #[default]
    Automatic,
    Always,
    Disabled,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorBlindness {
    #[default]
    None,
    Protanopia,
    Deuteranopia,
    Tritanopia,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessibilitySettings {
    pub screen_reader: ScreenReader,
    pub announce_notifications: bool,
    pub announce_notification_body: bool,
    pub announce_status: bool,
    pub focus_indicators: bool,
    pub focus_ring_width: f32,
    pub reduced_motion: bool,
    pub contrast_scrollbars: bool,
    pub color_blindness: ColorBlindness,
}
impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            screen_reader: ScreenReader::Automatic,
            announce_notifications: true,
            announce_notification_body: false,
            announce_status: false,
            focus_indicators: true,
            focus_ring_width: 2.0,
            reduced_motion: false,
            contrast_scrollbars: false,
            color_blindness: ColorBlindness::None,
        }
    }
}
impl AccessibilitySettings {
    pub fn ring_width(&self) -> f32 {
        if self.focus_ring_width.is_finite() {
            self.focus_ring_width.clamp(1.0, 6.0)
        } else {
            2.0
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_preferences_and_invalid_numeric_values_keep_an_operable_ui() {
        let settings: AccessibilitySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, AccessibilitySettings::default());
        assert_eq!(
            AccessibilitySettings {
                focus_ring_width: f32::NAN,
                ..Default::default()
            }
            .ring_width(),
            2.0
        );
    }
}

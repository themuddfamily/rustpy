//! Persisted accessibility policy and captured application announcements.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::accessibility_announcements::{AccessibilityPlugin, Announcement};
use rustpy_settings::accessibility::{AccessibilitySettings, ColorBlindness, ScreenReader};
#[derive(Default)]
pub(super) struct AccessibilityWorkspace {
    palette: crate::accessibility_palette::PaletteState,
    pending: std::collections::VecDeque<(u64, Announcement)>,
    policy: Option<AccessibilitySettings>,
    scope: Option<u64>,
}
impl IdeApp {
    pub(super) fn accessibility_frame(&mut self, ctx: &egui::Context) {
        let policy = self.settings.accessibility.clone();
        self.accessibility_workspace.palette.apply(
            &mut self.theme,
            &mut self.scheme,
            policy.color_blindness,
        );
        self.theme.apply(ctx);
        if ctx.with_plugin::<AccessibilityPlugin, _>(|_| ()).is_none() {
            ctx.add_plugin(AccessibilityPlugin::default());
        }
        crate::accessibility_policy::configure(ctx, &policy);
        let changed = self.accessibility_workspace.policy.as_ref() != Some(&policy)
            || self.accessibility_workspace.scope != Some(self.file_session);
        let pending = std::mem::take(&mut self.accessibility_workspace.pending);
        let scope = self.file_session;
        ctx.with_plugin::<AccessibilityPlugin, _>(|plugin| {
            if changed || policy.screen_reader == ScreenReader::Disabled {
                plugin.announcements.clear();
            }
            plugin.settings = policy.clone();
            if policy.screen_reader != ScreenReader::Disabled {
                for (stamp, announcement) in pending {
                    if stamp == scope {
                        plugin.announcements.push(announcement);
                    }
                }
            }
        });
        self.accessibility_workspace.policy = Some(policy);
        self.accessibility_workspace.scope = Some(scope);
    }
    fn queue_accessibility(&mut self, announcement: Announcement) {
        if self.accessibility_workspace.pending.len() >= 64 {
            self.accessibility_workspace.pending.pop_front();
        }
        self.accessibility_workspace
            .pending
            .push_back((self.file_session, announcement));
    }
    pub(super) fn accessibility_notification(
        &mut self,
        id: u64,
        kind: crate::notifications::NotificationKind,
        title: &str,
        body: &str,
    ) {
        if !self.settings.accessibility.announce_notifications {
            return;
        }
        if self
            .accessibility_workspace
            .pending
            .back()
            .is_some_and(|(_, a)| a.key == "status")
        {
            self.accessibility_workspace.pending.pop_back();
        }
        let title = crate::i18n::tr(title);
        let body = if self.settings.accessibility.announce_notification_body {
            crate::i18n::tr(body).into_owned()
        } else {
            String::new()
        };
        let label = match kind {
            crate::notifications::NotificationKind::Error => "Error",
            crate::notifications::NotificationKind::Warning => "Warning",
            crate::notifications::NotificationKind::Info => "Information",
        };
        let text = format!(
            "{}: {}{}{}",
            crate::i18n::tr(label),
            title,
            if body.is_empty() { "" } else { ". " },
            body
        );
        self.queue_accessibility(Announcement::new(
            format!("notification:{id}"),
            &text,
            kind == crate::notifications::NotificationKind::Error,
        ));
    }
    pub(super) fn accessibility_status(&mut self, text: &str) {
        if self.settings.accessibility.announce_status {
            self.queue_accessibility(Announcement::new("status".into(), text, false));
        }
    }
    pub(super) fn page_accessibility(&mut self, ui: &mut egui::Ui) {
        use crate::i18n::tr_static as tr;
        ui.heading(tr("Accessibility"));
        ui.label(tr("Native screen readers use AccessKit: AT-SPI on Linux, UI Automation on Windows, and NSAccessibility on macOS."));
        let p = &mut self.settings.accessibility;
        egui::ComboBox::from_id_salt("screen-reader-policy")
            .selected_text(tr(match p.screen_reader {
                ScreenReader::Automatic => "Detect screen reader",
                ScreenReader::Always => "Always provide accessibility tree",
                ScreenReader::Disabled => "Disable accessibility tree",
            }))
            .show_ui(ui, |ui| {
                for (value, label) in [
                    (ScreenReader::Automatic, "Detect screen reader"),
                    (ScreenReader::Always, "Always provide accessibility tree"),
                    (ScreenReader::Disabled, "Disable accessibility tree"),
                ] {
                    ui.selectable_value(&mut p.screen_reader, value, tr(label));
                }
            })
            .combo_named(crate::i18n::tr_static("Screen reader support"));
        ui.checkbox(&mut p.announce_notifications, tr("Announce notifications"));
        ui.add_enabled_ui(p.announce_notifications, |ui| {
            ui.checkbox(
                &mut p.announce_notification_body,
                tr("Include notification details (may contain private text)"),
            );
        });
        ui.checkbox(&mut p.announce_status, tr("Announce status messages"));
        ui.checkbox(
            &mut p.focus_indicators,
            tr("Show keyboard focus indicators"),
        );
        ui.add(
            egui::Slider::new(&mut p.focus_ring_width, 1.0..=6.0).text(tr("Focus indicator width")),
        );
        ui.checkbox(
            &mut p.reduced_motion,
            tr("Reduce motion and disable caret blinking"),
        );
        ui.checkbox(&mut p.contrast_scrollbars, tr("Use contrast scrollbars"));
        egui::ComboBox::from_id_salt("color-blindness-palette")
            .selected_text(tr(match p.color_blindness {
                ColorBlindness::None => "Default colors",
                ColorBlindness::Protanopia => "Protanopia",
                ColorBlindness::Deuteranopia => "Deuteranopia",
                ColorBlindness::Tritanopia => "Tritanopia",
            }))
            .show_ui(ui, |ui| {
                for (value, label) in [
                    (ColorBlindness::None, "Default colors"),
                    (ColorBlindness::Protanopia, "Protanopia"),
                    (ColorBlindness::Deuteranopia, "Deuteranopia"),
                    (ColorBlindness::Tritanopia, "Tritanopia"),
                ] {
                    ui.selectable_value(&mut p.color_blindness, value, tr(label));
                }
            })
            .combo_named(crate::i18n::tr_static("Color vision deficiency"));
        ui.small(tr(
            "Error, warning, success and VCS palettes supplement text labels and marker shapes.",
        ));
        // Root's Appearance scale is the same canonical zoom used by chrome_frame_start.
        ui.add(egui::Slider::new(&mut self.settings.chrome.scale, 0.5..=3.0).text(tr("IDE zoom")));
        if ui.button(tr("Use High Contrast theme")).clicked() {
            self.set_ui_theme("High Contrast", ui.ctx());
        }
        if ui.button(tr("Test notification announcement")).clicked() {
            self.notify(
                crate::notifications::NotificationKind::Info,
                "Accessibility",
                tr("Accessibility notification test"),
                "",
                Vec::new(),
            );
        }
        ui.separator();
        ui.label(tr("Use Tab and Shift+Tab to move focus, Enter or Space to activate controls, and Escape to close popups. Alt+1 focuses Project; F12 returns to the editor."));
        if ui.button(tr("Open Accessibility Guide")).clicked() {
            self.help_action(Action::AccessibilityGuide, ui.ctx());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_retirement_discards_queued_announcements() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.settings.accessibility.screen_reader = ScreenReader::Always;
        app.settings.accessibility.announce_status = true;
        app.accessibility_status("Old project private status");
        app.file_session = app.file_session.wrapping_add(1);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        app.accessibility_frame(&ctx);
        let output = ctx.run_ui(egui::RawInput::default(), |_| {});
        assert!(output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .all(|(_, node)| node.label() != Some("Old project private status")));
        output.drop_without_applying_deltas();
    }
    #[test]
    fn notification_body_is_opt_in_and_status_is_not_duplicated() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.settings.accessibility.announce_status = true;
        app.accessibility_status("Fixture title");
        app.accessibility_notification(
            1,
            crate::notifications::NotificationKind::Warning,
            "Fixture title",
            "private detail",
        );
        assert_eq!(app.accessibility_workspace.pending.len(), 1);
        let message = &app.accessibility_workspace.pending[0].1;
        assert!(message.text.contains("Fixture title"));
        assert!(!message.text.contains("private detail"));
        app.settings.accessibility.announce_notification_body = true;
        app.accessibility_notification(
            2,
            crate::notifications::NotificationKind::Warning,
            "Fixture title",
            "private detail",
        );
        assert!(app.accessibility_workspace.pending[1]
            .1
            .text
            .contains("private detail"));
    }
}

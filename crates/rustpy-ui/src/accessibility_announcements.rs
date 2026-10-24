//! One retained native live region, bounded and independent of keyboard focus.
use rustpy_settings::accessibility::{AccessibilitySettings, ScreenReader};
use std::collections::VecDeque;
#[derive(Clone, Debug)]
pub struct Announcement {
    pub key: String,
    pub text: String,
    pub urgent: bool,
}
impl Announcement {
    pub fn new(key: String, text: &str, urgent: bool) -> Self {
        Self {
            key,
            text: text.chars().take(1024).collect(),
            urgent,
        }
    }
}
#[derive(Default)]
pub struct Announcements {
    pending: VecDeque<Announcement>,
    live: Option<(Announcement, f64)>,
    last: Option<(String, String)>,
}
impl Announcements {
    pub fn push(&mut self, announcement: Announcement) {
        if announcement.text.trim().is_empty()
            || self.last.as_ref() == Some(&(announcement.key.clone(), announcement.text.clone()))
        {
            return;
        }
        self.pending.retain(|a| a.key != announcement.key);
        if self.pending.len() >= 64 {
            self.pending.pop_front();
        }
        self.pending.push_back(announcement);
    }
    fn next(&mut self, now: f64) -> Option<&Announcement> {
        if self
            .live
            .as_ref()
            .is_none_or(|(_, since)| now - *since >= 2.0)
            || (self.pending.iter().any(|a| a.urgent)
                && self.live.as_ref().is_some_and(|(a, _)| !a.urgent))
        {
            if let Some(index) = self.pending.iter().position(|a| a.urgent) {
                let next = self.pending.remove(index).unwrap();
                self.last = Some((next.key.clone(), next.text.clone()));
                self.live = Some((next, now));
            } else if let Some(next) = self.pending.pop_front() {
                self.last = Some((next.key.clone(), next.text.clone()));
                self.live = Some((next, now));
            }
        }
        self.live.as_ref().map(|(a, _)| a)
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.live = None;
        self.last = None;
    }
}
#[derive(Default)]
pub struct AccessibilityPlugin {
    pub settings: AccessibilitySettings,
    pub announcements: Announcements,
}
impl egui::plugin::Plugin for AccessibilityPlugin {
    fn debug_name(&self) -> &'static str {
        "RustPy Accessibility"
    }
    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        crate::accessibility_policy::configure(ui.ctx(), &self.settings);
    }
    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        crate::accessibility_policy::focus_indicator(ui.ctx());
        if self.settings.screen_reader == ScreenReader::Disabled
            || ui.input(|input| input.viewport().focused == Some(false))
        {
            return;
        }
        let now = ui.input(|input| input.time);
        let Some(announcement) = self.announcements.next(now) else {
            return;
        };
        let id = egui::Id::new("rustpy-accessibility-live-region").with(ui.ctx().viewport_id());
        let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::Vec2::ZERO);
        let response = ui.interact(rect, id.with("live-region"), egui::Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "IDE announcements")
        });
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(egui::accesskit::Role::Status);
            node.set_label(announcement.text.as_str());
            node.set_live(if announcement.urgent {
                egui::accesskit::Live::Assertive
            } else {
                egui::accesskit::Live::Polite
            });
            node.set_live_atomic();
        });
        if !self.announcements.pending.is_empty() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(2));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_is_bounded_coalesced_and_orders_errors_before_polite_status() {
        let mut a = Announcements::default();
        for i in 0..100 {
            a.push(Announcement::new(format!("status{i}"), "working", false));
        }
        assert_eq!(a.pending.len(), 64);
        a.push(Announcement::new("error".into(), "Failed", true));
        assert_eq!(a.next(0.0).unwrap().text, "Failed");
        a.push(Announcement::new("error".into(), "Failed", true));
        assert!(!a.pending.iter().any(|n| n.key == "error"));
        assert_eq!(a.next(1.0).unwrap().text, "Failed");
        assert_eq!(a.next(2.1).unwrap().text, "working");
        a.clear();
        assert!(a.next(3.0).is_none());
    }
    #[test]
    fn native_tree_contains_retained_live_status_without_moving_focus() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut plugin = AccessibilityPlugin::default();
        plugin.settings.screen_reader = ScreenReader::Always;
        plugin.announcements.push(Announcement::new(
            "notification:1".into(),
            "Error: connection failed",
            true,
        ));
        ctx.add_plugin(plugin);
        let owner = egui::Id::new("existing-keyboard-owner");
        let output = ctx.run_ui(
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            },
            |ui| {
                let response = ui.interact(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(30.0, 20.0)),
                    owner,
                    egui::Sense::click(),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Owner")
                });
                response.request_focus();
            },
        );
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        let region = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some("Error: connection failed"))
            .unwrap();
        assert_eq!(region.role(), egui::accesskit::Role::Status);
        assert_eq!(region.live(), Some(egui::accesskit::Live::Assertive));
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(owner));
        output.drop_without_applying_deltas();
    }
}

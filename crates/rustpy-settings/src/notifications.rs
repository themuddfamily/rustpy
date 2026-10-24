//! Settings › Appearance & Behavior › Notifications: the global balloon and
//! system-notification switches and, per notification group, how it pops up
//! (balloon, sticky balloon, tool window balloon, no popup) and whether it is
//! logged to the Notifications tool window.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How a notification group pops up (its "Popup type").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDisplay {
    /// A balloon at the bottom right that expires (errors stay until closed).
    #[default]
    Balloon,
    /// A balloon that stays until it is closed.
    StickyBalloon,
    /// A balloon next to the stripe button of the related tool window
    /// (a regular balloon when there is none).
    ToolWindowBalloon,
    /// No popup: the notification only goes to the log.
    NoPopup,
}

impl NotificationDisplay {
    pub const ALL: [NotificationDisplay; 4] = [
        NotificationDisplay::Balloon,
        NotificationDisplay::StickyBalloon,
        NotificationDisplay::ToolWindowBalloon,
        NotificationDisplay::NoPopup,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NotificationDisplay::Balloon => "Balloon",
            NotificationDisplay::StickyBalloon => "Sticky balloon",
            NotificationDisplay::ToolWindowBalloon => "Tool window balloon",
            NotificationDisplay::NoPopup => "No popup",
        }
    }
}

/// Groups tied to a tool window: they pop up next to its stripe
/// button by default ("Tool window balloon").
pub const TOOL_WINDOW_GROUPS: &[&str] = &["Git", "GitHub", "Python Interpreter", "Python Packages"];

/// One row of the groups table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupNotificationSettings {
    pub display: NotificationDisplay,
    /// The notification is recorded in the Notifications tool window.
    pub log: bool,
}

impl Default for GroupNotificationSettings {
    fn default() -> Self {
        Self {
            display: NotificationDisplay::Balloon,
            log: true,
        }
    }
}

/// How one notification is presented, decided from the settings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NotificationPresentation {
    /// The balloon style, or None when nothing pops up.
    pub popup: Option<NotificationDisplay>,
    /// Recorded in the Notifications tool window.
    pub log: bool,
    /// Also sent to the desktop's notification service.
    pub system: bool,
}

/// Appearance & Behavior › Notifications.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredNotificationSettings")]
pub struct NotificationSettings {
    /// "Display balloon notifications": off means no group pops up.
    pub display_balloons: bool,
    /// "Enable system notifications": desktop notifications while the IDE
    /// window is not focused.
    pub system_notifications: bool,
    /// Groups whose settings differ from their default ([`Self::default_group`]).
    pub groups: BTreeMap<String, GroupNotificationSettings>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            display_balloons: true,
            system_notifications: true,
            groups: BTreeMap::new(),
        }
    }
}

/// What is read from `settings.json`, including the older `disabled_groups`
/// list ("Don't show again"), which now means "No popup" for those groups.
#[derive(Deserialize)]
#[serde(default)]
struct StoredNotificationSettings {
    display_balloons: bool,
    system_notifications: bool,
    groups: BTreeMap<String, GroupNotificationSettings>,
    disabled_groups: Vec<String>,
}

impl Default for StoredNotificationSettings {
    fn default() -> Self {
        let d = NotificationSettings::default();
        Self {
            display_balloons: d.display_balloons,
            system_notifications: d.system_notifications,
            groups: d.groups,
            disabled_groups: Vec::new(),
        }
    }
}

impl From<StoredNotificationSettings> for NotificationSettings {
    fn from(stored: StoredNotificationSettings) -> Self {
        let mut s = NotificationSettings {
            display_balloons: stored.display_balloons,
            system_notifications: stored.system_notifications,
            groups: BTreeMap::new(),
        };
        for (group, settings) in stored.groups {
            s.set_group(&group, settings);
        }
        // An explicit per-group entry wins over the legacy list.
        for group in stored.disabled_groups {
            if !s.groups.contains_key(&group) {
                s.set_display(&group, NotificationDisplay::NoPopup);
            }
        }
        s
    }
}

impl NotificationSettings {
    /// The out-of-the-box row of `group`: a logged balloon, next to the tool
    /// window's stripe button for [`TOOL_WINDOW_GROUPS`].
    pub fn default_group(group: &str) -> GroupNotificationSettings {
        GroupNotificationSettings {
            display: if TOOL_WINDOW_GROUPS.contains(&group) {
                NotificationDisplay::ToolWindowBalloon
            } else {
                NotificationDisplay::Balloon
            },
            log: true,
        }
    }

    pub fn group(&self, group: &str) -> GroupNotificationSettings {
        self.groups
            .get(group)
            .copied()
            .unwrap_or_else(|| Self::default_group(group))
    }

    /// Stores the settings of `group` (a default row is not stored).
    pub fn set_group(&mut self, group: &str, settings: GroupNotificationSettings) {
        if settings == Self::default_group(group) {
            self.groups.remove(group);
        } else {
            self.groups.insert(group.to_string(), settings);
        }
    }

    pub fn set_display(&mut self, group: &str, display: NotificationDisplay) {
        let mut g = self.group(group);
        g.display = display;
        self.set_group(group, g);
    }

    pub fn set_log(&mut self, group: &str, log: bool) {
        let mut g = self.group(group);
        g.log = log;
        self.set_group(group, g);
    }

    /// Groups that do not pop up ("Don't show again" or "No popup").
    pub fn silenced_groups(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, g)| g.display == NotificationDisplay::NoPopup)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// How a notification of `group` is presented; `window_focused` is whether
    /// the IDE window has the focus (system notifications are only sent when
    /// it has not).
    pub fn presentation(&self, group: &str, window_focused: bool) -> NotificationPresentation {
        let g = self.group(group);
        let wants_popup = g.display != NotificationDisplay::NoPopup;
        NotificationPresentation {
            popup: (wants_popup && self.display_balloons).then_some(g.display),
            log: g.log,
            system: wants_popup && self.system_notifications && !window_focused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_disabled_groups_migrate_to_no_popup() {
        let s: NotificationSettings =
            serde_json::from_str(r#"{"disabled_groups": ["Git", "Docker"]}"#).unwrap();
        assert!(s.display_balloons && s.system_notifications);
        assert_eq!(s.group("Git").display, NotificationDisplay::NoPopup);
        assert!(s.group("Git").log, "migrated groups are still logged");
        assert_eq!(s.group("Safe Mode"), GroupNotificationSettings::default());
        assert_eq!(
            s.silenced_groups(),
            vec!["Docker".to_string(), "Git".to_string()]
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("disabled_groups"),
            "the legacy key is not written back: {json}"
        );
        let back: NotificationSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn explicit_group_rows_win_over_the_legacy_list() {
        let s: NotificationSettings = serde_json::from_str(
            r#"{"display_balloons": false, "system_notifications": false,
                "groups": {"Git": {"display": "sticky_balloon", "log": false},
                           "Docker": {"display": "balloon"}},
                "disabled_groups": ["Git"]}"#,
        )
        .unwrap();
        assert!(!s.display_balloons && !s.system_notifications);
        assert_eq!(
            s.group("Git"),
            GroupNotificationSettings {
                display: NotificationDisplay::StickyBalloon,
                log: false
            }
        );
        assert!(
            !s.groups.contains_key("Docker"),
            "default rows are not kept"
        );
        assert_eq!(
            serde_json::from_str::<NotificationSettings>("{}").unwrap(),
            NotificationSettings::default()
        );
    }

    #[test]
    fn setting_a_row_back_to_default_removes_it() {
        let mut s = NotificationSettings::default();
        s.set_display("Docker", NotificationDisplay::ToolWindowBalloon);
        s.set_log("Docker", false);
        assert_eq!(s.groups.len(), 1);
        s.set_display("Docker", NotificationDisplay::Balloon);
        s.set_log("Docker", true);
        assert!(s.groups.is_empty());
    }

    #[test]
    fn tool_window_groups_default_to_tool_window_balloons() {
        let mut s = NotificationSettings::default();
        for g in TOOL_WINDOW_GROUPS {
            assert_eq!(
                s.group(g).display,
                NotificationDisplay::ToolWindowBalloon,
                "{g}"
            );
        }
        assert_eq!(s.group("Docker").display, NotificationDisplay::Balloon);
        assert!(s.groups.is_empty(), "defaults are not stored");
        s.set_display("Git", NotificationDisplay::Balloon);
        assert_eq!(
            s.group("Git").display,
            NotificationDisplay::Balloon,
            "an explicit choice wins"
        );
        let back: NotificationSettings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(
            back.group("Git").display,
            NotificationDisplay::Balloon,
            "and is saved"
        );
        s.set_display("Git", NotificationDisplay::ToolWindowBalloon);
        assert!(
            s.groups.is_empty(),
            "back at the default, the row is dropped"
        );
    }

    #[test]
    fn presentation_follows_global_and_group_switches() {
        let mut s = NotificationSettings::default();
        s.set_display("Git", NotificationDisplay::Balloon);
        let p = s.presentation("Git", true);
        assert_eq!(
            p,
            NotificationPresentation {
                popup: Some(NotificationDisplay::Balloon),
                log: true,
                system: false
            }
        );
        assert!(
            s.presentation("Git", false).system,
            "unfocused windows get a desktop notification"
        );
        s.set_display("Git", NotificationDisplay::StickyBalloon);
        assert_eq!(
            s.presentation("Git", true).popup,
            Some(NotificationDisplay::StickyBalloon)
        );
        s.set_display("Git", NotificationDisplay::NoPopup);
        let p = s.presentation("Git", false);
        assert_eq!(
            (p.popup, p.log, p.system),
            (None, true, false),
            "no popup = log only"
        );
        s.set_log("Git", false);
        assert!(!s.presentation("Git", true).log);
        s.display_balloons = false;
        let p = s.presentation("Docker", false);
        assert_eq!(p.popup, None, "the global switch hides every balloon");
        assert!(p.system && p.log);
        s.system_notifications = false;
        assert!(!s.presentation("Docker", false).system);
    }
}

//! Notifications (roadmap §5.1 "balloons/notifications", §6 "Notifications"
//! tool window): balloons at the bottom right with actions and "Don't show
//! again", and a timeline (the Event Log) to review them later. Info balloons
//! expire, errors stay until they are closed. Settings › Appearance & Behavior
//! › Notifications decides per group how a notification pops up (balloon,
//! sticky balloon, tool window balloon, no popup) and whether it is logged.

use crate::a11y::AccessibleCombo;
use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{Align2, Color32, Pos2, Sense, Stroke, Vec2};
use rustpy_settings::{NotificationDisplay, NotificationSettings};

use crate::actions::Action;
use crate::theme::UiTheme;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotificationKind {
    Info,
    Warning,
    Error,
}

impl NotificationKind {
    pub fn color(self, theme: &UiTheme) -> Color32 {
        match self {
            NotificationKind::Info => theme.accent,
            NotificationKind::Warning => theme.warning,
            NotificationKind::Error => theme.error,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            NotificationKind::Info => "i",
            NotificationKind::Warning => "!",
            NotificationKind::Error => "×",
        }
    }

    /// How long a balloon stays (None = until closed).
    pub fn balloon_secs(self) -> Option<f64> {
        match self {
            NotificationKind::Info => Some(INFO_BALLOON_SECS),
            NotificationKind::Warning => Some(WARNING_BALLOON_SECS),
            NotificationKind::Error => None,
        }
    }
}

/// What a notification link does; the application carries it out.
#[derive(Clone, Debug, PartialEq)]
pub enum NotificationAction {
    GitContinuation {
        root: PathBuf,
        operation: String,
        action: String,
    },
    GitLog {
        root: PathBuf,
        revision: String,
    },
    RetrySetupWorkspace,
    RestoreBranch {
        root: PathBuf,
        name: String,
        oid: String,
    },
    /// Open a file (at a 0-based line).
    OpenFile(PathBuf, Option<usize>),
    RunAction(Action),
    /// Open Settings on a page (see `IdeApp::open_settings_page`).
    OpenSettings(&'static str),
    /// Install a requirements file into the project interpreter.
    InstallRequirements(PathBuf),
    /// Show a bottom tool window by title ("Git", "Python Packages",
    /// "Notifications"); unlike its toggle action it never hides it.
    ShowToolWindow(&'static str),
}

/// Groups the IDE posts to (Settings › Appearance & Behavior › Notifications
/// lists them even before they posted anything).
pub const KNOWN_GROUPS: &[&str] = &[
    "Background Tasks",
    "Docker",
    "External Tools",
    "File Watcher",
    "Git",
    "GitHub",
    "Project Setup",
    "Project Structure",
    "Python Interpreter",
    "Python Packages",
    "Safe Mode",
    "Settings",
];

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    /// Notification group ("Git", "Python Packages", …): "Don't show again" applies to it.
    pub group: String,
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    pub actions: Vec<(String, NotificationAction)>,
    /// UI time it was (last) posted.
    pub posted: f64,
    /// Wall-clock time for the timeline ("14:03:12").
    pub clock: String,
    /// Captured wall clock; rendered using the current region after locale changes.
    pub timestamp_millis: i64,
    pub read: bool,
    /// The balloon stays until closed, whatever the kind.
    pub sticky: bool,
    /// How many identical notifications were merged into this one.
    pub count: u32,
    /// Recorded in the Notifications tool window (the group's "Log" setting);
    /// an unlogged one only lives as long as its balloon.
    pub logged: bool,
}

/// A request from a balloon or the tool window.
#[derive(Clone, Debug, PartialEq)]
pub enum NotificationEvent {
    Action(NotificationAction),
    /// "Don't show again": the group no longer pops up (stored in the settings).
    DisableGroup(String),
    /// The group pops up as a balloon again.
    EnableGroup(String),
}

pub const INFO_BALLOON_SECS: f64 = 8.0;
pub const WARNING_BALLOON_SECS: f64 = 15.0;
/// Timeline length; older entries are dropped.
const MAX_ITEMS: usize = 300;
/// Balloons on screen at once; older ones leave the stack (they stay in the timeline).
const MAX_BALLOONS: usize = 4;
const BALLOON_WIDTH: f32 = 380.0;

#[derive(Clone, Debug)]
struct Balloon {
    id: u64,
    /// When the balloon was shown or last hovered (expiry counts from here).
    since: f64,
    /// "Sticky balloon": stays until it is closed.
    sticky: bool,
    /// "Tool window balloon": shown next to the related tool window's stripe button.
    tool_window: bool,
}

#[derive(Default)]
pub struct NotificationCenter {
    /// Oldest first.
    items: Vec<Notification>,
    balloons: Vec<Balloon>,
    /// Settings › Appearance & Behavior › Notifications.
    settings: NotificationSettings,
    /// Whether the IDE window has the focus (None until known: treated as focused).
    pub window_focused: Option<bool>,
    next_id: u64,
    /// Tool window group filter (None = all groups).
    pub filter: Option<String>,
    category: usize,
    query: String,
    collapsed: BTreeSet<u64>,
    pin: Option<u64>,
    autoscroll: bool,
    last_visible: Option<u64>,
}

impl NotificationCenter {
    /// Applies the Notifications settings: balloons of groups that no longer pop
    /// up close, the others take their group's current style.
    pub fn set_settings(&mut self, settings: &NotificationSettings) {
        self.settings = settings.clone();
        let mut balloons = std::mem::take(&mut self.balloons);
        balloons.retain_mut(|b| {
            let Some(n) = self.items.iter().find(|n| n.id == b.id) else {
                return false;
            };
            match self.settings.presentation(&n.group, true).popup {
                Some(display) => {
                    b.sticky = display == NotificationDisplay::StickyBalloon;
                    b.tool_window = display == NotificationDisplay::ToolWindowBalloon;
                    true
                }
                None => false,
            }
        });
        self.balloons = balloons;
        self.drop_unlogged();
    }

    pub fn settings(&self) -> &NotificationSettings {
        &self.settings
    }

    /// Whether `group` pops up ("Don't show again" / "No popup" turn it off).
    pub fn is_group_disabled(&self, group: &str) -> bool {
        self.settings.group(group).display == NotificationDisplay::NoPopup
    }

    /// Whether a notification of `group` posted now also goes to the desktop's
    /// notification service (system notifications on, the window not focused).
    pub fn wants_system_notification(&self, group: &str) -> bool {
        self.settings
            .presentation(group, self.window_focused != Some(false))
            .system
    }

    /// Posts a notification at UI time `now`: it pops up and is logged as its
    /// group's settings say. An identical notification whose balloon is still up
    /// is merged into it (its count goes up) instead of stacking. Returns the id.
    pub fn notify(
        &mut self,
        now: f64,
        kind: NotificationKind,
        group: &str,
        title: &str,
        body: &str,
        actions: Vec<(String, NotificationAction)>,
    ) -> u64 {
        if let Some(n) = self.items.last_mut() {
            let showing = self.balloons.iter().any(|b| b.id == n.id);
            if showing && n.kind == kind && n.group == group && n.title == title && n.body == body {
                n.count += 1;
                n.posted = now;
                n.read = false;
                n.clock = clock_now();
                n.timestamp_millis = chrono::Local::now().timestamp_millis();
                let id = n.id;
                if let Some(b) = self.balloons.iter_mut().find(|b| b.id == id) {
                    b.since = now;
                }
                return id;
            }
        }
        let presentation = self.settings.presentation(group, true);
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Notification {
            id,
            group: group.to_string(),
            kind,
            title: title.to_string(),
            body: body.to_string(),
            actions,
            posted: now,
            clock: clock_now(),
            timestamp_millis: chrono::Local::now().timestamp_millis(),
            read: false,
            sticky: false,
            count: 1,
            logged: presentation.log,
        });
        if self.items.len() > MAX_ITEMS {
            let drop = self.items.len() - MAX_ITEMS;
            let gone: Vec<u64> = self.items.drain(..drop).map(|n| n.id).collect();
            self.balloons.retain(|b| !gone.contains(&b.id));
            self.collapsed.retain(|id| !gone.contains(id));
        }
        if let Some(display) = presentation.popup {
            self.balloons.push(Balloon {
                id,
                since: now,
                sticky: display == NotificationDisplay::StickyBalloon,
                tool_window: display == NotificationDisplay::ToolWindowBalloon,
            });
            if self.balloons.len() > MAX_BALLOONS {
                let extra = self.balloons.len() - MAX_BALLOONS;
                self.balloons.drain(..extra);
            }
        }
        self.drop_unlogged();
        id
    }

    /// Forgets notifications that are not logged once their balloon is gone.
    fn drop_unlogged(&mut self) {
        let balloons = &self.balloons;
        self.items
            .retain(|n| n.logged || balloons.iter().any(|b| b.id == n.id));
    }

    /// Makes the balloon of `id` stay until it is closed.
    pub fn set_sticky(&mut self, id: u64) {
        if let Some(n) = self.items.iter_mut().find(|n| n.id == id) {
            n.sticky = true;
        }
    }

    /// The notifications, oldest first, including unlogged ones whose balloon
    /// is still up (see [`Self::logged`] for the tool window's timeline).
    pub fn items(&self) -> &[Notification] {
        &self.items
    }

    /// The timeline of the Notifications tool window.
    pub fn logged(&self) -> impl Iterator<Item = &Notification> {
        self.items.iter().filter(|n| n.logged)
    }

    pub fn get(&self, id: u64) -> Option<&Notification> {
        self.items.iter().find(|n| n.id == id)
    }

    /// Ids of the balloons on screen, oldest first.
    pub fn balloon_ids(&self) -> Vec<u64> {
        self.balloons.iter().map(|b| b.id).collect()
    }

    /// Ids of the "Tool window balloon" balloons on screen, oldest first.
    pub fn tool_window_balloon_ids(&self) -> Vec<u64> {
        self.balloons
            .iter()
            .filter(|b| b.tool_window)
            .map(|b| b.id)
            .collect()
    }

    pub fn unread(&self) -> usize {
        self.logged().filter(|n| !n.read).count()
    }

    /// Unread errors (the status-bar bell turns red).
    pub fn unread_errors(&self) -> usize {
        self.logged()
            .filter(|n| !n.read && n.kind == NotificationKind::Error)
            .count()
    }

    pub fn mark_all_read(&mut self) {
        for n in &mut self.items {
            n.read = true;
        }
    }

    /// Clears the timeline and the balloons.
    pub fn clear(&mut self) {
        self.items.clear();
        self.balloons.clear();
        self.collapsed.clear();
        self.pin = None;
    }

    /// Closes a balloon (the entry stays in the timeline, read).
    pub fn dismiss(&mut self, id: u64) {
        self.balloons.retain(|b| b.id != id);
        if let Some(n) = self.items.iter_mut().find(|n| n.id == id) {
            n.read = true;
        }
        self.drop_unlogged();
    }

    /// Stops popping up `group` (its notifications still reach the timeline
    /// when it is logged).
    pub fn disable_group(&mut self, group: &str) {
        let mut settings = self.settings.clone();
        settings.set_display(group, NotificationDisplay::NoPopup);
        self.set_settings(&settings);
    }

    /// `group` pops up again, in its default style.
    pub fn enable_group(&mut self, group: &str) {
        let mut settings = self.settings.clone();
        settings.set_display(group, NotificationSettings::default_group(group).display);
        self.set_settings(&settings);
    }

    /// Removes the balloons whose time is up: info after [`INFO_BALLOON_SECS`],
    /// warnings after [`WARNING_BALLOON_SECS`]; errors and sticky ones stay.
    pub fn expire(&mut self, now: f64) {
        let items = &self.items;
        self.balloons
            .retain(|b| match items.iter().find(|n| n.id == b.id) {
                Some(n) if n.sticky || b.sticky => true,
                Some(n) => n
                    .kind
                    .balloon_secs()
                    .is_none_or(|secs| now - b.since < secs),
                None => false,
            });
        self.drop_unlogged();
    }

    /// Seconds until the next balloon expires (to schedule a repaint).
    pub fn next_expiry(&self, now: f64) -> Option<f64> {
        self.balloons
            .iter()
            .filter_map(|b| {
                let n = self.get(b.id)?;
                if n.sticky || b.sticky {
                    return None;
                }
                n.kind.balloon_secs().map(|s| (b.since + s - now).max(0.0))
            })
            .min_by(|a, b| a.total_cmp(b))
    }

    /// Groups that have notifications in the timeline or do not pop up.
    pub fn groups(&self) -> Vec<String> {
        let mut g: BTreeSet<String> = self.logged().map(|n| n.group.clone()).collect();
        g.extend(self.settings.silenced_groups());
        g.into_iter().collect()
    }

    /// Draws the balloons at the bottom right of the window.
    pub fn show_balloons(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        now: f64,
    ) -> Vec<NotificationEvent> {
        self.show_balloons_at(ctx, theme, now, None)
    }

    /// Draws the balloons: "Tool window balloon" ones next to the related tool
    /// window's stripe button at `anchor` (with the others when there is none),
    /// the rest at the bottom right.
    pub fn show_balloons_at(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        now: f64,
        anchor: Option<Pos2>,
    ) -> Vec<NotificationEvent> {
        self.expire(now);
        let mut events = Vec::new();
        if self.balloons.is_empty() {
            return events;
        }
        let mut close: Vec<u64> = Vec::new();
        let mut hovered: Vec<u64> = Vec::new();
        let (anchored, regular): (Vec<&Balloon>, Vec<&Balloon>) = self
            .balloons
            .iter()
            .partition(|b| b.tool_window && anchor.is_some());
        let stacks: [(Vec<Notification>, &str, Option<Pos2>); 2] = [
            (
                regular
                    .iter()
                    .filter_map(|b| self.get(b.id).cloned())
                    .collect(),
                "notification_balloons",
                None,
            ),
            (
                anchored
                    .iter()
                    .filter_map(|b| self.get(b.id).cloned())
                    .collect(),
                "notification_tool_window_balloons",
                anchor,
            ),
        ];
        for (shown, id, anchor) in &stacks {
            if shown.is_empty() {
                continue;
            }
            let area = egui::Area::new(egui::Id::new(*id)).order(egui::Order::Foreground);
            let area = if let Some(pos) = anchor {
                area.fixed_pos(*pos).constrain(true)
            } else {
                area.anchor(Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -34.0))
            };
            Self::balloon_stack(
                area,
                ctx,
                theme,
                shown,
                &mut events,
                &mut close,
                &mut hovered,
            );
        }
        for b in &mut self.balloons {
            if hovered.contains(&b.id) {
                b.since = now;
            }
        }
        for id in close {
            self.dismiss(id);
        }
        for ev in &events {
            if let NotificationEvent::DisableGroup(g) = ev {
                self.disable_group(g);
            }
        }
        if let Some(t) = self.next_expiry(now) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(t + 0.05));
        }
        events
    }

    /// One stack of balloons in `area`.
    fn balloon_stack(
        area: egui::Area,
        ctx: &egui::Context,
        theme: &UiTheme,
        shown: &[Notification],
        events: &mut Vec<NotificationEvent>,
        close: &mut Vec<u64>,
        hovered: &mut Vec<u64>,
    ) {
        area.show(ctx, |ui| {
            ui.set_width(BALLOON_WIDTH);
            for n in shown {
                let accent = n.kind.color(theme);
                let frame = egui::Frame::popup(ui.style()).fill(theme.panel_bg).stroke(Stroke::new(1.0, accent.gamma_multiply(0.8))).inner_margin(egui::Margin::same(10));
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(BALLOON_WIDTH - 20.0);
                        ui.horizontal(|ui| {
                            kind_badge(ui, n.kind, theme);
                            let title = if n.count > 1 { format!("{} ({})", crate::i18n::tr(&n.title), n.count) } else { crate::i18n::tr(&n.title).into_owned() };
                            ui.add(egui::Label::new(egui::RichText::new(title).strong().color(theme.text)).wrap());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                if ui.add(egui::Button::new(egui::RichText::new(crate::i18n::tr_static("×")).color(theme.text_dim)).frame(false)).on_hover_text("Close").clicked() {
                                    close.push(n.id);
                                }
                            });
                        });
                        if !n.body.is_empty() {
                            ui.add(egui::Label::new(egui::RichText::new(truncate_lines(&crate::i18n::tr(&n.body),6)).color(theme.text)).wrap());
                        }
                        ui.horizontal_wrapped(|ui| {
                            for (label, action) in &n.actions {
                                if ui.link(egui::RichText::new(crate::i18n::tr(label)).color(theme.accent)).clicked() {
                                    events.push(NotificationEvent::Action(action.clone()));
                                    close.push(n.id);
                                }
                            }
                            if ui.link(egui::RichText::new(crate::i18n::tr_static("Don't show again")).color(theme.text_dim)).on_hover_text(format!("No more balloons for \"{}\" notifications (they still appear in View › Tool Windows › Notifications)", n.group)).clicked() {
                                events.push(NotificationEvent::DisableGroup(n.group.clone()));
                            }
                        });
                    })
                    .response;
                if resp.contains_pointer() {
                    hovered.push(n.id);
                }
                ui.add_space(6.0);
            }
        });
    }

    /// The Notifications tool window: timeline (newest first), Mark all as read,
    /// Clear all and a group filter. Showing it marks the entries read.
    pub fn tool_window(&mut self, ui: &mut egui::Ui, theme: &UiTheme) -> Vec<NotificationEvent> {
        let mut events = Vec::new();
        ui.horizontal(|ui| {
            let unread = self.unread();
            ui.label(
                egui::RichText::new(crate::i18n::tr_fmt(
                    "{n} notification(s), {unread} unread",
                    &[("n", &self.logged().count()), ("unread", &unread)],
                ))
                .color(theme.text_dim),
            );
            if ui
                .add_enabled(
                    unread > 0,
                    egui::Button::new(crate::i18n::tr_static("Mark All as Read")),
                )
                .clicked()
            {
                self.mark_all_read();
            }
            if ui
                .add_enabled(
                    self.logged().next().is_some(),
                    egui::Button::new(crate::i18n::tr_static("Clear All")),
                )
                .clicked()
            {
                self.clear();
            }
            let groups = self.groups();
            let current = self.filter.clone().unwrap_or_else(|| "All groups".into());
            egui::ComboBox::from_id_salt("notification_group_filter")
                .selected_text(crate::i18n::tr(&current))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            self.filter.is_none(),
                            crate::i18n::tr_static("All groups"),
                        )
                        .clicked()
                    {
                        self.filter = None;
                    }
                    for g in &groups {
                        if ui
                            .selectable_label(self.filter.as_deref() == Some(g.as_str()), g)
                            .clicked()
                        {
                            self.filter = Some(g.clone());
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("Group"));
            let silenced = self.settings.silenced_groups();
            if !silenced.is_empty() {
                ui.menu_button(crate::i18n::tr_static("Balloons Off"), |ui| {
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_static(
                            "These groups only log to this window:",
                        ))
                        .color(theme.text_dim)
                        .small(),
                    );
                    for g in silenced {
                        if ui
                            .button(crate::i18n::tr_fmt(
                                "Show balloons for {group}",
                                &[("group", &g)],
                            ))
                            .clicked()
                        {
                            self.enable_group(&g);
                            events.push(NotificationEvent::EnableGroup(g));
                            ui.close();
                        }
                    }
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (category, label) in [(0, "Timeline"), (1, "Important"), (2, "Suggestions")] {
                ui.selectable_value(&mut self.category, category, crate::i18n::tr_static(label));
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.query).hint_text("Speed search notifications"),
            );
            if ui.button("Expand all").clicked() {
                self.collapsed.clear();
            }
            if ui.button("Collapse all").clicked() {
                self.collapsed = self.items.iter().map(|item| item.id).collect();
            }
            let mut pin = self.pin.is_some();
            if ui.checkbox(&mut pin, "Pin timeline").changed() {
                self.pin = pin.then(|| self.logged().last().map_or(0, |item| item.id));
            }
            ui.checkbox(&mut self.autoscroll, "Autoscroll to latest");
            if ui.link("Notification settings…").clicked() {
                events.push(NotificationEvent::Action(NotificationAction::OpenSettings(
                    "notifications",
                )));
            }
        });
        ui.separator();
        let filter = self.filter.clone();
        let query = self.query.clone();
        let category = self.category;
        let cutoff = self.pin;
        let mut read = Vec::new();
        let newest = self.logged().last().map(|item| item.id);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("notifications_timeline")
            .auto_shrink([false, false]);
        if self.autoscroll && cutoff.is_none() && newest != self.last_visible {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        self.last_visible = newest;
        scroll.show(ui, |ui| {
            if self.logged().next().is_none() {ui.label(egui::RichText::new(crate::i18n::tr_static("No notifications yet. Errors, warnings and results of background tasks appear here.")).color(theme.text_dim));}
            for n in self.items.iter().rev().filter(|n| n.logged && filter.as_ref().is_none_or(|f| *f == n.group)
                && cutoff.is_none_or(|cutoff|n.id<=cutoff)
                && notification_category(n,category)
                && crate::project_tree::speed_search_matches(&format!("{} {} {}",n.group,n.title,n.body),&query)) {
                let expanded=!self.collapsed.contains(&n.id);
                ui.horizontal(|ui| {
                    if ui.small_button(if expanded{"▾"}else{"▸"}).clicked(){if expanded{self.collapsed.insert(n.id);}else{self.collapsed.remove(&n.id);}}
                    kind_badge(ui,n.kind,theme);
                    ui.label(egui::RichText::new(rustpy_i18n::regional::time_millis(n.timestamp_millis)).color(theme.text_dim).monospace());
                    ui.label(egui::RichText::new(crate::i18n::tr(&n.group)).color(theme.text_dim));
                    let mut title=egui::RichText::new(if n.count>1{format!("{} ({})",crate::i18n::tr(&n.title),n.count)}else{crate::i18n::tr(&n.title).into_owned()}).color(theme.text);
                    if !n.read{title=title.strong();}
                    ui.add(egui::Label::new(title).wrap());
                    if !n.read && ui.small_button("Mark read").clicked(){read.push(n.id);}
                });
                if expanded {
                    if !n.body.is_empty(){ui.horizontal(|ui|{ui.add_space(22.0);ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(&n.body)).color(theme.text)).wrap());});}
                    ui.horizontal_wrapped(|ui|{ui.add_space(22.0);for(label,action)in &n.actions{if ui.link(egui::RichText::new(crate::i18n::tr(label)).color(theme.accent)).clicked(){read.push(n.id);events.push(NotificationEvent::Action(action.clone()));}}});
                }
                ui.add_space(4.0);
            }
        });
        for id in read {
            self.dismiss(id);
        }
        events
    }
}

fn notification_category(notification: &Notification, category: usize) -> bool {
    match category {
        1 => notification.kind != NotificationKind::Info || notification.sticky,
        2 => notification.kind == NotificationKind::Info && !notification.actions.is_empty(),
        _ => true,
    }
}

fn kind_badge(ui: &mut egui::Ui, kind: NotificationKind, theme: &UiTheme) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    let c = rect.center();
    ui.painter().circle_filled(c, 7.5, kind.color(theme));
    ui.painter().text(
        Pos2::new(c.x, c.y),
        Align2::CENTER_CENTER,
        kind.glyph(),
        egui::FontId::proportional(10.5),
        Color32::WHITE,
    );
}

fn truncate_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_string();
    }
    format!("{}\n…", lines[..max].join("\n"))
}

fn clock_now() -> String {
    rustpy_i18n::regional::clock_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(
        c: &mut NotificationCenter,
        now: f64,
        kind: NotificationKind,
        group: &str,
        title: &str,
    ) -> u64 {
        c.notify(now, kind, group, title, "", Vec::new())
    }

    #[test]
    fn info_expires_errors_stay() {
        let mut c = NotificationCenter::default();
        let info = post(&mut c, 0.0, NotificationKind::Info, "Git", "Pushed");
        let warn = post(
            &mut c,
            0.0,
            NotificationKind::Warning,
            "Safe Mode",
            "Not trusted",
        );
        let err = post(&mut c, 0.0, NotificationKind::Error, "Git", "Push failed");
        assert_eq!(c.balloon_ids(), vec![info, warn, err]);
        assert_eq!(c.next_expiry(1.0), Some(INFO_BALLOON_SECS - 1.0));
        c.expire(INFO_BALLOON_SECS + 0.1);
        assert_eq!(c.balloon_ids(), vec![warn, err], "info balloons expire");
        c.expire(1000.0);
        assert_eq!(c.balloon_ids(), vec![err], "errors stay until closed");
        assert_eq!(c.next_expiry(1000.0), None);
        assert_eq!(c.items().len(), 3, "expired balloons stay in the timeline");
        assert_eq!(c.unread(), 3);
        assert_eq!(c.unread_errors(), 1);
        c.dismiss(err);
        assert!(c.balloon_ids().is_empty());
        assert_eq!(c.unread(), 2, "closing a balloon marks it read");
        let sticky = post(&mut c, 2000.0, NotificationKind::Info, "Setup", "Sticky");
        c.set_sticky(sticky);
        c.expire(5000.0);
        assert_eq!(c.balloon_ids(), vec![sticky]);
        c.mark_all_read();
        assert_eq!(c.unread(), 0);
        c.clear();
        assert!(c.items().is_empty() && c.balloon_ids().is_empty());
    }

    #[test]
    fn dont_show_again_suppresses_balloons_not_the_log() {
        let mut c = NotificationCenter::default();
        let first = post(
            &mut c,
            0.0,
            NotificationKind::Error,
            "Docker",
            "docker ps failed",
        );
        c.disable_group("Docker");
        assert!(c.balloon_ids().is_empty(), "the group's balloons close");
        post(
            &mut c,
            1.0,
            NotificationKind::Error,
            "Docker",
            "docker images failed",
        );
        assert!(c.balloon_ids().is_empty());
        assert_eq!(c.items().len(), 2);
        assert!(c.items().iter().any(|n| n.id == first));
        assert!(c.is_group_disabled("Docker"));
        assert_eq!(c.settings().silenced_groups(), vec!["Docker".to_string()]);
        let git = post(&mut c, 2.0, NotificationKind::Info, "Git", "Pushed");
        assert_eq!(c.balloon_ids(), vec![git], "other groups are not affected");
        c.enable_group("Docker");
        let again = post(
            &mut c,
            3.0,
            NotificationKind::Error,
            "Docker",
            "docker ps failed",
        );
        assert!(c.balloon_ids().contains(&again));
        let mut settings = NotificationSettings::default();
        settings.set_display("Git", NotificationDisplay::NoPopup);
        c.set_settings(&settings);
        assert_eq!(
            c.balloon_ids(),
            vec![again],
            "loading the settings closes balloons of disabled groups"
        );
        assert_eq!(c.groups(), vec!["Docker".to_string(), "Git".to_string()]);
    }

    #[test]
    fn repeats_merge_and_old_balloons_leave_the_stack() {
        let mut c = NotificationCenter::default();
        let a = post(
            &mut c,
            0.0,
            NotificationKind::Error,
            "File Watchers",
            "black failed",
        );
        let b = post(
            &mut c,
            1.0,
            NotificationKind::Error,
            "File Watchers",
            "black failed",
        );
        assert_eq!(
            a, b,
            "an identical notification with its balloon up is merged"
        );
        assert_eq!(c.items().len(), 1);
        assert_eq!(c.items()[0].count, 2);
        for i in 0..6 {
            post(
                &mut c,
                2.0 + i as f64,
                NotificationKind::Error,
                "Run",
                &format!("failure {i}"),
            );
        }
        assert_eq!(c.balloon_ids().len(), MAX_BALLOONS);
        assert!(
            !c.balloon_ids().contains(&a),
            "the oldest balloons go first"
        );
        assert_eq!(c.items().len(), 7);
        for i in 0..(MAX_ITEMS + 5) {
            post(
                &mut c,
                100.0 + i as f64,
                NotificationKind::Info,
                "Bulk",
                &format!("n{i}"),
            );
        }
        assert_eq!(c.items().len(), MAX_ITEMS);
        assert!(c.balloon_ids().iter().all(|id| c.get(*id).is_some()));
    }
}

#[cfg(test)]
mod display_type_fixtures {
    use super::*;

    fn center(group: &str, display: NotificationDisplay, log: bool) -> NotificationCenter {
        let mut settings = NotificationSettings::default();
        settings.set_group(
            group,
            rustpy_settings::GroupNotificationSettings { display, log },
        );
        let mut c = NotificationCenter::default();
        c.set_settings(&settings);
        c
    }

    #[test]
    fn sticky_balloons_do_not_expire() {
        let mut c = center("Git", NotificationDisplay::StickyBalloon, true);
        let sticky = c.notify(0.0, NotificationKind::Info, "Git", "Pushed", "", vec![]);
        let plain = c.notify(0.0, NotificationKind::Info, "Docker", "Pulled", "", vec![]);
        assert_eq!(c.next_expiry(0.0), Some(INFO_BALLOON_SECS));
        c.expire(1000.0);
        assert_eq!(c.balloon_ids(), vec![sticky]);
        assert!(!c.balloon_ids().contains(&plain));
        assert_eq!(c.next_expiry(1000.0), None);
        assert!(
            !c.get(sticky).unwrap().sticky,
            "the timeline entry is not marked important"
        );
    }

    #[test]
    fn tool_window_balloons_are_told_apart_and_fall_back_to_the_stack() {
        let mut c = center("Git", NotificationDisplay::ToolWindowBalloon, true);
        let git = c.notify(
            0.0,
            NotificationKind::Error,
            "Git",
            "Push failed",
            "",
            vec![],
        );
        let other = c.notify(0.0, NotificationKind::Error, "Docker", "Failed", "", vec![]);
        assert_eq!(c.balloon_ids(), vec![git, other]);
        assert_eq!(c.tool_window_balloon_ids(), vec![git]);
        // Without an anchor every balloon is drawn in the regular stack.
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            c.show_balloons_at(ui.ctx(), &UiTheme::dark(), 0.5, None);
        });
        output.textures_delta.clear();
        assert_eq!(c.balloon_ids(), vec![git, other]);
    }

    #[test]
    fn no_popup_logs_only_and_log_off_records_nothing() {
        let mut c = center("Git", NotificationDisplay::NoPopup, true);
        let id = c.notify(
            0.0,
            NotificationKind::Error,
            "Git",
            "Push failed",
            "",
            vec![],
        );
        assert!(c.balloon_ids().is_empty());
        assert_eq!(c.logged().map(|n| n.id).collect::<Vec<_>>(), vec![id]);
        assert_eq!(c.unread_errors(), 1);

        let mut c = center("Git", NotificationDisplay::Balloon, false);
        let id = c.notify(0.0, NotificationKind::Info, "Git", "Pushed", "", vec![]);
        assert_eq!(c.balloon_ids(), vec![id], "the balloon still shows");
        assert_eq!(c.logged().count(), 0, "but nothing is recorded");
        assert_eq!(c.unread(), 0);
        c.expire(INFO_BALLOON_SECS + 1.0);
        assert!(
            c.items().is_empty(),
            "an unlogged notification goes with its balloon"
        );

        let mut c = center("Git", NotificationDisplay::NoPopup, false);
        c.notify(
            0.0,
            NotificationKind::Error,
            "Git",
            "Push failed",
            "",
            vec![],
        );
        assert!(c.items().is_empty() && c.balloon_ids().is_empty());
    }

    #[test]
    fn global_switches_hide_balloons_and_gate_system_notifications() {
        let mut settings = NotificationSettings {
            display_balloons: false,
            ..Default::default()
        };
        let mut c = NotificationCenter::default();
        c.set_settings(&settings);
        let id = c.notify(
            0.0,
            NotificationKind::Error,
            "Git",
            "Push failed",
            "",
            vec![],
        );
        assert!(c.balloon_ids().is_empty());
        assert_eq!(c.logged().next().map(|n| n.id), Some(id));
        assert!(
            !c.wants_system_notification("Git"),
            "unknown focus counts as focused"
        );
        c.window_focused = Some(false);
        assert!(c.wants_system_notification("Git"));
        c.disable_group("Git");
        assert!(
            !c.wants_system_notification("Git"),
            "no popup sends nothing to the desktop"
        );
        settings.system_notifications = false;
        c.set_settings(&settings);
        assert!(!c.wants_system_notification("Docker"));
    }

    #[test]
    fn tool_window_groups_anchor_by_default_unless_the_user_chose_otherwise() {
        let mut c = NotificationCenter::default();
        let git = c.notify(
            0.0,
            NotificationKind::Error,
            "Git",
            "Push failed",
            "",
            vec![],
        );
        let docker = c.notify(0.0, NotificationKind::Error, "Docker", "Failed", "", vec![]);
        assert_eq!(c.balloon_ids(), vec![git, docker]);
        assert_eq!(
            c.tool_window_balloon_ids(),
            vec![git],
            "as before: Git is stripe-anchored"
        );
        c.disable_group("Git");
        c.enable_group("Git");
        let again = c.notify(
            1.0,
            NotificationKind::Error,
            "Git",
            "Pull failed",
            "",
            vec![],
        );
        assert_eq!(
            c.tool_window_balloon_ids(),
            vec![again],
            "re-enabling restores the default"
        );
        let mut settings = NotificationSettings::default();
        settings.set_display("Git", NotificationDisplay::Balloon);
        c.set_settings(&settings);
        assert!(
            c.tool_window_balloon_ids().is_empty(),
            "an explicit Balloon wins"
        );
    }

    #[test]
    fn changing_the_settings_restyles_and_closes_balloons() {
        let mut c = NotificationCenter::default();
        let id = c.notify(0.0, NotificationKind::Info, "Git", "Pushed", "", vec![]);
        let mut settings = NotificationSettings::default();
        settings.set_display("Git", NotificationDisplay::StickyBalloon);
        c.set_settings(&settings);
        c.expire(1000.0);
        assert_eq!(c.balloon_ids(), vec![id]);
        settings.display_balloons = false;
        c.set_settings(&settings);
        assert!(c.balloon_ids().is_empty());
        assert_eq!(c.logged().count(), 1);
    }
}

#[cfg(test)]
mod timeline_fixtures {
    use super::*;
    #[test]
    fn categories_use_actual_severity_sticky_state_and_suggested_actions() {
        let mut center = NotificationCenter::default();
        let info = center.notify(
            0.0,
            NotificationKind::Info,
            "Build",
            "Done",
            "Built",
            vec![],
        );
        let warning = center.notify(
            1.0,
            NotificationKind::Warning,
            "Build",
            "Failed",
            "Read details",
            vec![],
        );
        assert!(!notification_category(center.get(info).unwrap(), 1));
        assert!(notification_category(center.get(warning).unwrap(), 1));
        assert!(!notification_category(center.get(info).unwrap(), 2));
        center
            .items
            .iter_mut()
            .find(|item| item.id == info)
            .unwrap()
            .actions
            .push((
                "Settings".into(),
                NotificationAction::OpenSettings("notifications"),
            ));
        assert!(notification_category(center.get(info).unwrap(), 2));
        center.set_sticky(info);
        assert!(notification_category(center.get(info).unwrap(), 1));
        assert_eq!(center.unread(), 2);
        center.mark_all_read();
        assert_eq!(center.unread(), 0);
        center.collapsed.insert(info);
        center.pin = Some(info);
        center.clear();
        assert!(center.collapsed.is_empty() && center.pin.is_none());
    }
}

#[cfg(test)]
mod timeline_ui_fixtures {
    use super::*;
    #[test]
    fn opening_timeline_preserves_unread_and_exposes_real_category_and_settings_controls() {
        let mut center = NotificationCenter::default();
        center.notify(
            0.0,
            NotificationKind::Warning,
            "Git",
            "Review failed",
            "Details",
            vec![],
        );
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                center.tool_window(ui, &UiTheme::dark());
            },
        );
        for label in [
            "Timeline",
            "Important",
            "Suggestions",
            "Expand all",
            "Collapse all",
            "Mark All as Read",
            "Notification settings…",
        ] {
            assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()==label)),"missing actual event-log control {label}");
        }
        assert_eq!(center.unread(), 1);
        output.drop_without_applying_deltas();
    }
}

//! Notifications in the IDE (roadmap §5.1, §6): posting them, the balloons and
//! the Notifications tool window, carrying out their actions, turning the
//! results of background jobs into notifications, and the Settings ›
//! Appearance & Behavior › Notifications page (popup type and log per group).

use super::*;
use crate::a11y::AccessibleCombo;
use crate::notifications::{NotificationAction, NotificationEvent, NotificationKind, KNOWN_GROUPS};
use rustpy_settings::NotificationDisplay;

/// Seconds on the clock balloons expire by (wall time, independent of frames).
pub(super) fn notification_clock() -> f64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

/// Results that start like this are successes even when the rest of the line
/// happens to contain a word such as "error" (a file name in `git pull` output).
const SUCCESS_PREFIXES: &[&str] = &["Pull: ", "Pushed", "Fetched", "Committed", "Updated"];

/// Whether a result message describes a failure ("Push failed: …", "Cannot …",
/// a libgit2 error "…; class=Index (10); code=Locked (-14)").
pub(super) fn is_failure_message(msg: &str) -> bool {
    if SUCCESS_PREFIXES.iter().any(|p| msg.starts_with(p)) {
        return false;
    }
    let m = msg.to_ascii_lowercase();
    [
        "failed",
        "error",
        "cannot ",
        "can't ",
        "could not",
        "not found",
        "denied",
        "fatal",
        "refused",
        "rejected",
        "timed out",
        "; class=",
    ]
    .iter()
    .any(|w| m.contains(w))
}

/// A result message as a one-line title and a body: "Push failed: x" →
/// ("Push failed", "x"); "git pull --ff-only failed:\nfatal: …" → ("git pull
/// --ff-only failed", "fatal: …").
pub(super) fn split_message(msg: &str) -> (String, String) {
    let msg = msg.trim();
    let (first, rest) = msg.split_once('\n').unwrap_or((msg, ""));
    let first = first.trim();
    let (title, inline) = match first.split_once(": ") {
        Some((t, b)) if !t.trim().is_empty() && t.chars().count() <= 60 => (t.trim(), b.trim()),
        _ => (first.trim_end_matches(':').trim(), ""),
    };
    let body: Vec<&str> = [inline, rest.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    (title.to_string(), body.join("\n"))
}

/// Status-bar echo of a notification: the title and the start of the body,
/// short enough not to run into the status-bar widgets (the balloon and the
/// Notifications tool window have the full text).
fn status_text(title: &str, body: &str) -> String {
    const MAX_CHARS: usize = 90;
    let first = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let text = if first.is_empty() {
        title.to_string()
    } else {
        format!("{title}: {first}")
    };
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    let mut short: String = text.chars().take(MAX_CHARS - 1).collect();
    short.push('…');
    short
}

/// Notification group for the message of a background job.
fn background_group(msg: &str) -> &'static str {
    if msg.starts_with("Push")
        || msg.starts_with("Updated")
        || msg.starts_with("Update")
        || msg.starts_with("git")
    {
        "Git"
    } else if msg.starts_with("docker") {
        "Docker"
    } else {
        "Background Tasks"
    }
}

impl IdeApp {
    /// Posts a notification: a balloon and an entry in the Notifications tool
    /// window as its group's settings say, the status-bar text, and a desktop
    /// notification while the window is not focused (system notifications on).
    pub(super) fn notify(
        &mut self,
        kind: NotificationKind,
        group: &str,
        title: impl Into<String>,
        body: impl Into<String>,
        actions: Vec<(String, NotificationAction)>,
    ) {
        let (title, body) = (title.into(), body.into());
        self.set_status(status_text(&title, &body));
        self.sync_notification_groups();
        if self.notifications.wants_system_notification(group) {
            crate::system_notifications::send(&title, &body);
        }
        let id =
            self.notifications
                .notify(notification_clock(), kind, group, &title, &body, actions);
        self.accessibility_notification(id, kind, &title, &body);
    }

    /// An error notification without actions.
    pub(super) fn notify_error(
        &mut self,
        group: &str,
        title: impl Into<String>,
        body: impl Into<String>,
    ) {
        self.notify(NotificationKind::Error, group, title, body, Vec::new());
    }

    /// A run, tool or console was refused because the project is not trusted.
    pub(super) fn notify_safe_mode(&mut self, what: &str) {
        self.notify(
            NotificationKind::Warning,
            "Safe Mode",
            format!("{what} is disabled in safe mode"),
            "This project is not trusted yet, so RustPy does not run its code.",
            vec![(
                "Trust Project".into(),
                NotificationAction::RunAction(Action::TrustProject),
            )],
        );
    }

    /// Something needs an interpreter and the project has none.
    pub(super) fn notify_no_interpreter(&mut self) {
        self.notify(
            NotificationKind::Warning,
            "Python Interpreter",
            "No Python interpreter configured",
            "",
            vec![(
                "Configure Python Interpreter…".into(),
                NotificationAction::OpenSettings("interpreter"),
            )],
        );
    }

    /// The result of a background job (git push, docker): failures become error
    /// notifications, a successful push an info one, anything else a status message.
    pub(super) fn report_background_result(&mut self, msg: String) {
        let group = background_group(&msg);
        if is_failure_message(&msg) {
            let (title, body) = split_message(&msg);
            let actions = if group == "Git" {
                vec![(
                    "Show Git".to_string(),
                    NotificationAction::ShowToolWindow("Git"),
                )]
            } else {
                Vec::new()
            };
            self.notify(NotificationKind::Error, group, title, body, actions);
        } else if msg.starts_with("Pushed") {
            self.notify(NotificationKind::Info, group, msg, "", Vec::new());
        } else {
            self.set_status(msg);
        }
    }

    /// A message from the Git tool window: failures become error notifications.
    pub(super) fn report_git_status(&mut self, msg: String) {
        if is_failure_message(&msg) {
            let (title, body) = split_message(&msg);
            self.notify(
                NotificationKind::Error,
                "Git",
                title,
                body,
                vec![("Show Git".into(), NotificationAction::ShowToolWindow("Git"))],
            );
        } else {
            self.set_status(msg);
        }
    }

    /// Per frame: background work whose failure must not go unnoticed (pip
    /// tasks, Settings › Python Interpreter › Create virtual environment).
    pub(super) fn poll_failure_notifications(&mut self, ctx: &egui::Context) {
        self.poll_package_failures();
        self.poll_interpreter_venv(ctx);
    }

    /// A pip task of the Python Packages tool window that failed. A New Project
    /// setup waiting for its `pip install -r` reports the failure itself.
    fn poll_package_failures(&mut self) {
        if let Some(files) = self.packages_panel.take_lock_notification() {
            self.notify(
                NotificationKind::Info,
                "Python Packages",
                "Dependency files changed",
                format!("{files} changed. Install or sync the project dependencies?"),
                vec![(
                    "Review and install dependencies".into(),
                    NotificationAction::ShowToolWindow("Python Packages"),
                )],
            );
        }
        if self
            .pending_venv
            .as_ref()
            .is_some_and(|s| s.waits_for_pip())
        {
            return;
        }
        if let Some((label, detail)) = self.packages_panel.take_failure() {
            self.notify(
                NotificationKind::Error,
                "Python Packages",
                format!("{label} failed"),
                detail,
                vec![(
                    "Show Python Packages".into(),
                    NotificationAction::ShowToolWindow("Python Packages"),
                )],
            );
        }
    }

    /// Opens Settings on a page by id ("interpreter", "project-structure", "notifications", "appearance").
    pub(super) fn open_settings_page(&mut self, page: &str) {
        self.settings_ui.page = match page {
            "scientific" => settings_ui::SettingsPage::Scientific,
            "interpreter" => settings_ui::SettingsPage::Interpreter,
            "project-structure" => settings_ui::SettingsPage::ProjectStructure,
            "github" => settings_ui::SettingsPage::GitHub,
            "notifications" => settings_ui::SettingsPage::Notifications,
            "todo" => settings_ui::SettingsPage::Todo,
            "accessibility" => settings_ui::SettingsPage::Accessibility,
            "path-variables" => settings_ui::SettingsPage::PathVariables,
            _ => settings_ui::SettingsPage::Appearance,
        };
        self.dialog = Dialog::Settings;
    }

    /// Changes a setting and saves it at once. While the Settings dialog is open
    /// the change also goes into its snapshot, so neither Cancel undoes it nor
    /// does the save write the dialog's unapplied edits.
    pub(super) fn change_setting_now(&mut self, change: impl Fn(&mut Settings)) {
        change(&mut self.settings);
        if let Some(snap) = &mut self.settings_ui.snapshot {
            change(&mut snap.settings);
        }
        if let Err(e) = self.settings_to_persist().save() {
            log::warn!("cannot save the settings: {e}");
        }
    }

    /// The balloons follow the settings (the Notifications page, Cancel, Import).
    pub(super) fn sync_notification_groups(&mut self) {
        if *self.notifications.settings() != self.settings.notifications {
            self.notifications
                .set_settings(&self.settings.notifications);
        }
    }

    /// Draws the balloons (after the dialogs, so they stay on top) and runs their actions.
    pub(super) fn draw_notifications(&mut self, ctx: &egui::Context) {
        self.sync_notification_groups();
        self.notifications.window_focused = ctx.input(|i| i.viewport().focused);
        let theme = self.theme.clone();
        let anchor = self.tool_notification_anchor(ctx);
        for ev in self
            .notifications
            .show_balloons_at(ctx, &theme, notification_clock(), anchor)
        {
            self.handle_notification_event(ev, ctx);
        }
    }

    /// The Notifications tool window.
    pub(super) fn notifications_view(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let ctx = ui.ctx().clone();
        for ev in self.notifications.tool_window(ui, &theme) {
            self.handle_notification_event(ev, &ctx);
        }
    }

    pub(super) fn handle_notification_event(&mut self, ev: NotificationEvent, ctx: &egui::Context) {
        match ev {
            NotificationEvent::Action(NotificationAction::RetrySetupWorkspace) => {
                self.retry_setup_workspace_choice();
            }
            NotificationEvent::Action(NotificationAction::GitLog { root, revision }) => {
                self.git_panel.reveal_log_root(root, revision);
                self.bottom = Some(BottomTool::Git);
            }
            NotificationEvent::Action(NotificationAction::GitContinuation {
                root,
                operation,
                action,
            }) => {
                if self.vcs_workspace.mappings.roots.contains(&root) {
                    match rustpy_vcs::operations_workspace::Request::continuation(
                        &operation, &action,
                    ) {
                        Ok(request) => self.git_panel.operations_workspace.queue.push_back(
                            crate::git_jobs::GitOp::BranchNetwork {
                                root,
                                label: request.label,
                                args: request.args,
                            },
                        ),
                        Err(error) => self.set_status(error.to_string()),
                    }
                } else {
                    self.set_status("Open the operation's project before continuing it");
                }
            }
            NotificationEvent::Action(NotificationAction::OpenFile(path, line)) => match line {
                Some(l) => self.goto_file_line(&path, l),
                None => self.open_file(&path),
            },
            NotificationEvent::Action(NotificationAction::RestoreBranch { root, name, oid }) => {
                self.handle_branch_workspace_event(crate::git_panel::branch_workspace::Event {
                    root,
                    command: crate::git_panel::branch_workspace::Command::Local(
                        rustpy_vcs::branches::LocalAction::Restore { branch: name, oid },
                    ),
                })
            }
            NotificationEvent::Action(NotificationAction::RunAction(a)) => {
                self.handle_action(a, ctx)
            }
            NotificationEvent::Action(NotificationAction::OpenSettings(page)) => {
                self.open_settings_page(page)
            }
            NotificationEvent::Action(NotificationAction::InstallRequirements(path)) => {
                let Some(interpreter) = self
                    .project
                    .as_ref()
                    .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()))
                else {
                    self.notify_no_interpreter();
                    return;
                };
                self.packages_panel.interpreter = Some(interpreter);
                if let Some(e) = self.packages_panel.install_requirements(&path) {
                    self.notify_error("Python Packages", "Cannot install requirements", e);
                }
                self.bottom = Some(BottomTool::Packages);
            }
            NotificationEvent::Action(NotificationAction::ShowToolWindow(title)) => {
                if let Some(tool) = layout::bottom_tool_by_title(title) {
                    self.bottom = Some(tool);
                }
            }
            NotificationEvent::DisableGroup(g) => {
                self.change_setting_now(|s| {
                    s.notifications
                        .set_display(&g, NotificationDisplay::NoPopup)
                });
                self.set_status(format!(
                    "\"{g}\" notifications now only go to the Notifications tool window"
                ));
            }
            NotificationEvent::EnableGroup(g) => self.change_setting_now(|s| {
                s.notifications.set_display(
                    &g,
                    rustpy_settings::NotificationSettings::default_group(&g).display,
                )
            }),
        }
    }

    /// Settings › Appearance & Behavior › Notifications: the global switches and,
    /// per group, the popup type and whether it is logged.
    pub(super) fn page_notifications(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.heading(crate::i18n::tr_static("Notifications"));
        let n = &mut self.settings.notifications;
        ui.checkbox(
            &mut n.display_balloons,
            crate::i18n::tr_static("Display balloon notifications"),
        );
        ui.checkbox(
            &mut n.system_notifications,
            crate::i18n::tr_static("Enable system notifications"),
        )
        .on_hover_text(crate::i18n::tr_static(
            "Also show notifications on the desktop while the IDE window is not focused",
        ));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(crate::i18n::tr_static("Popup type: Balloon expires, Sticky balloon stays until closed, Tool window balloon shows next to the related tool window's button, No popup only logs. Log keeps the notification in View › Tool Windows › Notifications.")).color(theme.text_dim));
        ui.add_space(6.0);
        let mut groups: std::collections::BTreeSet<String> =
            KNOWN_GROUPS.iter().map(|g| g.to_string()).collect();
        groups.extend(self.notifications.groups());
        groups.extend(self.settings.notifications.groups.keys().cloned());
        let enabled = self.settings.notifications.display_balloons;
        egui::Grid::new("notification_groups")
            .num_columns(3)
            .striped(true)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new(crate::i18n::tr_static("Group")).strong());
                ui.label(egui::RichText::new(crate::i18n::tr_static("Popup")).strong());
                ui.label(egui::RichText::new(crate::i18n::tr_static("Log")).strong());
                ui.end_row();
                for g in groups {
                    let n = &mut self.settings.notifications;
                    let mut row = n.group(&g);
                    let group_label = ui.label(crate::i18n::tr(&g));
                    ui.add_enabled_ui(enabled, |ui| {
                        egui::ComboBox::from_id_salt(("notification_display", &g))
                            .selected_text(crate::i18n::tr_static(row.display.label()))
                            .show_ui(ui, |ui| {
                                for d in NotificationDisplay::ALL {
                                    ui.selectable_value(
                                        &mut row.display,
                                        d,
                                        crate::i18n::tr_static(d.label()),
                                    );
                                }
                            })
                            .combo_labelled(&group_label);
                    });
                    ui.checkbox(&mut row.log, "");
                    ui.end_row();
                    if row != n.group(&g) {
                        n.set_group(&g, row);
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_told_apart_from_results() {
        assert!(is_failure_message("Push failed: authentication required"));
        assert!(is_failure_message(
            "docker ps: Cannot connect to the Docker daemon"
        ));
        assert!(is_failure_message("fatal: not a git repository"));
        assert!(
            is_failure_message("the index is locked; class=Index (10); code=Locked (-14)"),
            "libgit2 errors"
        );
        assert!(!is_failure_message("Pushed: main -> main"));
        assert!(!is_failure_message("docker ps: done"));
        assert!(!is_failure_message("Committed 2 file(s) as 1a2b3c4"));
        assert!(
            !is_failure_message("Pull: error_handler.py | 2 +-"),
            "a successful pull listing a file named error…"
        );
        assert_eq!(background_group("Push failed: x"), "Git");
        assert_eq!(background_group("docker images: done"), "Docker");
        assert_eq!(background_group("Indexing finished"), "Background Tasks");
    }

    #[test]
    fn messages_split_into_title_and_body() {
        assert_eq!(
            split_message("Push failed: authentication required: remote: denied"),
            (
                "Push failed".to_string(),
                "authentication required: remote: denied".to_string()
            )
        );
        assert_eq!(
            split_message(
                "git pull --ff-only failed:\nfatal: Not possible to fast-forward, aborting.\n"
            ),
            (
                "git pull --ff-only failed".to_string(),
                "fatal: Not possible to fast-forward, aborting.".to_string()
            )
        );
        assert_eq!(
            split_message("Cannot create repository"),
            ("Cannot create repository".to_string(), String::new())
        );
        let long = format!("{}: tail", "x".repeat(80));
        assert_eq!(
            split_message(&long).0,
            long,
            "a long first part is not a title"
        );
    }

    #[test]
    fn status_bar_echo_stays_short() {
        assert_eq!(
            status_text("Push failed", "\n  rejected  \nmore"),
            "Push failed: rejected"
        );
        assert_eq!(status_text("Pushed", ""), "Pushed");
        let echo = status_text("Settings not loaded", &"/very/long/path".repeat(20));
        assert_eq!(echo.chars().count(), 90);
        assert!(echo.ends_with('…'));
    }
}

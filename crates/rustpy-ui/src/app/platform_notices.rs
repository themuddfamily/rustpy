//! Platform limits and fallbacks reported after startup: software rendering
//! (see `launch::render_fallback`) and the Linux inotify watch limit, with
//! "Show instructions" for raising it.
use super::*;
use crate::a11y::DialogWindow;
use crate::notifications::{NotificationAction, NotificationKind};

/// The kernel setting that bounds the file watcher on Linux.
const MAX_USER_WATCHES: &str = "/proc/sys/fs/inotify/max_user_watches";
/// The limit the instructions suggest.
const SUGGESTED_WATCHES: u64 = 524_288;
/// Drop-in file for a permanent limit.
const SYSCTL_CONF: &str = "/etc/sysctl.d/60-rustpy.conf";

#[derive(Default)]
pub(super) struct State {
    /// Software rendering is active; notified on the first frames.
    software_rendering: Option<crate::launch::SoftwareReason>,
    /// The inotify instructions window (the current limit when it opened).
    instructions: Option<Option<u64>>,
}

/// The command that raises the limit until the next reboot.
fn sysctl_command() -> String {
    format!("sudo sysctl fs.inotify.max_user_watches={SUGGESTED_WATCHES}")
}

/// The contents of [`SYSCTL_CONF`].
fn sysctl_conf_snippet() -> String {
    format!("fs.inotify.max_user_watches = {SUGGESTED_WATCHES}")
}

/// `fs.inotify.max_user_watches` from `/proc` (None when unreadable).
fn read_max_user_watches(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The instructions text for the current limit.
fn inotify_instructions(current: Option<u64>) -> String {
    let current = match current {
        Some(n) => format!("The current limit is fs.inotify.max_user_watches = {n}."),
        None => format!("The current limit could not be read from {MAX_USER_WATCHES}."),
    };
    format!(
        "RustPy watches project folders with inotify, and the system limit on watched folders was reached. \
         {current}\n\n\
         To raise the limit until the next reboot, run:\n    {}\n\n\
         To raise it permanently, create {SYSCTL_CONF} with the line:\n    {}\n\
         then apply it with:\n    sudo sysctl --system\n\n\
         Reopen the project afterwards so its folders are watched again.",
        sysctl_command(),
        sysctl_conf_snippet(),
    )
}

impl IdeApp {
    /// Startup ran with software rendering; a notification follows on the first frames.
    pub(crate) fn note_software_rendering(&mut self, reason: crate::launch::SoftwareReason) {
        self.platform_notices.software_rendering = Some(reason);
    }

    /// Notification actions for the inotify watch limit warning.
    pub(super) fn inotify_limit_actions() -> Vec<(String, NotificationAction)> {
        let mut actions = Vec::new();
        if cfg!(target_os = "linux") {
            actions.push((
                "Show instructions".to_string(),
                NotificationAction::RunAction(Action::ShowInotifyLimitInstructions),
            ));
        }
        actions.push((
            "Don't show again".to_string(),
            NotificationAction::RunAction(Action::HideInotifyLimitWarning),
        ));
        actions
    }

    pub(super) fn show_inotify_instructions(&mut self) {
        self.platform_notices.instructions =
            Some(read_max_user_watches(Path::new(MAX_USER_WATCHES)));
    }

    pub(super) fn hide_inotify_limit_warning(&mut self) {
        self.settings.platform.warn_inotify_limit = false;
        let _ = self.settings.save();
        self.set_status("The file watcher limit warning is off (Settings › Appearance › System)");
    }

    /// Settings › Appearance › System: the renderer and the watch limit warning.
    pub(super) fn platform_settings(&mut self, ui: &mut egui::Ui) {
        use crate::i18n::tr_static as tr;
        ui.heading(tr("System"));
        let p = &mut self.settings.platform;
        ui.checkbox(&mut p.software_rendering, tr("Use software rendering"))
            .on_hover_text(tr("Draw with the CPU instead of the GPU driver (also rustpy --software-rendering). Used automatically when the GPU's OpenGL context fails at startup."));
        ui.small(tr("Takes effect after a restart."));
        ui.checkbox(
            &mut p.warn_inotify_limit,
            tr("Warn when the file watcher limit is reached"),
        );
    }

    pub(super) fn platform_notices_tick(&mut self, ctx: &egui::Context) {
        if self.frame_count >= 2 {
            if let Some(reason) = self.platform_notices.software_rendering.take() {
                self.notify(
                    NotificationKind::Info,
                    "Project Setup",
                    "Software rendering is active",
                    reason.message(),
                    vec![(
                        "Appearance Settings".to_string(),
                        NotificationAction::OpenSettings("appearance"),
                    )],
                );
            }
        }
        let Some(current) = self.platform_notices.instructions else {
            return;
        };
        let mut open = true;
        egui::Window::new(crate::i18n::tr_static("Inotify Watches Limit"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show_modal(ctx, |ui| {
                ui.label(inotify_instructions(current));
                ui.separator();
                for (label, text) in [
                    ("Until reboot", sysctl_command()),
                    (SYSCTL_CONF, sysctl_conf_snippet()),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static(label));
                        ui.code(&text);
                        if ui.button(crate::i18n::tr_static("Copy")).clicked() {
                            ctx.copy_text(text.clone());
                        }
                    });
                }
            });
        if !open {
            self.platform_notices.instructions = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_show_the_current_limit_and_both_fixes() {
        let text = inotify_instructions(Some(8192));
        assert!(
            text.contains("fs.inotify.max_user_watches = 8192"),
            "{text}"
        );
        assert!(
            text.contains("sudo sysctl fs.inotify.max_user_watches=524288"),
            "{text}"
        );
        assert!(text.contains("/etc/sysctl.d/60-rustpy.conf"), "{text}");
        assert!(
            text.contains("fs.inotify.max_user_watches = 524288"),
            "{text}"
        );
        let unknown = inotify_instructions(None);
        assert!(unknown.contains("could not be read"), "{unknown}");
        assert_eq!(
            sysctl_conf_snippet(),
            "fs.inotify.max_user_watches = 524288"
        );
    }

    #[test]
    fn current_limit_is_read_from_proc_format() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("max_user_watches");
        std::fs::write(&file, "65536\n").unwrap();
        assert_eq!(read_max_user_watches(&file), Some(65536));
        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(read_max_user_watches(&file), None);
        assert_eq!(read_max_user_watches(&dir.path().join("missing")), None);
    }

    #[test]
    fn watch_limit_warning_offers_dont_show_again() {
        let actions = IdeApp::inotify_limit_actions();
        assert!(actions
            .iter()
            .any(|(label, action)| label == "Don't show again"
                && *action == NotificationAction::RunAction(Action::HideInotifyLimitWarning)));
        assert_eq!(
            actions
                .iter()
                .any(|(label, _)| label == "Show instructions"),
            cfg!(target_os = "linux")
        );
    }
}

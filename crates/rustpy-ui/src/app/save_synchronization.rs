//! Idle/deactivation scheduling. Automatic saves use the same prepared save and
//! disk-revision checks as explicit Save, without opening Save As or permissions
//! prompts for unnamed/protected editors.
use super::*;
use rustpy_settings::save_synchronization::SaveSynchronizationSettings;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    id: rustpy_text::DocumentId,
    version: u64,
    path: Option<PathBuf>,
    modified: bool,
    readonly: bool,
    encoding: rustpy_text::Encoding,
    ending: rustpy_text::LineEnding,
    scopes: usize,
    notebook_dirty: bool,
    notebook_busy: bool,
    revision: Option<Option<rustpy_core::vfs::ContentRevision>>,
}
fn stamps(app: &IdeApp) -> Vec<Stamp> {
    app.tabs
        .iter()
        .map(|tab| Stamp {
            id: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            modified: tab.state.doc.is_modified(),
            readonly: tab.state.doc.is_read_only(),
            encoding: tab.state.doc.encoding(),
            ending: tab.state.doc.line_ending(),
            scopes: tab.state.doc.transaction_depth(),
            notebook_dirty: tab.notebook.as_ref().is_some_and(|nb| nb.is_dirty()),
            notebook_busy: tab.notebook.as_ref().is_some_and(|nb| nb.is_busy()),
            revision: tab
                .state
                .path
                .as_ref()
                .and_then(|path| app.disk_revisions.get(path))
                .copied(),
        })
        .collect()
}
#[derive(Default)]
pub(super) struct SaveSynchronization {
    pub(super) automatic: bool,
    last_activity: Option<f64>,
    focused: bool,
    active: Option<rustpy_text::DocumentId>,
    observed: Vec<Stamp>,
    attempted: Option<Vec<Stamp>>,
    deactivated: bool,
    settings: Option<SaveSynchronizationSettings>,
    synchronization_pending: bool,
    input_seen: bool,
}
impl SaveSynchronization {
    /// Capture before shortcut/text widgets consume events. Document versions
    /// cannot reveal navigation, selection or an already-dirty notebook edit.
    pub(super) fn note_input(&mut self, raw: &egui::RawInput) {
        self.input_seen |= raw.events.iter().any(|event| {
            !matches!(
                event,
                egui::Event::WindowFocused(_) | egui::Event::Screenshot { .. }
            )
        });
    }
    fn observe(
        &mut self,
        now: f64,
        focused: bool,
        input: bool,
        active: Option<rustpy_text::DocumentId>,
        current: &[Stamp],
        settings: &SaveSynchronizationSettings,
    ) -> (bool, bool) {
        let initialized = self.last_activity.is_some();
        let synchronize = initialized
            && ((settings.synchronize_on_frame_activation && focused && !self.focused)
                || (settings.synchronize_on_tab_activation && active != self.active));
        if initialized && self.focused && !focused && settings.save_on_deactivation {
            self.deactivated = true;
        }
        if !settings.save_on_deactivation {
            self.deactivated = false;
        }
        self.synchronization_pending |= synchronize;
        if !initialized
            || input
            || self.observed != current
            || self.settings.as_ref() != Some(settings)
            || self.last_activity.is_some_and(|last| now < last)
        {
            self.last_activity = Some(now);
            self.attempted = None;
        }
        self.settings = Some(settings.clone());
        self.focused = focused;
        self.active = active;
        self.observed = current.to_vec();
        let idle = settings.save_when_idle
            && now - self.last_activity.unwrap_or(now) >= settings.idle_delay().as_secs_f64();
        (
            (self.deactivated || idle) && self.attempted.as_deref() != Some(current),
            self.synchronization_pending,
        )
    }
    fn attempted(&mut self, current: Vec<Stamp>) {
        self.attempted = Some(current);
        self.deactivated = false;
    }
}

pub(super) fn backup_root() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_BACKUP_ROOT.with(|value| value.borrow().clone()) {
        return path;
    }
    Settings::system_dir().join("backups").join("files")
}
#[cfg(test)]
thread_local! {
    static TEST_BACKUP_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

impl IdeApp {
    pub(super) fn checked_editor_write(
        &self,
        path: &Path,
        bytes: &[u8],
        expected: Option<rustpy_core::vfs::ContentRevision>,
    ) -> std::io::Result<rustpy_core::vfs::ContentRevision> {
        let root = self
            .settings
            .save_synchronization
            .backup_before_save
            .then(backup_root);
        crate::editor::file_backups::checked_write(
            &self.file_vfs(),
            path,
            bytes,
            expected,
            self.editor_read_limits(),
            root.as_deref(),
        )
    }
    pub(super) fn save_synchronization_tick(&mut self, ctx: &egui::Context) {
        let (now, focused, input) =
            ctx.input(|input| (input.time, input.focused, !input.events.is_empty()));
        let captured_input = std::mem::take(&mut self.save_synchronization.input_seen);
        let input = input || captured_input;
        let settings = self.settings.save_synchronization.clone();
        let current = stamps(self);
        let active = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.state.doc.identity());
        let (due, synchronize) = self
            .save_synchronization
            .observe(now, focused, input, active, &current, &settings);
        let allowed = self.editor_keyboard()
            && !self.coordinated_quit_active()
            && !self.coordinated_quit_committed;
        if synchronize && allowed {
            self.save_synchronization.synchronization_pending = false;
            self.queue_open_file_reconciliation();
            self.reconcile_external_changes();
        }
        if settings.save_when_idle
            && current
                .iter()
                .any(|stamp| stamp.modified || stamp.notebook_dirty)
            && self.save_synchronization.attempted.as_deref() != Some(current.as_slice())
        {
            let elapsed = now - self.save_synchronization.last_activity.unwrap_or(now);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (settings.idle_delay().as_secs_f64() - elapsed).max(0.1),
            ));
        }
        if !due || !allowed {
            return;
        }
        let mut failure = None;
        self.save_synchronization.automatic = true;
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            if !tab.has_unsaved_changes()
                || tab.state.path.is_none()
                || tab.image.is_some()
                || tab.state.doc.is_read_only()
                || tab.state.doc.transaction_depth() != 0
                || tab.notebook.as_ref().is_some_and(|nb| nb.is_busy())
            {
                continue;
            }
            // An attribute may have changed since the last watcher/focus event.
            if !self.refresh_file_permissions(index) || self.tabs[index].state.doc.is_read_only() {
                continue;
            }
            if !self.save_tab(index) {
                if failure.is_none() {
                    failure = self.status.clone();
                }
                // A conflict owns the existing dialog; do not attempt another
                // file behind it or replace its decision with a later one.
                if !matches!(self.dialog, Dialog::None) {
                    break;
                }
            }
        }
        self.save_synchronization.automatic = false;
        if failure.is_some() {
            self.status = failure;
        }
        let after = stamps(self);
        self.save_synchronization.attempted(after);
    }
    pub(super) fn save_synchronization_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("Saving and Synchronization");
        let settings = &mut self.settings.save_synchronization;
        ui.checkbox(
            &mut settings.save_on_deactivation,
            "Save files on frame deactivation",
        );
        ui.checkbox(
            &mut settings.save_when_idle,
            "Save files when the IDE is idle",
        );
        ui.add_enabled_ui(settings.save_when_idle, |ui| {
            ui.horizontal(|ui| {
                ui.label("Idle delay (seconds):");
                ui.add(egui::DragValue::new(&mut settings.idle_seconds).range(1..=3600));
            });
        });
        ui.checkbox(
            &mut settings.synchronize_on_frame_activation,
            "Synchronize files on frame activation",
        );
        ui.checkbox(
            &mut settings.synchronize_on_tab_activation,
            "Synchronize files on editor tab activation",
        );
        ui.checkbox(
            &mut settings.backup_before_save,
            "Back up files before saving",
        );
        ui.label("Safe write is always used for ordinary files (temporary file, then replacement). Hard links retain their shared inode.");
        ui.label(format!("Backups: {}", backup_root().display()));
        ui.small("Exact pre-save bytes are retained by source and content identity until you delete them. Open a .bak file with File › Open to inspect/recover it; use Save As to choose its destination. Existing backups are never overwritten.");
        if let Some(path) = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_ref())
        {
            ui.label(format!(
                "This file's backups: {}",
                crate::editor::file_backups::source_directory(&backup_root(), path).display()
            ));
        }
    }
}

#[cfg(test)]
#[path = "save_synchronization_tests.rs"]
mod tests;

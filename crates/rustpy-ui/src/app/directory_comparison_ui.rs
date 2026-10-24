//! App-owned directory comparison: UI markers never own worker retirement.
use super::*;
use crate::a11y::DialogWindow;

impl IdeApp {
    /// Stable frame boundaries only; dialogs() temporarily takes its marker.
    pub(super) fn poll_directory_comparison(&mut self, ctx: &egui::Context) {
        let consent = self.coordinated_quit_active() || self.exit_questions_open();
        if consent && self.directory_comparison.is_some() {
            self.directory_comparison_suspended = true;
        } else if self.directory_comparison_suspended && !self.exit_requested {
            self.restore_directory_comparison();
            self.directory_comparison_suspended = false;
        }
        if !consent && !matches!(self.dialog, Dialog::DirectoryComparison) {
            self.transfer_directory_comparison();
        }
        if let Some(state) = &mut self.directory_comparison {
            state.poll();
        }
        self.directory_diff_draining
            .retain_mut(|job| !job.drained());
        if self.directory_comparisons_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    pub(super) fn restore_directory_comparison(&mut self) {
        if self.directory_comparison.is_some() && matches!(self.dialog, Dialog::None) {
            self.dialog = Dialog::DirectoryComparison;
        }
    }

    pub(super) fn transfer_directory_comparison(&mut self) {
        if let Some(mut state) = self.directory_comparison.take() {
            if let Some(job) = state.take_job_for_drain() {
                self.directory_diff_draining.push(job);
            }
        }
        self.directory_comparison_suspended = false;
    }

    pub(super) fn cancel_directory_comparisons(&mut self) {
        self.cancel_diff_workspace_jobs();
        self.directory_diff_domain.cancel();
        self.transfer_directory_comparison();
        for job in &self.directory_diff_draining {
            job.cancel();
        }
        self.directory_diff_domain = rustpy_core::CancellationToken::new();
    }

    pub(super) fn directory_comparisons_pending(&self) -> bool {
        self.diff_workspace_jobs_pending()
            || self
                .directory_comparison
                .as_ref()
                .is_some_and(|state| state.pending())
            || self.directory_diff_draining.iter().any(|job| job.pending())
    }

    pub(super) fn directory_comparison_stamps(&self) -> Vec<String> {
        let mut stamps: Vec<_> = self
            .directory_comparison
            .iter()
            .map(|state| state.quit_stamp())
            .collect();
        stamps.extend(
            self.directory_diff_draining
                .iter()
                .map(|job| job.quit_stamp()),
        );
        stamps.sort();
        stamps.push(self.diff_workspace_job_stamp());
        stamps
    }

    pub(super) fn open_directory_comparison_picker(&mut self, left: &Path) {
        let start = left.parent().unwrap_or(left).to_path_buf();
        self.dialog =
            Dialog::DirectoryComparePick(Box::new((FileBrowser::new(start), left.into())));
    }

    pub(super) fn start_directory_comparison(&mut self, left: PathBuf, right: PathBuf) {
        if self.settings.diff_merge.external_folders {
            if let Err(error) = self.external_diff_paths(&left, &right, true) {
                self.set_status(error);
            }
            return;
        }
        self.transfer_directory_comparison();
        self.directory_comparison = Some(Box::new(directory_diff::DirectoryDiff::with_domain(
            rustpy_core::TaskManager::global(),
            &self.directory_diff_domain,
            left,
            right,
        )));
        self.dialog = Dialog::DirectoryComparison;
    }

    pub(super) fn draw_directory_comparison_picker(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<(FileBrowser, PathBuf)>,
    ) {
        let theme = self.theme.clone();
        let mut open = true;
        let mut cancel = false;
        let mut chosen = None;
        egui::Window::new("Compare Directory with…")
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 460.0])
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.add(egui::Label::new(format!("Left: {}", state.1.display())).truncate())
                    .on_hover_text(state.1.display().to_string());
                // Double-click navigates; explicit Compare chooses the directory.
                state.0.ui(ui, &theme, false, 360.0);
                ui.horizontal(|ui| {
                    if ui.button("Compare directory").clicked() {
                        chosen = Some(
                            state
                                .0
                                .selected
                                .clone()
                                .unwrap_or_else(|| state.0.dir.clone()),
                        );
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                cancel |= ui.input(|input| input.key_pressed(egui::Key::Escape));
            });
        if let Some(right) = chosen {
            if right.is_dir() {
                self.start_directory_comparison(state.1.clone(), right);
            } else {
                self.set_status("Choose an existing directory");
                self.dialog = Dialog::DirectoryComparePick(state);
            }
        } else if open && !cancel {
            self.dialog = Dialog::DirectoryComparePick(state);
        } else {
            self.focus_editor = true;
        }
    }

    pub(super) fn draw_directory_comparison(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.directory_comparison.take() else {
            return;
        };
        state.set_unsaved(
            self.tabs
                .iter()
                .filter(|tab| tab.has_unsaved_changes())
                .filter_map(|tab| tab.state.path.clone())
                .collect(),
        );
        let mut open = true;
        let mut pair = None;
        egui::Window::new("Compare directories")
            .open(&mut open)
            .show_modal(ctx, |ui| {
                pair = state.show(ui);
            });
        if let Some(current) = &pair {
            self.set_diff_file_sequence(state.file_pairs(), current);
        }
        self.directory_comparison = Some(state);
        self.dialog = Dialog::DirectoryComparison;
        if !open || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::None;
            self.focus_editor = true;
        } else if let Some((left, right)) = pair {
            match self.compare_paths_workspace(&left, &right) {
                Ok(()) => {}
                Err(error) => self.set_status(error),
            }
        }
    }
}

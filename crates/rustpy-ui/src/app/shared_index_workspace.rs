//! Global cache transfer ownership, independent of the currently selected project.
use super::*;
use crate::shared_index_download::{self, Ready, Request};
use rustpy_core::{TaskHandle, TaskManager};
#[derive(Default)]
pub(super) struct State {
    job: Option<TaskHandle<Ready>>,
    host: String,
    pub notice: String,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}
impl IdeApp {
    pub(super) fn shared_index_download_ui(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.strong("Download prebuilt standard-library and package indexes");
        ui.label("Use a publisher's .rpidx URL and SHA-256. Installed bundles are reused only for matching local source content; unmatched files are indexed normally.");
        let busy = self.shared_index_download.job.is_some();
        let mut start = false;
        let mut cancel = false;
        ui.add_enabled_ui(!busy, |ui| {
            let prefs = &mut self.settings.shared_indexes;
            ui.horizontal(|ui| {
                ui.label("HTTPS URL");
                ui.add(egui::TextEdit::singleline(&mut prefs.url).desired_width(400.0));
            });
            ui.horizontal(|ui| {
                ui.label("SHA-256");
                ui.add(egui::TextEdit::singleline(&mut prefs.sha256).desired_width(400.0));
            });
            ui.horizontal(|ui| {
                ui.label("Maximum download (MiB)");
                ui.add(egui::DragValue::new(&mut prefs.max_download_mib).range(1..=256));
            });
            start = ui.button("Download and install verified bundle").clicked();
        });
        if let Some(job) = &self.shared_index_download.job {
            let (_, details, fraction) = job.progress();
            ui.label(format!(
                "Downloading from {}",
                self.shared_index_download.host
            ));
            ui.label(details);
            if let Some(fraction) = fraction {
                ui.add(egui::ProgressBar::new(fraction));
            }
            cancel = ui
                .add_enabled(!job.is_cancelled(), egui::Button::new("Cancel download"))
                .clicked();
        }
        if !self.shared_index_download.notice.is_empty() {
            ui.label(&self.shared_index_download.notice);
        }
        if start {
            self.start_shared_index_download();
        }
        if cancel {
            self.stop_shared_index_download(None);
        }
    }
    fn start_shared_index_download(&mut self) {
        if self.shared_index_download.job.is_some() {
            return;
        }
        let prefs = &self.settings.shared_indexes;
        let request = Request {
            url: prefs.url.trim().into(),
            sha256: prefs.sha256.trim().into(),
            limit: u64::from(prefs.max_download_mib) * 1024 * 1024,
        };
        if let Err(error) = request.validate() {
            self.shared_index_download.notice = error;
            return;
        }
        let host = request.host();
        let store = self.symbol_indexes.store().clone();
        self.shared_index_download.job = Some(
            TaskManager::global()
                .task(format!("Download shared index from {host}"))
                .spawn(move |progress| shared_index_download::fetch(request, store, progress)),
        );
        self.shared_index_download.host = host;
        self.shared_index_download.notice.clear();
    }
    pub(super) fn poll_shared_index_download(&mut self, ctx: &egui::Context) {
        let result = self
            .shared_index_download
            .job
            .as_mut()
            .and_then(TaskHandle::try_result);
        if let Some(result) = result {
            let job = self
                .shared_index_download
                .job
                .take()
                .expect("completed download owner");
            if job.is_cancelled() {
                drop(result);
                self.shared_index_download.notice =
                    "Download cancelled; staged data was discarded".into();
            } else {
                self.shared_index_download.notice = match result {
                    Ok(ready) => match ready.stage.finish() {
                        Ok(bytes) => {
                            self.symbol_indexes.shared_bundles_changed();
                            format!(
                                "Installed {} ({} files, {} bytes) at {}",
                                ready.label,
                                ready.files,
                                bytes,
                                ready.path.display()
                            )
                        }
                        Err(error) => format!("Cannot publish shared index: {error}"),
                    },
                    Err(error) => format!("Shared-index download failed: {error}"),
                };
            }
            ctx.request_repaint();
        }
        if self.shared_index_download.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    pub(super) fn shared_index_download_owner(&self) -> Option<(u64, String)> {
        self.shared_index_download
            .job
            .as_ref()
            .map(|job| (job.id(), self.shared_index_download.host.clone()))
    }
    pub(super) fn shared_index_download_stamp(&self) -> String {
        format!(
            "{:?}",
            self.shared_index_download
                .job
                .as_ref()
                .map(|job| (job.id(), job.is_cancelled()))
        )
    }
    pub(super) fn stop_shared_index_download(&mut self, id: Option<u64>) {
        if let Some(job) = self
            .shared_index_download
            .job
            .as_ref()
            .filter(|job| id.is_none_or(|id| id == job.id()))
        {
            job.cancel();
        }
    }
}

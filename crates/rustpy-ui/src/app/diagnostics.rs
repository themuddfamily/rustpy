//! Explicit local diagnostic exports. Collection stays off the UI thread.

use rustpy_cli::diagnostics::BundleReport;
use rustpy_core::{TaskError, TaskHandle, TaskManager};

use super::IdeApp;
use crate::actions::Action;
use crate::notifications::{NotificationAction, NotificationKind};

#[derive(Default)]
pub(super) struct DiagnosticExport {
    job: Option<TaskHandle<BundleReport>>,
}

impl DiagnosticExport {
    pub(super) fn stamp(&self) -> Option<u64> {
        self.job.as_ref().map(TaskHandle::id)
    }
    /// No UI publication while final shutdown drains this noncancellable export.
    pub(super) fn retire(&mut self) -> bool {
        if let Some(result) = self.job.as_mut().and_then(TaskHandle::try_result) {
            if let Err(error) = result {
                log::warn!("Diagnostic export failed during shutdown: {error}");
            }
            self.job = None;
        }
        self.job.is_some()
    }
}

impl IdeApp {
    pub(super) fn collect_diagnostics(&mut self) {
        if self.diagnostic_export.job.is_some() {
            self.set_status("Diagnostic collection is already running");
            return;
        }
        let log_dir = rustpy_settings::Settings::log_dir();
        self.diagnostic_export.job = Some(
            TaskManager::global()
                .task("Collect Diagnostic Data")
                .cancellable(false)
                .spawn(move |progress| {
                    progress.set_text("Collecting logs and crash reports");
                    rustpy_cli::diagnostics::collect(&log_dir, None)
                        .map_err(|error| TaskError::Failed(error.to_string()))
                }),
        );
        self.set_status("Collecting diagnostic data…");
    }

    pub(super) fn poll_diagnostic_export(&mut self) {
        let Some(result) = self
            .diagnostic_export
            .job
            .as_mut()
            .and_then(TaskHandle::try_result)
        else {
            return;
        };
        self.diagnostic_export.job = None;
        match result {
            Ok(report) => {
                for issue in &report.issues {
                    log::warn!("diagnostic collection: {}: {}", issue.name, issue.reason);
                }
                let summary = format!(
                    "Diagnostic data saved to {} ({} files, {} collection notices)",
                    report.path.display(),
                    report.collected_files,
                    report.issues.len(),
                );
                self.notify(
                    if report.issues.is_empty() {
                        NotificationKind::Info
                    } else {
                        NotificationKind::Warning
                    },
                    "Background Tasks",
                    "Diagnostic data collected",
                    &summary,
                    vec![(
                        "Show Log Folder".into(),
                        NotificationAction::RunAction(Action::ShowLogInFiles),
                    )],
                );
                self.set_status(format!(
                    "Diagnostic data collected ({} files, {} notices)",
                    report.collected_files,
                    report.issues.len(),
                ));
            }
            Err(error) => {
                let summary = format!("Diagnostic collection failed: {error}");
                self.notify_error("Background Tasks", "Diagnostic collection failed", &summary);
                self.set_status(summary);
            }
        }
    }
}

//! Selectable inventory. File rows open through the existing live-buffer-aware diff route.
use super::inventory::{Difference, Report};
use super::job::DirectoryJob;
use crate::a11y::AccessibleCombo;
use rustpy_core::{CancellationToken, TaskManager};
use std::path::PathBuf;

pub(crate) struct DirectoryDiff {
    pub left: PathBuf,
    pub right: PathBuf,
    job: Option<DirectoryJob>,
    report: Option<Report>,
    selected: Option<usize>,
    failed: Option<String>,
    filter: usize,
    unsaved: std::collections::HashSet<PathBuf>,
    copy: Option<super::synchronize::CopyPlan>,
    copy_error: Option<String>,
    owner: CancellationToken,
}
impl DirectoryDiff {
    pub(crate) fn with_domain(
        manager: &TaskManager,
        owner: &CancellationToken,
        left: PathBuf,
        right: PathBuf,
    ) -> Self {
        let job = DirectoryJob::spawn(manager, owner, left.clone(), right.clone());
        Self {
            left,
            right,
            job: Some(job),
            report: None,
            selected: None,
            failed: None,
            filter: 0,
            unsaved: Default::default(),
            copy: None,
            copy_error: None,
            owner: owner.clone(),
        }
    }
    pub(crate) fn set_unsaved(&mut self, paths: std::collections::HashSet<PathBuf>) {
        self.unsaved = paths;
    }
    pub(crate) fn file_pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        self.report
            .as_ref()
            .map(|report| {
                report
                    .entries
                    .iter()
                    .filter(|entry| entry.paired_files)
                    .map(|entry| {
                        (
                            report.left.join(&entry.relative),
                            report.right.join(&entry.relative),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(crate) fn pending(&self) -> bool {
        self.job.as_ref().is_some_and(DirectoryJob::pending)
    }
    /// Caller retains this exact job in its App-owned drain list on dialog close.
    pub(crate) fn take_job_for_drain(&mut self) -> Option<DirectoryJob> {
        self.job.take().inspect(|job| job.cancel())
    }
    pub(crate) fn quit_stamp(&self) -> String {
        self.job
            .as_ref()
            .map(DirectoryJob::quit_stamp)
            .unwrap_or_else(|| "directory-task:none".into())
    }
    pub(crate) fn poll(&mut self) {
        if self.report.is_none() && self.failed.is_none() {
            if let Some(result) = self.job.as_mut().and_then(DirectoryJob::take_result) {
                match result {
                    Ok(report) => self.report = Some(report),
                    Err(error) => {
                        self.failed = Some(format!(
                            "Directory comparison failed; inventory incomplete: {error}"
                        ))
                    }
                }
            }
        }
    }
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Option<(PathBuf, PathBuf)> {
        self.poll();
        ui.label(format!("Left: {}", self.left.display()));
        ui.label(format!("Right: {}", self.right.display()));
        if let Some(error) = &self.failed {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return None;
        }
        let Some(report) = &self.report else {
            ui.label("Comparing directories…");
            if ui.button("Cancel comparison").clicked() {
                if let Some(job) = &self.job {
                    job.cancel();
                }
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
            return None;
        };
        if !report.complete {
            ui.colored_label(ui.visuals().warn_fg_color,"Incomplete comparison: errors, cancellation or limits occurred. Missing entries are not evidence of equality.");
        }
        ui.label("Left-only entries are removed; right-only entries are added. Symlinks are not followed.");
        egui::ComboBox::from_id_salt("directory-diff-filter")
            .selected_text(
                ["All", "Equal", "Different", "New left", "New right"][self.filter.min(4)],
            )
            .show_ui(ui, |ui| {
                for (index, label) in ["All", "Equal", "Different", "New left", "New right"]
                    .iter()
                    .enumerate()
                {
                    if ui
                        .selectable_value(&mut self.filter, index, *label)
                        .changed()
                    {
                        self.selected = None;
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Filter"));
        egui::ScrollArea::vertical()
            .max_height(400.0)
            .show(ui, |ui| {
                for (index, entry) in report.entries.iter().enumerate() {
                    let show = match self.filter {
                        1 => matches!(
                            entry.difference,
                            Difference::SameFile | Difference::SameDirectory
                        ),
                        2 => !matches!(
                            entry.difference,
                            Difference::SameFile | Difference::SameDirectory
                        ),
                        3 => matches!(entry.difference, Difference::Removed),
                        4 => matches!(entry.difference, Difference::Added),
                        _ => true,
                    };
                    if !show {
                        continue;
                    }
                    let status = match &entry.difference {
                        Difference::SameFile => "Same file".into(),
                        Difference::SameDirectory => "Directory".into(),
                        Difference::Added => "Added".into(),
                        Difference::Removed => "Removed".into(),
                        Difference::Changed => "Changed".into(),
                        Difference::TypeConflict => "Type conflict".into(),
                        Difference::SymlinkSkipped => "Symlink skipped (content unknown)".into(),
                        Difference::Error(error) => format!("Error: {error}"),
                    };
                    if ui
                        .selectable_label(
                            self.selected == Some(index),
                            format!("{} — {status}", entry.relative.display()),
                        )
                        .clicked()
                    {
                        self.selected = Some(index);
                    }
                }
            });
        let entry = self.selected.and_then(|index| report.entries.get(index));
        let allowed = entry.is_some_and(|entry| {
            entry.paired_files
                && matches!(entry.difference, Difference::SameFile | Difference::Changed)
        });
        if ui.add_enabled(allowed,egui::Button::new("Compare current files")).on_hover_text("Capture the current files, including unsaved editor buffers, using the existing file comparison.").clicked(){let entry=entry.unwrap();return Some((report.left.join(&entry.relative),report.right.join(&entry.relative)));}
        if let Some(entry) = entry.filter(|entry| {
            matches!(
                entry.difference,
                Difference::SameFile
                    | Difference::Changed
                    | Difference::Added
                    | Difference::Removed
            )
        }) {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        report.left.join(&entry.relative).is_file()
                            && !self.unsaved.contains(&report.left.join(&entry.relative))
                            && !self.unsaved.contains(&report.right.join(&entry.relative)),
                        egui::Button::new("Copy left → right…"),
                    )
                    .clicked()
                {
                    match super::synchronize::CopyPlan::capture(
                        &report.left,
                        &report.right,
                        &entry.relative,
                    ) {
                        Ok(plan) => self.copy = Some(plan),
                        Err(error) => self.copy_error = Some(error),
                    }
                }
                if ui
                    .add_enabled(
                        report.right.join(&entry.relative).is_file()
                            && !self.unsaved.contains(&report.left.join(&entry.relative))
                            && !self.unsaved.contains(&report.right.join(&entry.relative)),
                        egui::Button::new("Copy right → left…"),
                    )
                    .clicked()
                {
                    match super::synchronize::CopyPlan::capture(
                        &report.right,
                        &report.left,
                        &entry.relative,
                    ) {
                        Ok(plan) => self.copy = Some(plan),
                        Err(error) => self.copy_error = Some(error),
                    }
                }
            });
        }
        let mut copied = false;
        if let Some(plan) = &self.copy {
            ui.group(|ui| {
                ui.label(format!(
                    "Copy {} to {}? Existing destination content will be replaced.",
                    plan.source.display(),
                    plan.destination.display()
                ));
                if ui
                    .add_enabled(
                        !self.unsaved.contains(&plan.source)
                            && !self.unsaved.contains(&plan.destination),
                        egui::Button::new("Confirm synchronization"),
                    )
                    .on_disabled_hover_text(
                        "Save affected editor buffers before synchronizing files",
                    )
                    .clicked()
                {
                    match plan.apply() {
                        Ok(()) => copied = true,
                        Err(error) => self.copy_error = Some(error),
                    }
                }
            });
            if ui.button("Cancel synchronization").clicked() {
                self.copy = None;
            }
        }
        if let Some(error) = &self.copy_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if copied {
            self.copy = None;
            self.report = None;
            self.job = Some(DirectoryJob::spawn(
                TaskManager::global(),
                &self.owner,
                self.left.clone(),
                self.right.clone(),
            ));
        }
        None
    }
}
impl Drop for DirectoryDiff {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn actual_selectable_unicode_row_opens_only_existing_paired_file_operands() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let name = "λ🙂.py";
        std::fs::write(left.path().join(name), "before\n").unwrap();
        std::fs::write(right.path().join(name), "after\n").unwrap();
        std::fs::write(right.path().join("added.py"), "new\n").unwrap();
        let owner = CancellationToken::new();
        let mut panel = DirectoryDiff::with_domain(
            TaskManager::global(),
            &owner,
            left.path().into(),
            right.path().into(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while panel.report.is_none() {
            panel.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(panel.report.as_ref().unwrap().complete);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut frame = |time, events| {
            let mut picked = None;
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(700.0, 500.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| picked = panel.show(ui));
                },
            );
            let tree = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            (tree, picked)
        };
        let point = |tree: &egui::accesskit::TreeUpdate, label: &str| {
            let bounds = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .expect("actual named selectable control")
                .1
                .bounds()
                .unwrap();
            egui::Pos2::new(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            )
        };
        let event = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let (tree, _) = frame(0.0, vec![]);
        let row = point(&tree, &format!("{name} — Changed"));
        frame(0.1, vec![egui::Event::PointerMoved(row), event(row, true)]);
        let (tree, picked) = frame(0.2, vec![event(row, false)]);
        assert!(picked.is_none());
        let button = point(&tree, "Compare current files");
        frame(
            0.3,
            vec![egui::Event::PointerMoved(button), event(button, true)],
        );
        let (_, picked) = frame(0.4, vec![event(button, false)]);
        assert_eq!(
            picked,
            Some((left.path().join(name), right.path().join(name)))
        );
        assert_eq!(std::fs::read(left.path().join(name)).unwrap(), b"before\n");
        assert_eq!(std::fs::read(right.path().join(name)).unwrap(), b"after\n");
    }
}

//! Target-bound Safe Delete: one captured preview, one shared undoable transaction.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::vfs::{ContentRevision, ReadLimits, Vfs};
use rustpy_refactor::{
    refactor2::FileOp,
    safe_delete::{self, Plan},
};

pub(super) struct SafeDeleteState {
    plan: Plan,
    editors: Vec<(PathBuf, rustpy_text::Document, u64)>,
    deleted_tree: Vec<(PathBuf, Option<ContentRevision>)>,
}

/// Capture the destructive target too: source dependencies alone cannot detect
/// a new binary/resource or directory entry added while confirmation is open.
fn deletion_tree(operations: &[FileOp]) -> Result<Vec<(PathBuf, Option<ContentRevision>)>, String> {
    let mut pending: Vec<_> = operations
        .iter()
        .filter_map(|op| match op {
            FileOp::Delete(path) => Some(path.clone()),
            _ => None,
        })
        .collect();
    let mut result = Vec::new();
    let mut bytes = 0u64;
    let vfs = Vfs::new();
    while let Some(path) = pending.pop() {
        if result.len() + pending.len() >= 4096 {
            return Err("Safe Delete exceeds 4096 filesystem entries".into());
        }
        if path
            .ancestors()
            .any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
        {
            return Err("Safe Delete refuses symlinked paths".into());
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.permissions().readonly() {
            return Err(format!("{} is read-only", path.display()));
        }
        if metadata.is_dir() {
            result.push((path.clone(), None));
            for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                if pending.len() + result.len() >= 4096 {
                    return Err("Safe Delete exceeds 4096 filesystem entries".into());
                }
                pending.push(entry.map_err(|e| e.to_string())?.path());
            }
        } else if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or("Safe Delete size overflow")?;
            if bytes > 32 * 1024 * 1024 {
                return Err("Safe Delete exceeds the 32 MiB filesystem Undo budget".into());
            }
            let snapshot = vfs
                .read(
                    &path,
                    ReadLimits {
                        max_file_bytes: 32 * 1024 * 1024,
                        max_cached_bytes: 32 * 1024 * 1024,
                        max_cached_files: 1,
                    },
                )
                .map_err(|e| e.to_string())?;
            result.push((path, Some(snapshot.revision)));
        } else {
            return Err("Unsupported filesystem entry".into());
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}
impl IdeApp {
    pub(super) fn safe_delete_at_caret(&mut self) {
        self.sync_open_tabs_now();
        let Some(index) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        let caret = self.tabs[index].state.primary().head();
        let plan = safe_delete::symbol(&analysis, &file, caret, &self.project_files());
        self.preview_safe_delete(plan);
    }
    pub(super) fn open_tree_delete(&mut self, path: PathBuf) {
        self.sync_open_tabs_now();
        let Some(project) = self.project.as_ref() else {
            return;
        };
        // Library and content roots are never destructive Project View targets.
        if path == project.root || !path.starts_with(&project.root) {
            self.set_status("Choose a writable child of the project, not a library or root");
            return;
        }
        let plan = safe_delete::path(
            &project.analysis,
            &project.root,
            &path,
            &self.project_files(),
        );
        self.preview_safe_delete(plan);
    }
    fn preview_safe_delete(&mut self, plan: Result<Plan, String>) {
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                self.set_status(format!("Cannot safely delete: {error}"));
                return;
            }
        };
        if self
            .project
            .as_ref()
            .is_none_or(|project| !plan.target.starts_with(&project.root))
        {
            self.set_status("Safe Delete cannot modify a library declaration outside the project");
            return;
        }
        let deleted_tree = match deletion_tree(&plan.operations) {
            Ok(tree) => tree,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let editors = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state
                    .path
                    .as_ref()
                    .map(|path| (path.clone(), tab.state.doc.share(), tab.state.doc.version()))
            })
            .collect();
        self.dialog = Dialog::SafeDelete(Box::new(SafeDeleteState {
            plan,
            editors,
            deleted_tree,
        }));
    }
    pub(super) fn draw_safe_delete(&mut self, ctx: &egui::Context, state: Box<SafeDeleteState>) {
        let (mut open, mut accept, mut cancel) = (true, false, false);
        let plan = &state.plan;
        egui::Window::new("Safe Delete").default_width(650.0).default_height(450.0).open(&mut open).show_modal(ctx, |ui| {
            ui.label(format!("Delete {} · {:?}", plan.name, plan.kind));
            ui.label(format!("{} unresolved usages · {} updated calls · {} file operations", plan.blockers.len(), plan.updated_calls, plan.operations.len()));
            egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                for warning in &plan.warnings { ui.colored_label(egui::Color32::YELLOW, warning); }
                for usage in &plan.blockers {
                    ui.monospace(format!("{}:{}  {}", usage.path.display(), usage.line+1, usage.line_text.trim()));
                }
                ui.separator();
                for operation in &plan.operations {
                    match operation {
                        FileOp::Edit(path, edits) => { ui.collapsing(format!("{} ({} edits)",path.display(),edits.len()), |ui| {
                            for (range,text) in edits { ui.monospace(format!("{range:?} → {}", if text.is_empty() { "<remove>" } else { text })); }
                        }); },
                        other => { ui.monospace(format!("{other:?}")); }
                    }
                }
            });
            if plan.needs_force() { ui.label("Delete Anyway can leave unresolved references or remove argument side effects. Review every usage and warning above."); }
            ui.label("Applied as one refactoring, including filesystem Undo.");
            ui.horizontal(|ui| {
                accept = ui.button(if plan.needs_force() { "Delete Anyway" } else { "Delete" }).clicked();
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if accept {
            if self.coordinated_quit_active() {
                self.set_status("Finish the pending quit before refactoring");
                return;
            }
            self.sync_open_tabs_now();
            let result = (|| -> Result<(), String> {
                for (path, document, version) in &state.editors {
                    let Some(index) = self.tab_of(path) else {
                        return Err("Editors changed; rebuild Safe Delete".into());
                    };
                    if self.tabs[index].state.doc.identity() != document.identity()
                        || document.version() != *version
                    {
                        return Err("Editors changed; rebuild Safe Delete".into());
                    }
                }
                let analysis = self.analysis_handle().ok_or("Project closed")?;
                state
                    .plan
                    .validate_sources(&analysis, &self.project_files())?;
                if deletion_tree(&state.plan.operations)? != state.deleted_tree {
                    return Err("Deletion target changed; rebuild Safe Delete".into());
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.set_status(error);
                return;
            }
            let title = format!("Safe Delete {}", state.plan.name);
            self.apply_file_ops_failures(state.plan.operations, &title);
            self.focus_editor = true;
        } else if open && !cancel {
            self.dialog = Dialog::SafeDelete(state);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destructive_preview_detects_file_content_and_directory_membership_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("pkg");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("module.py");
        std::fs::write(&source, "before").unwrap();
        let ops = vec![FileOp::Delete(root.clone())];
        let before = deletion_tree(&ops).unwrap();
        std::fs::write(&source, "after!").unwrap();
        let changed = deletion_tree(&ops).unwrap();
        assert_ne!(before, changed);
        std::fs::create_dir(root.join("new_empty_folder")).unwrap();
        assert_ne!(changed, deletion_tree(&ops).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn destructive_preview_refuses_symlink_targets_and_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("data"), "retain").unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(deletion_tree(&[FileOp::Delete(link.clone())]).is_err());
        assert!(deletion_tree(&[FileOp::Delete(link.join("data"))]).is_err());
    }
}

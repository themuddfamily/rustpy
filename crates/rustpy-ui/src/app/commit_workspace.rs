//! App consumers keep source actions tied to the captured file, never the active editor.
use super::*;
use crate::git_panel::commit_workspace::{CheckKind, Event};
impl IdeApp {
    pub(super) fn commit_workspace_event(&mut self, event: Event) {
        match event {
            Event::Signing(signoff, sign) => {
                self.settings.vcs.preferences.sign_off = signoff;
                self.settings.vcs.preferences.sign_commits = sign;
                rustpy_vcs::policy::configure(&self.settings.vcs.preferences);
                let _ = self.settings.save();
            }
            Event::NewBranchCommit(name) => {
                let result = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                    .ok_or_else(|| "Repository unavailable".to_string())
                    .and_then(|repo| {
                        repo.create_branch(&name, true)
                            .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(()) => self.commit_from_git_panel(false, 0.0),
                    Err(error) => self.report_git_status(error),
                }
            }
            Event::History(path) => {
                if let Some(repo) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                {
                    self.git_panel.show_file_history(repo, &path);
                    self.bottom = Some(BottomTool::Git);
                }
            }
            Event::Annotate(path) => {
                self.open_file(&path);
                self.annotate_enabled = true;
                self.blame = None;
            }
            Event::Repository(path, revision) => {
                let text = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                    .map(|repo| repo.file_at_commit(&revision, &path));
                match text {
                    Some(Ok(text)) => {
                        let mut document = Document::new(&text);
                        document.set_read_only(true);
                        let virtual_path =
                            PathBuf::from(format!("<git:{revision}>/{}", path.display()));
                        let state = EditorState::new(document, Some(virtual_path));
                        self.push_tab(state);
                    }
                    Some(Err(error)) => self.set_status(error.to_string()),
                    None => self.set_status("Repository is no longer available"),
                }
            }
            Event::CheckFiles(kind) => {
                let mut paths: Vec<_> = self.git_panel.checked.iter().cloned().collect();
                paths.sort();
                if matches!(kind, CheckKind::Optimize) {
                    let message = match self.optimize_checked_commit_imports() {
                        Ok(()) => "Optimize imports completed for checked files".into(),
                        Err(error) => error,
                    };
                    self.report_git_status(message);
                    return;
                }
                if matches!(kind, CheckKind::Tests) {
                    if paths.is_empty() {
                        self.report_git_status(
                            "Commit test check failed: select test targets first".into(),
                        );
                        return;
                    }
                    let root = self
                        .project
                        .as_ref()
                        .map(|project| project.root.clone())
                        .unwrap_or_default();
                    self.git_panel.commit_workspace.preparation =
                        Some((root, CheckKind::Tests, std::collections::VecDeque::new()));
                    let epoch = self.test_panel.next_id;
                    self.run_tests(
                        paths
                            .iter()
                            .filter(|path| path.extension().is_some_and(|ext| ext == "py"))
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect(),
                        "",
                    );
                    if self.test_panel.next_id == epoch {
                        self.git_panel.commit_workspace.preparation = None;
                        self.report_git_status(
                            "Commit test check failed: the requested test run did not start".into(),
                        );
                    } else {
                        self.git_panel.commit_workspace.preparation_test =
                            Some((self.test_panel.next_id, self.commit_preparation_signature()));
                    }
                    return;
                }
                let root = self
                    .project
                    .as_ref()
                    .map(|project| project.root.clone())
                    .unwrap_or_default();
                self.git_panel.commit_workspace.preparation_warnings = false;
                self.git_panel.commit_workspace.preparation = Some((root, kind, paths.into()));
                self.poll_commit_preparation();
            }
        }
    }
    fn commit_preparation_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.active_test_owner().hash(&mut hasher);
        self.project
            .as_ref()
            .map(|project| &project.root)
            .hash(&mut hasher);
        self.git_panel
            .commit_workspace
            .prepare_before_commit
            .hash(&mut hasher);
        let mut paths: Vec<_> = self.git_panel.checked.iter().collect();
        paths.sort();
        for path in paths {
            path.hash(&mut hasher);
            if let Some(tab) = self
                .tabs
                .iter()
                .find(|tab| tab.state.path.as_ref() == Some(path))
            {
                tab.state.doc.text().hash(&mut hasher);
            } else {
                std::fs::read(path).ok().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
    pub(super) fn prepare_commit_workspace_submission(&mut self) -> bool {
        let option = self.git_panel.commit_workspace.prepare_before_commit;
        if option == 0 {
            return true;
        }
        if self.git_panel.commit_workspace.preparation.is_some() {
            self.set_status("Finish commit preparation before submitting");
            return false;
        }
        if self.git_panel.commit_workspace.prepared_signature
            == Some(self.commit_preparation_signature())
        {
            return true;
        }
        let kind = match option {
            1 => CheckKind::Reformat,
            2 => CheckKind::Rearrange,
            3 => CheckKind::Cleanup,
            4 => CheckKind::Analyze,
            _ => CheckKind::Tests,
        };
        self.commit_workspace_event(Event::CheckFiles(kind));
        self.set_status("Commit preparation started; review results then submit again");
        false
    }
    pub(super) fn poll_commit_preparation(&mut self) {
        if self.git_panel.commit_workspace.preparation.is_none() {
            return;
        }
        if self.bg.reformat.is_some()
            || self.bg.external_formatter.is_some()
            || self.bg.problems.infrastructure.dialog
            || self.bg.problems.job.is_some()
            || !matches!(self.dialog, Dialog::None)
        {
            return;
        }
        let Some((root, kind, mut paths)) = self.git_panel.commit_workspace.preparation.take()
        else {
            return;
        };
        if !self
            .project
            .as_ref()
            .is_some_and(|project| project.root == root)
        {
            self.git_panel.commit_workspace.prepared_signature = None;
            return;
        }
        if matches!(kind, CheckKind::Tests) {
            if self.git_panel.commit_workspace.preparation_test
                != Some((self.test_panel.next_id, self.commit_preparation_signature()))
            {
                self.git_panel.commit_workspace.prepared_signature = None;
                self.report_git_status(
                    "Commit test check invalidated: source changed or another test run replaced it"
                        .into(),
                );
                return;
            }
            match self.test_panel.commit_check_result() {
                None => {
                    self.git_panel.commit_workspace.preparation = Some((root, kind, paths));
                }
                Some(Ok(())) => {
                    self.git_panel.commit_workspace.prepared_signature =
                        Some(self.commit_preparation_signature());
                    self.git_panel
                        .commit_workspace
                        .results
                        .push("Commit test check passed".into());
                    self.report_git_status("Commit test check passed; submit when ready".into());
                }
                Some(Err(error)) => {
                    self.git_panel.commit_workspace.prepared_signature = None;
                    self.git_panel.commit_workspace.results.push(error.clone());
                    self.report_git_status(error);
                }
            }
            return;
        }
        if matches!(kind, CheckKind::Analyze)
            && self
                .bg
                .problems
                .results
                .iter()
                .any(|(_, results)| !results.is_empty())
        {
            self.git_panel.commit_workspace.preparation_warnings = true;
        }
        if let Some(path) = paths.pop_front() {
            self.open_file(&path);
            if !self
                .active
                .and_then(|index| self.tabs.get(index))
                .is_some_and(|tab| tab.state.path.as_ref() == Some(&path))
            {
                self.report_git_status(format!(
                    "Commit check failed: could not open {}",
                    path.display()
                ));
                return;
            }
            self.git_panel.commit_workspace.preparation = Some((root, kind, paths));
            match kind {
                CheckKind::Reformat => self.reformat_code(),
                CheckKind::Rearrange => self.request_rearrange(),
                CheckKind::Cleanup => self.code_cleanup(),
                CheckKind::Analyze => self.show_inspection_runner_for_path(&path),
                _ => {}
            }
        } else {
            if self.git_panel.commit_workspace.preparation_warnings
                && self.git_panel.commit_workspace.require_clean_checks
            {
                self.git_panel.commit_workspace.prepared_signature = None;
                self.report_git_status(
                    "Commit check failed: code inspection has warnings; review Problems".into(),
                );
                return;
            }
            self.git_panel.commit_workspace.prepared_signature =
                Some(self.commit_preparation_signature());
            self.git_panel
                .commit_workspace
                .results
                .push("Commit preparation finished; review edits and inspection results".into());
            self.report_git_status("Commit preparation finished; review edits and inspection results before submitting".into());
        }
    }
    /// Gutter source and patch must describe exactly the same saved buffer.
    pub(super) fn commit_workspace_gutter(
        &mut self,
        index: usize,
        line: usize,
        include: bool,
        single: bool,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        if std::fs::read_to_string(&path).ok().as_deref() != Some(tab.state.doc.text().as_str()) {
            self.set_status("Save the current buffer before staging its hunk or line");
            return;
        }
        let Some(repo) = self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
        else {
            return;
        };
        let result = (|| -> Result<(), String> {
            let diffs = if include {
                repo.diff_working_tree(Some(&path))
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|diff| diff.patch)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                repo.workspace_patch(true)
                    .map_err(|error| error.to_string())?
            };
            let patch = crate::git_panel::commit_workspace::hunk_at_line(
                &diffs,
                &path,
                repo.root(),
                line,
                single,
            )
            .ok_or("No matching saved hunk or line in this patch")?;
            let patch = if include {
                patch
            } else {
                crate::git_panel::commit_workspace::prepare_patch(&patch, 0, true)?
            };
            repo.workspace_apply_patch(&patch, true, true)
                .and_then(|_| repo.workspace_apply_patch(&patch, true, false))
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.git_panel.commit_workspace.index_commit = true;
                self.git_panel.last_refresh = -100.0;
                self.report_git_status(
                    if include {
                        "Included change in index commit"
                    } else {
                        "Excluded change from index commit"
                    }
                    .into(),
                );
            }
            Err(error) => self.report_git_status(error),
        }
    }
    pub(super) fn commit_workspace_move_gutter(&mut self, index: usize) {
        let path = self.tabs.get(index).and_then(|tab| tab.state.path.clone());
        if let Some(path) = path {
            if let Some(root) = self
                .project
                .as_ref()
                .and_then(|project| project.git.as_ref())
                .map(|repo| repo.root().to_path_buf())
            {
                self.git_panel.workspace_root(&root);
                self.git_panel.workspace_move_to_active(&path);
                self.set_status("Moved change to the active changelist");
            }
        }
    }
}

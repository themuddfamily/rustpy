//! Child of commit_workspace: uses the same persistent changelist authority.
use super::*;
impl GitPanel {
    pub(crate) fn workspace_task_named(
        &mut self,
        root: &Path,
        id: &str,
        summary: &str,
        name: &str,
    ) {
        self.workspace_root(root);
        self.commit_workspace.track_tasks = true;
        self.workspace_task(root, Some((id.into(), summary.into())));
        let Some(index) = self
            .commit_workspace
            .lists
            .iter()
            .position(|list| list.task == id)
        else {
            return;
        };
        let mut resolved = name.trim().to_owned();
        if resolved.is_empty() {
            resolved = format!("{id} {summary}");
        }
        if self
            .commit_workspace
            .lists
            .iter()
            .enumerate()
            .any(|(other, list)| other != index && list.name == resolved)
        {
            resolved = format!("{resolved} ({id})");
        }
        let base = resolved.clone();
        let mut suffix = 2;
        while self
            .commit_workspace
            .lists
            .iter()
            .enumerate()
            .any(|(other, list)| other != index && list.name == resolved)
        {
            resolved = format!("{base} {suffix}");
            suffix += 1;
        }
        self.commit_workspace.active = index;
        self.commit_workspace.lists[index].name = resolved;
        self.commit_workspace.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_task_activation_never_renames_another_tasks_list() {
        let directory = tempfile::tempdir().unwrap();
        let mut panel = GitPanel::default();
        panel.workspace_task_named(directory.path(), "one", "First", "Same name");
        panel.workspace_task_named(directory.path(), "two", "Second", "Same name");
        let first = panel
            .commit_workspace
            .lists
            .iter()
            .find(|list| list.task == "one")
            .unwrap()
            .name
            .clone();
        let second = panel
            .commit_workspace
            .lists
            .iter()
            .find(|list| list.task == "two")
            .unwrap()
            .name
            .clone();
        assert_ne!(first, second);
        panel.commit_workspace.active = panel
            .commit_workspace
            .lists
            .iter()
            .position(|list| list.task == "one")
            .unwrap();
        panel.workspace_task_named(directory.path(), "two", "Second", "Renamed second");
        assert_eq!(
            panel
                .commit_workspace
                .lists
                .iter()
                .find(|list| list.task == "one")
                .unwrap()
                .name,
            first
        );
        assert_eq!(
            panel.commit_workspace.lists[panel.commit_workspace.active].task,
            "two"
        );
    }
}

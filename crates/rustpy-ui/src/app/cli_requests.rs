//! Command-line requests (roadmap §8 "Command-line launcher"): the files and
//! folders of `rustpy [--line N] [--column M] path…`, at startup and when a
//! later launch forwards them to this IDE, and Tools › Create Command-line
//! Launcher….

use super::*;
use rustpy_cli::args::OpenTarget;

impl IdeApp {
    pub(crate) fn actual_current_project_root(&self) -> Option<&Path> {
        self.project.as_ref().map(|project| project.root.as_path())
    }
    // The independently integrated launch registry bridge consumes this method.
    #[allow(dead_code)]
    pub(crate) fn registry_error(&mut self, error: String) {
        self.notify_error("Project", "Window registration failed", error);
    }

    /// Opens command-line targets: a file in an editor tab with the caret at its
    /// line and column; a folder as the project (asking "This Window / New
    /// Window" like File › Open when another project is open; further folders
    /// open in new windows). At `startup` the first folder already is the
    /// project. Returns the canonical paths opened, for `--wait`.
    pub fn open_cli_targets(&mut self, targets: &[OpenTarget], startup: bool) -> Vec<PathBuf> {
        let mut opened = Vec::new();
        let mut folders = 0;
        for t in targets {
            let path = dunce::canonicalize(&t.path).unwrap_or_else(|_| t.path.clone());
            if path.is_dir() {
                let current = self.project.as_ref().map(|p| p.root.clone());
                if current.as_deref() != Some(path.as_path()) {
                    if folders == 0 && !startup {
                        self.request_open_project(&path);
                    } else {
                        self.open_in_new_window(&path);
                    }
                }
                folders += 1;
                opened.push(path);
            } else if path.is_file() {
                self.open_file(&path);
                if let Some(line) = t.line {
                    if let Some(tab) = self
                        .active
                        .and_then(|i| self.tabs.get_mut(i))
                        .filter(|tab| tab.state.path.as_deref() == Some(path.as_path()))
                    {
                        tab.view.cancel_session_restore();
                        tab.state
                            .go_to_line(line as usize, t.column.map(|c| c as usize));
                    }
                }
                self.focus_editor = true;
                opened.push(path);
            } else {
                self.set_status(format!(
                    "Cannot open {}: no such file or folder",
                    path.display()
                ));
            }
        }
        opened
    }

    /// Whether a path opened from the command line is still open: a file's tab,
    /// or a folder as the project (also while "This Window / New Window" is asked).
    pub fn cli_target_is_open(&self, path: &Path) -> bool {
        if self.project.as_ref().is_some_and(|p| p.root == path) {
            return true;
        }
        if matches!(&self.dialog, Dialog::OpenProject(st) if st.root == path) {
            return true;
        }
        self.tabs
            .iter()
            .any(|t| t.state.path.as_deref() == Some(path))
    }

    /// Restores "Reopen last project on startup" after a `dontReopenProjects` start.
    pub fn set_reopen_last_project(&mut self, reopen: bool) {
        self.settings.reopen_last_project = reopen;
    }

    /// Tools › Create Command-line Launcher… (`rustpy install-launcher`): writes
    /// the launcher script to the default place and says what happened.
    pub(super) fn create_command_line_launcher(&mut self) {
        let text = match rustpy_cli::launcher::install(None, false) {
            Ok(done) => format!("{}\n\nUse 'rustpy path…', 'rustpy --line N file' or 'rustpy --wait' (for $EDITOR) from a terminal; 'rustpy --help' lists the commands. To put the launcher elsewhere, run 'rustpy install-launcher <folder>'.", done.describe()),
            Err(e) => format!("The launcher was not created: {e}\n\nRun 'rustpy install-launcher <folder or file>' (with --force to replace a file) to choose another place."),
        };
        self.dialog = Dialog::Notice {
            title: "Create Command-line Launcher".into(),
            text,
            then: None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An IDE without a project (opening one would save the user's settings).
    fn app() -> IdeApp {
        IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Settings::default()
            },
            None,
        )
    }

    #[test]
    fn files_open_at_the_requested_position_and_close() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.py");
        std::fs::write(&file, "one\ntwo\nthree three\n").unwrap();
        let mut ide = app();
        let opened = ide.open_cli_targets(
            &[
                OpenTarget {
                    path: file.clone(),
                    line: Some(3),
                    column: Some(7),
                },
                OpenTarget::new(dir.path().join("missing.py")),
            ],
            false,
        );
        let canonical = dunce::canonicalize(&file).unwrap();
        assert_eq!(opened, vec![canonical.clone()]);
        assert!(ide.cli_target_is_open(&canonical));
        let tab = &ide.tabs[ide.active.unwrap()];
        assert_eq!(
            tab.state.primary_position(),
            rustpy_core::Position::new(2, 6)
        );
        // Opening it again moves the caret in the same tab.
        ide.open_cli_targets(
            &[OpenTarget {
                path: file.clone(),
                line: Some(1),
                column: None,
            }],
            false,
        );
        assert_eq!(ide.tabs.len(), 1);
        assert_eq!(
            ide.tabs[0].state.primary_position(),
            rustpy_core::Position::new(0, 0)
        );
        ide.close_tab(0);
        assert!(!ide.cli_target_is_open(&canonical));
    }
}

//! Git › Manage Remotes… (roadmap §4.1 "Manage Remotes"): list, add, edit
//! (name and URL) and remove the repository's remotes, e.g. to switch
//! `origin` from HTTPS to an SSH URL without the command line.

use super::*;
use crate::a11y::DialogWindow;

struct RemoteEdit {
    original: String,
    name: String,
    url: String,
}

pub(super) struct RemotesState {
    remotes: Vec<(String, String)>,
    /// `(name, push URL)` of the remotes that push to a separate URL.
    push_urls: Vec<(String, String)>,
    editing: Option<RemoteEdit>,
    new_name: String,
    new_url: String,
    /// Remote waiting for "Remove" to be confirmed.
    confirm_remove: Option<String>,
    error: Option<String>,
}

impl RemotesState {
    fn new(repo: &GitRepo) -> RemotesState {
        let mut st = RemotesState {
            remotes: Vec::new(),
            push_urls: Vec::new(),
            editing: None,
            new_name: String::new(),
            new_url: String::new(),
            confirm_remove: None,
            error: None,
        };
        st.reload(repo);
        if st.remotes.is_empty() {
            st.new_name = "origin".into();
        }
        st
    }

    fn reload(&mut self, repo: &GitRepo) {
        self.remotes = repo.remotes_with_urls().unwrap_or_default();
        self.push_urls = self
            .remotes
            .iter()
            .filter_map(|(name, url)| {
                repo.remote_push_url(name)
                    .filter(|p| p != url)
                    .map(|p| (name.clone(), p))
            })
            .collect();
    }

    fn push_url(&self, name: &str) -> Option<&str> {
        self.push_urls
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, u)| u.as_str())
    }
}

enum RemoteOp {
    Add,
    Save,
    Remove(String),
}

impl IdeApp {
    pub(super) fn open_remotes_dialog(&mut self) {
        match self.project.as_ref().and_then(|p| p.git.as_ref()) {
            Some(g) => self.dialog = Dialog::Remotes(Box::new(RemotesState::new(g))),
            None => self.set_status("Not a Git repository"),
        }
    }

    pub(super) fn remotes_dialog(&mut self, ctx: &egui::Context, mut st: Box<RemotesState>) {
        let theme = self.theme.clone();
        let Some(repo) = self.project.as_ref().and_then(|p| p.git.as_ref()) else {
            return;
        };
        let (mut open, mut close) = (true, false);
        let mut op: Option<RemoteOp> = None;
        egui::Window::new("Git Remotes").id(egui::Id::new("git_remotes_dialog")).collapsible(false).resizable(true).default_width(640.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            if st.remotes.is_empty() {
                ui.label(egui::RichText::new("No remotes. Add one below to push and fetch.").color(theme.text_dim));
            }
            egui::Grid::new("git_remotes_grid").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                for (name, url) in st.remotes.clone() {
                    let editing_this = st.editing.as_ref().is_some_and(|e| e.original == name);
                    let color = if editing_this { theme.accent } else { theme.text };
                    ui.label(egui::RichText::new(&name).strong().color(color));
                    match st.push_url(&name) {
                        Some(push) => {
                            // Unwrapped, like a URL without a push URL (a
                            // vertical cell would wrap at its first width).
                            ui.vertical(|ui| {
                                ui.add(egui::Label::new(egui::RichText::new(&url).monospace().color(color)).extend());
                                ui.add(egui::Label::new(egui::RichText::new(format!("push: {push}")).monospace().small().color(theme.text_dim)).extend()).on_hover_text("A separate push URL (remote.<name>.pushurl): pushes go there");
                            });
                        }
                        None => {
                            ui.label(egui::RichText::new(&url).monospace().color(color));
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!editing_this, egui::Button::new("Edit")).clicked() {
                            st.editing = Some(RemoteEdit { original: name.clone(), name: name.clone(), url: url.clone() });
                            st.confirm_remove = None;
                        }
                        if ui.button("Remove").clicked() {
                            st.confirm_remove = Some(name.clone());
                            st.editing = None;
                        }
                    });
                    ui.end_row();
                }
            });
            // The remote being edited, below the list (full-width fields).
            let editing_push_url = st.editing.as_ref().and_then(|e| st.push_url(&e.original)).map(str::to_owned);
            if let Some(e) = &mut st.editing {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("Edit remote '{}'", e.original)).strong());
                if let Some(push) = &editing_push_url {
                    ui.label(egui::RichText::new(format!("Pushes keep going to {push}: only the fetch URL is edited here.")).color(theme.text_dim).small());
                }
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(110.0));
                    ui.label("URL:");
                    let r = ui.add(egui::TextEdit::singleline(&mut e.url).desired_width(330.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        op = Some(RemoteOp::Save);
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        op = Some(RemoteOp::Save);
                    }
                    if ui.button("Cancel").clicked() {
                        st.editing = None;
                    }
                });
            }
            if let Some(name) = st.confirm_remove.clone() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("Remove the remote '{name}'? Its remote-tracking branches are deleted and local branches stop tracking it.")).color(theme.warning));
                ui.horizontal(|ui| {
                    if ui.button(egui::RichText::new("Remove").color(theme.error)).clicked() {
                        op = Some(RemoteOp::Remove(name.clone()));
                    }
                    if ui.button("Keep").clicked() {
                        st.confirm_remove = None;
                    }
                });
            }
            ui.separator();
            ui.label(egui::RichText::new("Add remote").strong());
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut st.new_name).desired_width(110.0).hint_text(if st.remotes.is_empty() { "origin" } else { "upstream" }));
                ui.label("URL:");
                let r = ui.add(egui::TextEdit::singleline(&mut st.new_url).desired_width(330.0).hint_text("git@github.com:user/repo.git"));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    op = Some(RemoteOp::Add);
                }
                if ui.add_enabled(!st.new_name.trim().is_empty() && !st.new_url.trim().is_empty(), egui::Button::new("Add")).clicked() {
                    op = Some(RemoteOp::Add);
                }
            });
            ui.label(egui::RichText::new("SSH (git@host:user/repo.git) and HTTPS URLs both work; credentials and key passphrases are asked for when Git needs them.").color(theme.text_dim).small());
            if let Some(e) = &st.error {
                ui.label(egui::RichText::new(e).color(theme.error));
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                if st.editing.is_some() || st.confirm_remove.is_some() {
                    st.editing = None;
                    st.confirm_remove = None;
                } else {
                    close = true;
                }
            }
        });
        let mut status: Option<String> = None;
        if let Some(op) = op {
            let result = match op {
                RemoteOp::Add => repo.add_remote_checked(&st.new_name, &st.new_url).map(|_| {
                    let msg = format!("Remote '{}' added", st.new_name.trim());
                    st.new_name.clear();
                    st.new_url.clear();
                    msg
                }),
                RemoteOp::Save => match st.editing.take() {
                    Some(e) => {
                        // Only what was edited is written (a separate push URL is kept).
                        let name = e.name.trim().to_string();
                        let r = repo
                            .update_remote(&e.original, &name, &e.url)
                            .map(|changed| {
                                if changed {
                                    format!("Remote '{name}' saved")
                                } else {
                                    String::new()
                                }
                            });
                        if r.is_err() {
                            st.editing = Some(e);
                        }
                        r
                    }
                    None => Ok(String::new()),
                },
                RemoteOp::Remove(name) => {
                    st.confirm_remove = None;
                    repo.remove_remote(&name)
                        .map(|_| format!("Remote '{name}' removed"))
                }
            };
            match result {
                Ok(msg) => {
                    st.error = None;
                    st.reload(repo);
                    status = Some(msg).filter(|m| !m.is_empty());
                }
                Err(e) => st.error = Some(e.to_string()),
            }
        }
        if let Some(m) = status {
            if let Some(p) = &mut self.project {
                p.git_refreshed = -100.0;
            }
            self.git_panel.last_refresh = -100.0;
            self.set_status(m);
        }
        if open && !close {
            self.dialog = Dialog::Remotes(st);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_without_remotes_suggests_origin() {
        let dir = crate::git_jobs::TestDir::new("remotes-dialog");
        let repo = GitRepo::init(dir.path()).unwrap();
        assert_eq!(RemotesState::new(&repo).new_name, "origin");
        repo.add_remote_checked("origin", "git@github.com:me/r.git")
            .unwrap();
        let st = RemotesState::new(&repo);
        assert_eq!(
            st.remotes,
            vec![("origin".to_string(), "git@github.com:me/r.git".to_string())]
        );
        assert_eq!(st.new_name, "");
        assert_eq!(st.push_url("origin"), None);
    }

    /// Review: Edit › Save rewrote a separate push URL to the fetch URL.
    #[test]
    fn a_separate_push_url_is_listed_and_survives_save() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = crate::git_jobs::TestDir::new("remotes-pushurl");
        let repo = GitRepo::init(dir.path()).unwrap();
        repo.add_remote_checked("origin", "https://github.com/me/repo.git")
            .unwrap();
        let out = std::process::Command::new("git")
            .args([
                "config",
                "remote.origin.pushurl",
                "git@github.com:me/repo.git",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        let st = RemotesState::new(&repo);
        assert_eq!(st.push_url("origin"), Some("git@github.com:me/repo.git"));
        // Save with the name changed only, then with nothing changed.
        assert!(repo
            .update_remote("origin", "upstream", "https://github.com/me/repo.git")
            .unwrap());
        assert!(!repo
            .update_remote("upstream", "upstream", "https://github.com/me/repo.git")
            .unwrap());
        let st = RemotesState::new(&repo);
        assert_eq!(
            st.remotes,
            vec![(
                "upstream".to_string(),
                "https://github.com/me/repo.git".to_string()
            )]
        );
        assert_eq!(st.push_url("upstream"), Some("git@github.com:me/repo.git"));
    }
}

//! UI authority owns editor mutations; the bounded transport cannot access disk,
//! execute actions, resolve arbitrary paths or impersonate another participant.
use super::*;
use crate::a11y::DialogWindow;
use crate::collaboration::{Authority, Event, Message, Session, SharedDocument, MAX_TEXT};
use std::collections::BTreeMap;
#[derive(Default)]
pub(super) struct State {
    pub visible: bool,
    pub session: Option<Session>,
    root: Option<PathBuf>,
    epoch: u64,
    name: String,
    invite: String,
    notice: String,
    authority: Authority,
    views: BTreeMap<u64, egui::Id>,
    identities: BTreeMap<u64, (rustpy_text::DocumentId, PathBuf)>,
    participants: BTreeMap<u64, Participant>,
    follow: Option<u64>,
    write: bool,
    peer: u64,
    next: u64,
    pending: BTreeMap<u64, String>,
    conflicts: BTreeMap<u64, String>,
    last_presence: Option<(Option<u64>, usize)>,
}
#[derive(Clone)]
struct Participant {
    name: String,
    document: Option<u64>,
    line: usize,
}
impl IdeApp {
    pub(super) fn cancel_collaboration_project(&mut self) {
        self.restore_collaboration_scratch_writable();
        if let Some(session) = &self.collaboration.session {
            session.stop();
        }
        self.collaboration.authority = Authority::default();
        self.collaboration.views.clear();
        self.collaboration.identities.clear();
        self.collaboration.participants.clear();
        self.collaboration.pending.clear();
        self.collaboration.conflicts.clear();
        self.collaboration.follow = None;
        self.collaboration.root = None;
    }
    fn restore_collaboration_scratch_writable(&mut self) {
        if self.collaboration.session.as_ref().is_some_and(|s| !s.host) {
            for view in self.collaboration.views.values() {
                if let Some(tab) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.view.id == *view && t.state.path.is_none())
                {
                    tab.state.doc.set_read_only(false);
                }
            }
        }
    }
    pub(super) fn collaboration_running(&self) -> bool {
        self.collaboration
            .session
            .as_ref()
            .is_some_and(Session::is_running)
    }
    pub(super) fn collaboration_stamp(&self) -> String {
        self.collaboration
            .session
            .as_ref()
            .map(Session::lifecycle_stamp)
            .unwrap_or_default()
    }
    pub(super) fn stop_collaboration(&mut self) {
        self.restore_collaboration_scratch_writable();
        if let Some(session) = &self.collaboration.session {
            session.stop();
        }
    }
    fn send_collaboration(&mut self, to: Option<u64>, message: Message) {
        if let Some(session) = &self.collaboration.session {
            if let Err(error) = session.send(to, message) {
                self.collaboration.notice = error;
                session.stop();
            }
        }
    }
    fn start_collaboration(&mut self, host: bool) {
        if self.collaboration_running() {
            self.collaboration.notice =
                "Disconnect and wait for the current session to stop before starting another"
                    .into();
            return;
        }
        if self.project_safe_mode() {
            self.collaboration.notice =
                "Trust the current project before starting or joining collaboration".into();
            return;
        }
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.collaboration.notice = "Open a trusted project first".into();
            return;
        };
        if self.collaboration_running() {
            return;
        }
        self.cancel_collaboration_project();
        let result = if host {
            Session::host()
        } else {
            Session::join(&self.collaboration.invite, self.collaboration.name.clone())
        };
        match result {
            Ok(session) => {
                self.collaboration.session = Some(session);
                self.collaboration.root = Some(root);
                self.collaboration.epoch = self.file_session;
                self.collaboration.next = 1;
                self.collaboration.write = false;
                self.collaboration.last_presence = None;
                self.collaboration.notice = if host {
                    "Hosting; invite holders can view only documents you explicitly share".into()
                } else {
                    "Connecting to host…".into()
                };
            }
            Err(error) => self.collaboration.notice = error,
        }
    }
    fn share_collaboration_editor(&mut self) {
        if !self.collaboration.session.as_ref().is_some_and(|s| s.host) {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let tab = &self.tabs[index];
        let Some(path) = tab.state.path.as_ref() else {
            self.collaboration.notice = "Share a saved project text file".into();
            return;
        };
        let allowed = dunce::canonicalize(path)
            .ok()
            .zip(
                self.collaboration
                    .root
                    .as_ref()
                    .and_then(|root| dunce::canonicalize(root).ok()),
            )
            .is_some_and(|(file, root)| file.starts_with(root));
        if !allowed
            || tab.image.is_some()
            || tab.notebook.is_some()
            || tab.state.is_large_file()
            || tab.state.doc.is_read_only()
        {
            self.collaboration.notice =
                "Only writable text documents inside this trusted project can be shared".into();
            return;
        }
        if self
            .collaboration
            .views
            .values()
            .any(|id| *id == tab.view.id)
        {
            return;
        }
        let text = tab.state.doc.text();
        if text.len() > MAX_TEXT || self.collaboration.views.len() >= 32 {
            self.collaboration.notice = "Sharing limit: 32 documents, 1 MiB each".into();
            return;
        }
        let id = self.collaboration.next;
        self.collaboration.next += 1;
        let document = SharedDocument {
            id,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .take(200)
                .collect(),
            text,
            version: 1,
        };
        self.collaboration.views.insert(id, tab.view.id);
        self.collaboration
            .identities
            .insert(id, (tab.state.doc.identity(), path.clone()));
        self.collaboration
            .authority
            .documents
            .insert(id, document.clone());
        self.send_collaboration(None, Message::Snapshot(document));
    }
    pub(super) fn collaboration_tick(&mut self, ctx: &egui::Context) {
        if self.collaboration.session.is_none() {
            return;
        }
        let valid = self
            .project
            .as_ref()
            .is_some_and(|p| Some(&p.root) == self.collaboration.root.as_ref())
            && self.file_session == self.collaboration.epoch
            && !self.project_safe_mode();
        if !valid {
            self.cancel_collaboration_project();
            return;
        }
        let host = self.collaboration.session.as_ref().is_some_and(|s| s.host);
        if host {
            let documents: Vec<_> = self
                .collaboration
                .views
                .iter()
                .map(|(id, view)| (*id, *view))
                .collect();
            for (id, view) in documents {
                let text =
                    self.tabs
                        .iter()
                        .find(|t| t.view.id == view)
                        .filter(|t| {
                            self.collaboration.identities.get(&id).is_some_and(
                                |(identity, path)| {
                                    *identity == t.state.doc.identity()
                                        && t.state.path.as_ref() == Some(path)
                                        && dunce::canonicalize(path)
                                            .ok()
                                            .zip(
                                                self.collaboration.root.as_ref().and_then(|root| {
                                                    dunce::canonicalize(root).ok()
                                                }),
                                            )
                                            .is_some_and(|(file, root)| file.starts_with(root))
                                },
                            )
                        })
                        .map(|t| t.state.doc.text());
                match text {
                    Some(text) if text.len() <= MAX_TEXT => {
                        let changed = self
                            .collaboration
                            .authority
                            .documents
                            .get(&id)
                            .is_some_and(|doc| doc.text != text);
                        if changed {
                            if let Some(doc) = self.collaboration.authority.documents.get_mut(&id) {
                                doc.version += 1;
                                doc.text = text;
                                let snapshot = doc.clone();
                                self.send_collaboration(None, Message::Snapshot(snapshot));
                            }
                        }
                    }
                    _ => {
                        self.collaboration.views.remove(&id);
                        self.collaboration.identities.remove(&id);
                        self.collaboration.authority.documents.remove(&id);
                        self.send_collaboration(None, Message::Removed(id));
                    }
                }
            }
        }
        let events = self
            .collaboration
            .session
            .as_ref()
            .map(Session::poll)
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Joined { peer, name } if host => {
                    self.collaboration.last_presence = None;
                    self.collaboration.participants.insert(
                        peer,
                        Participant {
                            name: name.clone(),
                            document: None,
                            line: 0,
                        },
                    );
                    self.collaboration.authority.permissions.insert(peer, false);
                    self.send_collaboration(Some(peer), Message::Permission { write: false });
                    let docs: Vec<_> = self
                        .collaboration
                        .authority
                        .documents
                        .values()
                        .cloned()
                        .collect();
                    for doc in docs {
                        self.send_collaboration(Some(peer), Message::Snapshot(doc));
                    }
                    let participants: Vec<_> = self
                        .collaboration
                        .participants
                        .iter()
                        .map(|(id, p)| (*id, p.clone()))
                        .collect();
                    for (id, p) in participants {
                        self.send_collaboration(
                            None,
                            Message::Presence {
                                peer: id,
                                name: p.name,
                                document: p.document,
                                line: p.line,
                            },
                        );
                    }
                }
                Event::Message { peer, message } => self.collaboration_message(host, peer, message),
                Event::Left(peer) => {
                    self.collaboration.participants.remove(&peer);
                    self.collaboration.authority.permissions.remove(&peer);
                    if host {
                        self.send_collaboration(None, Message::Left(peer));
                    }
                }
                Event::Error(error) => self.collaboration.notice = error,
                Event::Stopped => {
                    self.restore_collaboration_scratch_writable();
                    self.collaboration.notice =
                        "Session disconnected; local drafts retained".into();
                }
                _ => {}
            }
        }
        if self.collaboration_running() {
            let presence = self
                .active
                .and_then(|index| self.tabs.get(index))
                .map(|tab| {
                    let doc = self
                        .collaboration
                        .views
                        .iter()
                        .find(|(_, view)| **view == tab.view.id)
                        .map(|(id, _)| *id);
                    let offset: usize = tab.state.primary().head().into();
                    let text = tab.state.doc.text();
                    let line = text[..offset.min(text.len())]
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count();
                    (doc, line)
                })
                .unwrap_or((None, 0));
            if self.collaboration.last_presence != Some(presence) {
                self.collaboration.last_presence = Some(presence);
                self.send_collaboration(
                    None,
                    Message::Presence {
                        peer: 0,
                        name: self.collaboration.name.clone(),
                        document: presence.0,
                        line: presence.1,
                    },
                );
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    fn collaboration_message(&mut self, host: bool, peer: u64, message: Message) {
        if host {
            match message {
                Message::Edit { id, base, text } => {
                    let previous = self.collaboration.authority.documents.get(&id).cloned();
                    let result = self.collaboration.authority.edit(peer, id, base, text);
                    match result {
                        Ok(doc) => {
                            if self.apply_collaboration_text(id, &doc.text) {
                                self.send_collaboration(None, Message::Snapshot(doc));
                            } else {
                                if let Some(previous) = previous {
                                    self.collaboration
                                        .authority
                                        .documents
                                        .insert(id, previous.clone());
                                    self.send_collaboration(
                                        Some(peer),
                                        Message::Conflict {
                                            id,
                                            current: previous,
                                        },
                                    );
                                }
                                self.collaboration.notice =
                                    "Host editor is unavailable or read-only; edit was not applied"
                                        .into();
                                self.send_collaboration(
                                    Some(peer),
                                    Message::Notice(self.collaboration.notice.clone()),
                                );
                            }
                        }
                        Err(error) => {
                            self.send_collaboration(Some(peer), Message::Notice(error));
                            if let Some(doc) =
                                self.collaboration.authority.documents.get(&id).cloned()
                            {
                                self.send_collaboration(
                                    Some(peer),
                                    Message::Conflict { id, current: doc },
                                );
                            }
                        }
                    }
                }
                Message::Presence { document, line, .. } => {
                    if let Some(p) = self.collaboration.participants.get_mut(&peer) {
                        p.document = document
                            .filter(|id| self.collaboration.authority.documents.contains_key(id));
                        p.line = line.min(1_000_000);
                        let msg = Message::Presence {
                            peer,
                            name: p.name.clone(),
                            document: p.document,
                            line: p.line,
                        };
                        self.send_collaboration(None, msg);
                        self.follow_collaboration(peer);
                    }
                }
                _ => {}
            }
        } else {
            match message {
                Message::Welcome { peer } => {
                    self.collaboration.peer = peer;
                    self.collaboration.notice =
                        "Connected read-only; host controls write permission".into();
                }
                Message::Permission { write } => {
                    self.collaboration.write = write;
                    for view in self.collaboration.views.values() {
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.view.id == *view) {
                            tab.state.doc.set_read_only(!write);
                        }
                    }
                }
                Message::Snapshot(doc) => self.receive_collaboration_snapshot(doc, false),
                Message::Conflict { id: _, current } => {
                    self.receive_collaboration_snapshot(current, true)
                }
                Message::Removed(id) => {
                    self.collaboration.authority.documents.remove(&id);
                    self.collaboration.pending.remove(&id);
                    if let Some(view) = self.collaboration.views.remove(&id) {
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.view.id == view) {
                            tab.state.doc.set_read_only(false);
                        }
                    }
                    self.collaboration.notice =
                        "Host stopped sharing a document; your local copy was retained".into();
                }
                Message::Presence {
                    peer,
                    name,
                    document,
                    line,
                } => {
                    if self.collaboration.participants.len() < 32
                        || self.collaboration.participants.contains_key(&peer)
                    {
                        self.collaboration.participants.insert(
                            peer,
                            Participant {
                                name: name.chars().take(80).collect(),
                                document,
                                line: line.min(1_000_000),
                            },
                        );
                        self.follow_collaboration(peer);
                    }
                }
                Message::Left(peer) => {
                    self.collaboration.participants.remove(&peer);
                }
                Message::Notice(text) => {
                    self.collaboration.notice = text.chars().take(1000).collect()
                }
                _ => {}
            }
        }
    }
    fn apply_collaboration_text(&mut self, id: u64, text: &str) -> bool {
        let Some(view) = self.collaboration.views.get(&id).copied() else {
            return false;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.view.id == view) else {
            return false;
        };
        let readonly = tab.state.doc.is_read_only();
        let guest = self.collaboration.session.as_ref().is_some_and(|s| !s.host);
        if readonly && !guest {
            return false;
        }
        tab.state.doc.set_read_only(false);
        let range = rustpy_text::TextRange::new(0.into(), tab.state.doc.len());
        let applied = tab.state.doc.text() == text
            || tab
                .state
                .apply_text_edits(vec![(range, text.into())], "Collaboration edit");
        tab.state.doc.set_read_only(readonly);
        applied
    }
    fn receive_collaboration_snapshot(&mut self, doc: SharedDocument, conflict: bool) {
        if doc.text.len() > MAX_TEXT
            || doc.name.len() > 200
            || self.collaboration.authority.documents.len() >= 32
                && !self.collaboration.authority.documents.contains_key(&doc.id)
        {
            return;
        }
        if self
            .collaboration
            .authority
            .documents
            .get(&doc.id)
            .is_some_and(|old| old.version > doc.version)
        {
            return;
        }
        let view = self.collaboration.views.get(&doc.id).copied();
        let local = view
            .and_then(|view| self.tabs.iter().find(|tab| tab.view.id == view))
            .map(|tab| tab.state.doc.text());
        let dirty = local.as_ref().is_some_and(|text| {
            self.collaboration
                .authority
                .documents
                .get(&doc.id)
                .is_some_and(|old| old.text != *text)
        });
        let accepted = self
            .collaboration
            .pending
            .get(&doc.id)
            .is_some_and(|sent| sent == &doc.text)
            && local.as_ref() == Some(&doc.text);
        if view.is_none() || local.is_none() {
            let mut state = EditorState::from_text(&doc.text, Language::PlainText);
            state.doc.set_read_only(!self.collaboration.write);
            self.push_tab(state);
            if let Some(index) = self.active {
                self.tabs[index].pinned = true;
                self.collaboration
                    .views
                    .insert(doc.id, self.tabs[index].view.id);
            }
        } else if dirty && !accepted || conflict {
            self.collaboration
                .conflicts
                .insert(doc.id, local.unwrap_or_default());
            self.collaboration.notice="Concurrent edit: local draft retained. Compare and choose host text or explicitly resubmit against the latest version".into();
        } else {
            self.apply_collaboration_text(doc.id, &doc.text);
        }
        self.collaboration.pending.remove(&doc.id);
        self.collaboration.authority.documents.insert(doc.id, doc);
    }
    fn follow_collaboration(&mut self, peer: u64) {
        if self.collaboration.follow != Some(peer) {
            return;
        }
        let Some(p) = self.collaboration.participants.get(&peer) else {
            return;
        };
        let Some(view) = p
            .document
            .and_then(|id| self.collaboration.views.get(&id))
            .copied()
        else {
            return;
        };
        if let Some(index) = self.tabs.iter().position(|tab| tab.view.id == view) {
            self.active = Some(index);
            self.tabs[index]
                .state
                .go_to_line(p.line.saturating_add(1), None);
        }
    }
    pub(super) fn collaboration_ui(&mut self, ctx: &egui::Context) {
        if !self.collaboration.visible {
            return;
        }
        let mut open = true;
        egui::Window::new("Collaboration").id(egui::Id::new("collaboration-session")).open(&mut open).default_width(560.0).show_modeless(ctx,|ui|{
            ui.label("Only explicitly shared open text documents are transmitted. Guests cannot run code, browse the filesystem or control your terminal. Voice is not supported.");
            if !self.collaboration_running(){
                ui.horizontal(|ui|{ui.label("Display name:");ui.text_edit_singleline(&mut self.collaboration.name);});
                if ui.button("Host session").clicked(){self.start_collaboration(true);}
                ui.horizontal(|ui|{ui.label("Invitation:");ui.add(egui::TextEdit::singleline(&mut self.collaboration.invite).password(true));if ui.button("Join").clicked(){self.start_collaboration(false);}});
                ui.collapsing("Remote connection through SSH",|ui|{ui.label("On the guest machine, create an authenticated encrypted forward with your SSH client: ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:LOCAL_PORT:127.0.0.1:HOST_PORT user@host. Keep host-key verification enabled. Replace the invitation port with LOCAL_PORT; keep its secret token unchanged. RustPy binds only to loopback and does not create a public tunnel.");});
            }else{
                let host=self.collaboration.session.as_ref().is_some_and(|s|s.host);
                ui.horizontal(|ui|{ui.label(if host{"Hosting"}else if self.collaboration.write{"Guest: edit permission"}else{"Guest: read-only"});if ui.button("Disconnect / End session").clicked(){self.stop_collaboration();}});
                if host{
                    if ui.button("Copy authenticated invitation").clicked(){if let Some(invite)=self.collaboration.session.as_ref().and_then(|s|s.invitation.clone()){ui.ctx().copy_text(invite);}}
                    ui.label("Treat the invitation as a password. End the session to invalidate it.");
                    if ui.button("Share active editor").clicked(){self.share_collaboration_editor();}
                }
                ui.separator();ui.heading("Participants");
                let people:Vec<_>=self.collaboration.participants.iter().map(|(id,p)|(*id,p.clone())).collect();
                for(id,p)in people{ui.horizontal(|ui|{ui.label(format!("{} ({id})",p.name));let mut follow=self.collaboration.follow==Some(id);if ui.checkbox(&mut follow,"Follow").changed(){self.collaboration.follow=follow.then_some(id);self.follow_collaboration(id);}
if host{let mut write=self.collaboration.authority.permissions.get(&id).copied().unwrap_or(false);if ui.checkbox(&mut write,"Allow editing").changed(){self.collaboration.authority.permissions.insert(id,write);self.send_collaboration(Some(id),Message::Permission{write});}
if ui.button("Revoke / Disconnect").clicked(){self.collaboration.authority.permissions.remove(&id);if let Some(s)=&self.collaboration.session{s.revoke(id);}}}});}
                ui.separator();ui.heading("Shared documents");let docs:Vec<_>=self.collaboration.authority.documents.values().cloned().collect();
                for doc in docs{ui.horizontal(|ui|{if ui.button(format!("{} · v{}",doc.name,doc.version)).clicked(){if let Some(view)=self.collaboration.views.get(&doc.id){self.active=self.tabs.iter().position(|t|t.view.id==*view);self.focus_editor=true;}}
                    if host{if ui.button("Stop sharing").clicked(){self.collaboration.views.remove(&doc.id);self.collaboration.authority.documents.remove(&doc.id);self.send_collaboration(None,Message::Removed(doc.id));}}
                    else{if ui.add_enabled(self.collaboration.write&&!self.collaboration.pending.contains_key(&doc.id),egui::Button::new("Submit editor changes")).clicked(){let text=self.collaboration.views.get(&doc.id).and_then(|view|self.tabs.iter().find(|t|t.view.id==*view)).map(|tab|tab.state.doc.text());if let Some(text)=text{if text.len()<=MAX_TEXT{self.collaboration.pending.insert(doc.id,text.clone());self.send_collaboration(None,Message::Edit{id:doc.id,base:doc.version,text});}}}
                        if self.collaboration.conflicts.contains_key(&doc.id){if ui.button("Use host text").clicked(){self.apply_collaboration_text(doc.id,&doc.text);self.collaboration.conflicts.remove(&doc.id);}
if ui.button("Copy retained draft").clicked(){ui.ctx().copy_text(self.collaboration.conflicts[&doc.id].clone());}}
                    }
                });}
            }ui.separator();ui.label(&self.collaboration.notice);
        });
        self.collaboration.visible = open;
    }
}

#[cfg(test)]
mod connected_fixtures {
    use super::*;
    #[test]
    fn authority_applies_real_editor_edit_and_rejects_stale_guest() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("original", Language::PlainText));
        let index = app.active.unwrap();
        let view = app.tabs[index].view.id;
        app.collaboration.views.insert(1, view);
        app.collaboration.authority.documents.insert(
            1,
            SharedDocument {
                id: 1,
                name: "shared".into(),
                text: "original".into(),
                version: 3,
            },
        );
        app.collaboration.authority.permissions.insert(2, true);
        app.collaboration_message(
            true,
            2,
            Message::Edit {
                id: 1,
                base: 3,
                text: "changed".into(),
            },
        );
        assert_eq!(app.tabs[index].state.doc.text(), "changed");
        assert_eq!(app.collaboration.authority.documents[&1].version, 4);
        app.collaboration_message(
            true,
            2,
            Message::Edit {
                id: 1,
                base: 3,
                text: "stale".into(),
            },
        );
        assert_eq!(app.tabs[index].state.doc.text(), "changed");
        app.collaboration.authority.permissions.insert(2, false);
        app.collaboration_message(
            true,
            2,
            Message::Edit {
                id: 1,
                base: 4,
                text: "revoked".into(),
            },
        );
        assert_eq!(app.tabs[index].state.doc.text(), "changed");
    }
    #[test]
    fn divergent_guest_draft_survives_new_snapshot() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("guest draft", Language::PlainText));
        let index = app.active.unwrap();
        let view = app.tabs[index].view.id;
        app.collaboration.views.insert(1, view);
        app.collaboration.authority.documents.insert(
            1,
            SharedDocument {
                id: 1,
                name: "shared".into(),
                text: "base".into(),
                version: 1,
            },
        );
        app.receive_collaboration_snapshot(
            SharedDocument {
                id: 1,
                name: "shared".into(),
                text: "host update".into(),
                version: 2,
            },
            false,
        );
        assert_eq!(app.tabs[index].state.doc.text(), "guest draft");
        assert_eq!(app.collaboration.conflicts[&1], "guest draft");
        assert_eq!(
            app.collaboration.authority.documents[&1].text,
            "host update"
        );
    }
}

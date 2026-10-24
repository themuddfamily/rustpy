//! App owner: consume matching cached parse, not parsing from the paint path.
use super::gutter_actions::{Marker, Markers, Request, Scheduler, Stamp};
use super::*;
use crate::editor::gutter_actions::{Intent, Kind};
use std::sync::Arc;
#[derive(Default)]
pub(super) struct Runtime {
    scheduler: Scheduler,
    request: Option<Request>,
    markers: Vec<Marker>,
}
impl Runtime {
    fn cancel(&mut self) {
        self.scheduler.cancel();
        self.request = None;
        self.markers.clear();
    }
    pub(super) fn markers_for(&self, state: &EditorState) -> &[Marker] {
        self.request
            .as_ref()
            .filter(|r| {
                r.stamp.document == state.doc.identity()
                    && r.stamp.version == state.doc.version()
                    && state.path.as_ref() == Some(&r.stamp.path)
                    && r.stamp.source.as_ref() == state.doc.text()
                    && r.analysis.revision_excluding(&r.stamp.path) == r.stamp.semantic_revision
            })
            .map_or(&[], |_| self.markers.as_slice())
    }
}
impl IdeApp {
    pub(super) fn gutter_actions_tick(&mut self, ctx: &egui::Context) {
        let scope = self.project_scope();
        let buffers = self
            .tabs
            .iter()
            .filter(|t| t.state.language == Language::Python)
            .map(|t| {
                (
                    t.state.doc.identity(),
                    t.state.doc.version(),
                    t.state.path.clone(),
                )
            })
            .collect();
        // Observe the existing real watcher clock even when all Code Vision options are OFF.
        let content_generation = self
            .code_vision
            .gutter_content_generation(scope.clone(), buffers);
        let Some(index) = self.active.filter(|i| self.tabs.get(*i).is_some()) else {
            self.gutter_actions.cancel();
            return;
        };
        let state = &self.tabs[index].state;
        if self.safe_mode
            || self.settings.power_save_mode
            || state.language != Language::Python
            || state.is_large_file()
            || self.is_large_file_placeholder(index)
        {
            self.gutter_actions.cancel();
            return;
        }
        let Some(project) = self.project.as_ref() else {
            self.gutter_actions.cancel();
            return;
        };
        let Some(path) = state.path.clone() else {
            self.gutter_actions.cancel();
            return;
        };
        let Some(scope) = scope else {
            self.gutter_actions.cancel();
            return;
        };
        let complete = self.cached_list_is_current(&scope);
        let generation = self.project_index.files.generation();
        let files = self.code_vision.gutter_catalogue(&self.project_index.files);
        let source = self
            .gutter_actions
            .request
            .as_ref()
            .filter(|r| {
                r.stamp.document == state.doc.identity()
                    && r.stamp.version == state.doc.version()
                    && r.stamp.path == path
            })
            .map(|r| r.stamp.source.clone())
            .unwrap_or_else(|| Arc::from(state.doc.text()));
        let analysis = project.analysis.clone();
        // Read-only buffers are deliberately excluded by sync_analysis. Their
        // exact captured source is checked against the worker's bounded disk
        // snapshot before publication; no UI parse or synthetic open sync.
        if !state.doc.is_read_only()
            && !analysis
                .cached_file_for_path(&path)
                .is_some_and(|f| f.is_open && f.text == source)
        {
            self.gutter_actions.cancel();
            return;
        }
        let request = Request {
            analysis: analysis.clone(),
            stamp: Stamp {
                document: state.doc.identity(),
                version: state.doc.version(),
                path: path.clone(),
                source,
                semantic_revision: analysis.revision_excluding(&path),
                project: project.root.clone(),
                index_generation: generation,
                content_generation,
                files,
                complete: complete && generation == self.project_index.files.generation(),
                trusted: true,
            },
        };
        let initial = self.gutter_actions.scheduler.request(request.clone());
        let result = self
            .gutter_actions
            .scheduler
            .poll(&request)
            .unwrap_or(initial);
        self.gutter_actions.markers = match result {
            Markers::Ready(markers) => markers,
            Markers::Pending => {
                ctx.request_repaint_after(std::time::Duration::from_millis(40));
                Vec::new()
            }
            Markers::Unavailable => Vec::new(),
        };
        self.gutter_actions.request = Some(request);
    }
    pub(super) fn gutter_actions_intent(&mut self, index: usize, intent: Intent) {
        if let Kind::ReviewComment(id) = intent.kind {
            self.open_github_review_comment(index, id);
            return;
        }
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if intent.offset > tab.state.doc.len() {
            return;
        }
        // Implementations need complete indexes; queued as Go to Implementation at the marker.
        if intent.kind == Kind::Implementations
            && !self.dumb_mode_admits_with(crate::actions::Action::GotoImplementation, |app| {
                app.dumb_editor_target(index, Some(intent.offset))
            })
        {
            return;
        }
        let scope = self.project_scope();
        let buffers = self
            .tabs
            .iter()
            .filter(|t| t.state.language == Language::Python)
            .map(|t| {
                (
                    t.state.doc.identity(),
                    t.state.doc.version(),
                    t.state.path.clone(),
                )
            })
            .collect();
        let generation = self.code_vision.gutter_content_generation(scope, buffers);
        if intent.kind != Kind::QuickFix
            && !self.gutter_actions.request.as_ref().is_some_and(|r| {
                r.stamp.content_generation == generation
                    && r.stamp.index_generation == self.project_index.files.generation()
                    && self.project.as_ref().is_some_and(|p| {
                        p.root == r.stamp.project && Arc::ptr_eq(&p.analysis, &r.analysis)
                    })
            })
        {
            return;
        }
        let tab = &self.tabs[index];
        let locations = match intent.kind {
            Kind::ReviewComment(_) => return,
            Kind::QuickFix => {
                if !analysis_jobs::current_quick_fixes(&tab.state)
                    .iter()
                    .any(|(range, fixes)| {
                        !fixes.is_empty()
                            && tab.state.doc.line_of(range.start())
                                == tab.state.doc.line_of(intent.offset)
                    })
                {
                    return;
                }
                None
            }
            Kind::Super | Kind::Implementations => {
                let Some(marker) = self
                    .gutter_actions
                    .markers_for(&tab.state)
                    .iter()
                    .find(|m| m.offset == intent.offset)
                else {
                    return;
                };
                Some(if intent.kind == Kind::Super {
                    marker.supers.clone()
                } else {
                    marker.implementations.clone()
                })
            }
        };
        self.active = Some(index);
        self.group_active[self.tabs[index].group as usize] = Some(self.tabs[index].view.id);
        self.emacs_editing.interrupt();
        self.tabs[index].state.reveal_offset(intent.offset);
        self.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(intent.offset)]);
        if let Some(locations) = locations {
            self.navigate_locations(locations, "No current gutter destination");
        } else {
            self.open_intentions();
        }
    }
}

impl super::IdeApp {
    pub(super) fn gutter_view_inputs(
        &self,
        index: usize,
    ) -> (
        Vec<crate::editor::view::GutterMarker>,
        Vec<rustpy_core::TextSize>,
    ) {
        let state = &self.tabs[index].state;
        let markers = self
            .gutter_actions
            .markers_for(state)
            .iter()
            .map(|m| crate::editor::view::GutterMarker {
                offset: m.offset,
                has_super: !m.supers.is_empty(),
                has_implementations: !m.implementations.is_empty(),
            })
            .collect();
        let mut fixes = super::gutter_actions::quick_fix_offsets(
            &state.doc,
            super::analysis_jobs::current_quick_fixes(state),
        );
        if crate::editor::doc_render::reader_mode(state)
            && !self.settings.editor.reader_mode.show_warnings
        {
            fixes.retain(|offset| {
                state.extra_diagnostics.iter().any(|d| {
                    d.severity == rustpy_core::Severity::Error
                        && state.doc.line_of(d.range.start()) == state.doc.line_of(*offset)
                })
            });
        }
        if !self.settings.editor.show_intention_bulb {
            fixes.clear();
        }
        (markers, fixes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("π = 1", Language::Python));
        app
    }
    #[test]
    fn flags_off_clock_still_observes_buffers_and_rescan() {
        let mut app = fixture();
        assert!(!app.settings.editor.code_vision_usages);
        let ctx = egui::Context::default();
        app.gutter_actions_tick(&ctx);
        let first = app.code_vision.gutter_content_generation(None, vec![]);
        app.code_vision.note_rescan();
        let second = app.code_vision.gutter_content_generation(None, vec![]);
        assert!(second > first);
    }
    #[test]
    fn actual_quickfix_intent_uses_current_intentions_readonly_and_preserves_history() {
        let mut app = fixture();
        let state = &mut app.tabs[0].state;
        state.semantic_version = Some(state.doc.version());
        state.quick_fixes = vec![(
            TextRange::empty(TextSize::new(0)),
            vec![QuickFix {
                title: "Bound actual fix".into(),
                edits: vec![],
            }],
        )];
        state.doc.set_read_only(true);
        app.gutter_actions_intent(
            0,
            Intent {
                offset: TextSize::new(0),
                kind: Kind::QuickFix,
            },
        );
        assert!(app
            .intentions
            .as_ref()
            .is_some_and(|(fixes, _)| fixes.iter().any(|f| f.title == "Bound actual fix")));
        assert_eq!(app.tabs[0].state.doc.text(), "π = 1");
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn stale_quickfix_and_unowned_destinations_do_not_move_caret() {
        let mut app = fixture();
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(2))]);
        let before = app.tabs[0].state.primary().head();
        for kind in [Kind::Super, Kind::Implementations, Kind::QuickFix] {
            app.gutter_actions_intent(
                0,
                Intent {
                    offset: TextSize::new(0),
                    kind,
                },
            );
            assert_eq!(app.tabs[0].state.primary().head(), before);
        }
        assert!(app.intentions.is_none());
    }
    #[test]
    fn actual_flags_off_project_publishes_and_readonly_override_navigates() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("main.py");
        let source="class Base:\n    def work(self):\n        return 1\nclass Child(Base):\n    def work(self):\n        return 2\n";
        std::fs::write(&path, source).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        let mut state = EditorState::from_text(source, Language::Python);
        state.path = Some(path);
        state.doc.set_read_only(true);
        app.push_tab(state);
        app.sync_open_tabs_now();
        app.start_fs_watcher(&root);
        app.project_index
            .files
            .rebuild_now(app.project_scope().unwrap());
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            app.gutter_actions_tick(&ctx);
            if !app.gutter_actions.markers.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real gutter worker must publish with Code Vision flags off"
            );
            std::thread::yield_now();
        }
        let child = TextSize::new(source.rfind("work").unwrap() as u32);
        let base = TextSize::new(source.find("work").unwrap() as u32);
        app.gutter_actions_intent(
            0,
            Intent {
                offset: child,
                kind: Kind::Super,
            },
        );
        assert_eq!(app.tabs[0].state.primary().range().start(), base);
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert!(app.tabs[0].state.doc.is_read_only());
        app.code_vision.note_rescan();
        let before = app.tabs[0].state.primary().head();
        app.gutter_actions_intent(
            0,
            Intent {
                offset: child,
                kind: Kind::Super,
            },
        );
        assert_eq!(app.tabs[0].state.primary().head(), before);
    }
    #[test]
    #[cfg(unix)]
    fn initial_readonly_open_uses_normal_analysis_lifecycle_and_worker_snapshot() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("main.py");
        let source="class Base:\n    def work(self):\n        return 1\nclass Child(Base):\n    def work(self):\n        return 2\n";
        std::fs::write(&path, source).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.start_fs_watcher(&root);
        app.project_index
            .files
            .rebuild_now(app.project_scope().unwrap());
        app.open_file(&path);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.tabs[0].state.doc.is_read_only());
        app.sync_analysis(1.0);
        let analysis = app.project.as_ref().unwrap().analysis.clone();
        assert!(
            !analysis
                .cached_file_for_path(&path)
                .is_some_and(|file| file.is_open),
            "normal readonly lifecycle must not force open text into Analysis"
        );
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            app.gutter_actions_tick(&ctx);
            if !app.gutter_actions.markers.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "initial readonly file must publish semantic gutter destinations"
            );
            std::thread::yield_now();
        }
        let child = TextSize::new(source.rfind("work").unwrap() as u32);
        let base = TextSize::new(source.find("work").unwrap() as u32);
        assert!(app
            .gutter_actions
            .markers_for(&app.tabs[0].state)
            .iter()
            .any(|m| m.offset == child && !m.supers.is_empty()));
        app.gutter_actions_intent(
            0,
            Intent {
                offset: child,
                kind: Kind::Super,
            },
        );
        assert_eq!(app.tabs[0].state.primary().range().start(), base);
        assert!(app.tabs[0].state.doc.is_read_only());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert!(!analysis
            .cached_file_for_path(&path)
            .is_some_and(|file| file.is_open));
    }
}

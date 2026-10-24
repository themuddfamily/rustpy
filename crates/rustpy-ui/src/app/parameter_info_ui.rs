//! Parameter-info overload navigation; Root mounts the input/action/draw hooks.
use super::*;
use rustpy_semantic::{call_help, Inferer};
use rustpy_text::DocumentId;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CallKey {
    document: DocumentId,
    start: TextSize,
    name: String,
}
/// What a background parameter-info lookup finds: the call's `(start, name)`
/// and its signatures.
type Lookup = (Option<(TextSize, String)>, Option<SignatureHelp>);

#[derive(Default)]
pub(super) struct ParameterInfoUi {
    key: Option<CallKey>,
    manual: Option<String>,
    /// A lookup following the caret through a large file, off the UI thread:
    /// `((view, document version, caret), document, run)`.
    job: Option<CaretLookupJob>,
}

/// A caret-following lookup: `((view, document version, caret), document, run)`.
type CaretLookupJob = (
    (egui::Id, u64, TextSize),
    DocumentId,
    rustpy_core::TaskHandle<Lookup>,
);
impl ParameterInfoUi {
    fn update(&mut self, key: CallKey, help: &mut SignatureHelp, explicit: bool) {
        let same = self.key.as_ref() == Some(&key);
        if !same {
            self.manual = None;
        }
        self.key = Some(key);
        if let Some(label) = &self.manual {
            if let Some(index) = help.signatures.iter().position(|sig| sig.label == *label) {
                select(help, index);
            } else {
                self.manual = None;
            }
        }
        if explicit && same && help.signatures.len() > 1 {
            self.cycle(help, 1);
        }
    }
    fn cycle(&mut self, help: &mut SignatureHelp, direction: isize) {
        let count = help.signatures.len();
        if count < 2 {
            return;
        }
        let index =
            (help.active_signature as isize + direction).rem_euclid(count as isize) as usize;
        select(help, index);
        self.manual = Some(help.label.clone());
    }
    /// Whether a background lookup is following the caret.
    pub(super) fn lookup_pending(&self) -> bool {
        self.job.is_some()
    }
    fn clear(&mut self) {
        self.key = None;
        self.manual = None;
        if let Some((_, _, job)) = self.job.take() {
            job.cancel();
        }
    }
}
fn select(help: &mut SignatureHelp, index: usize) {
    let Some(sig) = help.signatures.get(index) else {
        return;
    };
    help.active_signature = index;
    help.label = sig.label.clone();
    help.params = sig.params.clone();
    help.active = sig.active;
}
impl IdeApp {
    /// Replaces the old update_param_info body: actual semantic current-argument
    /// mapping stays authoritative, while manual overload choice survives typing.
    pub(super) fn update_parameter_info(&mut self, explicit: bool) {
        let Some(i) = self.active else {
            self.close_parameter_info();
            return;
        };
        // Following the caret through a large file: the lookup (pushing the
        // text into the analysis, inferring the call) runs off the UI thread
        // and the popup shows the previous result meanwhile.
        if !explicit
            && usize::from(self.tabs[i].state.doc.len())
                >= super::analysis_jobs::BACKGROUND_SYNC_BYTES
            && self.start_parameter_info_lookup(i)
        {
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.close_parameter_info();
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        let key = call_help::call_context(&Inferer::new(&analysis), &file, caret).map(|context| {
            CallKey {
                document: self.tabs[i].state.doc.identity(),
                start: context.call.syntax().text_range().start(),
                name: context.name,
            }
        });
        self.param_info_at = Some((
            self.tabs[i].view.id,
            self.tabs[i].state.doc.version(),
            caret,
        ));
        match (key, features::signature_help(&analysis, &file, caret)) {
            (Some(key), Some(mut help)) => {
                self.parameter_info_ui.update(key, &mut help, explicit);
                self.param_info = Some(help);
            }
            _ => {
                self.close_parameter_info();
                if explicit {
                    self.set_status("No parameter info available");
                }
            }
        }
    }
    /// Starts the background lookup for editor `i` (see
    /// [`IdeApp::update_parameter_info`]): false when it cannot run there.
    fn start_parameter_info_lookup(&mut self, i: usize) -> bool {
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return false;
        };
        let state = &self.tabs[i].state;
        let Some(base) = state
            .path
            .as_deref()
            .filter(|_| !self.remote_bound(i))
            .and_then(|path| analysis.file_for_path(path))
        else {
            return false;
        };
        let caret = state.primary().head();
        let here = (self.tabs[i].view.id, state.doc.version(), caret);
        let document = state.doc.identity();
        let text = state.doc.text();
        if let Some((_, _, job)) = self.parameter_info_ui.job.take() {
            job.cancel();
        }
        let handle = rustpy_core::TaskManager::global()
            .task("Parameter info")
            .hidden()
            .spawn(move |progress| {
                // This text in the file's semantic context, without publishing it.
                let file = Arc::new(base.with_candidate_text(text));
                let token = progress.token().clone();
                let lookup = rustpy_semantic::infer::with_cancellation(&token, || {
                    let key = call_help::call_context(&Inferer::new(&analysis), &file, caret)
                        .map(|context| (context.call.syntax().text_range().start(), context.name));
                    (key, features::signature_help(&analysis, &file, caret))
                });
                progress.check()?;
                Ok(lookup)
            });
        self.parameter_info_ui.job = Some((here, document, handle));
        self.param_info_at = Some(here);
        true
    }

    /// Shows a finished background lookup if the caret is still where it was.
    pub(super) fn poll_parameter_info_lookup(&mut self) {
        let Some(result) = self
            .parameter_info_ui
            .job
            .as_mut()
            .and_then(|(_, _, job)| job.try_result())
        else {
            return;
        };
        let Some((at, document, _)) = self.parameter_info_ui.job.take() else {
            return;
        };
        let Some(i) = self.active else {
            return;
        };
        let state = &self.tabs[i].state;
        if (
            self.tabs[i].view.id,
            state.doc.version(),
            state.primary().head(),
        ) != at
            || state.doc.identity() != document
        {
            return;
        }
        match result {
            Ok((Some((start, name)), Some(mut help))) => {
                let key = CallKey {
                    document,
                    start,
                    name,
                };
                self.parameter_info_ui.update(key, &mut help, false);
                self.param_info = Some(help);
            }
            Ok(_) => self.close_parameter_info(),
            Err(_) => {}
        }
    }

    pub(super) fn close_parameter_info(&mut self) {
        self.param_info = None;
        self.param_info_at = None;
        self.parameter_info_ui.clear();
    }
    /// Root calls BEFORE editor key handling, only when editor owns keys and no
    /// dialog/global popup is active. Plain arrows remain normal caret movement.
    pub(super) fn parameter_info_keys(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active else {
            return;
        };
        if self.param_info.is_none() {
            return;
        }
        let here = Some((
            self.tabs[i].view.id,
            self.tabs[i].state.doc.version(),
            self.tabs[i].state.primary().head(),
        ));
        if self.param_info_at != here {
            self.update_parameter_info(false);
        }
        let Some(help) = self.param_info.as_mut() else {
            return;
        };
        if help.signatures.len() < 2 {
            return;
        }
        let direction = ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown) {
                Some(1)
            } else if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp) {
                Some(-1)
            } else {
                None
            }
        });
        if let Some(direction) = direction {
            self.parameter_info_ui.cycle(help, direction);
        }
    }
    /// Draws the existing signatures with controls inside the same popup.
    pub(super) fn draw_parameter_info_with_cycle(&mut self, ui: &mut egui::Ui, caret: Rect) {
        let Some(help) = self.param_info.clone() else {
            return;
        };
        self.draw_param_info(ui, caret, &help);
    }
    pub(super) fn parameter_info_cycle_controls(
        &mut self,
        ui: &mut egui::Ui,
        help: &SignatureHelp,
    ) {
        if help.signatures.len() < 2 {
            return;
        }
        let mut direction = None;
        ui.horizontal(|ui| {
            if ui
                .button("‹")
                .on_hover_text("Previous overload (Alt+Up)")
                .clicked()
            {
                direction = Some(-1);
            }
            ui.label(format!(
                "Overload {} / {}",
                help.active_signature + 1,
                help.signatures.len()
            ));
            if ui
                .button("›")
                .on_hover_text("Next overload (Alt+Down)")
                .clicked()
            {
                direction = Some(1);
            }
        });
        if let Some(direction) = direction {
            if let Some(help) = self.param_info.as_mut() {
                self.parameter_info_ui.cycle(help, direction);
            }
            self.focus_editor = true;
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::state::Caret;
    use rustpy_semantic::features::SignatureLabel;
    fn fixture() -> (tempfile::TempDir, IdeApp, String) {
        let dir = tempfile::tempdir().unwrap();
        let source = "from typing import overload\n@overload\ndef f(π: str, count: int) -> str: ...\n@overload\ndef f(π: int, count: int) -> int: ...\ndef f(*args): ...\nf('a', 2)\n".to_owned();
        let path = dir.path().join("main.py");
        std::fs::write(&path, &source).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        let root = dir.path().to_path_buf();
        app.project = Some(Project {
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            root,
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.open_file(&path);
        let offset = source.rfind(", 2").unwrap() + 2;
        app.tabs[app.active.unwrap()]
            .state
            .set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
        (dir, app, source)
    }
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                ..Default::default()
            },
            |ui| {
                app.parameter_info_keys(ui);
                app.draw_parameter_info_with_cycle(
                    ui,
                    Rect::from_min_size(Pos2::new(200.0, 350.0), Vec2::new(2.0, 18.0)),
                );
            },
        )
    }
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }
    #[test]
    fn actual_semantic_help_cycles_ctrl_p_and_keeps_current_parameter_for_manual_overload() {
        let (_dir, mut app, source) = fixture();
        let i = app.active.unwrap();
        let version = app.tabs[i].state.doc.version();
        app.update_parameter_info(true);
        let first = app.param_info.clone().unwrap();
        assert_eq!(first.signatures.len(), 2);
        assert_eq!(first.active, Some(1));
        app.update_parameter_info(true);
        let second = app.param_info.clone().unwrap();
        assert_eq!(second.active_signature, (first.active_signature + 1) % 2);
        assert_eq!(
            second.label,
            second.signatures[second.active_signature].label
        );
        assert_eq!(
            second.params,
            second.signatures[second.active_signature].params
        );
        assert_eq!(second.active, Some(1));
        let offset = source.rfind("'a'").unwrap() + 1;
        app.tabs[i]
            .state
            .set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
        app.update_parameter_info(false);
        assert_eq!(
            app.param_info.as_ref().unwrap().active_signature,
            second.active_signature
        );
        assert_eq!(app.param_info.as_ref().unwrap().active, Some(0));
        app.tabs[i]
            .state
            .set_carets(vec![Caret::at(TextSize::new(source.len() as u32))]);
        app.update_parameter_info(false);
        assert!(app.param_info.is_none());
        assert!(app.parameter_info_ui.key.is_none());
        assert_eq!(app.tabs[i].state.doc.text(), source);
        assert_eq!(app.tabs[i].state.doc.version(), version);
        assert!(!app.tabs[i].state.doc.can_undo());
    }
    #[test]
    fn actual_egui_alt_arrows_cycle_wrap_and_plain_arrow_is_not_consumed() {
        let (_dir, mut app, source) = fixture();
        app.update_parameter_info(true);
        let initial = app.param_info.as_ref().unwrap().active_signature;
        let ctx = context();
        frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
        let output = frame(
            &mut app,
            &ctx,
            0.02,
            key(egui::Key::ArrowDown, egui::Modifiers::ALT),
        );
        assert_eq!(
            app.param_info.as_ref().unwrap().active_signature,
            (initial + 1) % 2
        );
        output.drop_without_applying_deltas();
        frame(
            &mut app,
            &ctx,
            0.04,
            key(egui::Key::ArrowUp, egui::Modifiers::ALT),
        )
        .drop_without_applying_deltas();
        assert_eq!(app.param_info.as_ref().unwrap().active_signature, initial);
        let mut retained = false;
        ctx.run_ui(
            egui::RawInput {
                events: key(egui::Key::ArrowDown, egui::Modifiers::NONE),
                time: Some(0.06),
                ..Default::default()
            },
            |ui| {
                app.parameter_info_keys(ui);
                retained = ui.input(|input| input.key_pressed(egui::Key::ArrowDown));
            },
        )
        .drop_without_applying_deltas();
        assert!(retained);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
        assert!(!app.tabs[app.active.unwrap()].state.doc.can_undo());
    }
    #[test]
    fn actual_popup_buttons_cycle_and_selected_parameter_has_accent_sections() {
        let (_dir, mut app, _) = fixture();
        app.update_parameter_info(true);
        let initial = app.param_info.as_ref().unwrap().active_signature;
        let ctx = context();
        frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
        let output = frame(&mut app, &ctx, 0.02, vec![]);
        let rect = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "›" => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .expect("actual next-overload button painted");
        let active_label = app.param_info.as_ref().unwrap().label.clone();
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == active_label && text.galley.job.sections.iter().any(|section| section.format.color == app.theme.accent && text.galley.job.text[section.byte_range.start.0..section.byte_range.end.0].contains("count")))));
        output.drop_without_applying_deltas();
        let event = |pressed| egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &mut app,
            &ctx,
            0.04,
            vec![egui::Event::PointerMoved(rect.center()), event(true)],
        )
        .drop_without_applying_deltas();
        frame(&mut app, &ctx, 0.06, vec![event(false)]).drop_without_applying_deltas();
        assert_eq!(
            app.param_info.as_ref().unwrap().active_signature,
            (initial + 1) % 2
        );
    }
    #[test]
    fn changed_call_identity_or_removed_overload_discards_manual_choice() {
        let mut state = ParameterInfoUi::default();
        let doc = rustpy_text::Document::new("");
        let key = CallKey {
            document: doc.identity(),
            start: TextSize::new(0),
            name: "f".into(),
        };
        let signatures = vec![
            SignatureLabel {
                label: "f(a)".into(),
                params: vec![(2, 3)],
                active: Some(0),
            },
            SignatureLabel {
                label: "f(b)".into(),
                params: vec![(2, 3)],
                active: Some(0),
            },
        ];
        let mut help = SignatureHelp {
            label: signatures[0].label.clone(),
            params: signatures[0].params.clone(),
            active: Some(0),
            docstring: None,
            overloads: 2,
            signatures,
            active_signature: 0,
        };
        state.update(key.clone(), &mut help, false);
        state.cycle(&mut help, -1);
        assert_eq!(help.active_signature, 1);
        help.signatures.remove(1);
        select(&mut help, 0);
        state.update(key, &mut help, false);
        assert!(state.manual.is_none());
        state.update(
            CallKey {
                document: doc.identity(),
                start: TextSize::new(8),
                name: "f".into(),
            },
            &mut help,
            true,
        );
        assert!(state.manual.is_none());
        assert_eq!(help.active_signature, 0);
    }
}

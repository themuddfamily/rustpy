//! Inline action and shared preview bridge.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_refactor::inline::{self, InlineModel, InlineOptions};
pub(super) struct InlineState {
    view: egui::Id,
    document: rustpy_text::DocumentId,
    version: u64,
    offset: TextSize,
    model: InlineModel,
    options: InlineOptions,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_inline(&mut self) {
        let Some(i) = self.active else { return };
        if self.non_text_source_hidden() {
            self.set_status("Choose Source before inlining");
            return;
        }
        self.finish_inplace_rename();
        self.sync_analysis_now();
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Inline requires a Python source file in a project");
            return;
        };
        let offset = self.tabs[i].state.primary().head();
        match inline::model(&analysis, &file, offset) {
            Ok(model) => {
                let tab = &self.tabs[i];
                self.dialog = Dialog::Inline(Box::new(InlineState {
                    view: tab.view.id,
                    document: tab.state.doc.identity(),
                    version: tab.state.doc.version(),
                    offset,
                    model,
                    options: InlineOptions::default(),
                    error: None,
                }));
            }
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn inline_dialog(&mut self, ctx: &egui::Context, mut state: Box<InlineState>) {
        let mut open = true;
        let mut cancel = false;
        let mut apply = false;
        let mut preview = false;
        egui::Window::new("Inline").collapsible(false).default_width(500.0).open(&mut open).show_modal(ctx,|ui|{
            ui.heading(format!("Inline {} '{}'",state.model.kind,state.model.name));
            if state.model.supports_this {ui.radio_value(&mut state.options.this_only,false,"All usages / invocations");ui.radio_value(&mut state.options.this_only,true,"This usage / invocation only");ui.checkbox(&mut state.options.keep_declaration,"Keep declaration");}
            else{ui.label("Inline the common immutable argument/default value and remove this parameter from its declaration and calls.");}
            if let Some(error)=&state.error{ui.colored_label(self.theme.error,error);}
            ui.horizontal(|ui|{apply=ui.button("Do Refactor").clicked();preview=ui.button("Preview").clicked();cancel=ui.button("Cancel").clicked();});
        });
        if apply || preview {
            let Some(i) = self.tabs.iter().position(|tab| {
                tab.view.id == state.view
                    && tab.state.doc.identity() == state.document
                    && tab.state.doc.version() == state.version
            }) else {
                self.set_status("Inline source changed; reopen the action");
                return;
            };
            self.active = Some(i);
            if !self.refresh_file_permissions(i) {
                self.dialog = Dialog::Inline(state);
                return;
            }
            self.sync_analysis_now();
            let Some((analysis, file)) = self.analysis_file(i) else {
                return;
            };
            let files = self.project_files();
            match inline::plan(&analysis, &file, state.offset, &files, state.options) {
                Ok(plan) => {
                    if preview {
                        self.preview_multi(plan)
                    } else {
                        self.apply_multi(Ok(plan));
                    }
                }
                Err(error) => {
                    state.error = Some(error);
                    self.dialog = Dialog::Inline(state);
                }
            }
        } else if open && !cancel && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::Inline(state)
        } else {
            self.focus_editor = true;
        }
    }
}

//! Convert refactorings use the shared stale-checked preview and grouped Undo.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_refactor::conversions::{self, ConversionKind, ConversionOptions};
pub(super) struct ConversionState {
    view: egui::Id,
    document: rustpy_text::DocumentId,
    version: u64,
    offset: TextSize,
    options: ConversionOptions,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_conversion(&mut self, kind: ConversionKind) {
        let Some(i) = self.active else { return };
        if self.non_text_source_hidden() {
            self.set_status("Choose Source before converting");
            return;
        }
        self.finish_inplace_rename();
        let tab = &self.tabs[i];
        self.dialog = Dialog::Conversion(Box::new(ConversionState {
            view: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            offset: tab.state.primary().head(),
            options: ConversionOptions {
                kind,
                ..Default::default()
            },
            error: None,
        }));
    }
    pub(super) fn conversion_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<ConversionState>,
    ) {
        let mut open = true;
        let mut cancel = false;
        let mut preview = false;
        let mut apply = false;
        egui::Window::new("Convert").open(&mut open).collapsible(false).default_width(560.0).show_modal(ctx,|ui|{
            egui::ComboBox::from_id_salt("conversion-kind").selected_text(state.options.kind.label()).show_ui(ui,|ui|{for kind in ConversionKind::ALL {ui.selectable_value(&mut state.options.kind,*kind,kind.label());}}).combo_named(crate::i18n::tr_static("Conversion"));
            match state.options.kind {
                ConversionKind::LocalToField|ConversionKind::MethodFunction|ConversionKind::EncapsulateField|ConversionKind::ConstructorFactory|ConversionKind::IntroduceAlias=>{ui.label("Name (blank uses the suggested name)");ui.text_edit_singleline(&mut state.options.name);},
                ConversionKind::FunctionMethod=>{ui.label("Destination class in this file (the first function parameter becomes the receiver)");ui.text_edit_singleline(&mut state.options.target_class);},
                ConversionKind::ExplicitArgs=>{ui.label("Comma-separated explicit parameter names (blank generates names from the observed arity)");ui.text_edit_singleline(&mut state.options.name);},
                ConversionKind::MigrateApi=>{ui.label("One fully-qualified old=new API mapping per line");ui.text_edit_multiline(&mut state.options.migration);},
                ConversionKind::Comprehension=>{egui::ComboBox::from_id_salt("conversion-collection").selected_text(&state.options.collection).show_ui(ui,|ui|{for name in ["tuple","list","set","dict"]{ui.selectable_value(&mut state.options.collection,name.into(),crate::i18n::tr_static(name));}}).combo_named(crate::i18n::tr_static("Collection type"));},
                _=>{}
            }
            ui.label("Preview includes affected usages. Unsafe binding or evaluation changes are reported as conflicts.");
            if let Some(error)=&state.error{ui.colored_label(self.theme.error,error);}
            ui.horizontal(|ui|{preview=ui.button("Preview").clicked();apply=ui.button("Do Refactor").clicked();cancel=ui.button("Cancel").clicked();});
        });
        if preview || apply {
            let Some(i) = self.tabs.iter().position(|tab| {
                tab.view.id == state.view
                    && tab.state.doc.identity() == state.document
                    && tab.state.doc.version() == state.version
            }) else {
                self.set_status("Conversion source changed; reopen the action");
                return;
            };
            self.active = Some(i);
            if !self.refresh_file_permissions(i) {
                self.dialog = Dialog::Conversion(state);
                return;
            }
            if state.options.kind == ConversionKind::RenameModule {
                self.handle_action(Action::RenameModule, ctx);
                return;
            }
            self.sync_analysis_now();
            let Some((analysis, file)) = self.analysis_file(i) else {
                return;
            };
            let files = self.project_files();
            let root = self
                .project
                .as_ref()
                .map(|p| p.root.clone())
                .unwrap_or_default();
            let result = if state.options.kind == ConversionKind::IntroduceAlias {
                let binding = file
                    .scopes
                    .binding_at(state.offset)
                    .map(|(id, _)| id)
                    .or_else(|| {
                        file.scopes
                            .reference_at(state.offset)
                            .and_then(|reference| reference.resolved)
                    });
                binding
                    .and_then(|id| {
                        rustpy_analysis::intentions::import_actions::alias_import(
                            &file,
                            id,
                            &state.options.name,
                        )
                    })
                    .map(|action| {
                        vec![rustpy_refactor::refactor2::FileOp::Edit(
                            file.path.clone(),
                            action.edits,
                        )]
                    })
                    .ok_or_else(|| "Select an import binding and a non-capturing alias".into())
            } else {
                conversions::plan(
                    &analysis,
                    &file,
                    state.offset,
                    &files,
                    &root,
                    &state.options,
                )
            };
            match result {
                Ok(ops) => {
                    if preview {
                        self.preview_file_ops(ops, state.options.kind.label())
                    } else {
                        self.apply_file_ops(ops, state.options.kind.label());
                    }
                }
                Err(error) => {
                    state.error = Some(error);
                    self.dialog = Dialog::Conversion(state);
                }
            }
        } else if open && !cancel && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::Conversion(state)
        } else {
            self.focus_editor = true;
        }
    }
}

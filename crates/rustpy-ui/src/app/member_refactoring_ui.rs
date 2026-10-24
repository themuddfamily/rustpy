//! Member selection is bound to its captured class, never the subsequently active tab.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_refactor::members::{self, Catalogue, Direction, Selection};

pub(super) struct MemberState {
    catalogue: Catalogue,
    kind: MemberRefactoring,
    selections: Vec<(bool, bool)>,
    destinations: Vec<bool>,
    editors: Vec<(PathBuf, rustpy_text::Document, u64)>,
    project_files: Vec<PathBuf>,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_member_refactoring(&mut self, kind: MemberRefactoring) {
        self.sync_open_tabs_now();
        let Some(index) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        let mut project_files = self.project_files();
        project_files.sort();
        project_files.dedup();
        let catalogue = match members::catalogue(
            &analysis,
            &file,
            self.tabs[index].state.primary().head(),
            &project_files,
        ) {
            Ok(catalogue) => catalogue,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        if self
            .project
            .as_ref()
            .is_none_or(|project| !catalogue.file.path.starts_with(&project.root))
        {
            self.set_status(
                "Member refactorings require a writable project class, not an external library",
            );
            return;
        }
        if catalogue.members.is_empty() {
            self.set_status("This class has no directly declared methods or class attributes to move; move __init__ with its instance-field stores");
            return;
        }
        let count = match kind {
            MemberRefactoring::PullUp => catalogue.bases.len(),
            MemberRefactoring::PushDown => catalogue.subclasses.len(),
        };
        if count == 0 {
            self.set_status(match kind {
                MemberRefactoring::PullUp => "This class has no writable superclass",
                MemberRefactoring::PushDown => "This class has no project subclasses",
            });
            return;
        }
        let mut destinations = vec![kind == MemberRefactoring::PushDown; count];
        if kind == MemberRefactoring::PullUp {
            destinations[0] = true;
        }
        let selections = vec![(false, false); catalogue.members.len()];
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
        self.dialog = Dialog::Members(Box::new(MemberState {
            catalogue,
            kind,
            selections,
            destinations,
            editors,
            project_files,
            error: None,
        }));
    }
    pub(super) fn make_member_abstract(&mut self) {
        self.sync_open_tabs_now();
        let Some(index) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        match members::make_abstract(&analysis, &file, self.tabs[index].state.primary().head()) {
            Ok(plan) => self.preview_single_refactoring(plan),
            Err(error) => self.set_status(format!("Cannot make abstract: {error}")),
        }
    }
    pub(super) fn draw_member_refactoring(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<MemberState>,
    ) {
        let (mut open, mut go, mut cancel) = (true, false, false);
        let pull = state.kind == MemberRefactoring::PullUp;
        egui::Window::new(if pull{"Pull Members Up"}else{"Push Members Down"}).default_width(650.0).resizable(true).open(&mut open).show_modal(ctx,|ui|{
            ui.label(format!("{} · {}",state.catalogue.source.name,state.catalogue.file.path.display()));
            ui.label(if pull{"Select an actual superclass destination"}else{"Select the direct subclasses that will receive the members"});
            let destinations=if pull{&state.catalogue.bases}else{&state.catalogue.subclasses};
            let mut selected=None;
            for (index,destination) in destinations.iter().enumerate(){
                let label=format!("{} — {} (byte {})",destination.class.name,destination.path.display(),u32::from(destination.class.range.start()));
                if pull {if ui.radio(state.destinations[index],label).clicked(){selected=Some(index);}}
                else{ui.checkbox(&mut state.destinations[index],label);}
            }
            if let Some(index)=selected{state.destinations.fill(false);state.destinations[index]=true;}
            ui.separator();
            egui::Grid::new("member_selection").striped(true).show(ui,|ui|{
                ui.strong("Move member");ui.strong(if pull{"Abstract declaration in superclass"}else{"Retain abstract declaration here"});ui.end_row();
                for (index,member) in state.catalogue.members.iter().enumerate(){
                    ui.checkbox(&mut state.selections[index].0,&member.name);
                    ui.add_enabled(member.method&&state.selections[index].0,egui::Checkbox::new(&mut state.selections[index].1,"Abstract"));ui.end_row();
                }
            });
            ui.label(if pull{"Abstract conversion keeps the concrete implementation in the source class. Concrete selection moves the complete declaration."}else{"Abstract conversion keeps a signature contract in the source class; selected subclasses receive the implementations. Unselected inheritance and base-receiver uses are checked."});
            ui.small("Instance fields move with their declaring __init__; shared/destructuring assignments, private-mangling and owner-dependent super() require explicit source cleanup first.");
            if let Some(error)=&state.error{ui.colored_label(self.theme.error,error);}
            ui.horizontal(|ui|{
                go=ui.add_enabled(state.selections.iter().any(|(on,_)|*on)&&state.destinations.iter().any(|on|*on),egui::Button::new("Preview Changes")).clicked();
                cancel=ui.button("Cancel").clicked()||ui.input(|i|i.key_pressed(egui::Key::Escape));
            });
        });
        if go {
            self.sync_open_tabs_now();
            let result = (|| {
                let analysis = self.analysis_handle().ok_or("Project closed")?;
                let mut files = self.project_files();
                files.sort();
                files.dedup();
                if files != state.project_files {
                    return Err("Project catalogue changed; reopen the member chooser".to_owned());
                }
                for (path, document, version) in &state.editors {
                    let Some(index) = self.tab_of(path) else {
                        return Err("Editors changed; reopen the member chooser".to_owned());
                    };
                    if self.tabs[index].state.doc.identity() != document.identity()
                        || document.version() != *version
                    {
                        return Err("An editor changed; reopen the member chooser".to_owned());
                    }
                }
                let selected: Vec<_> = state
                    .selections
                    .iter()
                    .zip(&state.catalogue.members)
                    .filter(|((on, _), _)| *on)
                    .map(|((_, abstract_declaration), member)| Selection {
                        name: member.name.clone(),
                        abstract_declaration: *abstract_declaration,
                    })
                    .collect();
                let destinations = if pull {
                    &state.catalogue.bases
                } else {
                    &state.catalogue.subclasses
                };
                let targets = destinations
                    .iter()
                    .zip(&state.destinations)
                    .filter(|(_, on)| **on)
                    .map(|(destination, _)| destination.class.clone())
                    .collect::<Vec<_>>();
                members::plan(
                    &analysis,
                    &state.catalogue,
                    if pull {
                        Direction::PullUp
                    } else {
                        Direction::PushDown
                    },
                    &selected,
                    &targets,
                )
            })();
            match result {
                Ok(plan) => self.preview_multi(plan),
                Err(error) => {
                    state.error = Some(error);
                    self.dialog = Dialog::Members(state);
                }
            }
        } else if open && !cancel {
            self.dialog = Dialog::Members(state);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_member_dialog_captures_physical_destinations_and_renders_abstract_choices() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        let source="class Base:\n    pass\nclass Child(Base):\n    limit = 2\n    def run(self):\n        return self.limit\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.find("Child").unwrap() as u32,
            ))]);
        app.open_member_refactoring(MemberRefactoring::PullUp);
        let Dialog::Members(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("member chooser missing")
        };
        assert_eq!(state.catalogue.source.name.as_str(), "Child");
        assert_eq!(state.catalogue.bases.len(), 1);
        assert_eq!(state.catalogue.bases[0].class.name.as_str(), "Base");
        assert!(state
            .catalogue
            .members
            .iter()
            .any(|member| member.name == "limit" && !member.method));
        assert!(state
            .catalogue
            .members
            .iter()
            .any(|member| member.name == "run" && member.method));
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut state = Some(state);
        let mut shapes = Vec::new();
        // The first Area frame measures the window without painting its text.
        // Render the retained production state again for visible controls.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.draw_member_refactoring(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            // Retire CPU-only atlas updates before assertions can unwind.
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::Members(retained) = std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.catalogue.source.name.as_str(), "Child");
                assert_eq!(retained.catalogue.bases.len(), 1);
                assert_eq!(retained.catalogue.bases[0].class.name.as_str(), "Base");
                state = Some(retained);
            }
        }
        for label in [
            "Move member",
            "Abstract declaration in superclass",
            "Preview Changes",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text)if text.galley.text().contains(label))),"{label}");
        }
        assert_eq!(app.tabs[index].state.doc.text(), source);
    }
}

use super::*;

#[test]
fn implementation_super_and_type_actions_use_actual_navigation_chooser() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.py");
    let source = "class Base:\n    def run(self): pass\nclass Middle(Base):\n    def run(self): pass\nclass Child(Middle):\n    def run(self): pass\ndef use(value:Base, child:Child, either:Base|Child):\n    value.run()\n    child.run()\n    either\n";
    std::fs::write(&path, source).unwrap();
    let mut app = IdeApp::new_welcome(Settings::default());
    app.open_project(directory.path());
    app.open_file(&path);
    let context = egui::Context::default();
    for (needle, inside, action, count) in [
        ("value.run", 7, Action::GotoImplementation, 2),
        ("child.run", 7, Action::GotoSuperMethod, 2),
        ("    either\n", 6, Action::GoToTypeDeclaration, 2),
    ] {
        let index = app.active.unwrap();
        let offset = TextSize::new((source.find(needle).unwrap() + inside) as u32);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(offset)]);
        app.handle_action(action, &context);
        assert_eq!(app.location_choices.len(), count, "{action:?}");
        assert!(
            app.choice.is_some(),
            "multiple actual targets must have a chooser"
        );
        assert_eq!(app.tabs[index].state.doc.text(), source);
        app.choice = None;
    }
}

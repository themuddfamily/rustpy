//! Actual App action dispatch and declaration-versus-usages behavior.
use super::*;
fn project(source: &str) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let mut app = IdeApp::new_welcome(Settings::default());
    app.open_project(directory.path());
    app.open_file(&path);
    (directory, app, egui::Context::default())
}
#[test]
fn actual_declaration_action_opens_target_then_shows_usages_at_definition() {
    let source = "def target(): pass\ntarget()\ntarget()\n";
    let (_directory, mut app, context) = project(source);
    let index = app.active.unwrap();
    let offset = TextSize::new(source.rfind("target").unwrap() as u32 + 2);
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(offset)]);
    app.handle_action(Action::GoToDeclaration, &context);
    let index = app.active.unwrap();
    assert_eq!(
        app.tabs[index].state.primary().range(),
        TextRange::new(TextSize::new(4), TextSize::new(10))
    );
    app.handle_action(Action::GoToDeclaration, &context);
    let popup = app.choice.as_ref().expect("Show Usages popup");
    assert!(matches!(popup.kind, ChoiceKind::Nav));
    assert!(popup.title.starts_with("Usages of 'target'"));
    assert_eq!(popup.items.len(), 3);
    assert_eq!(app.tabs[index].state.doc.text(), source);
    assert!(!app.tabs[index].state.doc.can_undo());
}
#[test]
fn actual_source_stub_action_and_preference_navigate_matching_physical_files() {
    let (_directory, mut app, context) = project("from api import run\nrun(1)\n");
    let root = app.project.as_ref().unwrap().root.clone();
    let stub = root.join("api.pyi");
    let source = root.join("api.py");
    std::fs::write(&stub, "def run(value:int)->int: ...\n").unwrap();
    std::fs::write(&source, "def run(value):\n    return value\n").unwrap();
    let index = app.active.unwrap();
    let text = app.tabs[index].state.doc.text();
    let offset = TextSize::new(text.rfind("run").unwrap() as u32 + 1);
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(offset)]);
    app.settings.editor.prefer_source_navigation = false;
    app.handle_action(Action::GoToDeclaration, &context);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_deref(),
        Some(stub.as_path())
    );
    app.handle_action(Action::GoToSourceOrStub, &context);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_deref(),
        Some(source.as_path())
    );
    app.handle_action(Action::GoToSourceOrStub, &context);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_deref(),
        Some(stub.as_path())
    );
}
fn template_project(files: &[(&str, &str)]) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let directory = tempfile::tempdir().unwrap();
    for (name, text) in files {
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut app = IdeApp::new_welcome(Settings::default());
    app.open_project(directory.path());
    (directory, app, egui::Context::default())
}
fn declaration_at(app: &mut IdeApp, context: &egui::Context, file: &Path, needle: &str) {
    app.open_file(file);
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].state.language, Language::Template);
    let text = app.tabs[index].state.doc.text();
    let offset = TextSize::new((text.find(needle).unwrap() + 2) as u32);
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(offset)]);
    app.handle_action(Action::GoToDeclaration, context);
}
#[test]
fn declaration_action_follows_template_extends_include_and_blocks_to_app_and_settings_dirs() {
    let child = "{% extends \"shop/base.html\" %}\n{% block content %}{% include 'partials/nav.html' %}{% endblock %}\n";
    let (directory, mut app, context) = template_project(&[
        (
            "manage.py",
            "import os\nos.environ.setdefault('DJANGO_SETTINGS_MODULE', 'site.settings')\n",
        ),
        (
            "site/settings.py",
            "BASE_DIR = None\nTEMPLATES = [{'DIRS': [BASE_DIR / 'frontend'], 'APP_DIRS': True}]\n",
        ),
        // App templates directory (APP_DIRS) holds the parent template.
        (
            "shop/templates/shop/base.html",
            "<html>\n{% block content %}{% endblock %}\n</html>\n",
        ),
        ("shop/templates/shop/list.html", child),
        // A settings DIRS root outside any `templates` folder.
        ("frontend/partials/nav.html", "<nav></nav>\n"),
    ]);
    let root = app.project.as_ref().unwrap().root.clone();
    let list = root.join("shop/templates/shop/list.html");
    declaration_at(&mut app, &context, &list, "shop/base.html");
    let index = app.active.unwrap();
    assert_eq!(
        app.tabs[index].state.path.as_deref(),
        Some(root.join("shop/templates/shop/base.html").as_path())
    );
    declaration_at(&mut app, &context, &list, "partials/nav.html");
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_deref(),
        Some(root.join("frontend/partials/nav.html").as_path())
    );
    // `{% block %}` goes to the parent template's block of the same name.
    declaration_at(&mut app, &context, &list, "content %}{% include");
    let index = app.active.unwrap();
    assert_eq!(
        app.tabs[index].state.path.as_deref(),
        Some(root.join("shop/templates/shop/base.html").as_path())
    );
    let parent = app.tabs[index].state.doc.text();
    let block = TextSize::new(parent.find("content").unwrap() as u32);
    assert_eq!(app.tabs[index].state.primary().range().start(), block);
    drop(directory);
}
#[test]
fn flask_template_editor_receives_project_inputs_and_reports_jinja_problems() {
    let (directory, mut app, _context) = template_project(&[
        (
            "app.py",
            "from flask import Flask, render_template\napp = Flask(__name__)\n@app.route('/')\ndef index():\n    return render_template('index.html')\n",
        ),
        (
            "templates/macros.html",
            "{% macro field(name) %}<input name=\"{{ name }}\">{% endmacro %}\n",
        ),
        (
            "templates/index.html",
            "{% from \"macros.html\" import missing, field %}\n<p>{{ user.name|upperx }} {{ user.name|upper }}</p>\n",
        ),
    ]);
    let root = app.project.as_ref().unwrap().root.clone();
    app.open_file(&root.join("templates/index.html"));
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].state.language, Language::Template);
    // The per-frame sync is what hands a template editor its project inputs.
    app.sync_analysis(1.0);
    assert_eq!(
        app.tabs[index].state.framework_inputs.root.as_deref(),
        Some(root.as_path())
    );
    let messages: Vec<_> = app.tabs[index]
        .state
        .diagnostics()
        .into_iter()
        .filter(|d| d.code == "JinjaTemplate")
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "Undefined macro 'missing' in 'macros.html'",
            "Unknown Jinja2 filter 'upperx'",
        ],
    );
    drop(directory);
}

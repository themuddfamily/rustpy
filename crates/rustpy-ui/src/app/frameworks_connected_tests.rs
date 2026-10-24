//! Source fixtures: final verification owns execution.
#[cfg(target_os = "linux")]
mod data_provider_acceptance;
use super::*;
#[test]
fn canonical_facet_reaches_project_settings_and_all_launch_environment_consumers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("site")).unwrap();
    std::fs::write(
        root.path().join("manage.py"),
        "import os\nos.environ.setdefault('DJANGO_SETTINGS_MODULE','site.settings')\n",
    )
    .unwrap();
    std::fs::write(root.path().join("site/settings.py"),"DEBUG=False\nDATABASES={'default':{'ENGINE':'django.db.backends.sqlite3','NAME':'local.sqlite3'}}\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_frameworks();
    assert!(matches!(app.dialog, Dialog::Frameworks));
    assert_eq!(app.frameworks.facet.settings(root.path()), "site.settings");
    app.frameworks.facet.django = true;
    app.frameworks.facet.use_django_test_runner = false;
    app.frameworks.facet.settings_module = "site.custom_settings".into();
    assert!(app.save_framework_facet());
    let persisted = rustpy_project::ProjectModel::load(root.path());
    assert_eq!(persisted.frameworks.settings_module, "site.custom_settings");
    let mut cfg = RunConfig::default();
    app.framework_run_policy(&mut cfg);
    assert_eq!(cfg.env["DJANGO_SETTINGS_MODULE"], "site.custom_settings");
    cfg.env
        .insert("DJANGO_SETTINGS_MODULE".into(), "explicit.settings".into());
    app.framework_run_policy(&mut cfg);
    assert_eq!(cfg.env["DJANGO_SETTINGS_MODULE"], "explicit.settings");
}
#[test]
fn create_template_fix_uses_new_file_transaction_and_refuses_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().to_path_buf()));
    app.framework_create_template("pages/example.html");
    let path = root.path().join("templates/pages/example.html");
    let before = std::fs::read_to_string(&path).unwrap();
    app.framework_create_template("pages/example.html");
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    app.framework_create_template("../outside.html");
    assert!(!root.path().join("outside.html").exists());
}

// Explicit central acceptance only: these tests import real, admitted providers.
// Serial execution also keeps the production catalogue/document single-flight
// workers independent of one another; missing providers are failures, not skips.
static CONFIGURED_PROVIDERS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn admitted_framework_python(modules: &str) -> PathBuf {
    let selected = PathBuf::from(
        std::env::var_os("RUSTPY_FRAMEWORK_PYTHON")
            .expect("set RUSTPY_FRAMEWORK_PYTHON to the explicitly admitted provider interpreter"),
    );
    assert!(
        selected.is_absolute(),
        "admitted interpreter must be absolute"
    );
    assert!(selected.is_file(), "admitted interpreter exists");
    // A venv's executable may be a symlink to system Python. Preserve its
    // selected spelling so Python still discovers that venv's pyvenv.cfg.
    let interpreter = selected;
    let mut command = std::process::Command::new(&interpreter);
    command.args([
        "-I",
        "-c",
        &format!("import {modules}; print('RUSTPY_PROVIDER_READY')"),
    ]);
    let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        std::time::Duration::from_secs(5),
        262144,
    )
    .expect("admitted provider preflight must complete");
    assert!(
        status.success()
            && String::from_utf8_lossy(&out)
                .lines()
                .any(|line| line == "RUSTPY_PROVIDER_READY"),
        "required real providers unavailable: {}",
        String::from_utf8_lossy(&err)
    );
    interpreter
}

#[test]
#[ignore = "requires RUSTPY_FRAMEWORK_PYTHON with real Django; central configured acceptance"]
fn configured_django_catalogue_publishes_template_diagnostics_and_clean_control() {
    let _serial = CONFIGURED_PROVIDERS.lock().unwrap();
    let interpreter = admitted_framework_python("django");
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("templates")).unwrap();
    for (name, source) in [
        ("manage.py", "import os\nos.environ.setdefault('DJANGO_SETTINGS_MODULE','acceptance_settings')\n"),
        ("acceptance_settings.py", "SECRET_KEY='configured-acceptance-only'\nINSTALLED_APPS=[]\nROOT_URLCONF='acceptance_urls'\nDATABASES={'default':{'ENGINE':'django.db.backends.sqlite3','NAME':':memory:'}}\nTEMPLATES=[{'BACKEND':'django.template.backends.django.DjangoTemplates','APP_DIRS':False,'OPTIONS':{'libraries':{'acceptance':'acceptance_tags'}}}]\n"),
        ("acceptance_urls.py", "from django.urls import path\nfrom django.http import HttpResponse\ndef home(request):return HttpResponse('ok')\nurlpatterns=[path('',home,name='acceptance_home')]\n"),
        ("acceptance_tags.py", "from django import template\nregister=template.Library()\n@register.simple_tag\ndef acceptance_tag():return 'ok'\n"),
    ] {
        std::fs::write(root.path().join(name), source).unwrap();
    }
    let path = root.path().join("templates/index.html");
    let source = "{% load acceptance %}{% acceptance_tag %}{% url 'rustpy_missing_url' %}";
    std::fs::write(&path, source).unwrap();
    let catalog = rustpy_project::framework_catalog::capture(
        root.path(),
        &interpreter,
        1,
        &path,
        true,
        &rustpy_core::CancellationToken::new(),
    );
    assert_eq!(catalog.interpreter_valid, Some(true));
    assert!(catalog.installed.as_ref().is_some_and(|items| items
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("django"))));
    assert!(catalog.django_complete && catalog.django_templates);
    assert!(catalog.urls.iter().any(|name| name == "acceptance_home"));
    assert!(catalog
        .libraries
        .iter()
        .any(|(name, tags, _)| name == "acceptance"
            && tags.iter().any(|tag| tag == "acceptance_tag")));
    let mut state = crate::editor::EditorState::from_text(source, Language::Template);
    state.path = Some(path.clone());
    state.framework_inputs = Arc::new(rustpy_analysis::inspections::framework_tools::Inputs {
        root: Some(root.path().into()),
        trusted: true,
        interpreter: Some(interpreter),
        django_complete: catalog.django_complete,
        django_template_engine: catalog.django_templates,
        template_tags: catalog.tags,
        template_filters: catalog.filters,
        template_libraries: catalog.libraries,
        url_names: catalog.urls,
        settings_keys: catalog.settings,
        ..Default::default()
    });
    state.doc.set_read_only(true);
    let version = state.doc.version();
    let diagnostics = state.diagnostics();
    let missing = diagnostics
        .iter()
        .filter(|d| d.code == "DjangoTemplate")
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 1, "{diagnostics:?}");
    assert!(missing[0].message.contains("rustpy_missing_url"));
    assert_eq!(state.doc.text(), source);
    assert_eq!(state.doc.version(), version);
    assert!(state.doc.is_read_only());
    let clean = source.replace("rustpy_missing_url", "acceptance_home");
    let mut control = crate::editor::EditorState::from_text(&clean, Language::Template);
    control.path = Some(path.clone());
    control.framework_inputs = state.framework_inputs.clone();
    assert!(!control
        .diagnostics()
        .iter()
        .any(|d| d.code == "DjangoTemplate"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
#[ignore = "requires RUSTPY_FRAMEWORK_PYTHON with real pytest; central configured acceptance"]
fn configured_pytest_catalogue_reaches_actual_analysis_and_preserves_unknown_fixture() {
    let _serial = CONFIGURED_PROVIDERS.lock().unwrap();
    let interpreter = admitted_framework_python("pytest");
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(root.path().join("conftest.py"), "import pytest\n@pytest.fixture\ndef registered_fixture():\n    \"\"\"A real registered fixture.\"\"\"\n    return 1\n").unwrap();
    let path = root.path().join("test_catalogue.py");
    let source = "def test_registered(registered_fixture, rustpy_missing_fixture):\n    pass\n";
    std::fs::write(&path, source).unwrap();
    let db = rustpy_semantic::db::Analysis::for_project(root.path());
    let id = db.set_file_text(&path, source);
    let file = db.file(id).unwrap();
    let revision = db.revision_excluding(Path::new("<framework>"));
    let catalog = rustpy_project::framework_catalog::capture(
        root.path(),
        &interpreter,
        revision,
        &path,
        true,
        &rustpy_core::CancellationToken::new(),
    );
    assert_eq!(catalog.interpreter_valid, Some(true));
    assert!(catalog.fixtures_complete);
    assert!(catalog
        .fixtures
        .iter()
        .any(|name| name == "registered_fixture"));
    assert!(!catalog
        .fixtures
        .iter()
        .any(|name| name == "rustpy_missing_fixture"));
    let inputs = analysis_jobs::AnalysisInputs {
        inspections: InspectionOptions {
            framework: Arc::new(rustpy_analysis::inspections::framework_tools::Inputs {
                root: Some(root.path().into()),
                interpreter: Some(interpreter),
                trusted: true,
                interpreter_checked: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        disabled_inspections: Vec::new(),
        grammar: false,
        grammar_policy: None,
        dictionary: None,
        parameter_hints: false,
        type_hints: false,
        index: Default::default(),
    };
    let result =
        analysis_jobs::analyze_file(&db, &file, &inputs, &rustpy_core::CancellationToken::new())
            .unwrap();
    let unknown = result
        .diagnostics
        .iter()
        .filter(|d| d.code == "PytestUnknownFixture")
        .collect::<Vec<_>>();
    assert_eq!(unknown.len(), 1, "{:?}", result.diagnostics);
    assert!(unknown[0].message.contains("rustpy_missing_fixture"));
    let clean = source.replace(", rustpy_missing_fixture", "");
    std::fs::write(&path, &clean).unwrap();
    let id = db.set_file_text(&path, &clean);
    let control = analysis_jobs::analyze_file(
        &db,
        &db.file(id).unwrap(),
        &inputs,
        &rustpy_core::CancellationToken::new(),
    )
    .unwrap();
    assert!(!control
        .diagnostics
        .iter()
        .any(|d| d.code == "PytestUnknownFixture"));
}

fn configured_rst_diagnostics(
    state: &mut crate::editor::EditorState,
    expect_issues: bool,
) -> Vec<rustpy_core::Diagnostic> {
    let before = rustpy_project::document_tools::generation();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        // The actual editor consumer starts the supervised validator and maps
        // its source-bound results to document diagnostics after publication.
        Arc::make_mut(&mut state.framework_inputs).document_tools_generation =
            rustpy_project::document_tools::generation();
        let diagnostics = state.diagnostics();
        let ready = rustpy_project::document_tools::generation() > before;
        let found = diagnostics.iter().any(|d| d.code == "RstInspection");
        if ready && found == expect_issues {
            return diagnostics;
        }
        if std::time::Instant::now() >= deadline {
            // Inspect the public provider boundary only on failure; priming it
            // earlier could conceal a failure to start work from the editor.
            let inputs = &state.framework_inputs;
            let provider = rustpy_project::document_tools::inspect(
                &state.doc.text(),
                state.path.as_deref().unwrap(),
                inputs.interpreter.as_deref(),
                inputs.sql_dialect.as_deref(),
                inputs.check_unused_ignore_patterns,
                inputs.trusted,
                inputs.project_revision,
            );
            let manager = rustpy_core::TaskManager::global();
            panic!(
                "real RST validator did not publish expected={expect_issues}, path={:?}, interpreter={:?}, trusted={}, level={:?}, generation={} -> {}, provider={provider:?}, running={:?}, finished={:?}: {diagnostics:?}",
                state.path, inputs.interpreter, inputs.trusted, state.inspection_level,
                before, rustpy_project::document_tools::generation(),
                manager.running_all(), manager.finished()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires RUSTPY_FRAMEWORK_PYTHON with real Sphinx and Docutils; central configured acceptance"]
fn configured_sphinx_and_docutils_publish_source_bound_editor_diagnostics() {
    let _serial = CONFIGURED_PROVIDERS.lock().unwrap();
    let interpreter = admitted_framework_python("sphinx, docutils");
    for sphinx in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (source, clean, needle) = if sphinx {
            std::fs::write(root.path().join("conf.py"), "project='RustPy configured acceptance'\nmaster_doc='index'\nextensions=[]\nfrom sphinx.util.console import coloron\ncoloron()\nfrom docutils import nodes\ndef role(name,rawtext,text,lineno,inliner,options=None,content=None):return [nodes.inline(rawtext,text)],[]\ndef setup(app):app.add_role('rustpy_valid',role)\n").unwrap();
            std::fs::write(root.path().join("other.rst"), "Other\n=====\n").unwrap();
            (
                "Index\n=====\n\n:rustpy_valid:`accepted`\n\n:doc:`rustpy_missing_document`\n",
                "Index\n=====\n\n:rustpy_valid:`accepted`\n\n:doc:`other`\n",
                "rustpy_missing_document",
            )
        } else {
            (
                "Title\n=====\n\n.. rustpy_missing_directive::\n",
                "Title\n=====\n\n.. note:: A valid directive.\n",
                "rustpy_missing_directive",
            )
        };
        let path = root.path().join("index.rst");
        std::fs::write(&path, source).unwrap();
        let mut state = crate::editor::EditorState::from_text(source, Language::Rst);
        state.path = Some(path.clone());
        state.framework_inputs = Arc::new(rustpy_analysis::inspections::framework_tools::Inputs {
            root: Some(root.path().into()),
            interpreter: Some(interpreter.clone()),
            trusted: true,
            project_revision: 1,
            ..Default::default()
        });
        state.doc.set_read_only(true);
        let version = state.doc.version();
        let diagnostics = configured_rst_diagnostics(&mut state, true);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "RstInspection" && d.message.contains(needle)),
            "{diagnostics:?}"
        );
        if sphinx {
            // The real configured Sphinx producer deliberately enables colour;
            // editor diagnostics must contain source text, not terminal escapes.
            assert!(diagnostics.iter().all(|d| !d.message.contains('\u{1b}')));
            assert!(
                !diagnostics
                    .iter()
                    .any(|d| d.message.contains("Unknown interpreted text role")
                        && d.message.contains("rustpy_valid")),
                "configured Sphinx role must be loaded: {diagnostics:?}"
            );
        }
        assert_eq!(state.doc.text(), source);
        assert_eq!(state.doc.version(), version);
        assert!(state.doc.is_read_only());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::write(&path, clean).unwrap();
        let mut control = crate::editor::EditorState::from_text(clean, Language::Rst);
        control.path = Some(path);
        let mut inputs = (*state.framework_inputs).clone();
        inputs.project_revision = 2;
        control.framework_inputs = Arc::new(inputs);
        let clean_diagnostics = configured_rst_diagnostics(&mut control, false);
        assert!(!clean_diagnostics.iter().any(|d| d.code == "RstInspection"));
    }
}

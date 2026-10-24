//! Explicit acceptance of the same framework shell action used by the Console UI.
use super::*;
use std::time::{Duration, Instant};

struct ShellFixture {
    app: IdeApp,
    root: tempfile::TempDir,
    interpreter: PathBuf,
}

impl Drop for ShellFixture {
    fn drop(&mut self) {
        self.app.py_console.stop();
    }
}

fn configured_shell(django: bool) -> ShellFixture {
    let interpreter = PathBuf::from(
        std::env::var_os("RUSTPY_FRAMEWORK_PYTHON")
            .expect("set RUSTPY_FRAMEWORK_PYTHON to the admitted Django/Flask SDK"),
    );
    assert!(interpreter.is_absolute() && interpreter.is_file());
    let sdk =
        rustpy_project::interpreter::probe_detailed(&interpreter, "framework-shell-acceptance")
            .expect("admitted SDK must pass the production interpreter probe")
            .interpreter;
    assert_eq!(sdk.path, interpreter, "preserve selected venv spelling");
    let root = tempfile::Builder::new()
        .prefix("framework shell π ")
        .tempdir()
        .unwrap();
    std::fs::create_dir(root.path().join("working directory")).unwrap();
    std::fs::create_dir(root.path().join("shell_models")).unwrap();
    for (name, source) in [
        ("shell_settings.py", "SECRET_KEY='private-acceptance-only'\nINSTALLED_APPS=['shell_models']\nDATABASES={'default':{'ENGINE':'django.db.backends.sqlite3','NAME':':memory:'}}\nDEFAULT_AUTO_FIELD='django.db.models.AutoField'\n"),
        ("shell_models/__init__.py", ""),
        ("shell_models/models.py", "from django.db import models\nclass ShellRecord(models.Model):\n    label=models.CharField(max_length=64)\n"),
        ("shell_app.py", "from flask import Flask\nimport sqlite3\napp=Flask(__name__)\napp.config['SHELL_ACCEPTANCE']='actual-app-context'\n@app.shell_context_processor\ndef shell_context():\n    return {'shell_database':sqlite3.connect(':memory:'),'shell_context_value':'injected-by-flask'}\n"),
    ] {
        std::fs::write(root.path().join(name), source).unwrap();
    }
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.python_console.use_ipython = false;
    settings.python_console.startup_script = "startup_value='captured-startup'".into();
    settings.python_console.working_directory = Some(root.path().join("working directory"));
    settings.python_console.environment.insert(
        "RUSTPY_SHELL_CAPTURE".into(),
        "selected-environment-π".into(),
    );
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    let project = app.project.as_mut().unwrap();
    project.interpreter_rx = None;
    project.interpreter = Some(sdk);
    app.open_frameworks();
    app.frameworks.facet.django = django;
    app.frameworks.facet.auto_detect = false;
    app.frameworks.facet.settings_module = "shell_settings".into();
    app.frameworks.facet.flask_app = "shell_app:app".into();
    assert!(app.save_framework_facet());
    app.framework_shell(django);
    assert!(
        app.py_console.is_running(),
        "{:?}",
        app.py_console.start_error()
    );
    assert!(app
        .py_console
        .names()
        .last()
        .unwrap()
        .starts_with(if django { "Django " } else { "Flask " }));
    ShellFixture {
        app,
        root,
        interpreter,
    }
}

fn execute_snapshot(fixture: &mut ShellFixture, name: &str, code: &str) -> serde_json::Value {
    let path = fixture.root.path().join(name);
    let source = format!(
        "import json, os, sys, traceback\ntry:\n{}\n    _shell_result={{'pid':os.getpid(),'executable':sys.executable,'cwd':os.getcwd(),'environment':os.environ.get('RUSTPY_SHELL_CAPTURE'),'startup':startup_value,'answer':shell_answer}}\nexcept BaseException:\n    _shell_result={{'error':traceback.format_exc()}}\nwith open({},'w',encoding='utf-8') as _shell_file:\n    json.dump(_shell_result,_shell_file,ensure_ascii=False)\n",
        code.lines().map(|line| format!("    {line}")).collect::<Vec<_>>().join("\n"),
        serde_json::to_string(&path.to_string_lossy()).unwrap()
    );
    assert!(fixture.app.py_console.execute_fragment(&source));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        fixture.app.py_console.poll();
        if !fixture.app.py_console.execution_pending() && path.is_file() {
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(result.get("error").is_none(), "{result}");
            assert_eq!(
                result["executable"].as_str().unwrap(),
                fixture.interpreter.to_str().unwrap()
            );
            assert_eq!(
                PathBuf::from(result["cwd"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                fixture
                    .root
                    .path()
                    .join("working directory")
                    .canonicalize()
                    .unwrap()
            );
            assert_eq!(result["environment"], "selected-environment-π");
            assert_eq!(result["startup"], "captured-startup");
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "framework shell result missing; running={}, startup_error={:?}",
            fixture.app.py_console.is_running(),
            fixture.app.py_console.start_error()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop_and_assert_reaped(fixture: &mut ShellFixture, pid: u64) {
    assert!(
        PathBuf::from(format!("/proc/{pid}")).exists(),
        "actual helper must be alive before Stop"
    );
    fixture.app.py_console.stop();
    assert!(!fixture.app.py_console.is_running());
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "Stop must physically reap the helper"
    );
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit RUSTPY_FRAMEWORK_PYTHON with real Django; central provider acceptance"]
fn configured_django_shell_action_initializes_models_database_and_selected_launch_context() {
    let mut fixture = configured_shell(true);
    let first = execute_snapshot(&mut fixture, "django-first.json", "from django.apps import apps\nfrom django.db import connection\nassert apps.ready\nassert ShellRecord is apps.get_model('shell_models','ShellRecord')\nwith connection.schema_editor() as schema:\n    schema.create_model(ShellRecord)\nShellRecord.objects.create(label='actual-model-π')\nshell_answer=ShellRecord.objects.get().label");
    assert_eq!(first["answer"], "actual-model-π");
    let second = execute_snapshot(
        &mut fixture,
        "django-second.json",
        "shell_answer=ShellRecord.objects.count()+40",
    );
    assert_eq!(second["answer"], 41);
    assert_eq!(
        first["pid"], second["pid"],
        "persistent shell namespace and database"
    );
    stop_and_assert_reaped(&mut fixture, first["pid"].as_u64().unwrap());
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit RUSTPY_FRAMEWORK_PYTHON with real Flask; central provider acceptance"]
fn configured_flask_shell_action_pushes_app_context_and_selected_launch_context() {
    let mut fixture = configured_shell(false);
    let first = execute_snapshot(&mut fixture, "flask-first.json", "from flask import current_app, has_app_context\nassert has_app_context()\nassert current_app._get_current_object() is app\nassert app.config['SHELL_ACCEPTANCE']=='actual-app-context'\nassert shell_context_value=='injected-by-flask'\nshell_database.execute('create table records(value text)')\nshell_database.execute(\"insert into records values ('actual-flask-π')\")\nshell_answer=shell_database.execute('select value from records').fetchone()[0]");
    assert_eq!(first["answer"], "actual-flask-π");
    let second = execute_snapshot(&mut fixture, "flask-second.json", "assert current_app._get_current_object() is app\nshell_answer=shell_database.execute('select count(*) from records').fetchone()[0]+40");
    assert_eq!(second["answer"], 41);
    assert_eq!(first["pid"], second["pid"]);
    stop_and_assert_reaped(&mut fixture, first["pid"].as_u64().unwrap());
}

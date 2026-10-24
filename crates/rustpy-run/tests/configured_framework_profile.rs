//! Required-provider acceptance for named framework/test configurations.
//! Run explicitly with RUSTPY_REQUIRE_PROVIDER_TESTS=1 and an absolute
//! RUSTPY_PROVIDER_PYTHON, using --ignored --test-threads=1.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
use rustpy_project::{Interpreter, RunConfig, RunConfigKind};
use rustpy_run::profile::{run_with_profiler_opts, ProfileOptions, ProfileRun, PROFILE_PREFIX};

fn admitted_interpreter() -> Interpreter {
    assert_eq!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
        Ok("1"),
        "explicit required-provider admission is mandatory"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("set RUSTPY_PROVIDER_PYTHON to the provisioned SDK"),
    );
    assert!(
        python.is_absolute() && python.is_file(),
        "admit an absolute installed Python path"
    );
    let probe = Command::new(&python)
        .args([
            "-I",
            "-c",
            "import django, flask, pytest, coverage, yappi; print('FRAMEWORK_PROVIDERS_ADMITTED')",
        ])
        .output()
        .expect("selected provider SDK must launch");
    assert!(
        probe.status.success(),
        "required providers unavailable in {}: {}",
        python.display(),
        String::from_utf8_lossy(&probe.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        "FRAMEWORK_PROVIDERS_ADMITTED"
    );
    rustpy_project::interpreter::probe(&python, "required-framework-provider")
        .expect("selected SDK must report actual interpreter metadata")
}

const WORKLOAD: &str = r#"import os, sys

def framework_hot():
    return sum(i * i for i in range(64))

def never_called():
    raise AssertionError('cold function must not execute')

def finish(label, result):
    assert result == 256032, result
    print('FRAMEWORK_RESULT ' + label + ' ' + str(result), flush=True)
    print('FRAMEWORK_STDERR ' + label, file=sys.stderr, flush=True)
    if os.environ.get('FRAMEWORK_EXPECT_FAILURE') == '1':
        raise RuntimeError('FRAMEWORK_PROFILE_EXPECTED_FAILURE')

def run_workload(label):
    finish(label, sum(framework_hot() for _ in range(3)))
"#;

fn project(root: &Path) {
    std::fs::create_dir_all(root.join("fixtureapp/management/commands")).unwrap();
    for path in [
        "fixtureapp/__init__.py",
        "fixtureapp/management/__init__.py",
        "fixtureapp/management/commands/__init__.py",
    ] {
        std::fs::write(root.join(path), "").unwrap();
    }
    for (path, source) in [
        ("profile_workload.py", WORKLOAD),
        ("settings.py", "SECRET_KEY = 'isolated-profile-fixture'\nINSTALLED_APPS = ['fixtureapp']\nDATABASES = {}\nUSE_TZ = True\n"),
        ("manage.py", "import os, sys\nos.environ.setdefault('DJANGO_SETTINGS_MODULE', 'settings')\nfrom django.core.management import execute_from_command_line\nexecute_from_command_line(sys.argv)\n"),
        ("fixtureapp/management/commands/profile_hot.py", "from django.core.management.base import BaseCommand\nfrom profile_workload import run_workload\nclass Command(BaseCommand):\n    def handle(self, *args, **options):\n        run_workload('django-task')\n"),
        ("fixtureapp/tests.py", "from django.test import SimpleTestCase\nfrom profile_workload import run_workload\nclass ProfileCase(SimpleTestCase):\n    def test_hot(self):\n        run_workload('django-tests')\n"),
        ("test_profile.py", "from profile_workload import run_workload\ndef test_hot():\n    run_workload('pytest')\n"),
        ("test_profile_unittest.py", "from unittest import TestCase\nfrom profile_workload import run_workload\nclass ProfileCase(TestCase):\n    def test_hot(self):\n        run_workload('unittest')\n"),
        // Flask's real CLI resolves and invokes this factory. It exercises real
        // Flask request routing without opening a server socket, then exits
        // deliberately before `flask run` starts its long-lived server loop.
        ("profile_flask.py", "from flask import Flask\nfrom profile_workload import framework_hot, finish\ndef create_app():\n    app = Flask(__name__)\n    @app.get('/hot')\n    def hot_route():\n        return str(framework_hot())\n    with app.test_client() as client:\n        responses = [client.get('/hot') for _ in range(3)]\n        assert all(response.status_code == 200 for response in responses)\n        finish('flask', sum(int(response.data) for response in responses))\n    raise SystemExit(0)\n"),
    ] {
        std::fs::write(root.join(path), source).unwrap();
    }
}

fn configuration(label: &str, root: &Path, python: &Path, fail: bool) -> RunConfig {
    let mut cfg = RunConfig {
        name: format!("required framework profile {label}"),
        working_dir: Some(root.into()),
        interpreter: Some(python.into()),
        ..Default::default()
    };
    cfg.env
        .insert("PYTEST_DISABLE_PLUGIN_AUTOLOAD".into(), "1".into());
    cfg.env.insert("PYTHONUNBUFFERED".into(), "1".into());
    cfg.env.insert(
        "FRAMEWORK_EXPECT_FAILURE".into(),
        if fail { "1" } else { "0" }.into(),
    );
    match label {
        "django-task" | "django-tests" | "flask" => {
            let mut spec = ConfigurationSpec::new(match label {
                "django-task" => ConfigurationType::DjangoTask,
                "django-tests" => ConfigurationType::DjangoTests,
                _ => ConfigurationType::Flask,
            });
            if label == "flask" {
                spec.target = "profile_flask:create_app".into();
                spec.arguments = vec!["--no-reload".into()];
            } else {
                spec.target = "manage.py".into();
                spec.member = if label == "django-task" {
                    "profile_hot"
                } else {
                    "fixtureapp.tests"
                }
                .into();
            }
            cfg.type_options = Some(spec.clone());
            cfg = spec
                .materialize(&cfg)
                .expect("public named framework configuration lowers");
            match (&cfg.kind, label) {
                (RunConfigKind::PythonModule { module }, "flask") => assert_eq!(module, "flask"),
                (RunConfigKind::Python { script }, "django-task" | "django-tests") => {
                    assert_eq!(script, Path::new("manage.py"))
                }
                _ => panic!("unexpected public framework lowering: {:?}", cfg.kind),
            }
        }
        "pytest" => {
            cfg.kind = RunConfigKind::Pytest {
                target: "test_profile.py::test_hot".into(),
                keywords: String::new(),
            };
            cfg.parameters = "-q -s -p no:cacheprovider".into();
        }
        "unittest" => {
            cfg.kind = RunConfigKind::Unittest {
                target: "test_profile_unittest.ProfileCase.test_hot".into(),
            };
        }
        _ => unreachable!(),
    }
    cfg
}

fn retire(run: &mut ProfileRun) -> Vec<(String, bool)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut output = Vec::new();
    while run.is_running() && Instant::now() < deadline {
        output.extend(run.poll());
        std::thread::sleep(Duration::from_millis(20));
    }
    if run.is_running() {
        run.stop();
        let cleanup = Instant::now() + Duration::from_secs(10);
        while run.is_running() && Instant::now() < cleanup {
            output.extend(run.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "framework profiler exceeded its deadline; running={}; output={output:?}",
            run.is_running()
        );
    }
    output.extend(run.poll());
    output
}

fn configured_frameworks(engine: &str, base_id: u64) {
    let interpreter = admitted_interpreter();
    let root = tempfile::tempdir().unwrap();
    project(root.path());
    let options = ProfileOptions {
        engine: engine.into(),
        ..Default::default()
    };
    for (index, label) in ["django-task", "django-tests", "flask", "pytest", "unittest"]
        .into_iter()
        .enumerate()
    {
        for fail in [false, true] {
            let cfg = configuration(label, root.path(), &interpreter.path, fail);
            let id = base_id + index as u64 * 2 + u64::from(fail);
            let mut run =
                run_with_profiler_opts(id, &cfg, Some(&interpreter), root.path(), &options)
                    .expect("real configured framework profiler starts");
            let output = retire(&mut run);
            let exit = if fail { 1 } else { 0 };
            assert_eq!(
                run.exit_code(),
                Some(exit),
                "{engine}/{label}/fail={fail}: {output:?}"
            );
            assert!(run.error().is_none(), "{:?}: {output:?}", run.error());
            assert!(
                output
                    .iter()
                    .any(|(line, stderr)| !stderr
                        && line == &format!("FRAMEWORK_RESULT {label} 256032")),
                "{output:?}"
            );
            assert!(
                output
                    .iter()
                    .any(|(line, stderr)| *stderr && line == &format!("FRAMEWORK_STDERR {label}")),
                "{output:?}"
            );
            assert!(!output
                .iter()
                .any(|(line, _)| line.starts_with(PROFILE_PREFIX.trim_end())));
            if fail {
                assert!(
                    output
                        .iter()
                        .any(|(line, _)| line.contains("FRAMEWORK_PROFILE_EXPECTED_FAILURE")),
                    "framework failure must remain visible: {output:?}"
                );
            }
            let report = run
                .report()
                .expect("actual provider result table even on target failure");
            assert_eq!(report.engine, engine);
            assert_eq!(report.exit_code, Some(exit));
            assert!(!report.sampling);
            let hot = report
                .functions
                .iter()
                .find(|function| {
                    function.name == "framework_hot"
                        && function.file.ends_with("profile_workload.py")
                })
                .expect("actual framework hot function in result table");
            assert_eq!(
                dunce::canonicalize(Path::new(&hot.file)).unwrap(),
                dunce::canonicalize(root.path().join("profile_workload.py")).unwrap()
            );
            assert_eq!(hot.line, 3);
            assert_eq!(hot.ncalls, 3);
            assert_eq!(hot.primitive_calls, 3);
            assert!(hot.tottime.is_finite() && hot.tottime >= 0.0);
            assert!(hot.cumtime.is_finite() && hot.cumtime >= hot.tottime);
            assert!(!hot.callers.is_empty());
            assert!(!report
                .functions
                .iter()
                .any(|function| function.name == "never_called"
                    && function.file.ends_with("profile_workload.py")));
            let hot_index = report.find(&hot.file, hot.line, &hot.name).unwrap();
            assert!(report.project_functions(root.path()).contains(&hot_index));
            assert_eq!(report.sorted_by_tottime().len(), report.functions.len());
            let pstat = report
                .pstat_file
                .as_ref()
                .expect("actual provider pstat artifact");
            assert!(std::fs::metadata(pstat).unwrap().len() > 0);
            eprintln!("REQUIRED_FRAMEWORK_PROFILE engine={engine} target={label} expected_failure={fail} python={} calls={} functions={} pstat={}", interpreter.path.display(), hot.ncalls, report.functions.len(), pstat.display());
        }
    }
    let unsupported = RunConfig {
        kind: RunConfigKind::Program {
            program: interpreter.path.clone(),
            args: vec![
                "-c".into(),
                "raise AssertionError('must not launch')".into(),
            ],
        },
        ..Default::default()
    };
    let error = run_with_profiler_opts(
        base_id + 99,
        &unsupported,
        Some(&interpreter),
        root.path(),
        &options,
    )
    .err()
    .expect("non-Python transport must be refused before spawning");
    assert!(error.to_string().contains("only available for Python"));
}

#[test]
#[ignore = "requires explicitly admitted absolute SDK with Django/Flask/pytest/coverage/yappi"]
fn configured_cprofile_named_framework_and_test_targets() {
    configured_frameworks("cprofile", 73500);
}

#[test]
#[ignore = "requires explicitly admitted absolute SDK with Django/Flask/pytest/coverage/yappi"]
fn configured_yappi_named_framework_and_test_targets() {
    configured_frameworks("yappi", 73600);
}

// Keep this separate from the named task/test cases: this acceptance must
// actually enter Django's development HTTP server and dispatch a socket request.
#[cfg(unix)]
fn configured_django_server(engine: &str, id: u64) {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let interpreter = admitted_interpreter();
    let root = tempfile::tempdir().unwrap();
    project(root.path());
    std::fs::write(root.path().join("settings.py"), "SECRET_KEY = 'owned-server-profile'\nINSTALLED_APPS = []\nDATABASES = {}\nROOT_URLCONF = 'server_urls'\nALLOWED_HOSTS = ['127.0.0.1']\nMIDDLEWARE = []\n").unwrap();
    std::fs::write(
        root.path().join("server_urls.py"),
        r#"import os, signal, threading
from django.http import HttpResponse
from django.urls import path
from profile_workload import framework_hot

def actual_profile_view(request):
    assert request.path == '/owned-profile/'
    result = sum(framework_hot() for _ in range(3))
    assert result == 256032
    with open('served.pid', 'w') as stream:
        stream.write(str(os.getpid()))
    print('DJANGO_SERVER_PROFILE_RESPONSE ' + str(result), flush=True)
    # Signal only this fixture's own server process. Django catches SIGINT's
    # KeyboardInterrupt and returns normally, so both profilers finalize data.
    timer = threading.Timer(1.0, lambda: os.kill(os.getpid(), signal.SIGINT))
    timer.daemon = True
    timer.start()
    return HttpResponse('OWNED_DJANGO_PROFILE=' + str(result), content_type='text/plain')

urlpatterns = [path('owned-profile/', actual_profile_view)]
"#,
    )
    .unwrap();
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reservation.local_addr().unwrap();
    let mut spec = ConfigurationSpec::new(ConfigurationType::DjangoServer);
    spec.target = "manage.py".into();
    spec.arguments = vec![
        address.to_string(),
        "--noreload".into(),
        "--nothreading".into(),
    ];
    let base = RunConfig {
        name: format!("actual Django server {engine}"),
        working_dir: Some(root.path().into()),
        interpreter: Some(interpreter.path.clone()),
        ..Default::default()
    };
    let mut cfg = spec.materialize(&base).unwrap();
    cfg.type_options = Some(spec);
    cfg.env.insert("PYTHONUNBUFFERED".into(), "1".into());
    assert!(
        matches!(&cfg.kind, RunConfigKind::Python { script } if script == Path::new("manage.py"))
    );
    assert_eq!(
        RunConfig::split_args(&cfg.parameters),
        vec![
            "runserver".to_string(),
            address.to_string(),
            "--noreload".to_string(),
            "--nothreading".to_string(),
        ],
        "public DjangoServer lowering must preserve the exact server argv"
    );
    drop(reservation);
    let mut run = run_with_profiler_opts(
        id,
        &cfg,
        Some(&interpreter),
        root.path(),
        &ProfileOptions {
            engine: engine.into(),
            ..Default::default()
        },
    )
    .expect("public DjangoServer profiler starts");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut captured = Vec::new();
    let mut stream = loop {
        captured.extend(run.poll());
        assert!(
            run.is_running(),
            "server exited before HTTP readiness: {captured:?}"
        );
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "owned server readiness deadline: {captured:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(b"GET /owned-profile/ HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"),
        "{response}"
    );
    assert!(
        response.ends_with("OWNED_DJANGO_PROFILE=256032"),
        "{response}"
    );
    captured.extend(retire(&mut run));
    assert_eq!(run.exit_code(), Some(0), "{engine}: {captured:?}");
    assert!(run.error().is_none(), "{:?}: {captured:?}", run.error());
    assert!(
        captured
            .iter()
            .any(|(line, stderr)| !stderr && line == "DJANGO_SERVER_PROFILE_RESPONSE 256032"),
        "{captured:?}"
    );
    assert!(!captured
        .iter()
        .any(|(line, _)| line.starts_with(PROFILE_PREFIX.trim_end())));
    let report = run
        .report()
        .expect("clean real server exit publishes profile");
    assert_eq!(report.engine, engine);
    assert_eq!(report.exit_code, Some(0));
    assert!(!report.sampling);
    for (name, file, calls) in [
        ("actual_profile_view", "server_urls.py", 1),
        ("framework_hot", "profile_workload.py", 3),
    ] {
        let function = report
            .functions
            .iter()
            .find(|function| function.name == name && function.file.ends_with(file))
            .expect("real HTTP view and workload appear in profile");
        assert_eq!(function.ncalls, calls);
        assert_eq!(function.primitive_calls, calls);
        assert_eq!(
            dunce::canonicalize(Path::new(&function.file)).unwrap(),
            dunce::canonicalize(root.path().join(file)).unwrap()
        );
        assert!(
            function.line > 0
                && function.cumtime.is_finite()
                && function.cumtime >= function.tottime
        );
    }
    assert!(
        std::fs::metadata(report.pstat_file.as_ref().unwrap())
            .unwrap()
            .len()
            > 0
    );
    let pid: u32 = std::fs::read_to_string(root.path().join("served.pid"))
        .unwrap()
        .parse()
        .unwrap();
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "owned real server must physically retire"
    );
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err(),
        "owned server socket must close"
    );
    eprintln!("REQUIRED_DJANGO_SERVER_PROFILE engine={engine} pid={pid} address={address} response=256032 functions={}", report.functions.len());
}

#[cfg(unix)]
#[test]
#[ignore = "requires admitted real Django/yappi SDK; serves owned loopback HTTP and self-stops normally"]
fn configured_cprofile_actual_django_server_request_and_clean_report() {
    configured_django_server("cprofile", 73700);
}

#[cfg(unix)]
#[test]
#[ignore = "requires admitted real Django/yappi SDK; serves owned loopback HTTP and self-stops normally"]
fn configured_yappi_actual_django_server_request_and_clean_report() {
    configured_django_server("yappi", 73800);
}

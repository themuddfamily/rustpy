//! Opt-in offline acceptance using an explicitly admitted disposable venv.
//! The venv must already contain pip and the `build` frontend. The owned PEP517
//! backend needs no build dependencies and produces real installable artifacts.
use rustpy_packages::{
    project::{command_plan, Manager, Operation, Preferences},
    PipTask, PipTaskKind,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const NAME: &str = "rustpy-owned-acceptance";
const BACKEND: &str = r#"import base64, csv, hashlib, io, pathlib, tarfile, zipfile
ROOT = pathlib.Path(__file__).parent
NAME = 'rustpy_owned_acceptance'
def version():
    return (ROOT / 'version.txt').read_text().strip()
def metadata():
    return ('Metadata-Version: 2.2\nName: rustpy-owned-acceptance\nVersion: ' + version() + '\n\n').encode('utf-8')
def get_requires_for_build_sdist(config_settings=None):
    return []
def get_requires_for_build_wheel(config_settings=None):
    return []
def build_sdist(sdist_directory, config_settings=None):
    prefix = NAME + '-' + version()
    filename = prefix + '.tar.gz'
    with tarfile.open(pathlib.Path(sdist_directory) / filename, 'w:gz', format=tarfile.PAX_FORMAT) as archive:
        for name in ['pyproject.toml', 'owned_backend.py', 'version.txt']:
            archive.add(ROOT / name, arcname=prefix + '/' + name)
        contents = metadata()
        info = tarfile.TarInfo(prefix + '/PKG-INFO')
        info.size = len(contents)
        info.mode = 0o644
        archive.addfile(info, io.BytesIO(contents))
    return filename
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    v = version()
    info = NAME + '-' + v + '.dist-info'
    files = {
        NAME + '/__init__.py': ('VERSION = ' + repr(v) + '\nVALUE = ' + repr('owned-' + v) + '\n').encode(),
        info + '/METADATA': metadata(),
        info + '/WHEEL': b'Wheel-Version: 1.0\nGenerator: RustPy owned acceptance\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
    }
    record = io.StringIO(newline='')
    writer = csv.writer(record)
    for path, contents in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(contents).digest()).rstrip(b'=').decode()
        writer.writerow([path, 'sha256=' + digest, len(contents)])
    writer.writerow([info + '/RECORD', '', ''])
    files[info + '/RECORD'] = record.getvalue().encode()
    filename = NAME + '-' + v + '-py3-none-any.whl'
    with zipfile.ZipFile(pathlib.Path(wheel_directory) / filename, 'w') as archive:
        for path, contents in files.items():
            archive.writestr(path, contents)
    return filename
"#;

fn finish(mut task: PipTask) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut lines = Vec::new();
    while task.is_running() {
        lines.extend(task.poll());
        if Instant::now() >= deadline {
            task.stop();
            panic!("owned offline package operation did not finish: {lines:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    lines.extend(task.poll());
    assert_eq!(
        task.exit_code(),
        Some(0),
        "package operation failed: {lines:?}"
    );
    assert!(!task.was_stopped());
    lines
}

fn operation(root: &Path, python: &Path, operation: Operation) -> Vec<String> {
    let mut plan = command_plan(
        root,
        python,
        &Preferences {
            manager: Manager::Pip,
            ..Default::default()
        },
        &operation,
    )
    .unwrap();
    assert_eq!(
        plan.program, python,
        "selected SDK must own every operation"
    );
    // Override inherited index/config settings on each child; neither build nor
    // install may obtain packages from public repositories.
    plan.env.extend([
        ("PIP_NO_INDEX".into(), "1".into()),
        ("PIP_NO_CACHE_DIR".into(), "1".into()),
        (
            "PIP_CONFIG_FILE".into(),
            if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        ),
    ]);
    finish(plan.start().unwrap())
}

fn python_assert(python: &Path, root: &Path, source: &str) -> Vec<String> {
    let mut command = std::process::Command::new(python);
    command.args(["-I", "-c", source]).current_dir(root);
    finish(
        PipTask::spawn_command(
            PipTaskKind::Command {
                label: "Owned package import assertion".into(),
            },
            command,
            "selected Python -I import assertion".into(),
        )
        .unwrap(),
    )
}

fn assert_version(python: &Path, root: &Path, version: &str) {
    let packages = rustpy_packages::list_installed(python).unwrap();
    let found = packages
        .iter()
        .find(|p| rustpy_packages::normalize_name(&p.name) == NAME)
        .expect("actual selected interpreter inventory must contain owned distribution");
    assert_eq!(found.version, version);
    let source = format!("import rustpy_owned_acceptance as owned; import importlib.metadata as m; assert owned.VERSION == {version:?}; assert owned.VALUE == 'owned-' + {version:?}; assert m.version('rustpy-owned-acceptance') == {version:?}; print('IMPORT_OK_' + owned.VERSION)");
    let output = python_assert(python, root, &source);
    assert!(
        output
            .iter()
            .any(|line| line.contains(&format!("IMPORT_OK_{version}"))),
        "actual import output missing: {output:?}"
    );
}

#[test]
#[ignore = "requires explicitly admitted disposable venv with pip/build; all packages remain offline and owned"]
fn configured_offline_build_install_upgrade_uninstall_uses_selected_sdk() {
    assert_eq!(
        std::env::var("RUSTPY_TEST_PACKAGE_DISPOSABLE").as_deref(),
        Ok("yes"),
        "explicit disposable SDK admission required"
    );
    for key in [
        "PIP_TARGET",
        "PIP_PREFIX",
        "PIP_ROOT",
        "PIP_USER",
        "PYTHONHOME",
        "PYTHONPATH",
    ] {
        assert!(
            std::env::var_os(key).is_none_or(|value| value.is_empty()),
            "remove inherited {key} before admitting isolated SDK mutation"
        );
    }
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_PACKAGE_PYTHON")
            .expect("admitted absolute venv interpreter required"),
    );
    assert!(python.is_absolute() && python.is_file());
    let parent = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_PACKAGE_ROOT")
            .expect("admitted isolated temporary root required"),
    );
    assert!(parent.is_absolute() && parent.is_dir());
    let fixture = tempfile::Builder::new()
        .prefix("owned-package-")
        .tempdir_in(parent)
        .unwrap();
    let root = fixture.path();
    let output = python_assert(&python, root, "import sys, pip, build; assert sys.prefix != sys.base_prefix, 'disposable venv required'; print('DISPOSABLE_VENV_OK')");
    assert!(output
        .iter()
        .any(|line| line.contains("DISPOSABLE_VENV_OK")));
    assert!(
        !rustpy_packages::list_installed(&python)
            .unwrap()
            .iter()
            .any(|p| rustpy_packages::normalize_name(&p.name) == NAME),
        "refuse replacing a preexisting distribution"
    );
    std::fs::write(
        root.join("pyproject.toml"),
        "[build-system]\nrequires = []\nbuild-backend = 'owned_backend'\nbackend-path = ['.']\n",
    )
    .unwrap();
    std::fs::write(root.join("owned_backend.py"), BACKEND).unwrap();
    for version in ["1.0", "2.0"] {
        std::fs::write(root.join("version.txt"), version).unwrap();
        operation(root, &python, Operation::Build);
        let wheel = root.join("dist").join(format!(
            "rustpy_owned_acceptance-{version}-py3-none-any.whl"
        ));
        assert!(wheel.is_file(), "actual PEP517 build must publish wheel");
        assert!(root
            .join("dist")
            .join(format!("rustpy_owned_acceptance-{version}.tar.gz"))
            .is_file());
        let spec = wheel.to_str().unwrap().to_owned();
        operation(
            root,
            &python,
            if version == "1.0" {
                Operation::Install(vec![spec])
            } else {
                Operation::Upgrade(vec![spec])
            },
        );
        assert_version(&python, root, version);
    }
    operation(root, &python, Operation::Uninstall(vec![NAME.into()]));
    assert!(!rustpy_packages::list_installed(&python)
        .unwrap()
        .iter()
        .any(|p| rustpy_packages::normalize_name(&p.name) == NAME));
    let output = python_assert(&python, root, "import importlib.util; assert importlib.util.find_spec('rustpy_owned_acceptance') is None; print('UNINSTALL_OK')");
    assert!(output.iter().any(|line| line.contains("UNINSTALL_OK")));
}

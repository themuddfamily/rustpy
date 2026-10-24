//! Genuine Conda/Pipenv/Hatch/PDM discovery acceptance (including Hatch's
//! configured `dirs.env` and PDM's PEP 582 `__pypackages__` mode). Root supplies the
//! installed CLIs, a base Python and an exclusive populated offline Conda cache
//! below TMPDIR. Every environment is created by the real tool inside a private
//! HOME/XDG tree; a child process with that HOME, PATH and activation variables
//! then runs the public discovery and probes each found interpreter. Nothing
//! changes the caller's HOME or global test environment.
#![cfg(target_os = "linux")]

use rustpy_project::interpreter::{candidate_paths, candidate_paths_in, probe};
use rustpy_project::manager_layouts::{with_project_packages, PDM_PEP582_KIND};
use std::path::{Path, PathBuf};
use std::process::Command;

const SELECTOR: &str = "configured_conda_pipenv_hatch_pdm_environments_are_discovered_and_probed";

fn selected(key: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("required {key}")));
    assert!(
        path.is_absolute() && path.exists(),
        "invalid {key}: {path:?}"
    );
    path
}

/// Private per-command provider environment; HOME and XDG point inside `home`.
fn tool(program: &Path, args: &[&str], dir: &Path, home: &Path, extra: &[(&str, &Path)]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("PIP_NO_INDEX", "1")
        .env("PIP_CONFIG_FILE", "/dev/null")
        .env("PIPENV_YES", "1")
        .env("PIPENV_NOSPIN", "1")
        .env("PIPENV_DONT_LOAD_ENV", "1")
        .env("PDM_CHECK_UPDATE", "false")
        .env("VIRTUALENV_NO_PERIODIC_UPDATE", "1")
        .env("CONDA_OFFLINE", "true")
        .env("CONDA_AUTO_UPDATE_CONDA", "false")
        .env("CONDA_DEFAULT_THREADS", "2")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("CONDA_DEFAULT_ENV")
        .env_remove("WORKON_HOME")
        .env_remove("PIPENV_VENV_IN_PROJECT")
        .env_remove("HATCH_DATA_DIR")
        .env_remove("PDM_VENV_IN_PROJECT")
        .env_remove("HATCH_CONFIG")
        .env_remove("PYTHONPATH");
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().expect("spawn genuine manager");
    assert!(
        output.status.success(),
        "{program:?} {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Expectation {
    project: Option<PathBuf>,
    /// The environment prefix that must be discovered and probed.
    prefix: PathBuf,
    /// Accepted discovery kinds.
    kinds: Vec<String>,
    /// PDM PEP 582: the `__pypackages__/<X.Y>/lib` that analysis and runs must see.
    #[serde(default)]
    pep582_lib: Option<PathBuf>,
}

fn child(manifest: &Path) {
    let expectations: Vec<Expectation> =
        serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let process_env = |key: &str| std::env::var(key).ok();
    for expectation in expectations {
        // Static discovery alone (no manager CLI is run) must find it too.
        let static_candidates =
            candidate_paths_in(expectation.project.as_deref(), &home, &process_env);
        let static_kind = static_candidates
            .iter()
            .find(|(path, _)| {
                path.parent().and_then(Path::parent) == Some(expectation.prefix.as_path())
            })
            .map(|(_, kind)| kind.clone());
        assert!(
            static_kind
                .as_ref()
                .is_some_and(|kind| expectation.kinds.contains(kind)),
            "{:?} statically discovered as {static_kind:?} for {:?}, expected {:?}",
            expectation.prefix,
            expectation.project,
            expectation.kinds
        );
        let candidates = candidate_paths(expectation.project.as_deref());
        let found: Vec<_> = candidates
            .iter()
            .filter(|(path, _)| {
                path.parent().and_then(Path::parent) == Some(expectation.prefix.as_path())
            })
            .collect();
        assert!(
            !found.is_empty(),
            "{:?} not discovered for {:?}: {candidates:#?}",
            expectation.prefix,
            expectation.project
        );
        let (path, kind) = found[0];
        assert!(
            expectation.kinds.iter().any(|k| k == kind),
            "{path:?} discovered as {kind:?}, expected {:?}",
            expectation.kinds
        );
        let mut interpreter = probe(path, kind).expect("discovered interpreter must probe");
        if let Some(project) = &expectation.project {
            interpreter = with_project_packages(interpreter, project);
        }
        if let (Some(lib), Some(project)) = (&expectation.pep582_lib, &expectation.project) {
            assert!(
                interpreter.sys_path.contains(lib),
                "PEP 582 library missing from analysis sys.path: {:?}",
                interpreter.sys_path
            );
            let pythonpath = rustpy_project::env::pythonpath_for(
                project,
                &rustpy_project::ProjectModel::default(),
                false,
                false,
                None,
            );
            assert_eq!(pythonpath.as_deref().map(Path::new), Some(lib.as_path()));
            let output = Command::new(path)
                .args(["-c", "import rustpy_pep582_probe as m; print(m.VALUE)"])
                .env("PYTHONPATH", pythonpath.unwrap())
                .current_dir(project)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "pep582",
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(!interpreter.version.is_empty() && !interpreter.sys_path.is_empty());
        assert_eq!(
            interpreter.prefix.canonicalize().unwrap(),
            expectation.prefix.canonicalize().unwrap(),
            "{path:?}"
        );
        eprintln!(
            "discovered {kind}: {} ({})",
            path.display(),
            interpreter.version
        );
    }
}

#[test]
#[ignore = "requires admitted genuine Conda/Pipenv/Hatch/PDM CLIs, base Python and exclusive populated offline Conda cache; run alone"]
fn configured_conda_pipenv_hatch_pdm_environments_are_discovered_and_probed() {
    if let Some(manifest) = std::env::var_os("RUSTPY_DISCOVERY_FIXTURE_CHILD") {
        return child(Path::new(&manifest));
    }
    assert_eq!(
        std::env::var("RUSTPY_DISCOVERY_MANAGERS_ADMITTED").as_deref(),
        Ok("1")
    );
    let conda = selected("RUSTPY_CONDA_CLI");
    let pipenv = selected("RUSTPY_PIPENV_EXECUTABLE");
    let hatch = selected("RUSTPY_HATCH_EXECUTABLE");
    let pdm = selected("RUSTPY_PDM_EXECUTABLE");
    let base = selected("RUSTPY_ENVIRONMENT_PYTHON");
    let cache = selected("RUSTPY_CONDA_PKGS_DIR");
    let version = std::env::var("RUSTPY_CONDA_PYTHON_VERSION").expect("explicit Python version");
    let temporary = selected("TMPDIR").canonicalize().unwrap();
    assert!(
        cache.canonicalize().unwrap().starts_with(&temporary),
        "Root must admit an exclusive private offline cache below TMPDIR"
    );
    let private = tempfile::Builder::new()
        .prefix("configured-discovery-λ-")
        .tempdir_in(&temporary)
        .unwrap();
    let root = private.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".conda")).unwrap();
    let mut expectations = Vec::new();
    let conda_kind = vec!["conda".to_string()];

    // Conda: a registered prefix env (environments.txt in the private HOME)
    // and a named env inside CONDA_ENVS_PATH; the base install via CONDA_EXE.
    let condarc = root.join("condarc");
    std::fs::write(&condarc, "offline: true\nregister_envs: true\nnotify_outdated_conda: false\nreport_errors: false\ncreate_default_packages: []\n").unwrap();
    let envs_path = root.join("conda-envs");
    std::fs::create_dir(&envs_path).unwrap();
    let conda_extra: Vec<(&str, &Path)> = vec![
        ("CONDARC", &condarc),
        ("CONDA_PKGS_DIRS", &cache),
        ("CONDA_ENVS_PATH", &envs_path),
    ];
    let registered = root.join("elsewhere/registered-env");
    let python_spec = format!("python={version}");
    tool(
        &conda,
        &[
            "create",
            "--yes",
            "--quiet",
            "--prefix",
            registered.to_str().unwrap(),
            &python_spec,
        ],
        &root,
        &home,
        &conda_extra,
    );
    let listed = std::fs::read_to_string(home.join(".conda/environments.txt")).unwrap();
    assert!(
        listed.lines().any(|l| Path::new(l.trim()) == registered),
        "real conda must register the prefix env: {listed}"
    );
    expectations.push(Expectation {
        project: None,
        prefix: registered,
        kinds: conda_kind.clone(),
        pep582_lib: None,
    });
    tool(
        &conda,
        &[
            "create",
            "--yes",
            "--quiet",
            "--name",
            "rustpy-named",
            &python_spec,
        ],
        &root,
        &home,
        &conda_extra,
    );
    expectations.push(Expectation {
        project: None,
        prefix: envs_path.join("rustpy-named"),
        kinds: conda_kind.clone(),
        pep582_lib: None,
    });
    let conda_base = conda
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    if conda_base.join("conda-meta").is_dir() && conda_base.join("bin/python").is_file() {
        expectations.push(Expectation {
            project: None,
            prefix: conda_base,
            kinds: conda_kind,
            pep582_lib: None,
        });
    }

    let project = |name: &str, pyproject: Option<&str>, pipfile: bool| {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        if let Some(text) = pyproject {
            std::fs::write(dir.join("pyproject.toml"), text).unwrap();
        }
        if pipfile {
            std::fs::write(dir.join("Pipfile"), "[packages]\n[dev-packages]\n").unwrap();
        }
        dir
    };
    let base_arg = base.to_str().unwrap();

    // Pipenv: central WORKON store (XDG data virtualenvs) and in-project .venv.
    let central = project("pipenv-central", None, true);
    tool(&pipenv, &["--python", base_arg], &central, &home, &[]);
    let store = home.join(".local/share/virtualenvs");
    let pipenv_env = std::fs::read_dir(&store)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pipenv-central-")
        })
        .expect("real pipenv must create its central venv");
    expectations.push(Expectation {
        project: Some(central),
        prefix: pipenv_env,
        kinds: vec!["pipenv venv".into()],
        pep582_lib: None,
    });
    let in_project = project("pipenv-local", None, true);
    tool(
        &pipenv,
        &["--python", base_arg],
        &in_project,
        &home,
        &[("PIPENV_VENV_IN_PROJECT", Path::new("1"))],
    );
    expectations.push(Expectation {
        project: Some(in_project.clone()),
        prefix: in_project.join(".venv"),
        kinds: vec!["pipenv venv".into(), "project venv".into()],
        pep582_lib: None,
    });

    // Hatch: <data>/hatch/env/virtual/<project>/<hash>/<env>.
    let hatch_project = project(
        "hatch-proj",
        Some("[project]\nname='hatch-proj'\nversion='1.0'\ndependencies=[]\n[tool.hatch.envs.default]\nskip-install=true\n"),
        false,
    );
    tool(
        &hatch,
        &["env", "create"],
        &hatch_project,
        &home,
        &[("HATCH_PYTHON", &base)],
    );
    let hatch_store = home.join(".local/share/hatch/env/virtual/hatch-proj");
    let hatch_env = std::fs::read_dir(&hatch_store)
        .expect("real hatch must create its env store")
        .flatten()
        .flat_map(|h| {
            std::fs::read_dir(h.path())
                .unwrap()
                .flatten()
                .map(|e| e.path())
        })
        .next()
        .expect("hatch env");
    expectations.push(Expectation {
        project: Some(hatch_project),
        prefix: hatch_env,
        kinds: vec!["hatch venv".into()],
        pep582_lib: None,
    });

    // PDM: in-project .venv selected by `pdm use` (.pdm-python) and a
    // centrally stored venv (venv.in_project=false).
    let pdm_toml = |name: &str| {
        format!("[project]\nname='{name}'\nversion='1.0'\nrequires-python='>=3.8'\ndependencies=[]\n[tool.pdm]\ndistribution=false\n")
    };
    let pdm_local = project("pdm-local", Some(&pdm_toml("pdm-local")), false);
    tool(&pdm, &["venv", "create", base_arg], &pdm_local, &home, &[]);
    tool(
        &pdm,
        &["use", "-f", "--venv", "in-project"],
        &pdm_local,
        &home,
        &[],
    );
    assert!(pdm_local.join(".pdm-python").is_file());
    expectations.push(Expectation {
        project: Some(pdm_local.clone()),
        prefix: pdm_local.join(".venv"),
        kinds: vec!["pdm venv".into(), "project venv".into()],
        pep582_lib: None,
    });
    let pdm_central = project("pdm-central", Some(&pdm_toml("pdm-central")), false);
    tool(
        &pdm,
        &["venv", "create", base_arg],
        &pdm_central,
        &home,
        &[("PDM_VENV_IN_PROJECT", Path::new("false"))],
    );
    let pdm_env = std::fs::read_dir(home.join(".local/share/pdm/venvs"))
        .expect("real pdm must create its central store")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pdm-central-")
        })
        .expect("pdm central venv");
    expectations.push(Expectation {
        project: Some(pdm_central),
        prefix: pdm_env,
        kinds: vec!["pdm venv".into()],
        pep582_lib: None,
    });

    // Hatch with a custom `dirs.env.virtual` (HATCH_CONFIG) and a project whose
    // folder differs from its `[project] name`, plus an explicit env `path`.
    let hatch_config = root.join("hatch-config.toml");
    let hatch_custom = root.join("hatch-custom");
    std::fs::write(
        &hatch_config,
        format!("[dirs.env]\nvirtual = \"{}\"\n", hatch_custom.display()),
    )
    .unwrap();
    let hatch_named = project(
        "hatch-folder",
        Some("[project]\nname='My_Hatch.Proj'\nversion='1.0'\ndependencies=[]\n[tool.hatch.envs.default]\nskip-install=true\n[tool.hatch.envs.pinned]\nskip-install=true\npath='.pinned-env'\n"),
        false,
    );
    let hatch_extra: Vec<(&str, &Path)> =
        vec![("HATCH_PYTHON", &base), ("HATCH_CONFIG", &hatch_config)];
    tool(
        &hatch,
        &["env", "create"],
        &hatch_named,
        &home,
        &hatch_extra,
    );
    tool(
        &hatch,
        &["env", "create", "pinned"],
        &hatch_named,
        &home,
        &hatch_extra,
    );
    let named_store = hatch_custom.join("my-hatch-proj");
    let named_env = std::fs::read_dir(&named_store)
        .expect("real hatch must honour dirs.env and the normalised project name")
        .flatten()
        .map(|h| h.path().join("my-hatch-proj"))
        .find(|p| p.join("pyvenv.cfg").is_file())
        .expect("hatch default env in the custom store");
    assert!(!home
        .join(".local/share/hatch/env/virtual/my-hatch-proj")
        .exists());
    expectations.push(Expectation {
        project: Some(hatch_named.clone()),
        prefix: named_env,
        kinds: vec!["hatch venv".into()],
        pep582_lib: None,
    });
    assert!(hatch_named.join(".pinned-env/pyvenv.cfg").is_file());
    expectations.push(Expectation {
        project: Some(hatch_named.clone()),
        prefix: hatch_named.join(".pinned-env"),
        kinds: vec!["hatch venv".into()],
        pep582_lib: None,
    });

    // PDM PEP 582: no venv (`python.use_venv false`), packages in
    // `__pypackages__/<X.Y>/lib`, `.pdm-python` naming the base interpreter.
    let pep582 = project("pdm-pep582", Some(&pdm_toml("pdm-pep582")), false);
    tool(
        &pdm,
        &["config", "--local", "python.use_venv", "false"],
        &pep582,
        &home,
        &[],
    );
    std::fs::create_dir(pep582.join("__pypackages__")).unwrap();
    tool(&pdm, &["use", "-f", base_arg], &pep582, &home, &[]);
    tool(&pdm, &["install"], &pep582, &home, &[]);
    let selected_base = PathBuf::from(
        std::fs::read_to_string(pep582.join(".pdm-python"))
            .unwrap()
            .trim(),
    );
    assert!(!selected_base
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pyvenv.cfg")
        .exists());
    let pep582_lib = std::fs::read_dir(pep582.join("__pypackages__"))
        .unwrap()
        .flatten()
        .map(|e| e.path().join("lib"))
        .find(|lib| lib.is_dir())
        .expect("real pdm must create __pypackages__/<X.Y>/lib");
    std::fs::write(
        pep582_lib.join("rustpy_pep582_probe.py"),
        "VALUE = 'pep582'\n",
    )
    .unwrap();
    expectations.push(Expectation {
        project: Some(pep582),
        prefix: selected_base
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf(),
        kinds: vec![PDM_PEP582_KIND.into()],
        pep582_lib: Some(pep582_lib),
    });

    let manifest = root.join("manifest.json");
    std::fs::write(&manifest, serde_json::to_vec(&expectations).unwrap()).unwrap();
    let tools_path = std::env::join_paths([
        pipenv.parent().unwrap(),
        hatch.parent().unwrap(),
        pdm.parent().unwrap(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            SELECTOR,
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RUSTPY_DISCOVERY_FIXTURE_CHILD", &manifest)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("PATH", tools_path)
        .env("CONDA_EXE", &conda)
        .env("CONDA_ENVS_PATH", &envs_path)
        .env("HATCH_CONFIG", &hatch_config)
        .env("PIPENV_DONT_LOAD_ENV", "1")
        .env("PDM_CHECK_UPDATE", "false")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("WORKON_HOME")
        .env_remove("HATCH_DATA_DIR")
        .env_remove("PDM_VENV_IN_PROJECT")
        .env_remove("PYTHONPATH")
        .status()
        .expect("discovery child");
    assert!(status.success(), "real discovery child failed: {status}");
}

//! Static (tool-free) layouts of PDM PEP 582 projects and Hatch environment
//! stores, used by interpreter discovery and run environments.
//!
//! Nothing here runs a tool or project code: configuration files are read as
//! TOML and directories are listed.

use std::path::{Path, PathBuf};

use crate::Interpreter;

/// Discovery kind of a PDM project using `__pypackages__` (PEP 582 mode) with a
/// non-venv base interpreter recorded in `.pdm-python`.
pub const PDM_PEP582_KIND: &str = "pdm PEP 582";

fn read_toml(path: &Path) -> Option<toml::Value> {
    toml::from_str::<toml::Table>(&std::fs::read_to_string(path).ok()?)
        .ok()
        .map(toml::Value::Table)
}

/// PEP 503 normalisation (`My_Proj.X` -> `my-proj-x`), as Hatch names its
/// per-project environment folders.
pub fn normalize_project_name(name: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
        } else {
            if separator && !out.is_empty() {
                out.push('-');
            }
            separator = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// `[project] name` of `root/pyproject.toml`, normalised.
pub fn project_name(root: &Path) -> Option<String> {
    read_toml(&root.join("pyproject.toml"))?
        .get("project")?
        .get("name")?
        .as_str()
        .map(normalize_project_name)
        .filter(|n| !n.is_empty())
}

fn is_venv_python(python: &Path) -> bool {
    python
        .parent()
        .and_then(Path::parent)
        .is_some_and(|env| env.join("pyvenv.cfg").is_file())
}

/// The interpreter PDM selected for `root` (`.pdm-python`) with its kind:
/// [`PDM_PEP582_KIND`] when the project has `__pypackages__` and the
/// interpreter is not a virtual environment, otherwise `pdm venv`.
pub fn pdm_selected_python(root: &Path) -> Option<(PathBuf, &'static str)> {
    let text = std::fs::read_to_string(root.join(".pdm-python")).ok()?;
    let path = PathBuf::from(text.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let kind = if root.join("__pypackages__").is_dir() && !is_venv_python(&path) {
        PDM_PEP582_KIND
    } else {
        "pdm venv"
    };
    Some((path, kind))
}

/// `root/__pypackages__/<X.Y>/lib` for `version`; without a version, the
/// version of the cached `.pdm-python` probe, else the only version present.
pub fn pdm_pep582_library(root: &Path, version: Option<(u32, u32)>) -> Option<PathBuf> {
    let packages = root.join("__pypackages__");
    if !packages.is_dir() {
        return None;
    }
    let version = version.or_else(|| {
        let (python, kind) = pdm_selected_python(root)?;
        (kind == PDM_PEP582_KIND)
            .then(|| crate::interpreter::cached_probe(&python))
            .flatten()?
            .ok()
            .map(|m| m.interpreter.major_minor())
    });
    if let Some((major, minor)) = version {
        let lib = packages.join(format!("{major}.{minor}")).join("lib");
        return lib.is_dir().then_some(lib);
    }
    let mut libs = std::fs::read_dir(&packages)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("lib"))
        .filter(|lib| lib.is_dir());
    let only = libs.next()?;
    libs.next().is_none().then_some(only)
}

/// Adds the project's PEP 582 library to a probed interpreter's `sys.path`
/// (before its site-packages) when it is the project's `.pdm-python` base
/// interpreter; other interpreters are returned unchanged.
pub fn with_project_packages(mut interpreter: Interpreter, root: &Path) -> Interpreter {
    let Some((python, kind)) = pdm_selected_python(root) else {
        return interpreter;
    };
    let same = |a: &Path, b: &Path| {
        a == b
            || a.canonicalize()
                .ok()
                .is_some_and(|a| b.canonicalize().ok() == Some(a))
    };
    if kind != PDM_PEP582_KIND || interpreter.is_venv || !same(&python, &interpreter.path) {
        return interpreter;
    }
    if let Some(lib) = pdm_pep582_library(root, Some(interpreter.major_minor())) {
        if !interpreter.sys_path.contains(&lib) {
            let at = interpreter
                .sys_path
                .iter()
                .position(|p| {
                    p.components().any(|c| {
                        matches!(
                            c.as_os_str().to_str(),
                            Some("site-packages" | "dist-packages")
                        )
                    })
                })
                .unwrap_or(interpreter.sys_path.len());
            interpreter.sys_path.insert(at, lib);
        }
    }
    interpreter
}

/// Hatch's `config.toml`: `HATCH_CONFIG`, else the platform config directory.
fn hatch_config_file(home: &Path, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(path) = env("HATCH_CONFIG").filter(|p| !p.is_empty()) {
        return path.into();
    }
    let dir = if cfg!(windows) {
        env("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
    };
    dir.join("hatch/config.toml")
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/").or(path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
}

/// One Hatch virtual-environment store and how environments are laid out in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HatchStore {
    pub dir: PathBuf,
    /// `<dir>/<env>` (store inside the project or `~/.virtualenvs`) instead of
    /// `<dir>/<project>/<id>/<env>`.
    pub flat: bool,
}

/// Hatch virtual-environment stores: `dirs.env.virtual` from the config
/// (relative paths resolve against the project), then the data directory's
/// `env/virtual` (`HATCH_DATA_DIR`, config `dirs.data`, platform default).
pub fn hatch_stores(
    project: Option<&Path>,
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<HatchStore> {
    let config = read_toml(&hatch_config_file(home, env));
    let dirs = config.as_ref().and_then(|c| c.get("dirs"));
    let mut out: Vec<HatchStore> = Vec::new();
    let mut add = |dir: PathBuf| {
        let flat = dir == home.join(".virtualenvs")
            || project.is_some_and(|p| dir.starts_with(p) && dir != p);
        if !out.iter().any(|s| s.dir == dir) {
            out.push(HatchStore { dir, flat });
        }
    };
    let configured = dirs
        .and_then(|d| d.get("env"))
        .and_then(|e| e.get("virtual"))
        .and_then(toml::Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(|v| expand_home(v.trim(), home));
    if let Some(dir) = configured {
        if dir.is_absolute() {
            add(dir);
        } else if let Some(project) = project {
            add(project.join(dir));
        }
    }
    let data = env("HATCH_DATA_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            dirs.and_then(|d| d.get("data"))
                .and_then(toml::Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .map(|v| expand_home(v.trim(), home))
                .filter(|p| p.is_absolute())
        });
    if let Some(data) = data {
        add(data.join("env/virtual"));
    }
    if cfg!(windows) {
        if let Some(local) = env("LOCALAPPDATA") {
            add(PathBuf::from(local).join("hatch/env/virtual"));
        }
    } else {
        add(home.join("Library/Application Support/hatch/env/virtual"));
        add(env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("hatch/env/virtual"));
    }
    out
}

/// Explicit environment paths of the project's Hatch environments:
/// `[tool.hatch.envs.<name>] path` in `pyproject.toml` and `[envs.<name>] path`
/// in `hatch.toml`, resolved against the project.
pub fn hatch_explicit_env_paths(project: &Path) -> Vec<PathBuf> {
    let mut tables = Vec::new();
    if let Some(v) = read_toml(&project.join("pyproject.toml")) {
        if let Some(envs) = v
            .get("tool")
            .and_then(|t| t.get("hatch"))
            .and_then(|h| h.get("envs"))
        {
            tables.push(envs.clone());
        }
    }
    if let Some(envs) = read_toml(&project.join("hatch.toml")).and_then(|v| v.get("envs").cloned())
    {
        tables.push(envs);
    }
    let mut out = Vec::new();
    for envs in tables {
        let Some(envs) = envs.as_table() else {
            continue;
        };
        for config in envs.values() {
            if let Some(path) = config.get("path").and_then(toml::Value::as_str) {
                let path = PathBuf::from(path.trim());
                if path.as_os_str().is_empty() {
                    continue;
                }
                let path = if path.is_absolute() {
                    path
                } else {
                    project.join(path)
                };
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Whether `project` uses Hatch (a `[tool.hatch` table or `hatch.toml`).
pub fn is_hatch_project(project: &Path) -> bool {
    project.join("hatch.toml").is_file()
        || std::fs::read_to_string(project.join("pyproject.toml"))
            .is_ok_and(|text| text.contains("[tool.hatch"))
}

/// Hatch environment directories split into the project's own and the rest.
/// The project's per-project folder matches its normalised `[project] name`
/// (or, failing that, its directory name).
pub fn hatch_env_dirs(
    project: Option<&Path>,
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut names: Vec<String> = Vec::new();
    if let Some(project) = project {
        names.extend(project_name(project));
        if let Some(dir) = project.file_name() {
            let dir = normalize_project_name(&dir.to_string_lossy());
            if !dir.is_empty() && !names.contains(&dir) {
                names.push(dir);
            }
        }
    }
    let mut mine = Vec::new();
    let mut others = Vec::new();
    if let Some(project) = project.filter(|p| is_hatch_project(p)) {
        mine.extend(hatch_explicit_env_paths(project));
    }
    for store in hatch_stores(project, home, env) {
        if store.flat {
            let target = if project.is_some_and(|p| store.dir.starts_with(p)) {
                &mut mine
            } else {
                &mut others
            };
            target.extend(sorted_dirs(&store.dir));
            continue;
        }
        for folder in sorted_dirs(&store.dir) {
            let name =
                normalize_project_name(&folder.file_name().unwrap_or_default().to_string_lossy());
            let target = if names.contains(&name) {
                &mut mine
            } else {
                &mut others
            };
            for id in sorted_dirs(&folder) {
                target.extend(sorted_dirs(&id));
            }
        }
    }
    let mut seen = Vec::new();
    mine.retain(|p| {
        let keep = !seen.contains(p);
        seen.push(p.clone());
        keep
    });
    others.retain(|p| !seen.contains(p));
    (mine, others)
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(4096)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn venv(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::fs::write(dir.join("bin/python"), "").unwrap();
        dir.join("bin/python")
    }

    #[test]
    fn names_normalise_like_hatch() {
        assert_eq!(normalize_project_name("My_Hatch.Proj"), "my-hatch-proj");
        assert_eq!(normalize_project_name("a--b__c"), "a-b-c");
        assert_eq!(normalize_project_name("Rel.Proj"), "rel-proj");
    }

    #[test]
    fn hatch_config_dirs_env_and_project_name_select_the_project_envs() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("folder-differs");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            "[project]\nname='My_Hatch.Proj'\n[tool.hatch.envs.pathy]\npath='.pathy-env'\n",
        )
        .unwrap();
        let custom = temp.path().join("custom-envs");
        let config = temp.path().join("hatch-config.toml");
        std::fs::write(
            &config,
            format!("[dirs.env]\nvirtual = \"{}\"\n", custom.display()),
        )
        .unwrap();
        let mine_default = custom.join("my-hatch-proj/AbCd/my-hatch-proj");
        let mine_test = custom.join("my-hatch-proj/AbCd/tst");
        let other = custom.join("someone-else/XyZ/someone-else");
        for dir in [
            &mine_default,
            &mine_test,
            &other,
            &project.join(".pathy-env"),
        ] {
            venv(dir);
        }
        let env =
            |key: &str| (key == "HATCH_CONFIG").then(|| config.to_string_lossy().into_owned());
        let (mine, others) = hatch_env_dirs(Some(&project), &home, &env);
        assert_eq!(
            mine,
            vec![
                project.join(".pathy-env"),
                mine_default.clone(),
                mine_test.clone()
            ]
        );
        assert_eq!(others, vec![other.clone()]);
        // XDG config location when HATCH_CONFIG is unset (Linux).
        if cfg!(target_os = "linux") {
            let xdg = temp.path().join("xdg");
            std::fs::create_dir_all(xdg.join("hatch")).unwrap();
            std::fs::copy(&config, xdg.join("hatch/config.toml")).unwrap();
            let env =
                |key: &str| (key == "XDG_CONFIG_HOME").then(|| xdg.to_string_lossy().into_owned());
            assert!(hatch_env_dirs(Some(&project), &home, &env)
                .0
                .contains(&mine_default));
        }
        // The candidate list labels the project's envs `hatch venv`.
        let found = crate::interpreter::candidate_paths_in(Some(&project), &home, &|key: &str| {
            (key == "HATCH_CONFIG").then(|| config.to_string_lossy().into_owned())
        });
        let kind = |p: &Path| {
            found
                .iter()
                .find(|(q, _)| q == &p.join("bin/python"))
                .map(|(_, k)| k.as_str())
        };
        assert_eq!(kind(&mine_default), Some("hatch venv"));
        assert_eq!(kind(&other), Some("hatch"));
    }

    #[test]
    fn relative_dirs_env_is_flat_inside_the_project() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("rel");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            "[project]\nname='Rel.Proj'\n[tool.hatch.envs.default]\n",
        )
        .unwrap();
        let config = temp.path().join("c.toml");
        std::fs::write(&config, "[dirs.env]\nvirtual = \".hatch\"\n").unwrap();
        let env =
            |key: &str| (key == "HATCH_CONFIG").then(|| config.to_string_lossy().into_owned());
        let stores = hatch_stores(Some(&project), &home, &env);
        assert_eq!(
            stores[0],
            HatchStore {
                dir: project.join(".hatch"),
                flat: true
            }
        );
        venv(&project.join(".hatch/rel-proj"));
        assert_eq!(
            hatch_env_dirs(Some(&project), &home, &env).0,
            vec![project.join(".hatch/rel-proj")]
        );
        // Without a project a relative store has no anchor.
        assert!(hatch_stores(None, &home, &env)
            .iter()
            .all(|s| s.dir.is_absolute() && !s.dir.ends_with(".hatch")));
    }

    #[test]
    fn pdm_pep582_is_discovered_and_extends_sys_path() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("pep");
        let lib = project.join("__pypackages__/3.12/lib");
        std::fs::create_dir_all(&lib).unwrap();
        let base = temp.path().join("base/bin/python3");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, "").unwrap();
        std::fs::write(project.join(".pdm-python"), format!("{}\n", base.display())).unwrap();
        assert_eq!(
            pdm_selected_python(&project),
            Some((base.clone(), PDM_PEP582_KIND))
        );
        assert_eq!(pdm_pep582_library(&project, None), Some(lib.clone()));
        assert_eq!(pdm_pep582_library(&project, Some((3, 11))), None);
        let found = crate::interpreter::candidate_paths_in(
            Some(&project),
            &temp.path().join("home"),
            &|_| None,
        );
        assert_eq!(found.first(), Some(&(base.clone(), PDM_PEP582_KIND.into())));
        let site = temp.path().join("base/lib/python3.12/site-packages");
        let interpreter = Interpreter {
            path: base.clone(),
            version: "3.12.4".into(),
            prefix: temp.path().join("base"),
            sys_path: vec![temp.path().join("base/lib/python3.12"), site.clone()],
            is_venv: false,
            platform: "linux".into(),
            kind: PDM_PEP582_KIND.into(),
        };
        let extended = with_project_packages(interpreter.clone(), &project);
        assert_eq!(extended.sys_path[1], lib);
        assert_eq!(extended.sys_path[2], site);
        // Unrelated interpreters and venv-mode projects are unchanged.
        let other = Interpreter {
            path: temp.path().join("other"),
            ..interpreter.clone()
        };
        assert_eq!(with_project_packages(other.clone(), &project), other);
        venv(&temp.path().join("base"));
        assert_eq!(pdm_selected_python(&project).unwrap().1, "pdm venv");
        // The run-time PYTHONPATH carries the library while PEP 582 is active.
        std::fs::remove_file(temp.path().join("base/pyvenv.cfg")).unwrap();
        let model = crate::ProjectModel::default();
        let pp = crate::env::pythonpath_for(&project, &model, false, false, None).unwrap();
        assert_eq!(PathBuf::from(pp), lib);
    }
}

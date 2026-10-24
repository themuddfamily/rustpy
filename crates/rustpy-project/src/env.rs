//! Project import paths and selected-interpreter environment for new processes.
//!
//! Inherited values are explicit inputs: the caller reads its process environment
//! and retains responsibility for run-configuration overrides.

use std::path::{Path, PathBuf};

use crate::{Interpreter, ProjectModel};

/// The project's content and Sources/Test Sources roots, a PDM PEP 582
/// `__pypackages__` library, then inherited `PYTHONPATH`. Content roots retain their model order; source and test roots
/// are sorted together. Values are displayed and joined without deduplication,
/// normalization or splitting the inherited value.
pub fn pythonpath_for(
    root: &Path,
    model: &ProjectModel,
    add_content_roots: bool,
    add_source_roots: bool,
    inherited_pythonpath: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let included = |path: &Path| {
        !model
            .excluded
            .iter()
            .any(|excluded| path.starts_with(excluded))
    };
    if add_content_roots {
        if included(root) {
            parts.push(root.display().to_string());
        }
        parts.extend(
            model
                .content_roots
                .iter()
                .filter(|path| included(path))
                .map(|r| r.display().to_string()),
        );
    }
    if add_source_roots {
        let mut roots: Vec<&PathBuf> = model.source_roots.iter().chain(&model.test_roots).collect();
        roots.sort();
        parts.extend(
            roots
                .into_iter()
                .filter(|path| included(path))
                .map(|r| r.display().to_string()),
        );
        for namespace in &model.namespace_packages {
            if included(namespace) {
                if let Some(parent) = namespace.parent() {
                    let value = parent.display().to_string();
                    if !parts.contains(&value) {
                        parts.push(value);
                    }
                }
            }
        }
    }
    // A PDM project in PEP 582 mode runs its base interpreter; its packages
    // live in `__pypackages__/<X.Y>/lib`, which `pdm run` puts on the path.
    if crate::manager_layouts::pdm_selected_python(root)
        .is_some_and(|(_, kind)| kind == crate::manager_layouts::PDM_PEP582_KIND)
    {
        if let Some(lib) = crate::manager_layouts::pdm_pep582_library(root, None) {
            let value = lib.display().to_string();
            if !parts.contains(&value) {
                parts.push(value);
            }
        }
    }
    if let Some(existing) = inherited_pythonpath {
        if !existing.is_empty() {
            parts.push(existing.to_owned());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(if cfg!(windows) { ";" } else { ":" }))
    }
}

/// Environment for new terminal tabs. Activation is controlled by the terminal
/// setting and uses the selected interpreter, without running an activation script.
pub fn terminal_env(
    activate_virtualenv: bool,
    interpreter: Option<&Interpreter>,
    inherited_path: &str,
) -> Vec<(String, String)> {
    if activate_virtualenv {
        venv_env(interpreter, inherited_path)
    } else {
        Vec::new()
    }
}

/// Environment that activates the selected interpreter's virtualenv in new shells.
/// The interpreter directory is prepended even when inherited `PATH` is empty.
/// Non-virtualenv interpreters and a missing selection produce no overrides.
pub fn venv_env(interpreter: Option<&Interpreter>, inherited_path: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(i) = interpreter {
        let conda =
            i.kind.to_ascii_lowercase().contains("conda") || i.prefix.join("conda-meta").is_dir();
        if i.is_venv || conda {
            let bin = i
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            if conda {
                env.push((
                    "CONDA_PREFIX".into(),
                    i.prefix.to_string_lossy().into_owned(),
                ));
                env.push((
                    "CONDA_DEFAULT_ENV".into(),
                    i.prefix
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ));
            } else {
                env.push((
                    "VIRTUAL_ENV".into(),
                    i.prefix.to_string_lossy().into_owned(),
                ));
            }
            env.push((
                "PATH".into(),
                format!(
                    "{}{}{}",
                    bin.display(),
                    if cfg!(windows) { ";" } else { ":" },
                    inherited_path
                ),
            ));
            env.push((
                "VIRTUAL_ENV_PROMPT".into(),
                i.prefix
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ));
        }
    }
    env
}

#[cfg(test)]
mod manager_environment_tests {
    use super::*;
    #[test]
    fn selected_conda_activation_preserves_inherited_path_and_can_be_disabled() {
        let interpreter = Interpreter {
            path: "/envs/base/bin/python".into(),
            version: "3.12.1".into(),
            prefix: "/envs/base".into(),
            sys_path: Vec::new(),
            is_venv: false,
            platform: "linux".into(),
            kind: "Conda".into(),
        };
        let environment = terminal_env(true, Some(&interpreter), "/existing/bin");
        assert!(environment
            .iter()
            .any(|(key, value)| key == "CONDA_PREFIX" && value == "/envs/base"));
        assert!(environment
            .iter()
            .any(|(key, value)| key == "PATH" && value.ends_with("/existing/bin")));
        assert!(!environment.iter().any(|(key, _)| key == "VIRTUAL_ENV"));
        assert!(terminal_env(false, Some(&interpreter), "/existing/bin").is_empty());
    }
}

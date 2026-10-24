use super::*;
use rustpy_packages::{
    normalize_name,
    requirements::{is_satisfied_by, parse_requirement},
};
use rustpy_semantic::scope::BindingKind;
pub(super) fn declarations(
    root: &Path,
) -> (
    bool,
    Vec<rustpy_packages::Requirement>,
    Vec<(String, rustpy_packages::Requirement)>,
) {
    let mut found = false;
    let mut requirements = Vec::new();
    let mut incomplete = false;
    let mut optional = Vec::new();
    for name in ["requirements.txt", "requirements-dev.txt", "pyproject.toml"] {
        let Some(text) = read(&root.join(name)) else {
            continue;
        };
        if name.ends_with(".toml") {
            if let Ok(value) = toml::from_str::<toml::Value>(&text) {
                found |= value.get("project").is_some();
                incomplete |= value
                    .get("project")
                    .and_then(|v| v.get("dynamic"))
                    .and_then(|v| v.as_array())
                    .is_some_and(|values| {
                        values.iter().any(|v| {
                            matches!(v.as_str(), Some("dependencies" | "optional-dependencies"))
                        })
                    });
                if let Some(dependencies) = value
                    .get("tool")
                    .and_then(|v| v.get("poetry"))
                    .and_then(|v| v.get("dependencies"))
                    .and_then(|v| v.as_table())
                {
                    found = true;
                    requirements.extend(
                        dependencies
                            .keys()
                            .filter(|name| name.as_str() != "python")
                            .filter_map(|name| parse_requirement(name, 1)),
                    );
                }

                if let Some(values) = value
                    .get("project")
                    .and_then(|v| v.get("dependencies"))
                    .and_then(|v| v.as_array())
                {
                    requirements.extend(
                        values
                            .iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(|v| parse_requirement(v, 1)),
                    );
                }
                if let Some(groups) = value
                    .get("project")
                    .and_then(|v| v.get("optional-dependencies"))
                    .and_then(|v| v.as_table())
                {
                    for (group, values) in groups
                        .iter()
                        .filter_map(|(group, values)| Some((group, values.as_array()?)))
                    {
                        optional.extend(
                            values
                                .iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(|v| parse_requirement(v, 1))
                                .map(|requirement| (group.clone(), requirement)),
                        );
                    }
                }
            }
        } else {
            found = true;
            incomplete |= text.lines().any(|line| {
                ["-r ", "--requirement ", "-e ", "--editable "]
                    .iter()
                    .any(|prefix| line.trim_start().starts_with(*prefix))
            });
            if name == "requirements-dev.txt" {
                optional.extend(
                    rustpy_packages::parse_requirements(&text)
                        .into_iter()
                        .map(|requirement| ("dev".into(), requirement)),
                );
            } else {
                requirements.extend(rustpy_packages::parse_requirements(&text));
            }
        }
    }
    (found && !incomplete, requirements, optional)
}
pub(super) fn python(inf: &Inferer<'_>, file: &Arc<FileInfo>, inputs: &Inputs, sink: &mut Sink) {
    if inputs.interpreter_checked {
        if inputs.interpreter.is_none() {
            sink.push(
                TextRange::empty(TextSize::new(0)),
                Severity::Warning,
                "PyInterpreter",
                "No Python interpreter configured for the project",
                vec![],
            );
        } else if inputs.interpreter_valid == Some(false)
            || inputs.interpreter.as_ref().is_some_and(|p| !p.is_file())
        {
            sink.push(
                TextRange::empty(TextSize::new(0)),
                Severity::Warning,
                "PyInterpreter",
                "Invalid Python interpreter path",
                vec![],
            );
        }
    }
    let Some(root) = &inputs.root else {
        return;
    };
    let (found, mut declared, optional) = declarations(root);
    let known: std::collections::HashSet<_> = declared
        .iter()
        .chain(optional.iter().map(|(_, requirement)| requirement))
        .map(|r| normalize_name(&r.name))
        .collect();
    declared.extend(
        optional
            .into_iter()
            .filter(|(group, _)| inputs.active_extras.contains(group))
            .map(|(_, requirement)| requirement),
    );
    for binding in file.scopes.bindings.iter().take(10000) {
        let BindingKind::Import(import) = &binding.kind else {
            continue;
        };
        if import.level != 0 || import.module.is_empty() {
            continue;
        }
        let top = import.module.split('.').next().unwrap_or("");
        if rustpy_semantic::typeshed::is_stdlib(top) {
            continue;
        }
        if inf.db.module(top).is_some_and(|m| {
            !m.path.starts_with("<typeshed>")
                && inf.db.resolver.roots.iter().any(|r| m.path.starts_with(r))
        }) {
            continue;
        }
        let distribution =
            rustpy_packages::distribution_for_module(&import.module).unwrap_or_else(|| top.into());
        if found && !known.contains(&normalize_name(&distribution)) {
            sink.push(
                binding.range,
                Severity::WeakWarning,
                "PyUnlistedDependency",
                format!(
                    "Dependency '{distribution}' is not listed in requirements or pyproject.toml"
                ),
                vec![QuickFix {
                    title: format!("Add package requirement '{distribution}'"),
                    edits: vec![],
                }],
            );
        }
    }
    if let Some(installed) = &inputs.installed {
        for requirement in declared
            .iter()
            .filter(|r| {
                rustpy_packages::requirements::applies_in_environment(
                    r,
                    &inputs.marker_environment,
                    &inputs.active_extras,
                ) == Some(true)
            })
            .take(2048)
        {
            let package = installed
                .iter()
                .find(|(name, _)| normalize_name(name) == normalize_name(&requirement.name));
            if package.is_none_or(|(_, version)| !is_satisfied_by(requirement, version)) {
                let spec = format!(
                    "{}{}",
                    requirement.name,
                    requirement.specifier.as_deref().unwrap_or("")
                );
                sink.push(
                    TextRange::empty(TextSize::new(0)),
                    Severity::Warning,
                    "PyPackageRequirements",
                    format!(
                        "Package requirement '{spec}' is not satisfied{}",
                        package
                            .map(|(_, v)| format!(" (installed {v})"))
                            .unwrap_or_default()
                    ),
                    vec![QuickFix {
                        title: super::super::install_package_title(&spec),
                        edits: vec![],
                    }],
                );
            }
        }
        for (package, version, advisory) in inputs.vulnerabilities.iter().take(2048) {
            if installed
                .iter()
                .any(|(name, v)| normalize_name(name) == normalize_name(package) && v == version)
            {
                sink.push(
                    TextRange::empty(TextSize::new(0)),
                    Severity::Warning,
                    "PyVulnerablePackage",
                    format!("Package '{package} {version}' has vulnerable version: {advisory}"),
                    vec![],
                );
            }
        }
    }
}

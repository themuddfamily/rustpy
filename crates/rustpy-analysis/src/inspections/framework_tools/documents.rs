//! Shared non-Python inspection consumers. No tool execution or schema downloads.
use super::super::Problem;
use super::*;
use std::collections::{HashMap, HashSet};
pub(super) fn push(
    out: &mut Vec<Problem>,
    range: TextRange,
    code: &str,
    message: impl Into<String>,
) {
    out.push(Problem {
        diagnostic: rustpy_core::Diagnostic::warning(range, code, message),
        fixes: vec![],
    });
}
/// Inspect a complete immutable document. Syntactic validation is retained by
/// the language parser; this adds schema/configuration obligations only.
pub fn inspect(source: &str, path: &Path) -> Vec<Problem> {
    inspect_with_inputs(source, path, &Inputs::default())
}
pub fn inspect_with_inputs(source: &str, path: &Path, inputs: &Inputs) -> Vec<Problem> {
    if source.len() > 2 * 1024 * 1024 {
        return vec![];
    }
    let mut out = Vec::new();
    super::embedded::inspect(source, path, &mut out);
    if let Some(problem) = super::copyright(source, path, inputs) {
        out.push(problem);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "j2" | "jinja" | "jinja2" | "djhtml" | "html" | "txt") {
        if let Some(root) = &inputs.root {
            for link in rustpy_semantic::framework_templates::links(source) {
                let (directories, folder) = if link.kind == "static" {
                    (&inputs.static_directories, "static")
                } else {
                    (&inputs.template_directories, "templates")
                };
                if matches!(link.kind.as_str(), "block" | "endblock" | "macro") {
                    continue;
                }
                if rustpy_project::frameworks::resource(root, directories, &link.name, folder)
                    .is_none()
                {
                    push(
                        &mut out,
                        link.range,
                        if link.kind == "static" {
                            "UnresolvedStaticFile"
                        } else {
                            "UnresolvedTemplate"
                        },
                        format!("Unresolved {folder} resource '{}'", link.name),
                    );
                    if folder == "templates" {
                        out.last_mut().unwrap().fixes.push(QuickFix {
                            title: format!("Create template '{}'", link.name),
                            edits: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    if let Some(engine) =
        rustpy_syntax::template_alternate::engine(source, ext, &inputs.template_engine)
    {
        for (range, message) in rustpy_syntax::template_alternate::parse(source, engine).errors {
            push(
                &mut out,
                TextRange::new(
                    TextSize::new(range.start as u32),
                    TextSize::new(range.end as u32),
                ),
                "TemplateSyntax",
                message,
            );
        }
    }
    if name == "__init__.py"
        && path
            .parent()
            .is_some_and(|parent| inputs.namespace_roots.iter().any(|root| root == parent))
    {
        push(
            &mut out,
            TextRange::empty(TextSize::new(0)),
            "NamespacePackage",
            "__init__.py is incompatible with this explicitly marked implicit namespace package",
        );
    }
    if inputs.warn_non_ascii_paths && !path.to_string_lossy().is_ascii() {
        push(
            &mut out,
            TextRange::empty(TextSize::new(0)),
            "NonAsciiPath",
            "Path contains non-ASCII characters; verify compatibility with the configured tools",
        );
    }

    if (name.starts_with("requirements") && name.ends_with(".txt")) || name == "constraints.txt" {
        let parsed = rustpy_packages::requirements_file::parse(source);
        let requirements = rustpy_packages::parse_requirements(source);
        let mut exact = HashMap::<String, (rustpy_packages::version::Version, usize)>::new();
        for requirement in &requirements {
            let Some(specifier) = requirement.specifier.as_deref() else {
                continue;
            };
            if requirement.marker.is_some() {
                continue;
            }
            let Some(pin) = specifier
                .strip_prefix("==")
                .filter(|value| !value.contains([',', '*']))
            else {
                continue;
            };
            let name = rustpy_packages::normalize_name(&requirement.name);
            let version = rustpy_packages::version::parse_version(pin);
            if let Some((previous, line)) = exact.get(&name) {
                if previous != &version {
                    push(
                        &mut out,
                        position(source, requirement.line),
                        "PyRequirementConflict",
                        format!(
                            "Conflicting exact pins for {} (also line {})",
                            requirement.name, line
                        ),
                    );
                }
            } else {
                exact.insert(name, (version, requirement.line));
            }
        }
        for (line, text) in source.lines().enumerate() {
            if text.contains("#egg=")
                || text.trim_start().starts_with("--install-option")
                || text.contains("--use-deprecated=legacy-resolver")
            {
                push(&mut out,position(source,line+1),"PyDeprecatedRequirementSpecifier","Deprecated pip requirement syntax; use a PEP 508 named direct reference and current build settings");
            }
        }

        if let Some(installed) = &inputs.installed {
            for requirement in rustpy_packages::parse_requirements(source)
                .into_iter()
                .filter(|r| {
                    rustpy_packages::requirements::applies_in_environment(
                        r,
                        &inputs.marker_environment,
                        &inputs.active_extras,
                    ) == Some(true)
                })
                .take(2048)
            {
                let key = rustpy_packages::normalize_name(&requirement.name);
                let actual = installed
                    .iter()
                    .find(|(name, _)| rustpy_packages::normalize_name(name) == key);
                if name == "constraints.txt" && actual.is_none() {
                    continue;
                }
                if actual.is_none_or(|(_, v)| {
                    !rustpy_packages::requirements::is_satisfied_by(&requirement, v)
                }) {
                    let spec = format!(
                        "{}{}",
                        requirement.name,
                        requirement.specifier.as_deref().unwrap_or("")
                    );
                    push(
                        &mut out,
                        position(source, requirement.line),
                        "PyPackageRequirements",
                        format!("Package requirement '{spec}' is not satisfied"),
                    );
                    out.last_mut().unwrap().fixes.push(QuickFix {
                        title: super::super::install_package_title(&spec),
                        edits: vec![],
                    });
                }
            }
        }
        for error in parsed.diagnostics {
            push(
                &mut out,
                TextRange::new(
                    TextSize::new(error.range.start as u32),
                    TextSize::new(error.range.end as u32),
                ),
                "PyRequirementSpecifier",
                error.message,
            );
        }
    }
    if inputs.django_complete
        && (ext == "djhtml"
            || (inputs.django_template_engine
                && matches!(ext, "html" | "txt")
                && path.components().any(|p| p.as_os_str() == "templates")))
    {
        django_template(source, inputs, &mut out);
    }
    super::jinja::inspect(source, path, inputs, &mut out);
    if matches!(
        ext,
        "json" | "json5" | "jsonc" | "jsonl" | "ndjson" | "yaml" | "yml" | "toml"
    ) {
        if let Some(resource) = rustpy_project::data_schemas::resource(
            source,
            path,
            &inputs.schema_mappings,
            inputs.root.as_deref(),
        ) {
            rustpy_project::schema_store::request_resource(&resource, inputs.trusted);
            if let (Ok(parsed), Some(schema)) = (
                rustpy_project::data_formats::value(source, path),
                rustpy_project::data_schemas::schema(&resource, &inputs.schemas.values),
            ) {
                if parsed.nonfinite {
                    push(&mut out,TextRange::empty(TextSize::new(0)),"JsonSchemaNonFinite","JSON5 non-finite numbers cannot be represented in JSON Schema's finite-number data model");
                } else {
                    for mut problem in super::schema_engine::inspect(
                        &parsed.value.to_string(),
                        &schema,
                        Some(&resource),
                        &inputs.schemas,
                    ) {
                        problem.diagnostic.range = TextRange::empty(TextSize::new(0));
                        out.push(problem);
                    }
                }
            }
        }
    }
    if ext == "toml" {
        match toml::from_str::<toml::Value>(source) {
            Err(error) => {
                let range = error.span().unwrap_or(0..0);
                push(
                    &mut out,
                    TextRange::new(
                        TextSize::new(range.start as u32),
                        TextSize::new(range.end as u32),
                    ),
                    "TomlSchema",
                    error.message(),
                );
            }
            Ok(value) if name == "pyproject.toml" => pyproject(source, &value, &mut out),
            _ => {}
        }
    }
    if matches!(ext, "yaml" | "yml") && path.components().any(|p| p.as_os_str() == ".github") {
        github(source, &mut out);
    }
    if name.starts_with("Dockerfile") || ext == "dockerfile" {
        docker(source, &mut out);
    }
    if name == ".env" || name.starts_with(".env.") {
        duplicates(source, false, &mut out);
    }
    if matches!(name, ".gitignore" | ".dockerignore" | ".ignore") {
        duplicates(source, true, &mut out);
    }
    if name == ".editorconfig" {
        editorconfig(source, &mut out);
    }
    if matches!(ext, "md" | "markdown") {
        markdown(source, path, &mut out);
    }
    for (i, line) in source.lines().enumerate().take(20000) {
        let comment = line.trim_start();
        if (comment.starts_with('#')
            || comment.starts_with("//")
            || comment.starts_with("<!--")
            || comment.starts_with(".. "))
            && (comment.contains("TODO") || comment.contains("FIXME"))
        {
            push(
                &mut out,
                position(source, i + 1),
                "TodoComment",
                "Unfinished TODO/FIXME comment",
            );
        }
    }
    out.retain(|p| {
        !inputs
            .disabled
            .iter()
            .any(|code| code == p.diagnostic.code.as_str())
    });
    if let Some(policy) = &inputs.document_policy {
        policy.apply(path, &mut out);
    }
    out.truncate(128);
    out
}
fn pyproject(source: &str, value: &toml::Value, out: &mut Vec<Problem>) {
    let zero = TextRange::empty(TextSize::new(0));
    if let Some(project) = value.get("project") {
        let Some(project) = project.as_table() else {
            push(out, zero, "PyProjectSchema", "project must be a table");
            return;
        };
        if !project
            .get("name")
            .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            push(
                out,
                zero,
                "PyProjectSchema",
                "project.name must be a nonempty string",
            );
        }
        for key in [
            "name",
            "version",
            "description",
            "requires-python",
            "readme",
        ] {
            if let Some(v) = project.get(key) {
                if v.as_str().is_none() && !(key == "readme" && v.as_table().is_some()) {
                    push(
                        out,
                        zero,
                        "PyProjectSchema",
                        format!("project.{key} must be a string"),
                    );
                }
            }
        }
        let dynamic = project.get("dynamic").and_then(|v| v.as_array());
        if !project.contains_key("version")
            && !dynamic.is_some_and(|v| v.iter().any(|v| v.as_str() == Some("version")))
        {
            push(
                out,
                zero,
                "PyProjectSchema",
                "Declare project.version or list version as dynamic",
            );
        }
        if let Some(dynamic) = dynamic {
            for key in dynamic.iter().filter_map(|v| v.as_str()) {
                if project.contains_key(key) {
                    push(
                        out,
                        zero,
                        "PyProjectSchema",
                        format!("project.{key} cannot be both static and dynamic"),
                    );
                }
            }
        }
        let mut dependencies = Vec::new();
        if let Some(v) = project.get("dependencies") {
            if let Some(values) = v.as_array() {
                dependencies.extend(values.iter());
            } else {
                push(
                    out,
                    zero,
                    "PyProjectSchema",
                    "project.dependencies must be an array of PEP 508 strings",
                );
            }
        }
        if let Some(groups) = project.get("optional-dependencies") {
            if let Some(groups) = groups.as_table() {
                for values in groups.values() {
                    if let Some(values) = values.as_array() {
                        dependencies.extend(values.iter());
                    } else {
                        push(
                            out,
                            zero,
                            "PyProjectSchema",
                            "Optional dependencies must be arrays",
                        );
                    }
                }
            } else {
                push(
                    out,
                    zero,
                    "PyProjectSchema",
                    "project.optional-dependencies must be a table",
                );
            }
        }
        for value in dependencies.into_iter().take(2048) {
            if !value
                .as_str()
                .is_some_and(|v| rustpy_packages::requirements_file::parse_named(v).is_ok())
            {
                push(
                    out,
                    zero,
                    "PyRequirementSpecifier",
                    format!("Invalid package requirement {value}"),
                );
            }
        }
        if let Some(python) = project.get("requires-python").and_then(|v| v.as_str()) {
            if rustpy_packages::requirements_file::parse_named(&format!("python{python}")).is_err()
            {
                push(
                    out,
                    zero,
                    "PyRequirementSpecifier",
                    "Invalid requires-python version specifier",
                );
            }
        }
    }
    if let Some(build) = value.get("build-system") {
        if !build
            .get("requires")
            .and_then(|v| v.as_array())
            .is_some_and(|v| {
                v.iter().all(|v| {
                    v.as_str()
                        .is_some_and(|s| rustpy_packages::requirements_file::parse_named(s).is_ok())
                })
            })
        {
            push(
                out,
                zero,
                "PyProjectSchema",
                "build-system.requires must be an array of valid requirements",
            );
        }
        if build
            .get("build-backend")
            .is_some_and(|v| v.as_str().is_none())
        {
            push(
                out,
                zero,
                "PyProjectSchema",
                "build-system.build-backend must be a string",
            );
        }
    }
    let _ = source;
}
fn duplicates(source: &str, ignore: bool, out: &mut Vec<Problem>) {
    let mut seen = HashSet::new();
    for (i, line) in source.lines().enumerate() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let key = if ignore {
            text
        } else {
            let text = text.strip_prefix("export ").unwrap_or(text);
            let Some((key, _)) = text.split_once('=') else {
                push(
                    out,
                    position(source, i + 1),
                    "EnvFile",
                    "Expected KEY=value assignment",
                );
                continue;
            };
            key.trim()
        };
        if !seen.insert(key.to_string()) {
            let range = position(source, i + 1);
            push(
                out,
                range,
                if ignore {
                    "GitIgnorePattern"
                } else {
                    "EnvFile"
                },
                format!(
                    "Duplicate {} '{key}'",
                    if ignore {
                        "ignore pattern"
                    } else {
                        "environment variable"
                    }
                ),
            );
            let end = usize::from(range.end());
            let end = if source.as_bytes().get(end) == Some(&b'\n') {
                end + 1
            } else {
                end
            };
            if let Some(last) = out.last_mut() {
                last.fixes.push(QuickFix {
                    title: "Remove duplicate entry".into(),
                    edits: vec![(
                        TextRange::new(range.start(), TextSize::new(end as u32)),
                        String::new(),
                    )],
                });
            }
        }
    }
}
fn editorconfig(source: &str, out: &mut Vec<Problem>) {
    for issue in rustpy_settings::editorconfig::document::parse(source)
        .issues
        .into_iter()
        .filter(|issue| !issue.error)
    {
        let range = TextRange::new(
            TextSize::new(issue.range.start as u32),
            TextSize::new(issue.range.end as u32),
        );
        out.push(Problem {
            diagnostic: rustpy_core::Diagnostic::warning(range, "EditorConfig", issue.message),
            fixes: issue
                .replacement
                .map(|text| super::super::QuickFix {
                    title: "Use supported EditorConfig value".into(),
                    edits: vec![(range, text)],
                })
                .into_iter()
                .collect(),
        });
    }
}

fn docker(source: &str, out: &mut Vec<Problem>) {
    let mut aliases = HashSet::new();
    for (i, line) in source.lines().enumerate() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if !parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("FROM"))
        {
            continue;
        }
        let Some(image) = parts.iter().skip(1).find(|p| !p.starts_with("--")) else {
            push(
                out,
                position(source, i + 1),
                "Dockerfile",
                "FROM requires an image",
            );
            continue;
        };
        if *image != "scratch"
            && !image.contains('$')
            && !image.contains('@')
            && !image.rsplit('/').next().is_some_and(|n| n.contains(':'))
            && !aliases.contains(*image)
        {
            push(
                out,
                position(source, i + 1),
                "Dockerfile",
                "Pin the FROM image to an explicit tag or digest",
            );
        }
        if let Some(index) = parts.iter().position(|p| p.eq_ignore_ascii_case("AS")) {
            if let Some(alias) = parts.get(index + 1) {
                aliases.insert((*alias).to_string());
            }
        }
    }
}
fn markdown(source: &str, path: &Path, out: &mut Vec<Problem>) {
    let mut previous = 0;
    let mut fenced = false;
    let mut link_budget = 512usize;
    for (i, line) in source.lines().enumerate() {
        let text = line.trim();
        if text.starts_with("```") || text.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = text.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && text.as_bytes().get(level) == Some(&b' ') {
            if previous > 0 && level > previous + 1 {
                push(
                    out,
                    position(source, i + 1),
                    "Markdown",
                    "Heading skips a level",
                );
            }
            previous = level;
        }
        let mut rest = text;
        while let Some(start) = rest.find("](") {
            if link_budget == 0 {
                break;
            }
            link_budget -= 1;
            let after = &rest[start + 2..];
            let Some(end) = after.find(')') else {
                break;
            };
            let target = after[..end].split_whitespace().next().unwrap_or("");
            let target = target.split('#').next().unwrap_or("");
            if !target.is_empty()
                && !target.contains(':')
                && !target.contains(['{', '}'])
                && !target.starts_with('/')
                && !path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(target)
                    .exists()
            {
                push(
                    out,
                    position(source, i + 1),
                    "Markdown",
                    format!("Unresolved local link '{target}'"),
                );
            }
            rest = &after[end + 1..];
        }
    }
}
fn github(source: &str, out: &mut Vec<Problem>) {
    use rustpy_syntax::yaml::{Document, Kind};
    fn map(doc: &Document, id: usize) -> Option<HashMap<&str, usize>> {
        let Kind::Mapping(entries) = &doc.nodes.get(id)?.kind else {
            return None;
        };
        Some(
            entries
                .iter()
                .filter_map(|(k, v)| match &doc.nodes.get(*k)?.kind {
                    Kind::Scalar { value, .. } => Some((value.as_str(), *v)),
                    _ => None,
                })
                .collect(),
        )
    }
    let Ok(docs) = rustpy_syntax::yaml::parse(source) else {
        return;
    };
    for doc in docs {
        let Some(root) = map(&doc, doc.root) else {
            continue;
        };
        let Some(jobs) = root.get("jobs").and_then(|id| map(&doc, *id)) else {
            push(
                out,
                TextRange::empty(TextSize::new(0)),
                "YamlSchema",
                "GitHub workflow requires a jobs mapping",
            );
            continue;
        };
        for id in jobs.values() {
            let Some(job) = map(&doc, *id) else {
                push(
                    out,
                    position(source, doc.nodes[*id].line),
                    "YamlSchema",
                    "Job must be a mapping",
                );
                continue;
            };
            if !job.contains_key("uses") && !job.contains_key("runs-on") {
                push(
                    out,
                    position(source, doc.nodes[*id].line),
                    "YamlSchema",
                    "Job requires runs-on or reusable workflow uses",
                );
            }
            if let Some(steps) = job.get("steps") {
                if let Kind::Sequence(steps) = &doc.nodes[*steps].kind {
                    for step in steps {
                        if let Some(stepmap) = map(&doc, *step) {
                            if stepmap.contains_key("run") == stepmap.contains_key("uses") {
                                push(
                                    out,
                                    position(source, doc.nodes[*step].line),
                                    "YamlSchema",
                                    "Step requires exactly one of run or uses",
                                );
                            }
                        }
                    }
                } else {
                    push(
                        out,
                        position(source, doc.nodes[*steps].line),
                        "YamlSchema",
                        "steps must be a sequence",
                    );
                }
            }
        }
    }
}
/// Apply an already-captured local or SchemaStore JSON schema. The schema is
/// data, not executable code; recursion and aggregate work are bounded.
pub fn inspect_json_schema(source: &str, schema: &serde_json::Value) -> Vec<Problem> {
    super::schema_engine::inspect(source, schema, None, &super::SchemaSnapshot::default())
}

fn django_template(source: &str, inputs: &Inputs, out: &mut Vec<Problem>) {
    use rustpy_syntax::template::{self, Dialect, Kind};
    let document = template::parse(source, Dialect::Django);
    let mut tags: HashSet<_> = inputs.template_tags.iter().cloned().collect();
    let mut filters: HashSet<_> = inputs.template_filters.iter().cloned().collect();
    for region in document.regions.iter().take(10000) {
        if !matches!(region.kind, Kind::Statement | Kind::Expression) {
            continue;
        }
        let content = &source[region.content.clone()];
        let words: Vec<_> = content.split_whitespace().take(128).collect();
        let range = TextRange::new(
            TextSize::new(region.range.start as u32),
            TextSize::new(region.range.end as u32),
        );
        if region.kind == Kind::Statement {
            let name = words.first().copied().unwrap_or("");
            if name == "load" {
                let libraries: Vec<_> = if let Some(index) = words.iter().position(|w| *w == "from")
                {
                    words.get(index + 1).copied().into_iter().collect()
                } else {
                    words.iter().skip(1).copied().collect()
                };
                for library in libraries {
                    if let Some((_, libtags, libfilters)) = inputs
                        .template_libraries
                        .iter()
                        .find(|(name, _, _)| name == library)
                    {
                        tags.extend(libtags.iter().cloned());
                        filters.extend(libfilters.iter().cloned());
                    } else {
                        push(
                            out,
                            range,
                            "DjangoTemplate",
                            format!("Unknown template library '{library}'"),
                        );
                    }
                }
            } else if !name.starts_with("end")
                && !matches!(name, "else" | "elif" | "empty")
                && !tags.contains(name)
            {
                push(
                    out,
                    range,
                    "DjangoTemplate",
                    format!("Unresolved template tag '{name}'"),
                );
            }
            if name == "url" {
                if let Some(value) = words.get(1).filter(|s| s.starts_with(['\"', '\''])) {
                    let name = value.trim_matches(['\"', '\'']);
                    if !inputs.url_names.iter().any(|known| known == name) {
                        push(
                            out,
                            range,
                            "DjangoTemplate",
                            format!("Unresolved URL name '{name}'"),
                        );
                    }
                }
            }
        }
        let mut quote = None;
        let mut escaped = false;
        let mut rest = content.char_indices().peekable();
        while let Some((_, ch)) = rest.next() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if let Some(open) = quote {
                if ch == open {
                    quote = None;
                }
                continue;
            }
            if matches!(ch, '\"' | '\'') {
                quote = Some(ch);
                continue;
            }
            if ch == '|' {
                let mut name = String::new();
                while let Some((_, ch)) = rest.peek() {
                    if ch.is_alphanumeric() || *ch == '_' {
                        name.push(*ch);
                        rest.next();
                    } else {
                        break;
                    }
                }
                if !name.is_empty() && !filters.contains(&name) {
                    push(
                        out,
                        range,
                        "DjangoTemplate",
                        format!("Unresolved template filter '{name}'"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod data_document_fixtures {
    use super::*;
    #[test]
    fn requirement_conflicts_are_normalized_and_conditional_pins_not_conflated() {
        let issues=inspect("my_pkg==1.0\nmy-pkg==2\nconditional==1; sys_platform == 'win32'\nconditional==2; sys_platform == 'linux'\n",Path::new("requirements.txt"));
        assert_eq!(
            issues
                .iter()
                .filter(|issue| issue.diagnostic.code.as_str() == "PyRequirementConflict")
                .count(),
            1
        );
    }
    #[test]
    fn pip_legacy_url_requirement_is_reported_without_execution() {
        let issues = inspect(
            "git+https://example.invalid/project#egg=old\n",
            Path::new("requirements.txt"),
        );
        assert!(issues
            .iter()
            .any(|issue| issue.diagnostic.code.as_str() == "PyDeprecatedRequirementSpecifier"));
    }
}

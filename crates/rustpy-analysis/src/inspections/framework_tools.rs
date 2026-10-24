//! Framework/tool diagnostics. Context is captured by clients; inspections never
//! execute project code, package managers or network vulnerability queries.
use super::{InspectionOptions, QuickFix, Sink};
use rustpy_core::{Severity, TextRange, TextSize};
use rustpy_semantic::{db::FileInfo, infer::Inferer};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxNode,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
mod django_orm;
pub mod documents;
mod embedded;
mod jinja;
mod packages;
mod pytest;
mod schema_engine;
mod web;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub generation: u64,
    pub values: Arc<Vec<(String, String)>>,
}
impl std::hash::Hash for SchemaSnapshot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.generation, state);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Inputs {
    pub root: Option<PathBuf>,
    pub trusted: bool,
    pub namespace_roots: Vec<PathBuf>,
    pub warn_non_ascii_paths: bool,
    pub check_unused_ignore_patterns: bool,
    pub schemas: SchemaSnapshot,
    pub schema_mappings: Vec<(String, String)>,
    pub document_tools_generation: u64,
    pub project_revision: u64,
    pub sql_dialect: Option<String>,
    pub django_complete: bool,
    pub django_template_engine: bool,
    pub template_engine: String,
    pub template_data_language: String,
    pub template_directories: Vec<PathBuf>,
    pub static_directories: Vec<PathBuf>,
    pub template_tags: Vec<String>,
    pub template_libraries: Vec<(String, Vec<String>, Vec<String>)>,
    pub template_filters: Vec<String>,
    pub url_names: Vec<String>,
    pub settings_keys: Vec<String>,
    pub pending_migrations: Vec<String>,
    pub model_issues: Vec<(String, String, String)>,
    pub web_issues: Vec<(String, usize, String, String)>,
    pub document_policy: Option<super::infrastructure::Policy>,
    pub disabled: Vec<String>,
    /// False for library/CLI callers which did not capture interpreter selection.
    pub interpreter_checked: bool,
    pub interpreter_valid: Option<bool>,
    pub marker_environment: Vec<(String, String)>,
    pub active_extras: Vec<String>,
    pub interpreter: Option<PathBuf>,
    /// None means not loaded; an empty captured catalogue means no distributions.
    pub installed: Option<Vec<(String, String)>>,
    /// Already fetched advisories (package, affected installed version, advisory).
    pub vulnerabilities: Vec<(String, String, String)>,
    pub registered_marks: Vec<String>,
    pub plugin_fixtures: Vec<String>,
    /// Complete plugin fixture inventory; without it unknown plugin fixtures are
    /// not diagnosed simply because a plugin is unavailable to static discovery.
    pub fixtures_complete: bool,
    pub copyright_template: Option<String>,
    pub copyright_profiles: Option<String>,
    pub copyright_date: String,
    pub copyright_year: String,
    pub copyright_user: String,
}
pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    options: &InspectionOptions,
    sink: &mut Sink,
) {
    if file.text.len() > 2 * 1024 * 1024 {
        return;
    }
    let mut inputs = (*options.framework).clone();
    inputs.warn_non_ascii_paths |= options
        .policy
        .as_ref()
        .and_then(|p| p.rule("NonAsciiPath"))
        .and_then(|r| r.options.get("warn_non_ascii_paths"))
        .is_some_and(|value| value == "true");
    if let Some(policy) = &options.policy {
        if let Some(extras) = policy
            .rule("PyPackageRequirements")
            .and_then(|r| r.options.get("active_extras"))
        {
            inputs.active_extras.extend(
                extras
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        }

        if let Some(rule) = policy.rule("PytestUnknownFixture") {
            inputs.fixtures_complete |= rule
                .options
                .get("check_unknown_fixtures")
                .is_some_and(|v| v == "true");
            if let Some(names) = rule.options.get("known_fixtures") {
                inputs.plugin_fixtures.extend(
                    names
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );
            }
        }
        if inputs.root.as_ref().is_some_and(|r| {
            !r.join("requirements.txt").is_file() && !r.join("pyproject.toml").is_file()
        }) && policy
            .rule("PyPackageRequirements")
            .and_then(|r| r.options.get("require_requirements_file"))
            .is_some_and(|v| v == "true")
        {
            sink.push(
                TextRange::empty(TextSize::new(0)),
                Severity::Warning,
                "PyPackageRequirements",
                "Package requirements file not found",
                vec![QuickFix {
                    title: "Create package requirements file".into(),
                    edits: vec![],
                }],
            );
        }
    }
    packages::python(inf, file, &inputs, sink);
    pytest::run(inf, file, &inputs, sink);
    for problem in documents::inspect_with_inputs(&file.text, &file.path, &inputs) {
        sink.push(
            problem.diagnostic.range,
            problem.diagnostic.severity,
            problem.diagnostic.code.as_str(),
            problem.diagnostic.message,
            problem.fixes,
        );
    }

    web::run(inf, file, &inputs, sink);
    django_orm::run(inf, file, sink);
}
fn identity(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr) -> Option<String> {
    super::security::authority::identity(inf, file, expr)
}
fn warn(
    node: &SyntaxNode,
    code: &str,
    message: impl Into<String>,
    fixes: Vec<QuickFix>,
    sink: &mut Sink,
) {
    sink.push(node.text_range(), Severity::Warning, code, message, fixes);
}
fn literal(expr: &ast::Expr) -> Option<String> {
    super::security::calls_text(expr)
}
fn own(node: &SyntaxNode, owner: &SyntaxNode) -> bool {
    !node
        .ancestors()
        .skip(1)
        .take_while(|n| n != owner)
        .any(|n| {
            matches!(
                n.kind(),
                rustpy_syntax::SyntaxKind::FUNC_DEF
                    | rustpy_syntax::SyntaxKind::CLASS_DEF
                    | rustpy_syntax::SyntaxKind::LAMBDA_EXPR
            )
        })
}
fn read(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut source = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(262145)
        .read_to_string(&mut source)
        .ok()?;
    (source.len() <= 262144).then_some(source)
}
fn position(source: &str, line: usize) -> TextRange {
    let start = source
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let end = source[start.min(source.len())..]
        .find('\n')
        .map_or(source.len(), |n| start + n);
    TextRange::new(
        TextSize::new(start.min(source.len()) as u32),
        TextSize::new(end as u32),
    )
}

pub(super) fn copyright(source: &str, path: &Path, inputs: &Inputs) -> Option<super::Problem> {
    use rustpy_settings::copyright::{
        plan_update, CopyrightProfile, CopyrightProfilesSettings, Variables,
    };
    let configured = inputs
        .copyright_profiles
        .as_ref()
        .and_then(|text| serde_json::from_str::<CopyrightProfilesSettings>(text).ok());
    let profile = if let Some(configured) = configured {
        configured
            .profile_for(path, inputs.root.as_deref())
            .ok()??
            .clone()
    } else {
        CopyrightProfile::from_legacy(inputs.copyright_template.as_ref()?)
    };
    let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
    let project = inputs
        .root
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let variables = Variables {
        year: &inputs.copyright_year,
        date: &inputs.copyright_date,
        project,
        user: &inputs.copyright_user,
        file: name,
    };
    let edit = plan_update(source, path, &profile, &variables).ok()??;
    let range = TextRange::new(
        TextSize::new(edit.start as u32),
        TextSize::new(edit.end as u32),
    );
    Some(super::Problem {
        diagnostic: rustpy_core::Diagnostic::warning(
            range,
            "Copyright",
            "Copyright notice is missing or outdated",
        ),
        fixes: vec![QuickFix {
            title: "Update copyright notice".into(),
            edits: vec![(range, edit.replacement)],
        }],
    })
}

//! Pure external-tool launch plans, local/SSH path mapping and bounded output filters.
use crate::external_tools::{self, Invocation, InvocationError, MacroContext};
use rustpy_settings::{
    external_tools::{PathMapping, Ssh},
    ExternalTool,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsoleLinks {
    pub filters: Vec<String>,
    pub local_cwd: Option<PathBuf>,
    pub remote_cwd: Option<String>,
    pub mappings: Vec<PathMapping>,
}
pub(crate) fn compile_filter(pattern: &str) -> Result<String, String> {
    if pattern.len() > 8192 {
        return Err("Output filter exceeds 8192 bytes".into());
    }
    let pattern = pattern
        .replace("$FILE_PATH$", r"(?P<path>.+)")
        .replace("$LINE$", r"(?P<line>\d+)")
        .replace("$COLUMN$", r"(?P<column>\d+)");
    let regex = regex::RegexBuilder::new(&pattern)
        .size_limit(64 * 1024)
        .build()
        .map_err(|e| e.to_string())?;
    if !regex.capture_names().flatten().any(|name| name == "path") && regex.captures_len() < 2 {
        return Err("An output filter needs a named path capture or capture group 1".into());
    }
    Ok(pattern)
}
fn remote_root(root: &str) -> Result<String, String> {
    if !root.starts_with('/') || root.contains(['\0', '\n', '\r']) {
        return Err("Remote project/mapping root must be an absolute POSIX path".into());
    }
    let mut parts = Vec::new();
    for part in root.split('/') {
        match part {
            "" | "." => (),
            ".." => return Err("Remote roots cannot contain parent traversal".into()),
            _ => parts.push(part),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}
fn normalized_remote(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(format!("/{}", parts.join("/")))
}
fn remote_suffix<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root == "/" {
        Some(path.trim_start_matches('/'))
    } else {
        path.strip_prefix(root)
            .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
            .map(|suffix| suffix.trim_start_matches('/'))
    }
}
fn mappings(ssh: &Ssh, root: &Path) -> Result<Vec<PathMapping>, String> {
    let mut mappings = ssh
        .mappings
        .iter()
        .map(|mapping| {
            Ok(PathMapping {
                local: if mapping.local.is_absolute() {
                    mapping.local.clone()
                } else {
                    root.join(&mapping.local)
                },
                remote: remote_root(&mapping.remote)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    mappings.push(PathMapping {
        local: root.to_path_buf(),
        remote: remote_root(&ssh.remote_project_root)?,
    });
    mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.local.components().count()));
    Ok(mappings)
}
pub(crate) fn remote_path(path: &Path, mappings: &[PathMapping]) -> Result<PathBuf, String> {
    for mapping in mappings {
        if let Ok(suffix) = path.strip_prefix(&mapping.local) {
            if suffix
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err("Macro path escapes its SSH mapping".into());
            }
            let suffix = suffix
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            return Ok(PathBuf::from(format!(
                "{}/{}",
                mapping.remote.trim_end_matches('/'),
                suffix
            )));
        }
    }
    Err(format!("No SSH path mapping for {}", path.display()))
}
/// Paths outside a remote mapping never silently open an unrelated local file.
pub fn output_path(path: &Path, links: &ConsoleLinks) -> Option<PathBuf> {
    if let Some(cwd) = &links.remote_cwd {
        let text = path.to_string_lossy().into_owned();
        let absolute = normalized_remote(&if text.starts_with('/') {
            text
        } else {
            format!("{cwd}/{text}")
        })?;
        let mut mappings = links.mappings.iter().collect::<Vec<_>>();
        mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.remote.len()));
        for mapping in mappings {
            if let Some(suffix) = remote_suffix(&absolute, &mapping.remote) {
                return Some(mapping.local.join(suffix));
            }
        }
        None
    } else if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        links
            .local_cwd
            .as_ref()
            .map(|cwd| cwd.join(path))
            .or_else(|| Some(path.to_path_buf()))
    }
}
pub(crate) fn remote_context(
    ctx: &MacroContext,
    ssh: &Ssh,
) -> Result<(MacroContext, Vec<PathMapping>), String> {
    let root = ctx
        .project_root
        .as_deref()
        .ok_or("An SSH external tool requires an open project")?;
    let mappings = mappings(ssh, root)?;
    let mut remote = ctx.clone();
    remote.file = ctx
        .file
        .as_deref()
        .map(|file| remote_path(file, &mappings))
        .transpose()?;
    remote.project_root = Some(PathBuf::from(remote_root(&ssh.remote_project_root)?));
    remote.interpreter =
        (!ssh.remote_interpreter.is_empty()).then(|| PathBuf::from(&ssh.remote_interpreter));
    Ok((remote, mappings))
}
pub(crate) fn validate(tool: &ExternalTool) -> Result<Vec<String>, String> {
    if tool.name.trim().is_empty() {
        return Err("External tool needs a name".into());
    }
    for (key, value) in &tool.options.environment {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err("Invalid environment variable name/value".into());
        }
    }
    if tool.options.output_filters.len() > 32 {
        return Err("At most 32 output filters are supported per tool".into());
    }
    tool.options
        .output_filters
        .iter()
        .map(|pattern| compile_filter(pattern))
        .collect()
}
/// Local launches retain argv. SSH quotes each expanded argv value for the remote POSIX shell.
pub(crate) fn ssh_invocation(
    tool: &ExternalTool,
    inv: &Invocation,
    ctx: &MacroContext,
) -> Result<Invocation, String> {
    let ssh = tool.options.ssh.as_ref().ok_or("No SSH target")?;
    let target =
        rustpy_deploy::ssh::SshTarget::new(&ssh.host, ssh.port, &ssh.user, ssh.key_file.clone());
    target.validate().map_err(|e| e.to_string())?;
    if ssh.ssh_program.trim().is_empty() {
        return Err("SSH executable is empty".into());
    }
    let quote = rustpy_deploy::exec::sh_quote;
    let cwd = inv
        .working_dir
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| ssh.remote_project_root.clone());
    let cwd = if cwd.starts_with('/') {
        cwd
    } else {
        format!("{}/{cwd}", ssh.remote_project_root.trim_end_matches('/'))
    };
    let mut argv = vec![quote(&inv.program.to_string_lossy())];
    argv.extend(inv.args.iter().map(|arg| quote(arg)));
    let env = if tool.options.inherit_environment {
        "env --"
    } else {
        "env -i --"
    };
    let assignments = tool
        .options
        .environment
        .iter()
        .map(|(key, value)| {
            quote(&format!(
                "{key}={}",
                external_tools::expand_macros(value, ctx)
            ))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "cd {} && exec {env} {assignments} {}",
        quote(&cwd),
        argv.join(" ")
    );
    let mut captured = rustpy_deploy::transfer::current();
    if let Some(config) = &ssh.ssh_config_file {
        let text = config
            .to_str()
            .ok_or("SSH config path must be valid UTF-8")?;
        if !config.is_absolute() || text.chars().any(char::is_control) || !config.is_file() {
            return Err(
                "Select an existing absolute SSH config file without control characters".into(),
            );
        }
        captured.options.ssh_config_file = text.to_owned();
    }
    let command = rustpy_deploy::transfer::with_context(captured, || {
        rustpy_deploy::ssh::ssh_command(&target, &rustpy_deploy::ssh::remote_sh(&script))
    });
    Ok(Invocation {
        program: PathBuf::from(&ssh.ssh_program),
        args: command.args,
        working_dir: None,
    })
}
pub(crate) fn invocation(
    tool: &ExternalTool,
    ctx: &MacroContext,
) -> Result<Invocation, InvocationError> {
    if tool.options.ssh.is_some() {
        external_tools::build_invocation(
            &tool.program,
            &tool.arguments,
            &tool.working_dir,
            ctx,
            |_| true,
        )
    } else {
        external_tools::tool_invocation(tool, ctx)
    }
}

#[derive(Debug)]
pub struct Plan {
    pub config: rustpy_project::RunConfig,
    pub links: ConsoleLinks,
}
#[derive(Debug)]
pub enum PlanError {
    Invocation(InvocationError),
    Invalid(String),
}
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invocation(InvocationError::NeedsPrompt) => {
                f.write_str("Tool needs a prompt value")
            }
            Self::Invocation(InvocationError::NoProgram) => f.write_str("Tool program is empty"),
            Self::Invalid(error) => f.write_str(error),
        }
    }
}
/// An SSH launch can ask about first-use host keys only through the captured
/// IDE credential channel. The bridge stays local, never enters remote env.
/// The caller owns retiring its askpass job after the RunSession completes.
pub(crate) fn plan_with_ssh_confirmation(
    tool: &ExternalTool,
    ctx: &MacroContext,
    environment: &[(String, String)],
) -> Result<Plan, PlanError> {
    if !environment
        .iter()
        .any(|(key, value)| key == "RUSTPY_ASKPASS_ADDR" && !value.is_empty())
    {
        return Err(PlanError::Invalid(
            "Cannot start SSH host confirmation; credential bridge is unavailable".into(),
        ));
    }
    let mut context = rustpy_deploy::transfer::current();
    context.environment = environment.to_vec();
    let mut prepared = rustpy_deploy::transfer::with_context(context, || plan(tool, ctx))?;
    prepared.config.env.extend(environment.iter().cloned());
    Ok(prepared)
}

/// Prepare a captured launch without executing it. Callers still own project
/// trust admission and RunSession retirement; headless SSH refuses unknown keys.
pub fn plan(tool: &ExternalTool, ctx: &MacroContext) -> Result<Plan, PlanError> {
    let filters = validate(tool).map_err(PlanError::Invalid)?;
    if tool.program.trim().is_empty() {
        return Err(PlanError::Invocation(InvocationError::NoProgram));
    }
    if ctx.prompt.is_none()
        && tool
            .options
            .environment
            .values()
            .any(|value| external_tools::uses_prompt("", value, ""))
    {
        return Err(PlanError::Invocation(InvocationError::NeedsPrompt));
    }
    let root = ctx
        .project_root
        .clone()
        .ok_or_else(|| PlanError::Invalid("Open a project before running external tools".into()))?;
    let (context, mappings) = if let Some(ssh) = &tool.options.ssh {
        remote_context(ctx, ssh).map_err(PlanError::Invalid)?
    } else {
        (ctx.clone(), vec![])
    };
    let inv = invocation(tool, &context).map_err(PlanError::Invocation)?;
    if inv.program.to_string_lossy().contains('\0') || inv.args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(PlanError::Invalid(
            "Tool arguments cannot contain NUL bytes".into(),
        ));
    }
    let mut links = ConsoleLinks {
        filters,
        local_cwd: None,
        remote_cwd: None,
        mappings,
    };
    let inv = if let Some(ssh) = &tool.options.ssh {
        let dir = inv
            .working_dir
            .as_ref()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|| ssh.remote_project_root.clone());
        links.remote_cwd = Some(
            normalized_remote(&if dir.starts_with('/') {
                dir
            } else {
                format!("{}/{dir}", ssh.remote_project_root.trim_end_matches('/'))
            })
            .ok_or_else(|| PlanError::Invalid("Invalid remote working directory".into()))?,
        );
        links.local_cwd = Some(root.clone());
        ssh_invocation(tool, &inv, &context).map_err(PlanError::Invalid)?
    } else {
        links.local_cwd = Some(
            inv.working_dir
                .as_ref()
                .map(|dir| {
                    if dir.is_absolute() {
                        dir.clone()
                    } else {
                        root.join(dir)
                    }
                })
                .unwrap_or_else(|| root.clone()),
        );
        inv
    };
    let mut cfg = rustpy_project::RunConfig {
        name: tool.name.clone(),
        kind: rustpy_project::RunConfigKind::Program {
            program: inv.program,
            args: inv.args,
        },
        working_dir: Some(links.local_cwd.clone().unwrap_or(root)),
        temporary: true,
        add_content_roots: false,
        add_source_roots: false,
        show_console_on_stdout: false,
        show_console_on_stderr: false,
        ..Default::default()
    };
    if tool.options.ssh.is_none() {
        cfg.env = tool
            .options
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), external_tools::expand_macros(value, &context)))
            .collect();
        cfg.workflow.inherit_environment = tool.options.inherit_environment;
    }
    Ok(Plan { config: cfg, links })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssh_confirmation_cannot_be_requested_without_an_owned_bridge() {
        let tool = ExternalTool::default();
        assert!(matches!(
            plan_with_ssh_confirmation(&tool, &MacroContext::default(), &[]),
            Err(PlanError::Invalid(_))
        ));
    }
    fn context() -> MacroContext {
        MacroContext {
            project_root: Some("/local/project".into()),
            file: Some("/local/project/dir/a b.py".into()),
            selected_text: Some("x; $(inert) 'quoted'".into()),
            ..Default::default()
        }
    }
    fn tool() -> ExternalTool {
        ExternalTool {
            name: "Inspect".into(),
            program: "python3".into(),
            arguments: "$FilePath$ '$SelectedText$'".into(),
            working_dir: "$ProjectFileDir$".into(),
            ..Default::default()
        }
    }
    #[test]
    fn local_plan_preserves_macro_words_environment_and_cwd() {
        let mut tool = tool();
        tool.options
            .environment
            .insert("SOURCE".into(), "$FilePath$".into());
        tool.options.inherit_environment = false;
        let plan = plan(&tool, &context()).unwrap();
        let rustpy_project::RunConfigKind::Program { args, .. } = plan.config.kind else {
            panic!("program")
        };
        assert_eq!(
            args,
            vec!["/local/project/dir/a b.py", "x; $(inert) 'quoted'"]
        );
        assert_eq!(plan.config.env["SOURCE"], "/local/project/dir/a b.py");
        assert!(!plan.config.workflow.inherit_environment);
        assert!(!plan.config.show_console_on_stderr);
    }
    #[test]
    fn ssh_plan_quotes_remote_argv_and_maps_output_back() {
        let mut tool = tool();
        tool.options.ssh = Some(Ssh {
            host: "build.example".into(),
            user: "worker".into(),
            remote_project_root: "/srv/project".into(),
            ..Default::default()
        });
        let planned = plan(&tool, &context()).unwrap();
        let rustpy_project::RunConfigKind::Program { program, args } = planned.config.kind else {
            panic!("program")
        };
        assert_eq!(program, PathBuf::from("ssh"));
        assert!(args.iter().any(|arg| arg == "BatchMode=yes"));
        assert!(args.iter().any(|arg| arg == "worker@build.example"));
        let script = format!(
            "cd {} && exec env --  {} {} {}",
            rustpy_deploy::exec::sh_quote("/srv/project"),
            rustpy_deploy::exec::sh_quote("python3"),
            rustpy_deploy::exec::sh_quote("/srv/project/dir/a b.py"),
            rustpy_deploy::exec::sh_quote("x; $(inert) 'quoted'")
        );
        assert_eq!(
            args.last().unwrap(),
            &rustpy_deploy::ssh::remote_sh(&script)
        );
        assert_eq!(
            output_path(Path::new("dir/a b.py"), &planned.links),
            Some(PathBuf::from("/local/project/dir/a b.py"))
        );
        assert_eq!(
            output_path(Path::new("/srv/project-other/a.py"), &planned.links),
            None
        );
        assert_eq!(
            output_path(Path::new("../../etc/passwd"), &planned.links),
            None
        );
    }
    #[test]
    fn explicit_ssh_config_is_validated_and_scoped_to_its_plan() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("private ssh config");
        std::fs::write(&selected, "Host *\n  BatchMode yes\n").unwrap();
        let mut tool = tool();
        tool.options.ssh = Some(Ssh {
            host: "build.example".into(),
            remote_project_root: "/srv/project".into(),
            ssh_config_file: Some(selected.clone()),
            ..Default::default()
        });
        let before = rustpy_deploy::transfer::current().options.ssh_config_file;
        let planned = plan(&tool, &context()).unwrap();
        let rustpy_project::RunConfigKind::Program { args, .. } = planned.config.kind else {
            panic!("SSH program")
        };
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-F" && Path::new(&pair[1]) == selected.as_path()));
        assert_eq!(
            rustpy_deploy::transfer::current().options.ssh_config_file,
            before
        );
        for rejected in [
            PathBuf::from("relative-config"),
            directory.path().join("missing"),
            directory.path().join("bad\nconfig"),
        ] {
            tool.options.ssh.as_mut().unwrap().ssh_config_file = Some(rejected);
            assert!(plan(&tool, &context()).is_err());
            assert_eq!(
                rustpy_deploy::transfer::current().options.ssh_config_file,
                before
            );
        }
    }
    #[test]
    fn longest_mapping_is_source_authoritative() {
        let ssh = Ssh {
            remote_project_root: "/srv/project".into(),
            mappings: vec![PathMapping {
                local: "/local/project/generated".into(),
                remote: "/opt/generated".into(),
            }],
            ..Default::default()
        };
        let mappings = mappings(&ssh, Path::new("/local/project")).unwrap();
        assert_eq!(
            remote_path(Path::new("/local/project/generated/a.py"), &mappings).unwrap(),
            PathBuf::from("/opt/generated/a.py")
        );
        let links = ConsoleLinks {
            remote_cwd: Some("/srv/project".into()),
            mappings,
            ..Default::default()
        };
        assert_eq!(
            output_path(Path::new("/opt/generated/a.py"), &links),
            Some(PathBuf::from("/local/project/generated/a.py"))
        );
    }
    #[test]
    fn filters_link_unicode_paths_and_reject_oversized_or_captureless_patterns() {
        let pattern = compile_filter(r"^$FILE_PATH$:$LINE$:$COLUMN$:").unwrap();
        let regex = regex::Regex::new(&pattern).unwrap();
        let captures = regex.captures("/project/é x.py:23:4: error").unwrap();
        assert_eq!(&captures["path"], "/project/é x.py");
        assert_eq!(&captures["line"], "23");
        assert_eq!(&captures["column"], "4");
        assert!(compile_filter(".*").is_err());
        assert!(compile_filter(&"x".repeat(8193)).is_err());
        assert!(compile_filter("(").is_err());
    }
    #[test]
    fn prompt_in_environment_is_not_silently_dropped_and_ssh_options_cannot_be_hosts() {
        let mut tool = tool();
        tool.options
            .environment
            .insert("ANSWER".into(), "$Prompt$".into());
        assert!(matches!(
            plan(&tool, &context()),
            Err(PlanError::Invocation(InvocationError::NeedsPrompt))
        ));
        tool.options.environment.clear();
        tool.options.ssh = Some(Ssh {
            host: "-oProxyCommand=bad".into(),
            remote_project_root: "/srv/project".into(),
            ..Default::default()
        });
        assert!(matches!(
            plan(&tool, &context()),
            Err(PlanError::Invalid(_))
        ));
    }
}

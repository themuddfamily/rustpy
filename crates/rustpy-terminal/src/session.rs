//! Direct argv plans for local and explicitly configured remote terminal sessions.
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Default)]
pub struct Target {
    pub kind: String,
    pub name: String,
    pub shell: String,
    pub args: Vec<String>,
    pub directory: String,
    pub user: String,
    pub port: u16,
    pub key: String,
    pub environment: Vec<(String, String)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub remote: bool,
}
pub fn plan(target: &Target, cwd: &Path) -> anyhow::Result<Plan> {
    for value in [
        &target.name,
        &target.shell,
        &target.directory,
        &target.user,
        &target.key,
    ] {
        anyhow::ensure!(
            !value.contains(['\0', '\r', '\n']),
            "Terminal target values must be single-line"
        );
    }
    anyhow::ensure!(
        !target.name.starts_with('-') && !target.user.starts_with('-'),
        "Invalid terminal target"
    );
    for (key, value) in &target.environment {
        anyhow::ensure!(
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !key.starts_with(|c: char| c.is_ascii_digit())
                && !value.contains('\0'),
            "Invalid terminal environment variable"
        );
    }
    let shell = if target.shell.is_empty() {
        if target.kind == "local" || target.kind.is_empty() {
            crate::default_shell()
        } else {
            "/bin/sh".into()
        }
    } else {
        target.shell.clone()
    };
    let args = if target.args.is_empty()
        && (shell.ends_with("bash") || shell.ends_with("zsh") || shell.ends_with("fish"))
    {
        vec!["-l".into()]
    } else {
        target.args.clone()
    };
    let quote = super::integration::quote;
    let command = || {
        let mut command = String::new();
        if !target.directory.is_empty() {
            command.push_str(&format!("cd -- {} && ", quote(&target.directory)));
        }
        command.push_str("exec ");
        if !target.environment.is_empty() {
            command.push_str("env ");
            for (key, value) in &target.environment {
                command.push_str(&quote(&format!("{key}={value}")));
                command.push(' ');
            }
        }
        command.push_str(&quote(&shell));
        for arg in &args {
            command.push(' ');
            command.push_str(&quote(arg));
        }
        command
    };
    let (program, prefix, remote) = match target.kind.as_str() {
        "" | "local" => (shell, args, false),
        "ssh" => {
            anyhow::ensure!(!target.name.is_empty(), "Choose an SSH host");
            let mut out = vec![
                "-tt".into(),
                "-o".into(),
                "StrictHostKeyChecking=ask".into(),
                "-o".into(),
                "ServerAliveInterval=15".into(),
                "-p".into(),
                target.port.max(1).to_string(),
            ];
            if !target.key.is_empty() {
                out.extend(["-i".into(), target.key.clone()]);
            }
            if !target.user.is_empty() {
                out.extend(["-l".into(), target.user.clone()]);
            }
            out.extend(["--".into(), target.name.clone(), command()]);
            ("ssh".into(), out, true)
        }
        "wsl" => {
            let mut out = Vec::new();
            if !target.name.is_empty() {
                out.extend(["--distribution".into(), target.name.clone()]);
            }
            if !target.directory.is_empty() {
                out.extend(["--cd".into(), target.directory.clone()]);
            }
            out.push("--exec".into());
            if !target.environment.is_empty() {
                out.push("env".into());
                out.extend(
                    target
                        .environment
                        .iter()
                        .map(|(key, value)| format!("{key}={value}")),
                );
            }
            out.push(shell);
            out.extend(args);
            ("wsl.exe".into(), out, true)
        }
        "docker" | "compose" => {
            anyhow::ensure!(
                !target.name.is_empty(),
                "Choose a container or Compose service"
            );
            let mut out = if target.kind == "compose" {
                vec!["compose".into(), "exec".into()]
            } else {
                vec!["exec".into(), "-it".into()]
            };
            if !target.directory.is_empty() {
                out.extend(["--workdir".into(), target.directory.clone()]);
            }
            for (key, value) in &target.environment {
                out.extend(["--env".into(), format!("{key}={value}")]);
            }
            out.extend(["--".into(), target.name.clone(), shell]);
            out.extend(args);
            ("docker".into(), out, true)
        }
        "vagrant" => {
            let mut out = vec!["ssh".into()];
            if !target.name.is_empty() {
                out.push(target.name.clone());
            }
            out.extend(["--command".into(), command()]);
            ("vagrant".into(), out, true)
        }
        _ => anyhow::bail!("Unknown terminal target type"),
    };
    Ok(Plan {
        program,
        args: prefix,
        cwd: cwd.into(),
        remote,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssh_retains_literal_arguments_and_no_persisted_password() {
        let p = plan(
            &Target {
                kind: "ssh".into(),
                name: "host".into(),
                shell: "/bin/bash".into(),
                args: vec!["-i".into()],
                directory: "/a b/it's".into(),
                port: 22,
                ..Default::default()
            },
            Path::new("/local"),
        )
        .unwrap();
        assert!(p.args.last().unwrap().contains("'/a b/it'\\''s'"));
        assert!(p.args.contains(&"-tt".into()));
    }
    #[test]
    fn wsl_has_real_distribution_and_shell_argv() {
        let p = plan(
            &Target {
                kind: "wsl".into(),
                name: "Ubuntu".into(),
                shell: "/bin/fish".into(),
                ..Default::default()
            },
            Path::new("/local"),
        )
        .unwrap();
        assert_eq!(p.program, "wsl.exe");
        assert_eq!(&p.args[..2], &["--distribution", "Ubuntu"]);
    }
}

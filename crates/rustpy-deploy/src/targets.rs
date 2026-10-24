//! Captured Docker/Compose/registry and Vagrant command plans; no discovery side effects.
use crate::CommandSpec;
pub use rustpy_run::{DockerResources, LegacyDockerBuildResources};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Docker {
    pub name: String,
    pub executable: String,
    pub endpoint: String,
    pub context: String,
    pub tls: bool,
    pub ca: String,
    pub certificate: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<DockerResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_build_resources: Option<LegacyDockerBuildResources>,
}
impl Default for Docker {
    fn default() -> Self {
        Self {
            name: "Docker".into(),
            executable: "docker".into(),
            endpoint: String::new(),
            context: String::new(),
            tls: false,
            ca: String::new(),
            certificate: String::new(),
            key: String::new(),
            resources: None,
            legacy_build_resources: None,
        }
    }
}
impl Docker {
    pub fn command(&self, args: Vec<String>) -> anyhow::Result<CommandSpec> {
        if args.first().map(String::as_str) == Some("build") {
            if let Some(resources) = &self.legacy_build_resources {
                return Ok(self
                    .base_command(resources.apply(&args).map_err(anyhow::Error::msg)?)?
                    .with_env("DOCKER_BUILDKIT", "0"));
            }
        }
        let args = if let Some(resources) = &self.resources {
            match args.first().map(String::as_str) {
                Some("run" | "create") => resources
                    .apply_container_command(&args)
                    .map_err(anyhow::Error::msg)?,
                Some("build") => anyhow::bail!(
                    "Docker image build resource policy is not supported by this builder route"
                ),
                _ => args,
            }
        } else {
            args
        };
        self.base_command(args)
    }
    /// Bind relative build contexts to the captured project directory.
    pub fn command_at(&self, args: Vec<String>, working_dir: &Path) -> anyhow::Result<CommandSpec> {
        if args.first().map(String::as_str) == Some("build") {
            if let Some(resources) = &self.legacy_build_resources {
                return Ok(self
                    .base_command(
                        resources
                            .apply_at(&args, working_dir)
                            .map_err(anyhow::Error::msg)?,
                    )?
                    .with_env("DOCKER_BUILDKIT", "0"));
            }
        }
        self.command(args)
    }
    fn base_command(&self, args: Vec<String>) -> anyhow::Result<CommandSpec> {
        anyhow::ensure!(
            self.endpoint.is_empty() || self.context.is_empty(),
            "Choose either a daemon endpoint or a Docker context"
        );
        let mut result = CommandSpec::new(if self.executable.is_empty() {
            "docker"
        } else {
            &self.executable
        });
        if !self.endpoint.is_empty() {
            anyhow::ensure!(
                self.endpoint.starts_with("unix://")
                    || self.endpoint.starts_with("npipe://")
                    || self.endpoint.starts_with("tcp://")
                    || self.endpoint.starts_with("ssh://"),
                "Use a unix, npipe, TCP or SSH Docker endpoint"
            );
            result = result.args(["--host".into(), self.endpoint.clone()]);
        }
        if !self.context.is_empty() {
            result = result.args(["--context".into(), self.context.clone()]);
        }
        if self.tls {
            result = result.arg("--tlsverify");
            for (flag, value) in [
                ("--tlscacert", &self.ca),
                ("--tlscert", &self.certificate),
                ("--tlskey", &self.key),
            ] {
                if !value.is_empty() {
                    result = result.args([flag.into(), value.clone()]);
                }
            }
        }
        Ok(result.args(args))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub name: String,
    pub host: String,
    pub username: String,
    pub save_password: bool,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            name: "Docker Hub".into(),
            host: "docker.io".into(),
            username: String::new(),
            save_password: true,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub default_docker: String,
    pub docker: Vec<Docker>,
    pub registries: Vec<Registry>,
    pub vagrant_executable: String,
    pub machines: Vec<String>,
}
impl Tools {
    pub fn default_connection(&self) -> Docker {
        self.docker
            .iter()
            .find(|connection| connection.name == self.default_docker)
            .or_else(|| self.docker.first())
            .cloned()
            .unwrap_or_default()
    }
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(".rustpy/deployment-tools.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| Self {
                docker: vec![Docker::default()],
                registries: vec![Registry::default()],
                ..Default::default()
            })
    }
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let directory = root.join(".rustpy");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("deployment-tools.json");
        let temporary = directory.join(".deployment-tools.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
}
#[derive(Clone, Debug, Default)]
pub struct DockerArguments {
    pub target: String,
    pub image: String,
    pub name: String,
    pub command: Vec<String>,
    pub ports: Vec<String>,
    pub volumes: Vec<String>,
    pub environment: Vec<String>,
    pub path: String,
    pub destination: String,
    pub compose_file: String,
    pub service: String,
    pub profiles: Vec<String>,
    pub env_file: String,
    pub scale: u32,
    pub build: bool,
}
fn target(value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !value.is_empty() && !value.starts_with('-') && !value.contains(['\0', '\r', '\n']),
        "Select a resource or enter its name"
    );
    Ok(value.into())
}
pub fn docker_args(
    operation: &str,
    a: &DockerArguments,
    root: &Path,
) -> anyhow::Result<Vec<String>> {
    let t = || target(&a.target);
    let image = || target(&a.image);
    let mut args = match operation {
        "Test connection" => vec!["version".into()],
        "Images" => vec![
            "image".into(),
            "ls".into(),
            "--format".into(),
            "{{json .}}".into(),
        ],
        "Pull image" => vec!["image".into(), "pull".into(), image()?],
        "Push image" => vec!["image".into(), "push".into(), image()?],
        "Delete image" => vec!["image".into(), "rm".into(), t()?],
        "Containers" => vec![
            "ps".into(),
            "--all".into(),
            "--format".into(),
            "{{json .}}".into(),
        ],
        "Start" | "Stop" | "Restart" | "Delete container" => vec![
            match operation {
                "Start" => "start",
                "Stop" => "stop",
                "Restart" => "restart",
                _ => "rm",
            }
            .into(),
            t()?,
        ],
        "Logs" => vec![
            "logs".into(),
            "--timestamps".into(),
            "--tail=500".into(),
            t()?,
        ],
        "Attach" => vec!["attach".into(), t()?],
        "Exec terminal" => vec![
            "exec".into(),
            "-it".into(),
            t()?,
            if a.command.is_empty() {
                "/bin/sh".into()
            } else {
                a.command[0].clone()
            },
        ],
        "Inspect" | "Properties" => vec!["inspect".into(), t()?],
        "Ports" => vec!["port".into(), t()?],
        "Environment" => vec![
            "inspect".into(),
            "--format".into(),
            "{{json .Config.Env}}".into(),
            t()?,
        ],
        "Files" => vec![
            "exec".into(),
            t()?,
            "ls".into(),
            "-1ap".into(),
            "--".into(),
            if a.path.is_empty() {
                "/".into()
            } else {
                a.path.clone()
            },
        ],
        "Download container file" => vec![
            "cp".into(),
            format!("{}:{}", t()?, a.path),
            target(&a.destination)?,
        ],
        "Upload container file" => vec![
            "cp".into(),
            target(&a.path)?,
            format!("{}:{}", t()?, target(&a.destination)?),
        ],
        "Volumes" => vec![
            "volume".into(),
            "ls".into(),
            "--format".into(),
            "{{json .}}".into(),
        ],
        "Networks" => vec![
            "network".into(),
            "ls".into(),
            "--format".into(),
            "{{json .}}".into(),
        ],
        "Inspect volume" | "Delete volume" | "Create volume" | "Inspect network"
        | "Delete network" | "Create network" => vec![
            if operation.ends_with("volume") {
                "volume"
            } else {
                "network"
            }
            .into(),
            if operation.starts_with("Inspect") {
                "inspect"
            } else if operation.starts_with("Delete") {
                "rm"
            } else {
                "create"
            }
            .into(),
            t()?,
        ],
        "Build Dockerfile" => vec![
            "build".into(),
            "--file".into(),
            target(&a.path)?,
            "--tag".into(),
            image()?,
            if a.destination.is_empty() {
                root.to_string_lossy().into_owned()
            } else {
                a.destination.clone()
            },
        ],
        "Create container" | "Run image" => {
            let mut result = vec![if operation == "Run image" {
                "run"
            } else {
                "create"
            }
            .into()];
            if operation == "Run image" {
                result.push("--detach".into());
            }
            if !a.name.is_empty() {
                result.extend(["--name".into(), target(&a.name)?]);
            }
            for (flag, values) in [
                ("--publish", &a.ports),
                ("--volume", &a.volumes),
                ("--env", &a.environment),
            ] {
                for value in values {
                    result.extend([flag.into(), value.clone()]);
                }
            }
            result.push(image()?);
            result.extend(a.command.clone());
            result
        }
        "Compose up" | "Compose down" | "Compose restart" | "Compose logs" | "Compose scale"
        | "Compose services" | "Validate Compose" => {
            let mut result = vec![
                "compose".into(),
                "--project-directory".into(),
                root.to_string_lossy().into_owned(),
            ];
            if !a.compose_file.is_empty() {
                result.extend(["--file".into(), a.compose_file.clone()]);
            }
            if !a.env_file.is_empty() {
                result.extend(["--env-file".into(), a.env_file.clone()]);
            }
            for profile in &a.profiles {
                result.extend(["--profile".into(), profile.clone()]);
            }
            match operation {
                "Compose up" | "Compose scale" => {
                    result.extend(["up".into(), "--detach".into()]);
                    if a.build {
                        result.push("--build".into());
                    }
                    if operation == "Compose scale" {
                        result.extend([
                            "--scale".into(),
                            format!("{}={}", target(&a.service)?, a.scale.max(1)),
                        ]);
                    }
                }
                "Compose down" => result.push("down".into()),
                "Compose restart" => result.push("restart".into()),
                "Compose logs" => result.extend(["logs".into(), "--tail=500".into()]),
                "Compose services" => result.extend(["config".into(), "--services".into()]),
                _ => result.extend(["config".into(), "--format=json".into()]),
            }
            if !a.service.is_empty()
                && !matches!(
                    operation,
                    "Compose down" | "Compose services" | "Validate Compose"
                )
            {
                result.push(a.service.clone());
            }
            result
        }
        _ => anyhow::bail!("Unknown Docker operation"),
    };
    if operation == "Exec terminal" && a.command.len() > 1 {
        args.extend(a.command[1..].to_vec());
    }
    Ok(args)
}
pub fn vagrant_args(operation: &str, machine: &str, box_name: &str) -> anyhow::Result<Vec<String>> {
    let mut args = match operation {
        "Status" => vec!["status".into(), "--machine-readable".into()],
        "Up" | "Halt" | "Reload" | "Destroy" | "SSH" | "SSH config" => vec![match operation {
            "SSH config" => "ssh-config",
            "SSH" => "ssh",
            "Up" => "up",
            "Halt" => "halt",
            "Reload" => "reload",
            _ => "destroy",
        }
        .into()],
        "Boxes" => vec!["box".into(), "list".into()],
        "Add box" => vec!["box".into(), "add".into(), target(box_name)?],
        "Remove box" => vec!["box".into(), "remove".into(), target(box_name)?],
        "Update boxes" => vec!["box".into(), "update".into()],
        _ => anyhow::bail!("Unknown Vagrant operation"),
    };
    if operation == "Destroy" {
        args.push("--force".into());
    }
    if !machine.is_empty()
        && !matches!(
            operation,
            "Boxes" | "Add box" | "Remove box" | "Update boxes" | "Status"
        )
    {
        args.push(target(machine)?);
    }
    Ok(args)
}
pub fn registry_login(
    connection: &Docker,
    registry: &Registry,
    password: String,
) -> anyhow::Result<CommandSpec> {
    let mut command = connection.command(vec![
        "login".into(),
        "--username".into(),
        target(&registry.username)?,
        "--password-stdin".into(),
        target(&registry.host)?,
    ])?;
    anyhow::ensure!(
        !password.contains(['\0', '\r', '\n']),
        "Registry password must be one line"
    );
    command.stdin = Some(format!("{password}\n"));
    Ok(command)
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn tls_and_compose_scale_are_actual_argv() {
        let docker = Docker {
            endpoint: "tcp://host:2376".into(),
            tls: true,
            ca: "/keys/ca.pem".into(),
            ..Default::default()
        };
        let args = docker_args(
            "Compose scale",
            &DockerArguments {
                service: "api".into(),
                scale: 3,
                profiles: vec!["dev".into()],
                ..Default::default()
            },
            Path::new("/p"),
        )
        .unwrap();
        let command = docker.command(args).unwrap();
        assert!(command.args.contains(&"--tlsverify".into()));
        assert!(command.args.contains(&"api=3".into()));
    }
    #[cfg(unix)]
    #[test]
    fn vagrant_adapter_pins_the_project_vagrantfile() {
        let root = std::env::temp_dir().join(format!(
            "rustpy-vagrant-adapter-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = install_python_adapter(None, "web", "python3", Some("vagrant"), &root).unwrap();
        let script = std::fs::read_to_string(&file).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            script.contains(&format!(
                "VAGRANT_CWD={}\nexport VAGRANT_CWD\n",
                crate::sh_quote(&root.to_string_lossy())
            )),
            "{script}"
        );
        // The guest shell must receive every argument verbatim.
        let quote = script.lines().find(|l| l.starts_with("quote()")).unwrap();
        let value = "it's a \"$HOME\" \\ π";
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("{quote}\neval \"set -- $(quote \"$1\")\"\nprintf '%s' \"$1\""),
                "sh",
                value,
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), value);
    }
    #[test]
    fn login_keeps_password_out_of_command_and_debug() {
        let command = registry_login(
            &Docker::default(),
            &Registry {
                username: "user".into(),
                ..Default::default()
            },
            "secret".into(),
        )
        .unwrap();
        assert!(!command.display().contains("secret"));
        assert!(!format!("{command:?}").contains("secret"));
        assert!(command.stdin.unwrap().contains("secret"));
    }
}
/// A genuine Python argv adapter for a running container or configured Vagrant machine.
pub fn python_command(
    connection: Option<&Docker>,
    machine: &str,
    python: &str,
    args: &[String],
    vagrant: Option<&str>,
) -> anyhow::Result<CommandSpec> {
    let python = target(python)?;
    let machine = target(machine)?;
    if let Some(connection) = connection {
        let mut prefix = vec!["exec".into(), "-i".into()];
        if let Some(resources) = &connection.resources {
            resources.validate().map_err(anyhow::Error::msg)?;
            prefix.extend(resources.environment_args());
        }
        prefix.push(machine);
        if let Some(resources) = &connection.resources {
            prefix.extend(resources.process_prefix().map_err(anyhow::Error::msg)?);
        }
        prefix.push(python);
        prefix.extend(args.iter().cloned());
        connection.command(prefix)
    } else {
        let mut remote = format!("exec {}", crate::sh_quote(&python));
        for arg in args {
            remote.push(' ');
            remote.push_str(&crate::sh_quote(arg));
        }
        Ok(
            CommandSpec::new(vagrant.filter(|s| !s.is_empty()).unwrap_or("vagrant")).args([
                "ssh".into(),
                machine,
                "--command".into(),
                remote,
            ]),
        )
    }
}
pub fn install_python_adapter(
    connection: Option<&Docker>,
    machine: &str,
    python: &str,
    vagrant: Option<&str>,
    root: &Path,
) -> anyhow::Result<std::path::PathBuf> {
    let mut command = python_command(connection, machine, python, &[], vagrant)?;
    let remote = if connection.is_none() {
        command.args.pop()
    } else {
        None
    };
    let directory = root.join(".rustpy/interpreters");
    std::fs::create_dir_all(&directory)?;
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (machine, python, format!("{connection:?}")).hash(&mut hash);
    let name = format!("deployment-target-{:x}", hash.finish());
    #[cfg(not(windows))]
    {
        let file = directory.join(name);
        let prefix = std::iter::once(command.program.as_str())
            .chain(command.args.iter().map(String::as_str))
            .map(crate::sh_quote)
            .collect::<Vec<_>>()
            .join(" ");
        let body = if let Some(remote) = remote {
            // Interpreters are launched from arbitrary working directories; pin
            // Vagrant to the project whose Vagrantfile defines the machine.
            format!(
                "VAGRANT_CWD={}\nexport VAGRANT_CWD\nquote() {{ printf \"'%s'\" \"$(printf '%s' \"$1\" | sed \"s/'/'\\\\\\\\''/g\")\"; }}\nremote={}\nfor arg do remote=\"$remote $(quote \"$arg\")\"; done\nexec {prefix} \"$remote\"\n",
                crate::sh_quote(&root.to_string_lossy()),
                crate::sh_quote(&remote)
            )
        } else {
            format!("exec {prefix} \"$@\"\n")
        };
        std::fs::write(&file, format!("#!/bin/sh\n{body}"))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        let file = directory.join(format!("{name}.cmd"));
        let ps = directory.join(format!("{name}.ps1"));
        let json = serde_json::to_string(&command.args)?.replace('\'', "''");
        let body = if let Some(remote) = remote {
            format!(
                "$env:VAGRANT_CWD='{}'\n$remote='{}'\nforeach($a in $args){{$remote += \" '\" + $a.Replace(\"'\",\"'\\''\") + \"'\"}}\n& '{}' @prefix $remote",
                root.to_string_lossy().replace('\'', "''"),
                remote.replace('\'', "''"),
                command.program.replace('\'', "''")
            )
        } else {
            format!("& '{}' @prefix @args", command.program.replace('\'', "''"))
        };
        std::fs::write(
            &ps,
            format!("$prefix=ConvertFrom-Json '{json}'\n{body}\nexit $LASTEXITCODE\n"),
        )?;
        std::fs::write(
            &file,
            format!(
                "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
                ps.display()
            ),
        )?;
        Ok(file)
    }
}
pub fn document_kind(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with("Dockerfile") || name.ends_with(".dockerfile") {
        Some("Dockerfile")
    } else if matches!(
        name,
        "compose.yaml" | "compose.yml" | "docker-compose.yaml" | "docker-compose.yml"
    ) {
        Some("Compose")
    } else {
        None
    }
}
pub fn document_command(root: &Path, path: &Path) -> anyhow::Result<Option<CommandSpec>> {
    let Some(kind) = document_kind(path) else {
        return Ok(None);
    };
    let configuration = Tools::load(root);
    let connection = configuration.default_connection();
    let command = if kind == "Dockerfile" {
        let name = root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let name = name.trim_matches(|ch| matches!(ch, '-' | '_' | '.'));
        let image = if name.is_empty() {
            "rustpy-image".into()
        } else {
            format!("{name}:latest")
        };
        connection.command_at(
            docker_args(
                "Build Dockerfile",
                &DockerArguments {
                    path: path.to_string_lossy().into_owned(),
                    image,
                    ..Default::default()
                },
                root,
            )?,
            root,
        )?
    } else {
        connection.command(docker_args(
            "Compose up",
            &DockerArguments {
                compose_file: path.to_string_lossy().into_owned(),
                ..Default::default()
            },
            root,
        )?)?
    };
    Ok(Some(command))
}
pub fn document_completions(text: &str, offset: usize, dockerfile: bool) -> (usize, Vec<String>) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut start = end;
    while start > 0
        && (text.as_bytes()[start - 1].is_ascii_alphanumeric()
            || matches!(text.as_bytes()[start - 1], b'_' | b'-'))
    {
        start -= 1;
    }
    let line = text[..start]
        .rsplit_once('\n')
        .map_or(&text[..start], |(_, line)| line);
    if !line.trim().is_empty() {
        return (start, Vec::new());
    }
    let prefix = &text[start..end];
    let candidates: &[&str] = if dockerfile {
        &[
            "FROM",
            "RUN",
            "CMD",
            "LABEL",
            "EXPOSE",
            "ENV",
            "ADD",
            "COPY",
            "ENTRYPOINT",
            "VOLUME",
            "USER",
            "WORKDIR",
            "ARG",
            "ONBUILD",
            "STOPSIGNAL",
            "HEALTHCHECK",
            "SHELL",
        ]
    } else {
        &[
            "services",
            "name",
            "version",
            "image",
            "build",
            "ports",
            "volumes",
            "networks",
            "environment",
            "depends_on",
            "command",
            "entrypoint",
            "restart",
            "healthcheck",
            "profiles",
            "secrets",
            "configs",
            "deploy",
            "env_file",
        ]
    };
    (
        start,
        candidates
            .iter()
            .filter(|item| item.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(|item| {
                if dockerfile {
                    format!("{item} ")
                } else {
                    format!("{item}: ")
                }
            })
            .collect(),
    )
}
#[cfg(test)]
mod document_fixtures {
    use super::*;
    #[test]
    fn dockerfile_and_compose_completion_replace_only_current_instruction() {
        let (start, items) = document_completions("FROM python\nCO", 14, true);
        assert_eq!(start, 12);
        assert!(items.contains(&"COPY ".into()));
        let (_, items) = document_completions("services:\n  web:\n    im", 23, false);
        assert!(items.contains(&"image: ".into()));
    }
}
const INTERPRETER_ENVIRONMENT: [&str; 4] = [
    "PYTHONUNBUFFERED",
    "PYTHONIOENCODING",
    "PYTHONUTF8",
    "PYTHONDONTWRITEBYTECODE",
];
pub fn image_python_command(
    connection: &Docker,
    image: &str,
    python: &str,
    root: &Path,
    remote_root: &str,
    args: &[String],
) -> anyhow::Result<CommandSpec> {
    let remote = if remote_root.is_empty() {
        "/work"
    } else {
        remote_root
    };
    let mut prefix = vec!["run".into(), "--rm".into(), "-i".into()];
    if let Some(resources) = &connection.resources {
        prefix.extend(resources.container_args().map_err(anyhow::Error::msg)?);
    }
    // `docker run` does not inherit the run configuration's interpreter
    // environment; unset names stay unset in the container.
    for name in INTERPRETER_ENVIRONMENT {
        prefix.extend(["--env".into(), name.into()]);
    }
    prefix.extend([
        "--volume".into(),
        format!("{}:{remote}", root.display()),
        "--workdir".into(),
        remote.into(),
        target(image)?,
    ]);
    if let Some(resources) = &connection.resources {
        prefix.extend(resources.process_prefix().map_err(anyhow::Error::msg)?);
    }
    prefix.push(target(python)?);
    prefix.extend(args.iter().cloned());
    // This SDK plan already includes its canonical resource/environment/nice
    // arguments. Preserve that existing route without applying them twice.
    connection.base_command(prefix)
}

pub fn compose_python_command(
    connection: &Docker,
    service: &str,
    python: &str,
    root: &Path,
    compose_file: &str,
    remote_root: &str,
    args: &[String],
) -> anyhow::Result<CommandSpec> {
    let mut prefix = vec![
        "compose".into(),
        "--project-directory".into(),
        root.to_string_lossy().into_owned(),
    ];
    if !compose_file.is_empty() {
        prefix.extend(["--file".into(), compose_file.into()]);
    }
    prefix.extend(["exec".into(), "-T".into()]);
    for name in INTERPRETER_ENVIRONMENT {
        prefix.extend(["--env".into(), name.into()]);
    }
    if !remote_root.is_empty() {
        prefix.extend(["--workdir".into(), remote_root.into()]);
    }
    prefix.extend([target(service)?, target(python)?]);
    prefix.extend(args.iter().cloned());
    connection.command(prefix)
}
/// `remote_root` is the container directory where `root` is visible (the image
/// SDK's volume, the Compose service's project mount): script, test-target and
/// other argument paths under `root` are rewritten to it before launch.
pub fn install_argv_adapter(
    command: &CommandSpec,
    root: &Path,
    label: &str,
    remote_root: Option<&str>,
) -> anyhow::Result<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (label, &command.program, &command.args, remote_root).hash(&mut hash);
    let name = format!("deployment-python-{:x}", hash.finish());
    let directory = root.join(".rustpy/interpreters");
    std::fs::create_dir_all(&directory)?;
    #[cfg(not(windows))]
    {
        let file = directory.join(name);
        let remote_root = remote_root.filter(|r| r.starts_with('/'));
        let mut previous = "";
        let prefix = std::iter::once(command.program.as_str())
            .chain(command.args.iter().map(String::as_str))
            .map(|arg| {
                // The container starts in the mapped launch directory, not
                // always at the mapped project root.
                let word =
                    if remote_root.is_some() && previous == "--workdir" && Some(arg) == remote_root
                    {
                        "\"$workdir\"".to_string()
                    } else {
                        crate::sh_quote(arg)
                    };
                previous = arg;
                word
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mapping = match remote_root {
            Some(remote) => format!(
                "caller=$PWD\n{}",
                rustpy_run::path_mapping_script(root, remote)
            ),
            None => String::new(),
        };
        std::fs::write(&file, format!("#!/bin/sh\n{mapping}exec {prefix} \"$@\"\n"))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        let file = directory.join(format!("{name}.cmd"));
        let ps = directory.join(format!("{name}.ps1"));
        let args = serde_json::to_string(&command.args)?.replace('\'', "''");
        std::fs::write(
            &ps,
            format!(
                "$prefix=ConvertFrom-Json '{args}'\n& '{}' @prefix @args\nexit $LASTEXITCODE\n",
                command.program.replace('\'', "''")
            ),
        )?;
        std::fs::write(
            &file,
            format!(
                "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
                ps.display()
            ),
        )?;
        Ok(file)
    }
}

#[cfg(test)]
mod container_resource_tests {
    use super::*;
    fn connection() -> Docker {
        Docker {
            executable: "/private/docker".into(),
            endpoint: "unix:///private/docker.sock".into(),
            resources: Some(DockerResources {
                cpuset_cpus: "2,4".into(),
                cpu_millis: 2000,
                threads: 2,
                nice: 10,
                nice_program: "/usr/bin/nice".into(),
                name: "owned".into(),
                labels: Default::default(),
            }),
            ..Default::default()
        }
    }
    #[test]
    fn deployment_run_and_create_apply_policy_while_read_only_and_sdk_routes_are_unchanged() {
        let connection = connection();
        for operation in ["Run image", "Create container"] {
            let args = DockerArguments {
                image: "python:3.12".into(),
                command: vec!["/usr/local/bin/python3".into(), "-V".into()],
                ..Default::default()
            };
            let command = connection
                .command(docker_args(operation, &args, Path::new("/project")).unwrap())
                .unwrap();
            assert_eq!(command.program, "/private/docker");
            assert!(command
                .args
                .starts_with(&["--host".into(), "unix:///private/docker.sock".into()]));
            assert_eq!(
                command
                    .args
                    .iter()
                    .filter(|value| value.as_str() == "--cpu-quota")
                    .count(),
                1
            );
            assert!(command
                .args
                .windows(2)
                .any(|pair| pair == ["--entrypoint", "/usr/bin/nice"]));
        }
        assert_eq!(
            connection.command(vec!["version".into()]).unwrap().args,
            vec!["--host", "unix:///private/docker.sock", "version"]
        );
        let sdk = image_python_command(
            &connection,
            "python:3.12",
            "python3",
            Path::new("/project"),
            "/work",
            &["-V".into()],
        )
        .unwrap();
        assert_eq!(
            sdk.args
                .iter()
                .filter(|value| value.as_str() == "--cpu-quota")
                .count(),
            1
        );
        assert!(sdk.args.ends_with(&[
            "python:3.12".into(),
            "/usr/bin/nice".into(),
            "-n".into(),
            "10".into(),
            "--".into(),
            "python3".into(),
            "-V".into()
        ]));
        assert!(connection
            .command(vec!["build".into(), ".".into()])
            .is_err());
        assert!(connection
            .command(vec!["run".into(), "image".into()])
            .is_err());
        assert!(connection
            .command(vec![
                "run".into(),
                "--unknown".into(),
                "image".into(),
                "python".into()
            ])
            .is_err());
    }
}

#[cfg(test)]
mod sdk_fixtures {
    use super::*;
    #[test]
    fn image_and_compose_sdks_have_real_path_mapping_and_service_argv() {
        let docker = Docker::default();
        let image = image_python_command(
            &docker,
            "python:3",
            "python3",
            Path::new("/project"),
            "/work",
            &["-V".into()],
        )
        .unwrap();
        assert!(image.args.contains(&"/project:/work".into()));
        assert!(image
            .args
            .ends_with(&["python:3".into(), "python3".into(), "-V".into()]));
        let compose = compose_python_command(
            &docker,
            "api",
            "python3",
            Path::new("/project"),
            "compose.yaml",
            "/app",
            &["-V".into()],
        )
        .unwrap();
        assert!(compose
            .args
            .ends_with(&["api".into(), "python3".into(), "-V".into()]));
        assert!(compose.args.contains(&"-T".into()));
    }
    #[cfg(unix)]
    #[test]
    fn image_sdk_adapter_maps_script_paths_and_launch_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "rustpy-argv-adapter-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let root = directory.join("project 'π'");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let cli = directory.join("docker");
        std::fs::write(&cli, "#!/bin/sh\nprintf '%s\\000' \"$@\"\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let docker = Docker {
            executable: cli.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let command =
            image_python_command(&docker, "python:3", "python3", &root, "/work", &[]).unwrap();
        let adapter = install_argv_adapter(&command, &root, "Docker image", Some("/work")).unwrap();
        let output = std::process::Command::new(adapter)
            .arg(root.join("pkg/main.py"))
            .arg("unrelated")
            .current_dir(root.join("pkg"))
            .output()
            .unwrap();
        let argv: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect();
        let _ = std::fs::remove_dir_all(&directory);
        assert!(
            argv.windows(2).any(|w| w == ["--workdir", "/work/pkg"]),
            "{argv:?}"
        );
        assert!(argv.ends_with(&[
            "python:3".into(),
            "python3".into(),
            "/work/pkg/main.py".into(),
            "unrelated".into()
        ]));
        assert!(argv.windows(2).any(|w| w == ["--env", "PYTHONUNBUFFERED"]));
    }
    #[test]
    fn vagrant_destruction_and_box_management_are_explicit() {
        assert_eq!(
            vagrant_args("Destroy", "web", "").unwrap(),
            vec!["destroy", "--force", "web"]
        );
        assert_eq!(
            vagrant_args("Add box", "", "ubuntu/noble64").unwrap(),
            vec!["box", "add", "ubuntu/noble64"]
        );
    }
}

#[cfg(test)]
mod legacy_build_policy_tests {
    use super::*;
    #[test]
    fn build_policy_is_captured_without_changing_defaults() {
        let args = vec![
            "build".into(),
            "--tag".into(),
            "owned:fixture".into(),
            ".".into(),
        ];
        let ordinary = Docker::default().command(args.clone()).unwrap();
        assert_eq!(ordinary.args, args);
        assert!(ordinary.env.is_empty());
        let docker = Docker {
            legacy_build_resources: Some(LegacyDockerBuildResources {
                cpuset_cpus: "0,1".into(),
                cpu_millis: 2000,
            }),
            ..Default::default()
        };
        let root = std::env::current_dir().unwrap();
        let command = docker.command_at(args, &root).unwrap();
        assert_eq!(
            command.env.get("DOCKER_BUILDKIT").map(String::as_str),
            Some("0")
        );
        assert!(command.args.iter().any(|arg| arg == "--force-rm"));
        assert!(docker.command(command.args).is_err());
    }
}

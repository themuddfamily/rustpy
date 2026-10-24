//! Provider identities, marker detection and real integration setup commands.
//! Provider operation windows live independently from project directory mappings.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Git,
    Mercurial,
    Subversion,
    Perforce,
    Tfs,
}
impl Provider {
    pub const ALL: [Self; 5] = [
        Self::Git,
        Self::Mercurial,
        Self::Subversion,
        Self::Perforce,
        Self::Tfs,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::Mercurial => "Mercurial",
            Self::Subversion => "Subversion",
            Self::Perforce => "Perforce",
            Self::Tfs => "TFS / TFVC (optional plugin)",
        }
    }
    pub fn marker(self) -> &'static str {
        match self {
            Self::Git => ".git",
            Self::Mercurial => ".hg",
            Self::Subversion => ".svn",
            Self::Perforce => ".p4config",
            Self::Tfs => "$tf",
        }
    }
    pub fn detect(self, path: &Path) -> bool {
        match self {
            Self::Git => path.join(".git").exists(),
            Self::Mercurial => path.join(".hg").is_dir(),
            Self::Tfs => path.join("$tf").is_dir(),
            Self::Subversion => {
                path.join(".svn/wc.db").is_file() || path.join(".svn/entries").is_file()
            }
            Self::Perforce => {
                if path.join(".p4root").is_dir() {
                    return true;
                }
                let configured = std::env::var("P4CONFIG")
                    .ok()
                    .filter(|s| Path::new(s).components().count() == 1);
                for name in [
                    Some(".p4config"),
                    Some("p4config.txt"),
                    configured.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    if !std::fs::symlink_metadata(path.join(name)).is_ok_and(|m| {
                        m.is_file() && !m.file_type().is_symlink() && m.len() <= 64 * 1024
                    }) {
                        continue;
                    }
                    if std::fs::File::open(path.join(name))
                        .ok()
                        .is_some_and(|file| {
                            let mut text = String::new();
                            file.take(64 * 1024).read_to_string(&mut text).is_ok()
                                && text
                                    .lines()
                                    .any(|line| line.trim_start().starts_with("P4CLIENT="))
                        })
                    {
                        return true;
                    }
                }
                false
            }
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Integration {
    pub provider: Provider,
    pub root: PathBuf,
    /// Empty means the provider's usual executable on PATH.
    pub executable: String,
    /// SVN repository URL, or Perforce server's host:port.
    pub endpoint: String,
    pub create_local: bool,
    /// Required when creating a local SVN repository; never placed inside its checkout.
    pub repository: PathBuf,
    pub workspace: String,
    pub user: String,
    pub depot: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub stdin: Option<String>,
}
fn field(value: &str, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.contains(['\r', '\n', '\0']),
        "{label} must contain one line"
    );
    Ok(())
}
fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!(
        "file://{}{}",
        if encoded.starts_with('/') { "" } else { "/" },
        encoded
    )
}
impl Integration {
    pub fn commands(&self) -> anyhow::Result<Vec<SetupCommand>> {
        field(&self.root.to_string_lossy(), "Directory")?;
        field(&self.repository.to_string_lossy(), "Repository directory")?;
        anyhow::ensure!(
            self.root.is_absolute(),
            "The directory mapping must be an absolute path"
        );
        for (value, label) in [
            (&self.endpoint, "Server"),
            (&self.workspace, "Workspace"),
            (&self.user, "User"),
            (&self.depot, "Depot"),
        ] {
            field(value, label)?;
        }
        let exe = |default: &str| {
            if self.executable.trim().is_empty() {
                default.to_string()
            } else {
                self.executable.trim().to_string()
            }
        };
        let command = |executable: String, args: Vec<String>, stdin: Option<String>| SetupCommand {
            executable,
            args,
            cwd: self.root.clone(),
            stdin,
        };
        match self.provider {
            Provider::Tfs => crate::other_vcs::tfs::integration(self),
            Provider::Git => Ok(Vec::new()), // libgit2 initializes this provider without a child process.
            Provider::Mercurial => Ok(vec![command(exe("hg"), vec!["init".into()], None)]),
            Provider::Subversion => {
                let mut commands = Vec::new();
                let url = if self.create_local {
                    anyhow::ensure!(
                        self.repository.is_absolute()
                            && !self.repository.starts_with(&self.root)
                            && !self.root.starts_with(&self.repository),
                        "Choose a separate absolute SVN repository directory outside the checkout"
                    );
                    let svnadmin = Path::new(&exe("svn"))
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| {
                            p.join(if cfg!(windows) {
                                "svnadmin.exe"
                            } else {
                                "svnadmin"
                            })
                        })
                        .unwrap_or_else(|| PathBuf::from("svnadmin"));
                    commands.push(command(
                        svnadmin.to_string_lossy().into_owned(),
                        vec![
                            "create".into(),
                            self.repository.to_string_lossy().into_owned(),
                        ],
                        None,
                    ));
                    file_url(&self.repository)
                } else {
                    anyhow::ensure!(
                        !self.endpoint.trim().is_empty(),
                        "Enter an SVN repository URL"
                    );
                    self.endpoint.trim().to_string()
                };
                commands.push(command(
                    exe("svn"),
                    vec![
                        "checkout".into(),
                        "--non-interactive".into(),
                        "--".into(),
                        url,
                        ".".into(),
                    ],
                    None,
                ));
                Ok(commands)
            }
            Provider::Perforce if self.create_local => Ok(vec![command(
                exe("p4"),
                vec![
                    "-d".into(),
                    self.root.to_string_lossy().into_owned(),
                    "init".into(),
                ],
                None,
            )]),
            Provider::Perforce => {
                anyhow::ensure!(
                    !self.endpoint.trim().is_empty() && !self.user.trim().is_empty(),
                    "Enter the Perforce server and user"
                );
                anyhow::ensure!(
                    !self.workspace.is_empty()
                        && self
                            .workspace
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
                    "A workspace name must contain letters, numbers, dot, dash or underscore"
                );
                let depot = if self.depot.trim().is_empty() {
                    "//depot/..."
                } else {
                    self.depot.trim()
                };
                anyhow::ensure!(
                    depot.starts_with("//")
                        && depot.ends_with("/...")
                        && !depot.contains(char::is_whitespace),
                    "Enter a depot mapping such as //depot/..."
                );
                let spec=format!("Client: {}\nOwner: {}\nRoot: {}\nOptions: noallwrite noclobber nocompress unlocked nomodtime normdir\nLineEnd: local\nView:\n\t{} //{}/...\n",self.workspace,self.user,self.root.display(),depot,self.workspace);
                Ok(vec![command(
                    exe("p4"),
                    vec![
                        "-p".into(),
                        self.endpoint.clone(),
                        "-u".into(),
                        self.user.clone(),
                        "client".into(),
                        "-i".into(),
                    ],
                    Some(spec),
                )])
            }
        }
    }
    /// Invoked only by an explicit Enable Integration action, on a worker.
    pub fn enable(&self) -> anyhow::Result<String> {
        anyhow::ensure!(self.provider != Provider::Tfs, "Use the supervised TFVC workspace setup workflow (Initialize / MapWorkspace), not the legacy synchronous integration helper");
        let reservation = crate::RepositoryReservation::try_acquire(
            &self.root,
            "Enable version control integration",
        )?;
        reservation.run(|| self.enable_owned())
    }
    fn enable_owned(&self) -> anyhow::Result<String> {
        anyhow::ensure!(self.root.is_dir(), "The mapping directory does not exist");
        if self.provider.detect(&self.root) {
            return Ok(format!(
                "{} directory mapping registered",
                self.provider.label()
            ));
        }
        if self.provider == Provider::Git {
            git2::Repository::init(&self.root)?;
            return Ok("Git integration enabled".into());
        }
        let commands = self.commands()?;
        let mut output = Vec::new();
        for command in commands {
            output.push(command.run(Duration::from_secs(60))?);
        }
        if self.provider == Provider::Perforce && !self.create_local {
            let config = self.root.join(".p4config");
            anyhow::ensure!(!config.exists(),"The workspace was created, but .p4config already exists; preserve it and add the directory mapping manually");
            std::fs::write(
                config,
                format!(
                    "P4PORT={}\nP4USER={}\nP4CLIENT={}\n",
                    self.endpoint, self.user, self.workspace
                ),
            )?;
        }
        Ok(format!(
            "{} integration enabled\n{}",
            self.provider.label(),
            output.join("\n")
        ))
    }
}
impl SetupCommand {
    fn run(&self, timeout: Duration) -> anyhow::Result<String> {
        let mut command = Command::new(&self.executable);
        crate::job::detach(&mut command);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let capture = |stream: Option<std::process::ChildStdout>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut stream) = stream {
                    let _ = stream.by_ref().take(1024 * 1024).read_to_string(&mut text);
                    let _ = std::io::copy(&mut stream, &mut std::io::sink());
                }
                text
            })
        };
        let out = capture(stdout);
        let err = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stderr {
                let _ = stream.by_ref().take(1024 * 1024).read_to_string(&mut text);
                let _ = std::io::copy(&mut stream, &mut std::io::sink());
            }
            text
        });
        if let (Some(input), Some(mut stdin)) = (&self.stdin, child.stdin.take()) {
            if let Err(error) = stdin.write_all(input.as_bytes()) {
                crate::job::kill_hard(&mut child);
                let _ = child.wait();
                return Err(error.into());
            }
        }
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > timeout {
                crate::job::kill_hard(&mut child);
                let _ = child.wait();
                anyhow::bail!(
                    "{} setup timed out; inspect the partially created repository before retrying",
                    self.executable
                );
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let drain = Instant::now();
        while (!out.is_finished() || !err.is_finished()) && drain.elapsed() < Duration::from_secs(2)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        anyhow::ensure!(out.is_finished() && err.is_finished(),"{} exited but a helper kept its output streams open; inspect the working copy before retrying setup",self.executable);
        let output = format!(
            "{}{}",
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default()
        );
        anyhow::ensure!(
            status.success(),
            "{} setup failed: {}",
            self.executable,
            output.trim()
        );
        Ok(output)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn svn_setup_creates_a_separate_repository_and_checkout() {
        let plan = Integration {
            provider: Provider::Subversion,
            root: PathBuf::from("/project/work"),
            repository: PathBuf::from("/repositories/project space"),
            create_local: true,
            ..Default::default()
        };
        let commands = plan.commands().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].args[0], "create");
        assert!(commands[1]
            .args
            .contains(&"file:///repositories/project%20space".into()));
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn perforce_setup_does_not_put_credentials_in_the_client_spec() {
        let plan = Integration {
            provider: Provider::Perforce,
            root: PathBuf::from("/project/work"),
            endpoint: "ssl:p4.example:1666".into(),
            workspace: "alice-work".into(),
            user: "alice".into(),
            ..Default::default()
        };
        let commands = plan.commands().unwrap();
        let spec = commands[0].stdin.as_ref().unwrap();
        assert!(spec.contains("//depot/... //alice-work/..."));
        assert!(!spec.contains("Password"));
        assert!(Integration {
            workspace: "bad\nView: //...".into(),
            ..plan
        }
        .commands()
        .is_err());
    }
    #[test]
    fn non_git_markers_are_detected_without_running_providers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".hg")).unwrap();
        assert!(Provider::Mercurial.detect(dir.path()));
        assert!(!Provider::Perforce.detect(dir.path()));
        std::fs::write(
            dir.path().join(".p4config"),
            "P4PORT=localhost:1666\nP4CLIENT=fixture\n",
        )
        .unwrap();
        assert!(Provider::Perforce.detect(dir.path()));
    }
}
